//! Inserting into a latched tree, as the unsync tree inserts
//! (`tree/insert.rs`).
//!
//! - **An equal key** the tree holds already stays in place. Its value is
//!   changed by a closure that takes the new one ([`UpdateOrInsert`]; std's
//!   `BTreeMap::insert` replaces it) or left as it is ([`ReadOrInsert`],
//!   which reads the entry instead): the insert's [`InsertPolicy`] decides.
//! - **An append** to the last leaf latches that leaf alone, if the key goes
//!   after every key there and the leaf has room (`append.rs`, D45).
//! - **Otherwise** a descent finds the leaf where the key belongs and
//!   latches it exclusively; the key goes in if the leaf has room. A key
//!   that lacks what every key shares first shrinks the shared prefix, and
//!   descends again (`shared_prefix.rs`, D50).
//! - **A full leaf splits.** The insert descends again, keeping its path,
//!   and takes, without waiting, every node the split changes or reads a
//!   separator of:
//!   - the path from the leaf's parent up to the lowest node with room,
//!     which takes the separator;
//!   - the path from that node up to the nearest nodes holding its fences,
//!     which the halves' prefixes are fit to (`tree/prefix.rs`).
//!
//!   Then it takes every page the split allocates, and runs
//!   [`NodeStackMut::insert_prefixed`] along them. The pages come latched,
//!   and stay so until it is done (`pages.rs`). A split of the last leaf
//!   points appends at the new last leaf.
//! - **A failed allocation** fails the insert before it changes anything,
//!   and gives the entry back ([`InsertError`]): the root page of a new
//!   tree, or a page a split takes.
//!
//! [`NodeStackMut::insert_prefixed`]: super::super::node_stack::NodeStackMut::insert_prefixed

use core::alloc::Layout;

use super::super::insert::SplitPages;
use super::super::interleaving::Backoff;
use super::super::node::Node;
use super::super::node::annexed_leaf::AnnexedLeaf;
use super::super::node::inline_keys::SearchKey;
use super::super::node::internal::Internal;
use super::super::node::latch::Latched;
use super::super::node::leaf::Leaf;
use super::super::node::page::PageAllocator;
use super::super::node_stack::LastEdge;
use super::super::prefix::PrefixedKey;
use super::LatchedTree;
use super::descent::{Exclusive, HeldLeaf, TreePath};
use super::held::HeldNodes;
use super::pages::NodePages;
use crate::allocator::{Allocator, InsertError};
use crate::inline_key::InlineKeyStrategy;

/// What an insert does where the tree holds a key equal to its own, and
/// where it holds none, in the leaf it holds exclusively, with the leaf's
/// annex (`node/annexed_leaf.rs`); and what it returns. Either may refuse,
/// for want of a page of the layout it gives back, having changed nothing:
/// the insert then gives the entry back. A map's policies never refuse.
pub(crate) trait InsertPolicy<K, V, S: InlineKeyStrategy<K>> {
    type Output;

    /// The tree holds `stored`, equal to `key`, with `stored_value`, in
    /// `leaf`.
    fn occupied(
        self,
        leaf: AnnexedLeaf<'_, K, V, S>,
        key: K,
        value: V,
        stored: &K,
        stored_value: &mut V,
    ) -> Result<Self::Output, (K, V, Layout)>;

    /// `key` and `value` go into `leaf` right after this call, unless it
    /// refuses: the insert can no longer fail.
    fn vacant(
        self,
        leaf: AnnexedLeaf<'_, K, V, S>,
        key: &K,
        value: &V,
    ) -> Result<Self::Output, Layout>;
}

/// Runs a closure on the stored entry, its value exclusive, and the new
/// value, which the closure takes; the stored key stays. Returns what the
/// closure returned, or `None` for an entry inserted. std's
/// `BTreeMap::insert` is the closure that replaces the value.
pub(crate) struct UpdateOrInsert<F>(pub(crate) F);

