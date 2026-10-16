//! Removing from a latched tree, as bbtree removes (`tree/remove.rs`, D22).
//!
//! - **A condition** the caller gives decides, run on the entry while its
//!   leaf is latched exclusively, before anything changes.
//! - **An entry whose leaf keeps its minimum**, or is the root, comes out
//!   with the leaf latched exclusively, nothing else held.
//! - **Otherwise** the removal descends again, keeping its path, and takes,
//!   without waiting, every node bbtree's removal will change or read a
//!   separator of: for each level it leaves underfull, from the leaf up,
//!   the parent and the sibling the child merges with or takes an entry
//!   from, as `Internal::fix_underfull_child` decides; for a separator, the
//!   node holding it, the nodes on the path from there down, whose lower
//!   fence its successor becomes, and the nodes along the right border of
//!   the subtree left of it, whose upper fence it becomes; and the nodes
//!   above the topmost one it changes that hold that node's fences, which
//!   the changed nodes' prefixes are fit to (`tree/prefix.rs`). Then it runs
//!   bbtree's removal along them ([`NodeStackMut::remove`]), which fixes the
//!   levels, fits the prefixes, and lowers the tree if the root lost its
//!   last separator, to a leaf too, in the root page.
//! - **Freed pages**, a merged node's and the root's child when the root
//!   lowers, go on the tree's free lists once the removal lets go
//!   (`pages.rs`). If the last leaf was one, appends go to the leaf that
//!   took its entries.
//!
//! [`NodeStackMut::remove`]: super::super::node_stack::NodeStackMut::remove

use core::borrow::Borrow;

