//! Finding where a key belongs in a latched tree while other threads change
//! it, as the unsync tree's search does (`tree/search.rs`). The descent is
//! optimistic through the internal nodes and latches the leaf (optimistic
//! lock coupling, `node/optimistic.rs`).
//!
//! - **The root first.** A key that lacks what every key of the tree
//!   shares (D50) is in no node. The descent says so once the root's
//!   version vouches for what it read of the shared prefix, or while the
//!   root leaf is latched, which holds the prefix still.
//! - **Each internal node** is read at a version. The descent routes the
//!   key through it, reads the edge and the child's version, then checks
//!   the node's version. So the child was still the node's child when the
//!   child's version was read.
//! - **The root's version** is checked before the descent follows an edge
//!   read from it, too. The root page turns from a leaf into an internal
//!   node and back in place (`node/root_page.rs`), so an edge read from it
//!   may be a leaf's bytes. For the same reason, the descent checks the
//!   root pointer's tag again once it has the root's version. Every other
//!   page keeps its kind: an edge read from one, even stale, is null or a
//!   node of the kind its tag says (`pages.rs`).
//! - **The leaf** is latched, shared or exclusive, or read at a version
//!   ([`Optimistic`]). Then its parent's version is checked again, so the
//!   leaf is still where the key belongs. A failed check starts over from
//!   the root.
//!
//! A descent may keep the internal nodes it went through, each at the
//! version it read ([`TreePath`]). A split or a rebalancing removal takes
//! them before it changes them, then hands them to the unsync tree's
//! operations as a node stack ([`TreePath::node_stack_to_leaf`]).

use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::mem::MaybeUninit;
use core::sync::atomic::Ordering;

use super::super::interleaving::Interleaving;
use super::super::node::Node;
use super::super::node::inline_keys::SearchKey;
use super::super::node::internal::Internal;
use super::super::node::latch::{Latched, NodeLatch, Version};
use super::super::node::leaf::Leaf;
use super::super::node::optimistic::Routed;
use super::super::node::root_page::RootPage;
use super::super::node_stack::{MAX_DEPTH, NodeStackMut};
use super::super::root::Tree;
use super::LatchedTree;
use crate::inline_key::InlineKeyStrategy;

/// A leaf of a latched tree, latched in mode `M` until dropped.
pub(super) struct HeldLeaf<K, V, S: InlineKeyStrategy<K>, M: LatchMode> {
    leaf: Leaf<K, V, Latched<S>>,
    held: M::Held,
}

/// How a leaf is latched: [`Shared`] or [`Exclusive`]; or not at all, read
/// at a version instead ([`Optimistic`]).
pub(super) trait LatchMode {
    /// What letting go needs.
    type Held: Copy;

    /// Whether the mode latches the leaf, so that what the descent reads of
    /// it stays put; otherwise a version check after the read tells.
    const LATCHES: bool;

    fn lock(latch: &NodeLatch) -> Self::Held;

    fn unlock(latch: &NodeLatch, held: Self::Held);

    /// Whether the leaf is still as it was when [`Self::lock`] returned
    /// `held`: always while it is latched; by its version otherwise.
    fn is_unchanged(latch: &NodeLatch, held: Self::Held) -> bool;
}

/// Latched for reading.
pub(super) enum Shared {}

/// Latched for changing.
pub(super) enum Exclusive {}

/// Captures a version without retaining a latch. Each later node snapshot
/// takes its own short shared hold, and the reader checks the version after
/// the snapshots (`lookup.rs`).
pub(super) enum Optimistic {}

impl LatchMode for Shared {
    type Held = ();

    const LATCHES: bool = true;

    #[inline(always)]
    fn lock(latch: &NodeLatch) {
        latch.lock_shared();
    }

    #[inline(always)]
    fn unlock(latch: &NodeLatch, (): ()) {
        latch.unlock_shared();
    }

    #[inline(always)]
    fn is_unchanged(_: &NodeLatch, (): ()) -> bool {
        true
    }
}

impl LatchMode for Exclusive {
    type Held = Version;

    const LATCHES: bool = true;

    #[inline(always)]
    fn lock(latch: &NodeLatch) -> Version {
        latch.lock_exclusive()
    }

    #[inline(always)]
    fn unlock(latch: &NodeLatch, version: Version) {
        latch.unlock_exclusive(version);
    }

    #[inline(always)]
    fn is_unchanged(_: &NodeLatch, _: Version) -> bool {
        true
    }
}

impl LatchMode for Optimistic {
    type Held = Version;

    const LATCHES: bool = false;

    #[inline(always)]
    fn lock(latch: &NodeLatch) -> Version {
        latch.read_version()
    }

    #[inline(always)]
    fn unlock(_: &NodeLatch, _: Version) {}