impl<K, V, S: InlineKeyStrategy<K>, R, F: FnOnce(&K, &mut V, V) -> R> InsertPolicy<K, V, S>
    for UpdateOrInsert<F>
{
    type Output = Option<R>;

    #[inline(always)]
    fn occupied(
        self,
        _leaf: AnnexedLeaf<'_, K, V, S>,
        key: K,
        value: V,
        stored: &K,
        stored_value: &mut V,
    ) -> Result<Option<R>, (K, V, Layout)> {
        // As std's insert, the new key is dropped before the value changes.
        drop(key);
        Ok(Some((self.0)(stored, stored_value, value)))
    }

    #[inline(always)]
    fn vacant(
        self,
        _leaf: AnnexedLeaf<'_, K, V, S>,
        _key: &K,
        _value: &V,
    ) -> Result<Option<R>, Layout> {
        Ok(None)
    }
}

/// Runs a closure on the key's entry: the stored one, left as it is, or the
/// one inserted.
pub(crate) struct ReadOrInsert<F>(pub(crate) F);

impl<K, V, S: InlineKeyStrategy<K>, R, F: FnOnce(&K, &V) -> R> InsertPolicy<K, V, S>
    for ReadOrInsert<F>
{
    type Output = R;

    #[inline(always)]
    fn occupied(
        self,
        _leaf: AnnexedLeaf<'_, K, V, S>,
        key: K,
        value: V,
        stored: &K,
        stored_value: &mut V,
    ) -> Result<R, (K, V, Layout)> {
        drop((key, value));
        Ok((self.0)(stored, stored_value))
    }

    #[inline(always)]
    fn vacant(self, _leaf: AnnexedLeaf<'_, K, V, S>, key: &K, value: &V) -> Result<R, Layout> {
        Ok((self.0)(key, value))
    }
}

/// Runs the first closure on the stored entry, the second on the new one,
/// each with the leaf and its annex; either may refuse. The new key and
/// value are dropped once the first runs.
pub(crate) struct WithLeaf<O, N>(pub(crate) O, pub(crate) N);

