//! One end of a walk over a tree in key order: a position between two
//! entries, kept as the edge indexes that lead to it from the root, with the
//! leaf and its parent at hand (D23). Moving it to the next or previous entry
//! never calls `Ord` (D11).
//!
//! A position is always an edge of a leaf. Within the leaf, the next entry is
//! beside it. Past the leaf's last entry, it is the separator of the lowest
//! ancestor that has a key right of the path, whose value is the head of the
//! leftmost leaf right of that key (D17); the cursor descends to that leaf and
//! starts there. Moving backward mirrors this: before the leaf's first entry,
//! the previous one is the separator of the lowest ancestor with a key left of
//! the path, whose value is this leaf's head, and the cursor goes on from the
//! end of the rightmost leaf left of that key.
//!
//! The parent answers most of those trips up the tree: only when the path's
//! edge is the parent's last (or first) does the cursor walk down again from
//! the root, by its edge indexes, to find the separator further up.
//!
//! A cursor changes in place, field by field, and is never rebuilt and
//! copied over: a copy reads the cursor with wide loads right after narrow
//! stores to it, which the CPU can't forward, so the copy waits for the
//! stores to retire, and with them every older cache miss. Over many small
//! maps, that serialized their misses (D23).

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{
    Edge, ForceResult, Immut, Internal, Kv, Leaf, LeafOrInternal, NodeKind, NodeRef, SearchResult,
};
use crate::node::inline_keys::SearchKey;

/// The most levels above the leaves that a tree can have. A non-root
/// internal node has at least 3 children (D13, D22) and a leaf takes at
/// least 1 KB (D15), so a tree one level taller would need more than 2^64
/// bytes of leaves.
const MAX_HEIGHT: usize = 34;

/// A shared handle to a tree's root, which a cursor walks down from.
pub(super) type Root<'a, K, V, S> = NodeRef<Immut<'a>, K, V, S, LeafOrInternal>;

/// Which way a cursor moves through the entries.
#[derive(Clone, Copy)]
enum Direction {
    /// Toward greater keys.
    Forward,
    /// Toward smaller keys.
    Backward,
}

impl Direction {
    /// The edge where a walk in this direction enters a node of `len` keys:
    /// its first, or its last.
    fn entry_edge(self, len: usize) -> usize {
        match self {
            Self::Forward => 0,
            Self::Backward => len,
        }
    }

    /// The key next to `edge` in this direction, if any.
    fn key_beyond<'a, K, V, S, Kind: NodeKind>(
        self,
        edge: Edge<NodeRef<Immut<'a>, K, V, S, Kind>>,
    ) -> Option<Kv<NodeRef<Immut<'a>, K, V, S, Kind>>> {
        match self {
            Self::Forward => edge.right_kv().ok(),
            Self::Backward => edge.left_kv().ok(),
        }
    }
}

/// Where a cursor stands relative to a key it looked up.
#[derive(Clone, Copy)]
pub(super) enum KeySide {
    /// Just before the key, or where it would be.
    Before,
    /// Just after the key, or where it would be.
    After,
}

/// One end of a walk: a position between two entries of a leaf, or not
/// started yet, in which case its first move starts it at the first or last
/// position of the tree, by the direction of the move.
///
/// Invariant, once started: `edges[0]` is an edge of `leaf`, and for every
/// height `h` from 1 to the root's, `edges[h]` is the edge of the node at
/// height `h` that leads toward `leaf`: walking down from the root by them
/// reaches it. `parent` is the node at height 1, if the root is not the leaf
/// itself. Entries above the root's height are 0. A node holds at most 1023
/// keys (`layout.rs`), so every edge index fits in a `u16`.
pub(super) struct LeafCursor<'a, K, V, S> {
    /// `None` until the cursor starts.
    leaf: Option<NodeRef<Immut<'a>, K, V, S, Leaf>>,
    parent: Option<NodeRef<Immut<'a>, K, V, S, Internal>>,
    edges: [u16; MAX_HEIGHT + 1],
}

impl<K, V, S> Clone for LeafCursor<'_, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for LeafCursor<'_, K, V, S> {}

impl<'a, K, V, S: InlineKeyStrategy<K>> LeafCursor<'a, K, V, S> {
    /// A cursor that has not started.
    pub(super) const UNSTARTED: Self = Self {
        leaf: None,
        parent: None,
        edges: [0; MAX_HEIGHT + 1],
    };

    /// Moves to the position before the first entry of the tree under `root`.
    pub(super) fn move_to_first(&mut self, root: Root<'a, K, V, S>) {
        self.move_to_end(root, Direction::Forward);
    }

    /// Moves to the position after the last entry of the tree under `root`.
    pub(super) fn move_to_last(&mut self, root: Root<'a, K, V, S>) {
        self.move_to_end(root, Direction::Backward);
    }

