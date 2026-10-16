//! Inserting into a latched tree, as bbtree inserts (`tree/insert.rs`).
//!
//! - **An append** to the last leaf, if the key goes after every key there
//!   and the leaf has room, latches that leaf alone (`append.rs`, D45).
//! - **Otherwise** a descent finds the leaf where the key belongs and
//!   latches it exclusively; the key goes in if the leaf has room. A key
//!   that lacks what every key shares shrinks it first, and descends again
//!   (`shared_prefix.rs`, D50).
//! - **A full leaf splits**: the insert descends again, keeping its path,
//!   and takes the nodes the split changes, from the leaf's parent up to
//!   the lowest node with room, which takes the separator, and up to the
//!   nodes above that one that hold its fences, which the halves' prefixes
//!   are fit to (`tree/prefix.rs`); then runs bbtree's insert along them
//!   ([`NodeStackMut::insert_prefixed`]). The pages it allocates come held
//!   until it is done (`pages.rs`). A split of the last leaf points appends
//!   at the leaf last now.
//!
//! [`NodeStackMut::insert_prefixed`]: super::super::node_stack::NodeStackMut::insert_prefixed

use core::mem;

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

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Inserts `key` and `value` and returns `None`; or, if an equal key is
    /// there, keeps it, drops `key`, replaces its value and returns the old
    /// one, as std's `BTreeMap::insert` does. Pages come from `alloc`.
    #[inline]
    pub(crate) fn insert(&self, key: K, value: V, alloc: &impl Allocator) -> Option<V>
    where
        K: Ord,
    {
        if self.root_page().is_none() {
            self.start_tree(&self.pages(alloc));
        }
        let (mut key, value) = match self.append(key, value) {
            Ok(()) => return None,
            Err(entry) => entry,
        };
        loop {
            let Some((held, at_separator)) =
                self.descend::<K, Exclusive, ()>(&SearchKey::new(&key), &mut ())
            else {
                key = self.shrink_shared_prefix_for(key, alloc);
                continue;
            };
            return match self.insert_into(&held, at_separator, key, value, alloc) {
                Ok(replaced) => replaced,
                Err((key, value, _)) => {
                    drop(held);
                    self.insert_splitting(key, value, alloc)
                }
            };
        }
    }

    /// Inserts into `held`, the leaf where `key` belongs, as [`Self::insert`]
    /// does, if it holds `key` or has room; or gives both back, with the
    /// edge where `key` goes in the full leaf.
    #[inline(always)]
    fn insert_into(
        &self,
        held: &HeldLeaf<K, V, S, Exclusive>,
        at_separator: bool,
        key: K,
        value: V,
        alloc: &impl Allocator,
    ) -> Result<Option<V>, (K, V, usize)>
    where
        K: Ord,
    {
        let leaf = held.leaf();
        if at_separator {
            // As std, the new key is dropped before the value changes.
            drop(key);
            return Ok(Some(mem::replace(leaf.head_value_mut(), value)));
        }
        match leaf.search_always_inlined(&SearchKey::new(&key)) {
            Ok(index) => {
                drop(key);
                Ok(Some(mem::replace(leaf.value_mut(index), value)))
            }
            Err(edge) if leaf.len() < Leaf::<K, V, Latched<S>>::capacity() => {
                let key = self.prefixed(key, leaf, alloc);
                leaf.insert_fit(edge, key.window_at(leaf.prefix_len()), value);
                Ok(None)
            }
            Err(edge) => Err((key, value, edge)),
        }
    }

    /// `key`, which the descent found shares what every key of the tree
    /// shares, bound for `leaf`, which the caller holds exclusively. A root
    /// leaf, the tree's only node, is every border, so that the caller holds
    /// what every key shares: bbtree's insert then starts it, in a tree that
    /// had no key, or shrinks it, as it does for every key
    /// ([`Tree::shrink_shared_prefix_for`]); `alloc` is the map's.
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
    fn insert_splitting(&self, mut key: K, mut value: V, alloc: &impl Allocator) -> Option<V>
    where
        K: Ord,
    {
        // Dropped last, after the nodes the split held: see `NodePages`.
        let pages = self.pages(alloc);
        loop {
            let mut path = TreePath::new();
            let Some((held, at_separator)) =
                self.descend::<K, Exclusive, _>(&SearchKey::new(&key), &mut path)
            else {
                key = self.shrink_shared_prefix_for(key, alloc);
                continue;
            };
            let edge;
            (key, value, edge) = match self.insert_into(&held, at_separator, key, value, alloc) {
                Ok(replaced) => return replaced,
                Err(full) => full,
            };
            match self.split(&path, &held, edge, key, value, &pages) {
                Ok(()) => return None,
                Err(entry) => (key, value) = entry,
            }
        }
    }

    /// Splits `held`, the full leaf `path` leads to, inserting `key` and
    /// `value` at `edge` of it, with bbtree's insert, carrying the split up
    /// as far as it goes, and fitting the halves' prefixes; or, if a node it
    /// takes is held by another thread or changed since the descent read it,
    /// changes nothing and gives both back. Pages come from `pages`.
    fn split(
        &self,
        path: &TreePath<K, V, S>,
        held: &HeldLeaf<K, V, S, Exclusive>,
        edge: usize,
        key: K,
        value: V,
        pages: &impl Allocator,
    ) -> Result<(), (K, V)>
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
            return Err((key, value));
        }
        let leaf = held.leaf();
        let was_last_leaf = self.is_rightmost_leaf(leaf);
        let key = self.prefixed(key, leaf, pages);
        self.change_held(|tree| {
            // A latched tree counts no entries.
            let mut uncounted = 0;
            path.node_stack_to_leaf(tree, top, leaf, edge)
                .insert_prefixed(key, value, pages, &mut uncounted);
        });
        if was_last_leaf {
            // The last leaf is the last under the node that took the
            // separator, or the root, which the split holds, as every node
            // down its last edges.
            let took_separator = match room {
                Some(level) => Node::Internal(path.level(level).node),
                None => self.root_page().expect("a tree that split").node(),
            };
            self.point_appends_to(took_separator.border_leaf::<LastEdge>());
        }
        drop(held_nodes);
        Ok(())
    }
}
