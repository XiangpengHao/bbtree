//! Finding where a key belongs in a latched tree, as bbtree's search does
//! (`tree/search.rs`), while other threads change it: optimistic through
//! the internal nodes, latched at the leaf (optimistic lock coupling,
//! `node/optimistic.rs`).
//!
//! - **The root first.** A key that lacks what every key of the tree
//!   shares (D50) is in no node: the descent says so, once the root's
//!   version vouches for what it read of the shared prefix, or with the
//!   root leaf latched, which holds it still.
//! - **Each internal node** is read at a version: the descent routes the
//!   key through it, reads the edge, and checks the version before it
//!   follows the edge, then again after it reads the child's version, so
//!   that the child was the node's child when its version was read. The
//!   first check is for the root page, which turns from a leaf into an
//!   internal node and back in place (`node/root_page.rs`): an edge read
//!   from it may be a leaf's bytes. For the same reason the descent checks
//!   the root pointer's tag again once it has the root's version. Every
//!   other page keeps its kind (`pages.rs`).
//! - **The leaf** is latched, shared or exclusive, and then its parent's
//!   version checked again, so that the leaf is still where the key
//!   belongs. A failed check starts over from the root.
//!
//! A descent may keep the internal nodes it went through, each at the
//! version it read ([`TreePath`]): what a split or a rebalancing removal
//! takes before it changes them, and then hands to bbtree's operations as a
//! node stack ([`TreePath::node_stack_to_leaf`]).

use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::sync::atomic::Ordering;

use super::super::node::Node;
use super::super::node::inline_keys::SearchKey;
use super::super::node::internal::Internal;
use super::super::node::latch::{Latched, NodeLatch, Version};
use super::super::node::leaf::Leaf;
use super::super::node::optimistic::Routed;
use super::super::node_stack::{MAX_DEPTH, NodeStackMut};
use super::super::root::Tree;
use super::LatchedTree;
use crate::inline_key::InlineKeyStrategy;

/// A leaf of a latched tree, latched in mode `M` until dropped.
pub(super) struct HeldLeaf<K, V, S: InlineKeyStrategy<K>, M: LatchMode> {
    leaf: Leaf<K, V, Latched<S>>,
    held: M::Held,
}

/// How a leaf is latched: [`Shared`] or [`Exclusive`].
pub(super) trait LatchMode {
    /// What letting go needs.
    type Held: Copy;

    fn lock(latch: &NodeLatch) -> Self::Held;

    fn unlock(latch: &NodeLatch, held: Self::Held);
}

/// Latched for reading.
pub(super) enum Shared {}

/// Latched for changing.
pub(super) enum Exclusive {}

impl LatchMode for Shared {
    type Held = ();

    #[inline(always)]
    fn lock(latch: &NodeLatch) {
        latch.lock_shared();
    }

    #[inline(always)]
    fn unlock(latch: &NodeLatch, (): ()) {
        latch.unlock_shared();
    }
}

impl LatchMode for Exclusive {
    type Held = Version;

    #[inline(always)]
    fn lock(latch: &NodeLatch) -> Version {
        latch.lock_exclusive()
    }

    #[inline(always)]
    fn unlock(latch: &NodeLatch, version: Version) {
        latch.unlock_exclusive(version);
    }
}

impl<K, V, S: InlineKeyStrategy<K>, M: LatchMode> HeldLeaf<K, V, S, M> {
    /// Latches `leaf`, waiting for the other mode's holders.
    #[inline(always)]
    pub(super) fn lock(leaf: Leaf<K, V, Latched<S>>) -> Self {
        Self {
            leaf,
            held: M::lock(leaf.latch()),
        }
    }

    pub(super) fn leaf(&self) -> Leaf<K, V, Latched<S>> {
        self.leaf
    }
}

impl<K, V, S: InlineKeyStrategy<K>, M: LatchMode> Drop for HeldLeaf<K, V, S, M> {
    #[inline(always)]
    fn drop(&mut self) {
        M::unlock(self.leaf.latch(), self.held);
    }
}

/// Where a descent ends: the leaf where its key belongs, latched in mode
/// `M`, and whether the key is that leaf's head; or, for a key that lacks
/// what every key shares, where it sorts against them all.
pub(super) type Descent<K, V, S, M> = Result<(HeldLeaf<K, V, S, M>, bool), KeyOrdering>;

