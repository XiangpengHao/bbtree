//! What an operation that changes more than its leaf holds.
//!
//! Before it changes any node, an operation takes every node it will change
//! or read a separator of ([`HeldNodes`]), all without waiting:
//! - internal nodes by a compare-and-swap at the version it read them at,
//!   or at the version they are at;
//! - leaves by an exclusive latch.
//!
//! It holds its own leaf already, and waits for nothing while it does, so
//! no two operations wait on each other. A node another thread holds, or
//! one that changed, makes it let go of all and start over.
//!
//! Holding a node holds its separators' keys too, though they are heads of
//! leaves below (D49): only a thread holding the node changes them. A
//! separator's head changes when the separator is removed and replaced by
//! its successor, or when a merge or a move between siblings rotates it
//! through the node; each of those holds the node. So the unsync tree's
//! operations, run on the held nodes, read every key they need where it is.

use super::super::node::internal::Internal;
use super::super::node::latch::{Latched, Version};
use super::super::node::leaf::Leaf;
use super::super::node_stack::MAX_DEPTH;
use super::descent::{PathLevel, TreePath};
use crate::inline_key::InlineKeyStrategy;

/// The nodes an operation holds exclusively, besides the leaf its descent
/// latched: let go of together when dropped, each at the next version, so
/// that a reader that read one meanwhile starts over.
pub(super) struct HeldNodes<K, V, S: InlineKeyStrategy<K>> {
    internals: TakenNodes<Internal<K, V, Latched<S>>, HELD_INTERNALS>,
    leaves: TakenNodes<Leaf<K, V, Latched<S>>, HELD_LEAVES>,
}

/// Up to `CAP` nodes of one kind, each with the version it was taken at.
struct TakenNodes<N, const CAP: usize> {
    nodes: [Option<(N, Version)>; CAP],
    len: usize,
}

/// The most internal nodes one operation holds. A removal holds its path,
/// the sibling of each node on it and the border under a separator it
/// removes. A shrinking of the shared prefix holds the root and the nodes
/// along both borders.
const HELD_INTERNALS: usize = 3 * MAX_DEPTH + 2;

/// The most leaves one operation holds besides its own: a sibling and the
/// last leaf left of a separator it removes; or the two border leaves.
const HELD_LEAVES: usize = 2;

impl<N: Copy, const CAP: usize> TakenNodes<N, CAP> {
    fn new() -> Self {
        Self {
            nodes: [None; CAP],
            len: 0,
        }
    }

    fn iter(&self) -> impl Iterator<Item = (N, Version)> + '_ {
        self.nodes[..self.len].iter().flatten().copied()
    }

    fn push(&mut self, node: N, version: Version) {
        self.nodes[self.len] = Some((node, version));
        self.len += 1;
    }
}

impl<K, V, S: InlineKeyStrategy<K>> HeldNodes<K, V, S> {
    pub(super) fn new() -> Self {
        Self {
            internals: TakenNodes::new(),
            leaves: TakenNodes::new(),
        }
    }

    fn holds_internal(&self, node: Internal<K, V, Latched<S>>) -> bool {
        (self.internals.iter()).any(|(held, _)| held.node_ptr().is(node.node_ptr()))
    }

    fn holds_leaf(&self, leaf: Leaf<K, V, Latched<S>>) -> bool {
        (self.leaves.iter()).any(|(held, _)| held.node_ptr().is(leaf.node_ptr()))
    }

    /// Takes `node` exclusively if it is still at `version`, the one the
    /// caller read it at; `false` if it changed or another thread holds it.
    /// A node held already stays so.
    pub(super) fn take_internal_at(
        &mut self,
        node: Internal<K, V, Latched<S>>,
        version: Version,
    ) -> bool {
        if self.holds_internal(node) {
            return true;
        }
        if !node.latch().try_lock_at(version) {
            return false;
        }
        self.internals.push(node, version);
        true
    }

    /// Takes `node` exclusively at the version it is at, unless another
    /// thread holds it: for a node the caller reached through nodes it
    /// holds, which keep it where it is.
    pub(super) fn take_internal(&mut self, node: Internal<K, V, Latched<S>>) -> bool {
        if self.holds_internal(node) {
            return true;
        }
        node.latch()
            .try_read_version()
            .is_some_and(|version| self.take_internal_at(node, version))
    }

    /// Latches `leaf` exclusively, unless another thread holds it in either
    /// mode. A leaf held already stays so.
    pub(super) fn take_leaf(&mut self, leaf: Leaf<K, V, Latched<S>>) -> bool {
        if self.holds_leaf(leaf) {
            return true;
        }
        let Some(version) = leaf.latch().try_lock_exclusive() else {
            return false;
        };
        self.leaves.push(leaf, version);
        true
    }

    /// Takes the internal nodes of `path` from level `top` down to the
    /// leaf's parent, each at the version the descent read it at: `false`
    /// if one changed since, or another thread holds it. Held, each is
    /// still the parent of the next.
    pub(super) fn take_path(&mut self, path: &TreePath<K, V, S>, top: usize) -> bool {
        (top..path.depth()).all(|level| {
            let PathLevel { node, version, .. } = path.level(level);
            self.take_internal_at(node, version)
        })
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for HeldNodes<K, V, S> {
    fn drop(&mut self) {
        for (node, version) in self.internals.iter() {
            node.latch().unlock_exclusive(version);
        }
        for (leaf, version) in self.leaves.iter() {
            leaf.latch().unlock_exclusive(version);
        }
    }
}