    /// Moves to the position just before or just after `key` in the tree
    /// under `root`, or, if no entry has that key, where it would be. A
    /// separator stands between the end of the leaf left of it and the start
    /// of the leaf right of it (D17).
    pub(super) fn move_beside_key<Q>(
        &mut self,
        root: Root<'a, K, V, S>,
        key: &SearchKey<'_, K, S, Q>,
        side: KeySide,
    ) where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut parent = None;
        let mut node = root;
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => {
                    let edge = match (leaf.search(key), side) {
                        (SearchResult::Found(kv), KeySide::Before) => kv.left_edge(),
                        (SearchResult::Found(kv), KeySide::After) => kv.right_edge(),
                        (SearchResult::GoDown(edge), _) => edge,
                    };
                    self.edges[0] = edge.index() as u16;
                    self.parent = parent;
                    self.leaf = Some(leaf);
                    return;
                }
                ForceResult::Internal(internal) => match internal.search(key) {
                    SearchResult::Found(separator) => {
                        match side {
                            KeySide::Before => {
                                self.descend(separator.left_edge(), Direction::Backward)
                            }
                            KeySide::After => {
                                self.descend(separator.right_edge(), Direction::Forward)
                            }
                        };
                        return;
                    }
                    SearchResult::GoDown(edge) => {
                        self.edges[internal.height()] = edge.index() as u16;
                        parent = Some(internal);
                        edge.descend()
                    }
                },
            };
        }
    }

    /// Moves over the next entry and returns it, or returns `None`, staying
    /// put, at the end of the tree under `root`. A cursor that has not
    /// started starts at the first position.
    pub(super) fn next(&mut self, root: Root<'a, K, V, S>) -> Option<(&'a K, &'a V)> {
        let leaf = match self.leaf {
            Some(leaf) => leaf,
            None => self.move_to_end(root, Direction::Forward),
        };
        if let Ok(kv) = leaf.edge_at(usize::from(self.edges[0])).right_kv() {
            self.edges[0] += 1;
            return Some(kv.into_key_value());
        }
        let separator = self.separator_beyond_leaf(root, Direction::Forward)?;
        let leaf = self.descend(separator.right_edge(), Direction::Forward);
        let head = leaf.head().expect("a leaf right of a separator has a head");
        Some((separator.into_key(), head.into_value()))
    }

    /// Moves back over the previous entry and returns it, or returns `None`,
    /// staying put, at the start of the tree under `root`. A cursor that has
    /// not started starts at the last position.
    pub(super) fn next_back(&mut self, root: Root<'a, K, V, S>) -> Option<(&'a K, &'a V)> {
        let leaf = match self.leaf {
            Some(leaf) => leaf,
            None => self.move_to_end(root, Direction::Backward),
        };
        if let Ok(kv) = leaf.edge_at(usize::from(self.edges[0])).left_kv() {
            self.edges[0] -= 1;
            return Some(kv.into_key_value());
        }
        let separator = self.separator_beyond_leaf(root, Direction::Backward)?;
        let head = leaf.head().expect("a leaf right of a separator has a head");
        self.descend(separator.left_edge(), Direction::Backward);
        Some((separator.into_key(), head.into_value()))
    }

    /// How this position compares with `other`, a position in the same tree:
    /// in key order, by the edge indexes from the root down. Both cursors
    /// have started.
    pub(super) fn cmp_position(&self, other: &Self) -> Ordering {
        self.edges.iter().rev().cmp(other.edges.iter().rev())
    }

    /// Whether this position is `other`, a position in the same tree. Both
    /// cursors have started. Checks the leaf's edge first, which tells apart
    /// most positions.
    pub(super) fn is_at(&self, other: &Self) -> bool {
        self.edges[0] == other.edges[0] && self.edges == other.edges
    }

    /// Moves to the start of the tree under `root` going forward, or to its
    /// end going backward, and returns the leaf it is in.
    fn move_to_end(
        &mut self,
        root: Root<'a, K, V, S>,
        direction: Direction,
    ) -> NodeRef<Immut<'a>, K, V, S, Leaf> {
        match root.force() {
            ForceResult::Leaf(leaf) => {
                self.edges[0] = direction.entry_edge(leaf.len()) as u16;
                self.parent = None;
                self.leaf = Some(leaf);
                leaf
            }
            ForceResult::Internal(internal) => {
                let edge = internal.edge_at(direction.entry_edge(internal.len()));
                self.descend(edge, direction)
            }
        }
    }

    /// Takes `edge` and goes on down to a leaf, along first edges to the
    /// leaf's start going forward, or along last edges to its end going
    /// backward; returns the leaf. The edges above `edge` stay.
    fn descend(
        &mut self,
        edge: Edge<NodeRef<Immut<'a>, K, V, S, Internal>>,
        direction: Direction,
    ) -> NodeRef<Immut<'a>, K, V, S, Leaf> {
        let mut parent = edge.into_node();
        self.edges[parent.height()] = edge.index() as u16;
        let mut node = edge.descend();
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => {
                    self.edges[0] = direction.entry_edge(leaf.len()) as u16;
                    self.parent = Some(parent);
                    self.leaf = Some(leaf);
                    return leaf;
                }
                ForceResult::Internal(internal) => {
                    let edge = internal.edge_at(direction.entry_edge(internal.len()));
                    self.edges[internal.height()] = edge.index() as u16;
                    parent = internal;
                    edge.descend()
                }
            };
        }
    }

    /// The separator next to the path in `direction` in the lowest internal
    /// node that has one: the next entry that way once the leaf's own are
    /// passed. `None` at the start or end of the tree.
    ///
    /// The parent is checked first. Only if the path leaves it by its last
    /// edge (or first, going backward) does the search walk down again from
    /// `root`, by the edge indexes, to the nodes above the parent.
    fn separator_beyond_leaf(
        &self,
        root: Root<'a, K, V, S>,
        direction: Direction,
    ) -> Option<Kv<NodeRef<Immut<'a>, K, V, S, Internal>>> {
        let parent = self.parent?;
        let parent_edge = parent.edge_at(usize::from(self.edges[1]));
        if let Some(separator) = direction.key_beyond(parent_edge) {
            return Some(separator);
        }
        let mut lowest = None;
        let mut node = root;
        while let ForceResult::Internal(internal) = node.force()
            && internal.height() > 1
        {
            let edge = internal.edge_at(usize::from(self.edges[internal.height()]));
            lowest = direction.key_beyond(edge).or(lowest);
            node = edge.descend();
        }
        lowest
    }
}