/// What a descent keeps of the internal nodes it goes through: nothing, for
/// most operations, or each node, for a split or a removal that rebalances
/// ([`TreePath`]).
pub(super) trait DescentPath<K, V, S> {
    /// Forgets every node: the descent starts over.
    fn clear(&mut self);

    /// The next node down, read at `version`, and where the key went from it.
    fn record(&mut self, node: Internal<K, V, Latched<S>>, version: Version, routed: Routed);
}

impl<K, V, S> DescentPath<K, V, S> for () {
    #[inline(always)]
    fn clear(&mut self) {}

    #[inline(always)]
    fn record(&mut self, _: Internal<K, V, Latched<S>>, _: Version, _: Routed) {}
}

/// An internal node a descent went through, at the version it read, and
/// where the key went from it.
pub(super) struct PathLevel<K, V, S> {
    pub(super) node: Internal<K, V, Latched<S>>,
    pub(super) version: Version,
    pub(super) routed: Routed,
}

impl<K, V, S> Clone for PathLevel<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for PathLevel<K, V, S> {}

/// The internal nodes from the root down to a leaf's parent: level 0 is the
/// root, and the leaf is at level [`Self::depth`].
pub(super) struct TreePath<K, V, S> {
    levels: [Option<PathLevel<K, V, S>>; MAX_DEPTH],
    depth: usize,
}

impl<K, V, S> TreePath<K, V, S> {
    pub(super) fn new() -> Self {
        Self {
            levels: [None; MAX_DEPTH],
            depth: 0,
        }
    }

    /// The leaf's level: how many internal nodes are above it.
    pub(super) fn depth(&self) -> usize {
        self.depth
    }

    /// The levels above `depth`, to go down another way from there.
    pub(super) fn prefix(&self, depth: usize) -> Self {
        let mut prefix = Self::new();
        prefix.levels[..depth].copy_from_slice(&self.levels[..depth]);
        prefix.depth = depth;
        prefix
    }

    /// Level `level`, which the descent went through.
    ///
    /// # Panics
    ///
    /// If it didn't.
    pub(super) fn level(&self, level: usize) -> PathLevel<K, V, S> {
        self.levels[level].expect("a level the descent went through")
    }

    /// The topmost level an operation reads that fits the prefixes of the
    /// children of the node at `level` to their fences (`tree/prefix.rs`):
    /// the nearest levels above it with a separator left of the path and
    /// right of it, which hold the fences the node lacks on that side; the
    /// node's own level where it lacks a fence above it on both sides, as
    /// only the root does. A side without one is the tree's border, which a
    /// node stays on whatever other threads change, and whose fence is what
    /// every key shares (D50).
    pub(super) fn fences_top(&self, level: usize) -> usize {
        let nearest = |has_separator: fn(Routed) -> bool| {
            (0..level)
                .rev()
                .find(|&above| has_separator(self.level(above).routed))
        };
        let low = nearest(|routed| routed.edge > 0);
        let high = nearest(|routed| routed.edge < routed.len);
        [low, high].into_iter().flatten().fold(level, usize::min)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> TreePath<K, V, S> {
    /// A node stack down `tree` along the path, from level `top` down to
    /// `leaf`, the path's leaf, ending at `index` in it: for bbtree's
    /// operations, on nodes the caller holds from `top` down
    /// ([`NodeStackMut::along`]). The node at `top` stands for the root:
    /// the operation goes no higher, and finds no fence above it, which
    /// `top` holds ([`Self::fences_top`]).
    pub(super) fn node_stack_to_leaf<'t>(
        &self,
        tree: &'t mut Tree<K, V, Latched<S>>,
        top: usize,
        leaf: Leaf<K, V, Latched<S>>,
        index: usize,
    ) -> NodeStackMut<'t, K, V, Latched<S>> {
        let internals = (top..self.depth).map(|level| {
            let PathLevel { node, routed, .. } = self.level(level);
            (Node::Internal(node), routed.edge)
        });
        NodeStackMut::along(tree, internals.chain([(Node::Leaf(leaf), index)]))
    }

