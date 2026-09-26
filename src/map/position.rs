//! Where an entry is, or where a new one goes, as a path of indexes from the
//! root: what an entry remembers between finding a key and using it, without
//! parent pointers (D11). Following a position down calls no `Ord`.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{Edge, ForceResult, Kv, LeafOrInternal, NodeKind, NodeRef, SearchResult};
use crate::node::inline_keys::SearchKey;
use crate::node::walk::MAX_HEIGHT;

/// A path from a tree's root: the edge taken at each height above
/// `height`, and at `height`, the index of a key, or of the edge where a new
/// key goes. It stays right while the tree it was found in is unchanged, or
/// changed only by the insert that moved it (`insert.rs`).
///
/// A node holds at most 1023 keys (`layout.rs`), so every index fits in a
/// `u16`.
#[derive(Clone, Copy)]
pub(super) struct Position {
    indexes: [u16; MAX_HEIGHT + 1],
    /// The height of the node the path ends in.
    height: usize,
}

/// What a search for a key found.
pub(super) enum KeyPosition {
    /// The key is there, at this position.
    Occupied(Position),
    /// The key is not there; this is the edge of a leaf where it goes.
    Vacant(Position),
}

impl Position {
    /// The first edge of a tree that is one leaf: where a new key goes in an
    /// empty tree.
    pub(super) const START_OF_LEAF: Self = Self {
        indexes: [0; MAX_HEIGHT + 1],
        height: 0,
    };

    /// Searches the tree under `root` for `key`, as `insert` does, with every
    /// `Ord` call of the entry's lifetime.
    pub(super) fn of_key<BorrowType, K, V, S, Q>(
        root: NodeRef<BorrowType, K, V, S, LeafOrInternal>,
        key: &SearchKey<'_, K, S, Q>,
    ) -> KeyPosition
    where
        S: InlineKeyStrategy<K>,
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut position = Self::START_OF_LEAF;
        let mut node = root;
        loop {
            let height = node.height();
            node = match node.search_expecting_append(key) {
                SearchResult::Found(kv) => {
                    position.height = height;
                    position.indexes[height] = kv.index() as u16;
                    return KeyPosition::Occupied(position);
                }
                SearchResult::GoDown(edge) => {
                    position.indexes[height] = edge.index() as u16;
                    match edge.force() {
                        ForceResult::Leaf(_) => return KeyPosition::Vacant(position),
                        ForceResult::Internal(edge) => edge.descend(),
                    }
                }
            };
        }
    }

    /// The position of the entry with the smallest key in the tree under
    /// `root`, or `None` if the tree is empty: the first of its leftmost
    /// leaf, which has no head (D17).
    pub(super) fn of_first<BorrowType, K, V, S: InlineKeyStrategy<K>>(
        root: NodeRef<BorrowType, K, V, S, LeafOrInternal>,
    ) -> Option<Self> {
        let position = Self::START_OF_LEAF;
        let mut node = root;
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => return (leaf.len() > 0).then_some(position),
                ForceResult::Internal(internal) => internal.first_edge().descend(),
            };
        }
    }

    /// The position of the entry with the largest key in the tree under
    /// `root`, or `None` if the tree is empty: the last of its rightmost
    /// leaf.
    pub(super) fn of_last<BorrowType, K, V, S: InlineKeyStrategy<K>>(
        root: NodeRef<BorrowType, K, V, S, LeafOrInternal>,
    ) -> Option<Self> {
        let mut position = Self::START_OF_LEAF;
        let mut node = root;
        loop {
            let height = node.height();
            node = match node.force() {
                ForceResult::Leaf(leaf) => {
                    let last = leaf.last_kv()?;
                    position.indexes[0] = last.index() as u16;
                    return Some(position);
                }
                ForceResult::Internal(internal) => {
                    let edge = internal.last_edge();
                    position.indexes[height] = edge.index() as u16;
                    edge.descend()
                }
            };
        }
    }

    /// The height of the node the path ends in.
    pub(super) fn height(&self) -> usize {
        self.height
    }

    /// The index the path takes at `height`.
    pub(super) fn index_at(&self, height: usize) -> usize {
        usize::from(self.indexes[height])
    }

    /// Makes the path take index `index` at `height`.
    pub(super) fn set_index_at(&mut self, height: usize, index: usize) {
        self.indexes[height] = index as u16;
    }

    /// The edge the path takes in `node`, which is on the path, at or above
    /// the height it ends at.
    pub(super) fn edge_in<BorrowType, K, V, S, Kind>(
        &self,
        node: NodeRef<BorrowType, K, V, S, Kind>,
    ) -> Edge<NodeRef<BorrowType, K, V, S, Kind>>
    where
        Kind: NodeKind,
    {
        let index = self.index_at(node.height());
        node.edge_at(index)
    }

    /// The key/value at this position, which is an entry's, in the tree
    /// under `root`, with the access `root` has.
    pub(super) fn kv<BorrowType, K, V, S: InlineKeyStrategy<K>>(
        &self,
        root: NodeRef<BorrowType, K, V, S, LeafOrInternal>,
    ) -> Kv<NodeRef<BorrowType, K, V, S, LeafOrInternal>> {
        let mut node = root;
        while node.height() > self.height {
            node = match self.edge_in(node).force() {
                ForceResult::Internal(edge) => edge.descend(),
                ForceResult::Leaf(_) => unreachable!("a position ends at or above the leaves"),
            };
        }
        node.kv_at(self.index_at(self.height))
    }
}
