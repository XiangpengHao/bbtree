//! Removing from a latched tree, as the unsync tree removes
//! (`tree/remove.rs`, D22).
//!
//! - **A condition** the caller gives decides. It runs on the entry while
//!   its leaf is latched exclusively, before anything changes.
//! - **An entry whose leaf keeps its minimum**, or whose leaf is the root,
//!   comes out with that leaf latched exclusively and nothing else held.
//! - **Otherwise** the removal descends again, keeping its path. It takes,
//!   without waiting, every node [`NodeStackMut::remove`] will change or
//!   read a separator of:
//!   - for each level it leaves underfull, from the leaf up, the parent and
//!     the sibling that `Internal::fix_underfull_child` merges the child
//!     with or moves an entry from;
//!   - for a removed separator, the node holding it, the path below it
//!     (whose lower fence the successor becomes), and the right border of
//!     the subtree left of it (whose upper fence the successor becomes);
//!   - the path from the topmost changed node up to the nearest nodes
//!     holding its fences, which the changed nodes' prefixes are fit to
//!     (`tree/prefix.rs`).
//!
//!   Then it runs [`NodeStackMut::remove`] along them. That fixes the
//!   levels, fits the prefixes, and lowers the tree in the root page when
//!   the root loses its last separator, down to a leaf if need be.
//! - **Freed pages** (a merged node's, and the root's child when the root
//!   lowers) go on the tree's free lists once the removal lets go
//!   (`pages.rs`). If the last leaf was freed, appends go to the leaf that
//!   took its entries.
//!
//! [`NodeStackMut::remove`]: super::super::node_stack::NodeStackMut::remove

use core::borrow::Borrow;

use super::super::interleaving::Interleaving;
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

/// A removal found a node it needs held by another thread, or changed since
/// its descent. It changed nothing, and starts over.
struct StartOver;

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Removes the entry whose key equals `key`, if `condition` holds of
    /// it, and returns it, with the key as stored; `None` without one, or if
    /// `condition` doesn't hold. `condition` runs once per try, while the
    /// entry's leaf is latched exclusively. Pages a removal frees go on the
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
        // Declared outside the loop, so it drops last, after the nodes each
        // try held: see `NodePages`.
        let pages = self.pages(alloc);
        loop {
            if let Ok(removed) = self.try_remove_rebalancing(key, condition, &pages) {
                return removed;
            }
            // Another thread holds a node the removal needs.
            Interleaving::wait(1);
        }
    }

    /// One try of [`Self::remove_rebalancing`]. [`StartOver`], with nothing
    /// changed, if a node it takes is held by another thread or changed
    /// since the descent.
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
            // A latched tree keeps no entry count: `NodeStackMut::remove`
            // takes this one out of a count of its own.
            let mut uncounted = 1;
            stack.remove(pages, &mut uncounted)
        });
        // A freed last leaf merged into the leaf left of it, under the
        // topmost node fixed, or moved into the root page. The new last leaf
        // is the last under that node, or the root. The removal holds it, as
        // it holds every node down to it along last edges.
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
    /// removal leaves it with `leaf_len` entries. For each level left
    /// underfull, from the leaf up, those are its parent and the sibling
    /// that `Internal::fix_underfull_child` will merge it with or move an
    /// entry from. Returns how many levels to fix; [`StartOver`] if a node
    /// is held by another thread or changed since the descent.
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
            // The sibling and the merge the fixing picks (`tree/remove.rs`).
            let sibling_edge = Internal::<K, V, Latched<S>>::fixing_sibling(routed.edge);
            let merges = if fixes == 0 {
                let sibling = parent.leaf_child(sibling_edge);
                if !held_nodes.take_leaf(sibling) {
                    return Err(StartOver);
                }
                Leaf::<K, V, Latched<S>>::fit_merged(child_len, sibling.len())
            } else {
                let sibling = parent.internal_child(sibling_edge);
                if !held_nodes.take_internal(sibling) {
                    return Err(StartOver);
                }
                Internal::<K, V, Latched<S>>::fit_merged(child_len, sibling.len())
            };
            fixes += 1;
            if !merges {
                // The child takes an entry from its sibling: the parent keeps
                // its separators.
                return Ok(fixes);
            }
            child_len = parent.len() - 1;
        }
    }

    /// Takes the nodes along the right border of `node`'s child at `edge`,
    /// down to its last leaf; the caller holds `node`. Each is taken at the
    /// version it is at, as its held parent keeps it in place. `false` if
    /// another thread holds one.
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
