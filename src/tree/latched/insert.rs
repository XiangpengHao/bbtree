//! Inserting into a latched tree, as the unsync tree inserts
//! (`tree/insert.rs`).
//!
//! - **An equal key** the tree holds already stays in place. Its value is
//!   replaced ([`Replace`], as std's `BTreeMap::insert` does) or left as it
//!   is ([`ReadOrInsert`], which reads the entry instead): the insert's
//!   [`InsertPolicy`] decides.
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
//!   Then it runs [`NodeStackMut::insert_prefixed`] along them. The pages
//!   it allocates come latched, and stay so until it is done (`pages.rs`).
//!   A split of the last leaf points appends at the new last leaf.
//!
//! [`NodeStackMut::insert_prefixed`]: super::super::node_stack::NodeStackMut::insert_prefixed

use core::mem;

use super::super::interleaving::Interleaving;
use super::super::node::Node;
use super::super::node::inline_keys::SearchKey;
use super::super::node::internal::Internal;
use super::super::node::latch::Latched;
use super::super::node::leaf::Leaf;
use super::super::node_stack::LastEdge;
use super::super::prefix::PrefixedKey;
use super::LatchedTree;
use super::descent::{Exclusive, HeldLeaf, TreePath};
use super::held::HeldNodes;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// What an insert does where the tree holds a key equal to its own, and
/// what it returns.
pub(crate) trait InsertPolicy<K, V> {
    type Output;

    /// The tree holds `stored`, equal to `key`, with `stored_value`, in a
    /// leaf the insert holds exclusively.
    fn occupied(self, key: K, value: V, stored: &K, stored_value: &mut V) -> Self::Output;

    /// `key` and `value` go in right after this call, into a leaf the insert
    /// holds exclusively: the insert can no longer fail.
    fn vacant(self, key: &K, value: &V) -> Self::Output;
}

/// std's `BTreeMap::insert`: the stored key stays, its value is replaced,
/// and the old one returned.
pub(crate) struct Replace;

impl<K, V> InsertPolicy<K, V> for Replace {
    type Output = Option<V>;

    #[inline(always)]
    fn occupied(self, key: K, value: V, _stored: &K, stored_value: &mut V) -> Option<V> {
        // As std, the new key is dropped before the value changes.
        drop(key);
        Some(mem::replace(stored_value, value))
    }

    #[inline(always)]
    fn vacant(self, _key: &K, _value: &V) -> Option<V> {
        None
    }
}

/// Runs a closure on the key's entry: the stored one, left as it is, or the
/// one inserted.
pub(crate) struct ReadOrInsert<F>(pub(crate) F);

impl<K, V, R, F: FnOnce(&K, &V) -> R> InsertPolicy<K, V> for ReadOrInsert<F> {
    type Output = R;

    #[inline(always)]
    fn occupied(self, key: K, value: V, stored: &K, stored_value: &mut V) -> R {
        drop((key, value));
        (self.0)(stored, stored_value)
    }

    #[inline(always)]
    fn vacant(self, key: &K, value: &V) -> R {
        (self.0)(key, value)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Inserts `key` and `value`, or, if an equal key is there, keeps it and
    /// does what `policy` does; returns what `policy` returns. Pages come
    /// from `alloc`.
    #[inline]
    pub(crate) fn insert<P: InsertPolicy<K, V>>(
        &self,
        key: K,
        value: V,
        policy: P,
        alloc: &impl Allocator,
    ) -> P::Output
    where
        K: Ord,
    {
        if self.root_page().is_none() {
            self.start_tree(&self.pages(alloc));
        }
        let (mut key, value, policy) = match self.append(key, value, policy) {
            Ok(output) => return output,
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
                Ok(output) => output,
                Err((key, value, policy, _)) => {
                    drop(held);
                    self.insert_splitting(key, value, policy, alloc)
                }
            };
        }
    }

    /// Inserts into `held`, the leaf where `key` belongs, as [`Self::insert`]
    /// does, if it holds `key` or has room; or gives all back, with the edge
    /// where `key` goes in the full leaf.
    #[inline(always)]
    pub(super) fn insert_into<P: InsertPolicy<K, V>>(
        &self,
        held: &HeldLeaf<K, V, S, Exclusive>,
        at_separator: bool,
        key: K,
        value: V,
        policy: P,
        alloc: &impl Allocator,
    ) -> Result<P::Output, (K, V, P, usize)>
    where
        K: Ord,
    {
        let leaf = held.leaf();
        if at_separator {
            return Ok(policy.occupied(key, value, leaf.head_key(), leaf.head_value_mut()));
        }
        match leaf.search_always_inlined(&SearchKey::new(&key)) {
            Ok(index) => Ok(policy.occupied(key, value, leaf.key(index), leaf.value_mut(index))),
            Err(edge) if leaf.len() < Leaf::<K, V, Latched<S>>::capacity() => {
                let key = self.prefixed(key, leaf, alloc);
                let output = policy.vacant(key.key(), &value);
                leaf.insert_fit(edge, key.window_at(leaf.prefix_len()), value);
                Ok(output)
            }
            Err(edge) => Err((key, value, policy, edge)),
        }
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
        alloc: &impl Allocator,
    ) -> PrefixedKey<K> {
        if self.is_root(leaf) {
            self.change_held(|tree| tree.shrink_shared_prefix_for(key, alloc))
        } else {
            PrefixedKey::checked_by_descent(key)
        }
    }

    /// [`Self::insert`] where the leaf was full: searches again, keeping
    /// the internal nodes it goes through, and splits, until it inserts.
    #[inline(never)]
    pub(super) fn insert_splitting<P: InsertPolicy<K, V>>(
        &self,
        mut key: K,
        mut value: V,
        mut policy: P,
        alloc: &impl Allocator,
    ) -> P::Output
    where
        K: Ord,
    {
        // Declared outside the loop, so it drops last, after the nodes each
        // split held: see `NodePages`.
        let pages = self.pages(alloc);
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
                    Ok(output) => return output,
                    Err(full) => full,
                };
            match self.split(&path, &held, edge, key, value, policy, &pages) {
                Ok(output) => return output,
                Err(entry) => (key, value, policy) = entry,
            }
            // Another thread holds a node the split needs.
            Interleaving::wait(1);
        }
    }

    /// Splits `held`, the full leaf `path` leads to, and inserts `key` and
    /// `value` at its `edge`, with `NodeStackMut::insert_prefixed`. The
    /// split goes up as far as it must, and the halves' prefixes are fit to
    /// their fences. If a node it takes is held by another thread or changed
    /// since the descent read it, it changes nothing and gives all back.
    /// Pages come from `pages`.
    #[expect(
        clippy::too_many_arguments,
        reason = "a refused split gives the entry and its policy back"
    )]
    fn split<P: InsertPolicy<K, V>>(
        &self,
        path: &TreePath<K, V, S>,
        held: &HeldLeaf<K, V, S, Exclusive>,
        edge: usize,
        key: K,
        value: V,
        policy: P,
        pages: &impl Allocator,
    ) -> Result<P::Output, (K, V, P)>
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
        let leaf = held.leaf();
        let was_last_leaf = self.is_rightmost_leaf(leaf);
        let key = self.prefixed(key, leaf, pages);
        let output = policy.vacant(key.key(), &value);
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
        Ok(output)
    }
}