    /// A node stack down `tree` along the path, from level `top` down to the
    /// node at `level`, ending at its separator `index`: for bbtree's
    /// removal of that separator ([`Self::node_stack_to_leaf`]).
    pub(super) fn node_stack_to_separator<'t>(
        &self,
        tree: &'t mut Tree<K, V, Latched<S>>,
        top: usize,
        level: usize,
        index: usize,
    ) -> NodeStackMut<'t, K, V, Latched<S>> {
        let above = (top..level).map(|above| {
            let PathLevel { node, routed, .. } = self.level(above);
            (Node::Internal(node), routed.edge)
        });
        let bottom = (Node::Internal(self.level(level).node), index);
        NodeStackMut::along(tree, above.chain([bottom]))
    }
}

impl<K, V, S> DescentPath<K, V, S> for TreePath<K, V, S> {
    fn clear(&mut self) {
        self.depth = 0;
    }

    fn record(&mut self, node: Internal<K, V, Latched<S>>, version: Version, routed: Routed) {
        self.levels[self.depth] = Some(PathLevel {
            node,
            version,
            routed,
        });
        self.depth += 1;
    }
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// The leaf where `key` belongs, latched in mode `M`, and whether `key`
    /// is its head, the entry of the separator left of it (D17); with `path`
    /// keeping the internal nodes it went through. If `key` lacks what every
    /// key of the tree shares (D50), it is in no node: where it sorts against
    /// all of them instead, before or after.
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    pub(super) fn descend<Q, M: LatchMode, P: DescentPath<K, V, S>>(
        &self,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        path: &mut P,
    ) -> Descent<K, V, S, M>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        'restart: loop {
            path.clear();
            let root_page = self.root_page().expect("a tree to descend");
            let mut node = match root_page.node() {
                Node::Leaf(leaf) => {
                    let held = HeldLeaf::lock(leaf);
                    // A split that raised the tree changed the tag first.
                    if !self.is_root(leaf) {
                        continue 'restart;
                    }
                    // The root leaf is on both borders: the shared prefix
                    // stays as it is while the leaf is latched.
                    if let Some(outside) = root_page.shared_prefix().place_outside(key) {
                        return Err(outside);
                    }
                    return Ok((held, false));
                }
                Node::Internal(internal) => internal,
            };
            let mut version = node.latch().read_version();
            // A lowering that turned the root into a leaf may have finished
            // between the two reads: it retags the pointer before letting go,
            // so the pointer tells.
            if self.root.load(Ordering::Acquire) != node.node_ptr().tagged().as_ptr() {
                continue 'restart;
            }
            // SAFETY: the tree is latched, and its root page allocated; the
            // copy is checked by the root's version below.
            let shared = unsafe { root_page.load_racing_shared_prefix() };
            if let Some(outside) = shared.and_then(|shared| shared.place_outside(key)) {
                if node.latch().is_unchanged(version) {
                    return Err(outside);
                }
                continue 'restart;
            }
            let mut at_separator = false;
            loop {
                let Some(routed) = node.route_racing(key, version) else {
                    continue 'restart;
                };
                let Some(child) = node.racing_child(routed.edge) else {
                    continue 'restart;
                };
                // Followed only once the node's version vouches for it.
                if !node.latch().is_unchanged(version) {
                    continue 'restart;
                }
                at_separator |= routed.at_separator;
                path.record(node, version, routed);
                match child {
                    Node::Leaf(leaf) => {
                        let held = HeldLeaf::lock(leaf);
                        if !node.latch().is_unchanged(version) {
                            continue 'restart;
                        }
                        // The separator the key matched above may have been
                        // removed since, its successor the head now: the
                        // parent doesn't change for that.
                        if at_separator && !Self::is_head(leaf, key) {
                            continue 'restart;
                        }
                        return Ok((held, at_separator));
                    }
                    Node::Internal(child) => {
                        let child_version = child.latch().read_version();
                        if !node.latch().is_unchanged(version) {
                            continue 'restart;
                        }
                        (node, version) = (child, child_version);
                    }
                }
            }
        }
    }

    /// Whether `leaf`, latched, has `key` as its head's key.
    #[cold]
    #[inline(never)]
    fn is_head<Q>(leaf: Leaf<K, V, Latched<S>>, key: &SearchKey<'_, K, Latched<S>, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        leaf.has_head() && key.cmp_full(leaf.head_key()) == KeyOrdering::Equal
    }
}