    #[inline(always)]
    fn is_unchanged(latch: &NodeLatch, version: Version) -> bool {
        latch.is_unchanged(version)
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

    /// Whether the leaf is still as the descent found it, so that what the
    /// caller read of it since is what it held then
    /// ([`LatchMode::is_unchanged`]).
    #[inline(always)]
    pub(super) fn is_unchanged(&self) -> bool {
        M::is_unchanged(self.leaf.latch(), self.held)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> HeldLeaf<K, V, S, Exclusive> {
    /// Latches `leaf` exclusively if it is still at `version`, with no
    /// holder; `None` without waiting otherwise.
    #[inline(always)]
    pub(super) fn try_lock_at(leaf: Leaf<K, V, Latched<S>>, version: Version) -> Option<Self> {
        // Lazily: a guard made for a latch not taken would let it go.
        leaf.latch().try_lock_at(version).then(|| Self {
            leaf,
            held: version,
        })
    }
}

impl<K, V, S: InlineKeyStrategy<K>> HeldLeaf<K, V, S, Optimistic> {
    /// The version the leaf was read at.
    pub(super) fn version(&self) -> Version {
        self.held
    }
}

impl<K, V, S: InlineKeyStrategy<K>, M: LatchMode> Drop for HeldLeaf<K, V, S, M> {
    #[inline(always)]
    fn drop(&mut self) {
        M::unlock(self.leaf.latch(), self.held);
    }
}

/// Where a descent ends: the leaf where its key belongs, latched in mode
/// `M`, and whether the key is that leaf's head. For a key that lacks what
/// every key shares, where it sorts against them all instead.
pub(super) type Descent<K, V, S, M> = Result<(HeldLeaf<K, V, S, M>, bool), KeyOrdering>;

/// What a descent reads at an internal root ([`LatchedTree::read_root`]).
pub(super) enum RootRead {
    /// The root's version, which vouches for what it read of the shared
    /// prefix.
    At(Version),
    /// The key lacks what every key shares: where it sorts against all of
    /// them.
    Outside(KeyOrdering),
    /// The root changed meanwhile: start over.
    Changed,
}

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
///
/// Invariant: levels `0..depth` are initialized. Those past it are not, so
/// that making a path writes nothing, and copying one copies its depth.
pub(super) struct TreePath<K, V, S> {
    levels: [MaybeUninit<PathLevel<K, V, S>>; MAX_DEPTH],
    depth: usize,
}

impl<K, V, S> TreePath<K, V, S> {
    pub(super) fn new() -> Self {
        Self {
            levels: [const { MaybeUninit::uninit() }; MAX_DEPTH],
            depth: 0,
        }
    }

    /// The leaf's level: how many internal nodes are above it.
    pub(super) fn depth(&self) -> usize {
        self.depth
    }

    /// Makes this path `other`'s, copying its levels alone.
    pub(super) fn copy_from(&mut self, other: &Self) {
        self.copy_levels_from(other, other.depth);
    }

    /// Makes levels `0..depth` of `other` this path's, to go down another
    /// way from there. In place: a path returned by value would copy the
    /// whole array wherever the compiler keeps the move.
    ///
    /// # Panics
    ///
    /// If `depth` is past `other`'s.
    pub(super) fn copy_levels_from(&mut self, other: &Self, depth: usize) {
        assert!(depth <= other.depth, "levels the path went through");
        self.levels[..depth].copy_from_slice(&other.levels[..depth]);
        self.depth = depth;
    }

    /// Level `level`, which the descent went through.
    ///
    /// # Panics
    ///
    /// If it didn't.
    pub(super) fn level(&self, level: usize) -> PathLevel<K, V, S> {
        assert!(level < self.depth, "a level the descent went through");
        // SAFETY: levels `0..depth` are initialized (invariant).
        unsafe { self.levels[level].assume_init() }
    }

    /// The topmost level read by an operation that fits the prefixes of the
    /// children of the node at `level` to their fences (`tree/prefix.rs`).
    /// That is the topmost of the nearest levels above with a separator left
    /// of the path and right of it, which hold the node's fences. A side
    /// with no such level is the tree's border: a node stays on it whatever
    /// other threads change, and its fence there is what every key shares
    /// (D50). With neither, as only for the root, it is `level` itself.
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
    /// A node stack down `tree` along the path, from level `top` to `leaf`,
    /// the path's leaf, ending at `index` in it. It is for the unsync tree's
    /// operations, on nodes the caller holds from `top` down
    /// ([`NodeStackMut::along`]). The node at `top` stands for the root: the
    /// operation goes no higher and finds no fence above it, so `top` must
    /// hold every fence it needs ([`Self::fences_top`]).
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
    /// node at `level`, ending at its separator `index`: for
    /// [`NodeStackMut::remove`] of that separator
    /// ([`Self::node_stack_to_leaf`]).
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
        self.levels[self.depth].write(PathLevel {
            node,
            version,
            routed,
        });
        self.depth += 1;
    }
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// The leaf where `key` belongs, latched in mode `M`, and whether `key`
    /// is its head, the entry of the separator left of it (D17); an
    /// [`Optimistic`] caller confirms the head itself. `path` keeps the
    /// internal nodes the descent went through. If `key` lacks what every
    /// key of the tree shares (D50), it is in no node: returns where it
    /// sorts against all of them instead, before or after.
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
            let (mut node, mut version) = match root_page.node() {
                Node::Leaf(leaf) => {
                    let held = HeldLeaf::<K, V, S, M>::lock(leaf);
                    // A split that raised the tree changed the tag first.
                    if !self.is_root(leaf) {
                        continue 'restart;
                    }
                    // The root leaf is on both borders: the shared prefix
                    // stays as it is while the leaf is latched, and changes
                    // only while it is held.
                    let outside = if M::LATCHES {
                        root_page.shared_prefix().place_outside(key)
                    } else {
                        // SAFETY: the tree is latched, and its root page
                        // allocated; the copy is checked by the leaf's
                        // version below.
                        let shared = unsafe { root_page.load_racing_shared_prefix() };
                        let outside = shared.and_then(|shared| shared.place_outside(key));
                        if !held.is_unchanged() {
                            continue 'restart;
                        }
                        outside
                    };
                    if let Some(outside) = outside {
                        return Err(outside);
                    }
                    return Ok((held, false));
                }
                Node::Internal(root) => match self.read_root(root_page, root, key) {
                    RootRead::At(version) => (root, version),
                    RootRead::Outside(outside) => return Err(outside),
                    RootRead::Changed => continue 'restart,
                },
            };
            let mut at_separator = false;
            let mut at_root = true;
            loop {
                let Some(routed) = node.route_racing(key, version) else {
                    continue 'restart;
                };
                let Some(child) = node.racing_child(routed.edge) else {
                    continue 'restart;
                };
                // An edge read from the root page is followed only once the
                // root's version vouches for it: the page may hold a leaf
                // now. Any other edge is null or a node's (`pages.rs`).
                if at_root && !node.latch().is_unchanged(version) {
                    continue 'restart;
                }
                at_root = false;
                at_separator |= routed.at_separator;
                path.record(node, version, routed);
                match child {
                    Node::Leaf(leaf) => {
                        let held = HeldLeaf::lock(leaf);
                        if !node.latch().is_unchanged(version) {
                            continue 'restart;
                        }
                        // The separator the key matched above may have been
                        // removed since, and its successor be the head now:
                        // the parent's version check may not catch that. An
                        // snapshot caller checks the head it reads itself.
                        if M::LATCHES && at_separator && !Self::is_head(leaf, key) {
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

    /// `root`'s version, for a descent from it; `root` is the root page's
    /// internal node. For a key that lacks what every key of the tree shares
    /// (D50), where it sorts against all of them instead, once the root's
    /// version vouches for the shared prefix read.
    #[inline(always)]
    pub(super) fn read_root<Q>(
        &self,
        root_page: RootPage<K, V, Latched<S>>,
        root: Internal<K, V, Latched<S>>,
        key: &SearchKey<'_, K, Latched<S>, Q>,
    ) -> RootRead
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some(version) = self.root_version(root) else {
            return RootRead::Changed;
        };
        // SAFETY: the tree is latched, and its root page allocated; the
        // copy is checked by the root's version below.
        let shared = unsafe { root_page.load_racing_shared_prefix() };
        match shared.and_then(|shared| shared.place_outside(key)) {
            None => RootRead::At(version),
            Some(outside) if root.latch().is_unchanged(version) => RootRead::Outside(outside),
            Some(_) => RootRead::Changed,
        }
    }

    /// `root`'s version, if the tree's root pointer still tags `root`
    /// internal once the version is read; `root` is the root page's internal
    /// node. A lowering that turned the root into a leaf may have finished
    /// between the two reads, and it retags the pointer before letting go.
    #[inline(always)]
    pub(super) fn root_version(&self, root: Internal<K, V, Latched<S>>) -> Option<Version> {
        let version = root.latch().read_version();
        Interleaving::switch_point();
        (self.root.load(Ordering::Acquire) == root.node_ptr().tagged().as_ptr()).then_some(version)
    }

    /// Whether `leaf`, latched, has `key` as its head's key.
    #[cold]
    #[inline(never)]
    pub(super) fn is_head<Q>(
        leaf: Leaf<K, V, Latched<S>>,
        key: &SearchKey<'_, K, Latched<S>, Q>,
    ) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        leaf.has_head() && key.cmp_full(leaf.head_key()) == KeyOrdering::Equal
    }
}