impl<K, V, S, R, O, N> InsertPolicy<K, V, S> for WithLeaf<O, N>
where
    S: InlineKeyStrategy<K>,
    O: FnOnce(AnnexedLeaf<'_, K, V, S>, &K, &mut V) -> Result<R, Layout>,
    N: FnOnce(AnnexedLeaf<'_, K, V, S>, &K, &V) -> Result<R, Layout>,
{
    type Output = R;

    #[inline(always)]
    fn occupied(
        self,
        leaf: AnnexedLeaf<'_, K, V, S>,
        key: K,
        value: V,
        stored: &K,
        stored_value: &mut V,
    ) -> Result<R, (K, V, Layout)> {
        match (self.0)(leaf, stored, stored_value) {
            Ok(output) => {
                drop((key, value));
                Ok(output)
            }
            Err(layout) => Err((key, value, layout)),
        }
    }

    #[inline(always)]
    fn vacant(self, leaf: AnnexedLeaf<'_, K, V, S>, key: &K, value: &V) -> Result<R, Layout> {
        (self.1)(leaf, key, value)
    }
}

/// Why [`LatchedTree::insert_into`] didn't insert.
pub(super) enum NotInserted<K, V, P> {
    /// The leaf is full: the key goes at this edge, once it splits.
    Full(K, V, P, usize),
    /// The policy refused.
    Refused(InsertError<K, V>),
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Inserts `key` and `value`, or, if an equal key is there, keeps it and
    /// does what `policy` does; returns what `policy` returns. Pages come
    /// from `alloc`; if it has none to give, the tree is unchanged, and the
    /// entry comes back.
    #[inline]
    pub(crate) fn insert<P: InsertPolicy<K, V, S>>(
        &self,
        key: K,
        value: V,
        policy: P,
        alloc: &impl Allocator,
    ) -> Result<P::Output, InsertError<K, V>>
    where
        K: Ord,
    {
        if self.root_page().is_none()
            && let Err(layout) = self.start_tree(alloc)
        {
            return Err(InsertError::new(key, value, layout));
        }
        let (mut key, value, policy) = match self.append(key, value, policy) {
            Ok(appended) => return appended,
            Err(entry) => entry,
        };
        loop {
            let Ok((held, at_separator)) =
                self.descend::<K, Exclusive, ()>(&SearchKey::new(&key), &mut ())
            else {
                key = self.shrink_shared_prefix_for(key, alloc);
                continue;
            };
            return match self.insert_into(&held, at_separator, key, value, policy, alloc) {
                Ok(output) => Ok(output),
                Err(NotInserted::Refused(error)) => Err(error),
                Err(NotInserted::Full(key, value, policy, _)) => {
                    drop(held);
                    self.insert_splitting(key, value, policy, alloc)
                }
            };
        }
    }

    /// Inserts into `held`, the leaf where `key` belongs, as [`Self::insert`]
    /// does, if it holds `key` or has room; or gives all back, with the edge
    /// where `key` goes in the full leaf ([`NotInserted`]).
    #[inline(always)]
    pub(super) fn insert_into<P: InsertPolicy<K, V, S>>(
        &self,
        held: &HeldLeaf<K, V, S, Exclusive>,
        at_separator: bool,
        key: K,
        value: V,
        policy: P,
        alloc: &impl PageAllocator,
    ) -> Result<P::Output, NotInserted<K, V, P>>
    where
        K: Ord,
    {
        let leaf = held.leaf();
        let (stored, stored_value) = if at_separator {
            (leaf.head_key(), leaf.head_value_mut())
        } else {
            match leaf.search_always_inlined(&SearchKey::new(&key)) {
                Ok(index) => (leaf.key(index), leaf.value_mut(index)),
                Err(edge) if leaf.len() < Leaf::<K, V, Latched<S>>::capacity() => {
                    let output = match policy.vacant(AnnexedLeaf::new(leaf), &key, &value) {
                        Ok(output) => output,
                        Err(layout) => {
                            return Err(NotInserted::Refused(InsertError::new(key, value, layout)));
                        }
                    };
                    let key = self.prefixed(key, leaf, alloc);
                    leaf.insert_fit(edge, key.window_at(leaf.prefix_len()), value);
                    return Ok(output);
                }
                Err(edge) => return Err(NotInserted::Full(key, value, policy, edge)),
            }
        };
        policy
            .occupied(AnnexedLeaf::new(leaf), key, value, stored, stored_value)
            .map_err(|(key, value, layout)| {
                NotInserted::Refused(InsertError::new(key, value, layout))
            })
    }

    /// `key`, bound for `leaf`, which the caller holds exclusively; the
    /// descent found that `key` shares what every key of the tree shares.
    /// A root leaf is the tree's only node and lies on every border, so
    /// holding it holds the shared prefix. For a root leaf, the unsync
    /// tree's [`Tree::shrink_shared_prefix_for`] runs, as on every insert
    /// there: it starts the prefix in a tree that had no key, or shrinks it.
    /// `alloc` is the map's.
    ///
    /// [`Tree::shrink_shared_prefix_for`]: super::super::root::Tree::shrink_shared_prefix_for
    #[inline(always)]
    fn prefixed(
        &self,
        key: K,
        leaf: Leaf<K, V, Latched<S>>,
        alloc: &impl PageAllocator,
    ) -> PrefixedKey<K> {
        if self.is_root(leaf) {
            self.change_held(|tree| tree.shrink_shared_prefix_for(key, alloc))
        } else {
            PrefixedKey::checked_by_descent(key)
        }
    }

    /// [`Self::insert`] where the leaf was full: searches again, keeping
    /// the internal nodes it goes through, and splits, until it inserts or
    /// `alloc` has no page for the split.
    #[inline(never)]
    pub(super) fn insert_splitting<P: InsertPolicy<K, V, S>>(
        &self,
        mut key: K,
        mut value: V,
        mut policy: P,
        alloc: &impl Allocator,
    ) -> Result<P::Output, InsertError<K, V>>
    where
        K: Ord,
    {
        // Declared outside the loop, so it drops last, after the nodes each
        // split held: see `NodePages`.
        let pages = self.pages(alloc);
        let mut backoff = Backoff::new();
        loop {
            let mut path = TreePath::new();
            let Ok((held, at_separator)) =
                self.descend::<K, Exclusive, _>(&SearchKey::new(&key), &mut path)
            else {
                key = self.shrink_shared_prefix_for(key, alloc);
                continue;
            };
            let edge;
            (key, value, policy, edge) =
                match self.insert_into(&held, at_separator, key, value, policy, alloc) {
                    Ok(output) => return Ok(output),
                    Err(NotInserted::Refused(error)) => return Err(error),
                    Err(NotInserted::Full(key, value, policy, edge)) => (key, value, policy, edge),
                };
            match self.split(&path, &held, edge, key, value, policy, &pages) {
                Ok(inserted) => return inserted,
                Err(entry) => (key, value, policy) = entry,
            }
            // Another thread holds a node the split needs.
            backoff.wait();
        }
    }

    /// Splits `held`, the full leaf `path` leads to, and inserts `key` and
    /// `value` at its `edge`, with `NodeStackMut::insert_prefixed`; or, if
    /// `pages` has none for the split, or the policy refuses, changes
    /// nothing and gives the entry back. The split goes up as far as it must, and the halves' prefixes
    /// are fit to their fences. If a node it takes is held by another
    /// thread or changed since the descent read it, it changes nothing and
    /// gives all back, to try again.
    #[expect(
        clippy::too_many_arguments,
        reason = "a refused split gives the entry and its policy back"
    )]
    #[expect(
        clippy::type_complexity,
        reason = "the insert's outcome, or all back to try again"
    )]
    fn split<P: InsertPolicy<K, V, S>>(
        &self,
        path: &TreePath<K, V, S>,
        held: &HeldLeaf<K, V, S, Exclusive>,
        edge: usize,
        key: K,
        value: V,
        policy: P,
        pages: &NodePages<'_, impl Allocator>,
    ) -> Result<Result<P::Output, InsertError<K, V>>, (K, V, P)>
    where
        K: Ord,
    {
        // The lowest node with room takes the split's separator; every node
        // below it splits. Without one, the root splits, and the tree grows.
        let room = (0..path.depth())
            .rev()
            .find(|&level| path.level(level).routed.len < Internal::<K, V, Latched<S>>::capacity());
        let top = path.fences_top(room.unwrap_or(0));
        let mut held_nodes = HeldNodes::new();
        if !held_nodes.take_path(path, top) {
            return Err((key, value, policy));
        }
        // The pages the split allocates, which the nodes it holds tell.
        if let Err(layout) = pages.reserve(SplitPages::of_full_leaf(path.depth(), room)) {
            return Ok(Err(InsertError::new(key, value, layout)));
        }
        let leaf = held.leaf();
        let output = match policy.vacant(AnnexedLeaf::new(leaf), &key, &value) {
            Ok(output) => output,
            Err(layout) => {
                pages.unreserve();
                return Ok(Err(InsertError::new(key, value, layout)));
            }
        };
        let was_last_leaf = self.is_rightmost_leaf(leaf);
        let key = self.prefixed(key, leaf, pages);
        self.change_held(|tree| {
            // A latched tree keeps no entry count.
            let mut uncounted = 0;
            path.node_stack_to_leaf(tree, top, leaf, edge)
                .insert_prefixed(key, value, pages, &mut uncounted);
        });
        if was_last_leaf {
            // The new last leaf is the last under the node that took the
            // separator, or under the root if none did. The split holds that
            // node, and every node down its last edges.
            let took_separator = match room {
                Some(level) => Node::Internal(path.level(level).node),
                None => self.root_page().expect("a tree that split").node(),
            };
            self.point_appends_to(took_separator.border_leaf::<LastEdge>());
        }
        drop(held_nodes);
        Ok(Ok(output))
    }
}