use super::super::node::Child;
use super::super::node::Node;
use super::super::node::inline_keys::SearchKey;
use super::super::node::internal::Internal;
use super::super::node::latch::Latched;
use super::super::node::leaf::Leaf;
use super::super::node_stack::{LastEdge, NodeEnd};
use super::LatchedTree;
use super::descent::{Exclusive, PathLevel, TreePath};
use super::held::HeldNodes;
use super::pages::NodePages;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// A removal that found a node it changes held by another thread, or
/// changed since its descent: it changed nothing, and starts over.
struct StartOver;

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Removes the entry whose key equals `key`, if `condition` holds of
    /// it, and returns it, with the key as stored; `None` without one, or if
    /// `condition` doesn't hold. `condition` runs while the entry's leaf is
    /// latched exclusively, once a try. Pages a removal frees go on the
    /// tree's free lists (`pages.rs`); `alloc` is the map's allocator.
    #[inline]
    pub(crate) fn remove_entry<Q>(
        &self,
        key: &Q,
        mut condition: impl FnMut(&K, &V) -> bool,
        alloc: &impl Allocator,
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let key = SearchKey::new(key);
        {
            let (held, at_separator) = self.descend::<Q, Exclusive, ()>(&key, &mut ()).ok()?;
            let leaf = held.leaf();
            if at_separator {
                if !condition(leaf.head_key(), leaf.head_value()) {
                    return None;
                }
            } else {
                let index = leaf.find(&key, ())?;
                let (stored, value) = leaf.key_value(index);
                if !condition(stored, value) {
                    return None;
                }
                if Self::keeps_its_minimum(leaf) || self.is_root(leaf) {
                    return Some(leaf.remove(index));
                }
            }
        }
        self.remove_rebalancing(&key, &mut condition, alloc)
    }

    /// Whether `leaf` keeps its minimum after losing an entry.
    #[inline(always)]
    fn keeps_its_minimum(leaf: Leaf<K, V, Latched<S>>) -> bool {
        leaf.len() > Leaf::<K, V, Latched<S>>::min_len()
    }

    /// [`Self::remove_entry`] of a separator, or of an entry whose leaf
    /// would be left underfull: tries until no other thread is in the way.
    #[inline(never)]
    fn remove_rebalancing<Q>(
        &self,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        condition: &mut impl FnMut(&K, &V) -> bool,
        alloc: &impl Allocator,
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        // Dropped last, after the nodes the removal held: see `NodePages`.
        let pages = self.pages(alloc);
        loop {
            if let Ok(removed) = self.try_remove_rebalancing(key, condition, &pages) {
                return removed;
            }
        }
    }

    /// One try of [`Self::remove_rebalancing`]: the removal, or, if a node
    /// it takes is in another thread's way, nothing changed.
    fn try_remove_rebalancing<Q, A: Allocator>(
        &self,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        condition: &mut impl FnMut(&K, &V) -> bool,
        pages: &NodePages<'_, A>,
    ) -> Result<Option<(K, V)>, StartOver>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut path = TreePath::new();
        let Ok((held, at_separator)) = self.descend::<Q, Exclusive, _>(key, &mut path) else {
            return Ok(None);
        };
        let leaf = held.leaf();
        let depth = path.depth();
        let mut held_nodes = HeldNodes::new();
        // The separator's level, or the entry's index in the leaf.
        let (separator_level, leaf_index) = if at_separator {
            if !condition(leaf.head_key(), leaf.head_value()) {
                return Ok(None);
            }
            let level = (0..depth)
                .find(|&level| path.level(level).routed.at_separator)
                .expect("a descent that matched a separator went through it");
            (Some(level), 0)
        } else {
            let Some(index) = leaf.find(key, ()) else {
                return Ok(None);
            };
            let (stored, value) = leaf.key_value(index);
            if !condition(stored, value) {
                return Ok(None);
            }
            // Another thread may have filled the leaf since.
            if Self::keeps_its_minimum(leaf) || self.is_root(leaf) {
                return Ok(Some(leaf.remove(index)));
            }
            (None, index)
        };
        // Either way, the leaf loses an entry: a separator's successor is its
        // first key, which becomes its head.
        let fixes = Self::take_fixes(&path, leaf.len() - 1, &mut held_nodes)?;
        let mut top_changed = depth - fixes;
        if let Some(level) = separator_level {
            let PathLevel { node, routed, .. } = path.level(level);
            if !held_nodes.take_path(&path, level)
                || !Self::take_left_border(node, routed.edge - 1, &mut held_nodes)
            {
                return Err(StartOver);
            }
            top_changed = top_changed.min(level);
        }
        let top = path.fences_top(top_changed);
        if !held_nodes.take_path(&path, top) {
            return Err(StartOver);
        }

        let removed = self.change_held(|tree| {
            let mut stack = match separator_level {
                Some(level) => {
                    let index = path.level(level).routed.edge - 1;
                    path.node_stack_to_separator(tree, top, level, index)
                }
                None => path.node_stack_to_leaf(tree, top, leaf, leaf_index),
            };
            // A latched tree counts no entries: bbtree's removal counts this
            // one out of a count of its own.
            let mut uncounted = 1;
            stack.remove(pages, &mut uncounted)
        });
        // A freed last leaf merged into the leaf left of it, under the
        // topmost node fixed, or moved into the root page: what is last
        // under that node, or the root, which the removal holds, as every
        // node down its last edges.
        if (self.rightmost_leaf()).is_some_and(|last| pages.has_freed(last.node_ptr().page())) {
            let top_fixed = match depth - fixes {
                0 => self
                    .root_page()
                    .expect("a tree that a removal fixed")
                    .node(),
                level => Node::Internal(path.level(level).node),
            };
            self.point_appends_to(top_fixed.border_leaf::<LastEdge>());
        }
        drop(held_nodes);
        drop(held);
        Ok(Some(removed))
    }

    /// Takes the nodes that fixing the leaf of `path` changes, once a
    /// removal leaves it with `leaf_len` entries: for each level left
    /// underfull, from the leaf up, its parent and the sibling it merges
    /// with or takes an entry from, as `Internal::fix_underfull_child` will.
    /// Returns how many levels to fix; [`StartOver`] if a node is held by
    /// another thread or changed since the descent.
    fn take_fixes(
        path: &TreePath<K, V, S>,
        leaf_len: usize,
        held_nodes: &mut HeldNodes<K, V, S>,
    ) -> Result<usize, StartOver> {
        let mut fixes = 0;
        let mut child_len = leaf_len;
        loop {
            let level = path.depth() - fixes;
            let min_len = if fixes == 0 {
                Leaf::<K, V, Latched<S>>::min_len()
            } else {
                Internal::<K, V, Latched<S>>::min_len()
            };
            if child_len >= min_len || level == 0 {
                return Ok(fixes);
            }
            let PathLevel {
                node: parent,
                version,
                routed,
            } = path.level(level - 1);
            if !held_nodes.take_internal_at(parent, version) {
                return Err(StartOver);
            }
            let sibling_edge = if routed.edge > 0 {
                routed.edge - 1
            } else {
                routed.edge + 1
            };
            let (sibling_len, capacity) = if fixes == 0 {
                let sibling = parent.leaf_child(sibling_edge);
                if !held_nodes.take_leaf(sibling) {
                    return Err(StartOver);
                }
                (sibling.len(), Leaf::<K, V, Latched<S>>::capacity())
            } else {
                let sibling = parent.internal_child(sibling_edge);
                if !held_nodes.take_internal(sibling) {
                    return Err(StartOver);
                }
                (sibling.len(), Internal::<K, V, Latched<S>>::capacity())
            };
            fixes += 1;
            if child_len + 1 + sibling_len > capacity {
                // The child takes an entry from its sibling: the parent keeps
                // its separators.
                return Ok(fixes);
            }
            child_len = parent.len() - 1;
        }
    }

    /// Takes the nodes along the right border of the child at edge `edge` of
    /// `node`, which the caller holds, down to its last leaf: each where the
    /// one above it, held, keeps it. `false` if another thread holds one.
    fn take_left_border(
        node: Internal<K, V, Latched<S>>,
        edge: usize,
        held_nodes: &mut HeldNodes<K, V, S>,
    ) -> bool {
        let mut child = node.child(edge);
        loop {
            match child {
                Node::Leaf(leaf) => return held_nodes.take_leaf(leaf),
                Node::Internal(internal) => {
                    if !held_nodes.take_internal(internal) {
                        return false;
                    }
                    child = internal.child(LastEdge::edge(child));
                }
            }
        }
    }
}
