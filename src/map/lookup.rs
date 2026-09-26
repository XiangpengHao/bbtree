//! Looking up a key: a loop down from the root, one node search per level;
//! and the first or last entry: a loop down the leftmost or rightmost edges,
//! with no search.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{ForceResult, Kv, Leaf, LeafOrInternal, NodeRef, SearchResult};
use crate::node::inline_keys::SearchKey;

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> NodeRef<BorrowType, K, V, S, LeafOrInternal> {
    /// The key/value in the subtree under this node whose key equals `key`,
    /// with the access this handle had, or `None` if there is none.
    ///
    /// The key's inline key, if it has one to compare (D18), is computed
    /// once for the whole descent.
    pub(super) fn search_tree<Q>(self, key: &Q) -> Option<Kv<Self>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let key = SearchKey::new(key);
        let mut node = self;
        loop {
            node = match node.search(&key) {
                SearchResult::Found(kv) => return Some(kv),
                SearchResult::GoDown(edge) => match edge.force() {
                    ForceResult::Leaf(_) => return None,
                    ForceResult::Internal(edge) => edge.descend(),
                },
            };
        }
    }

    /// The entry with the smallest key in the tree under this root: the
    /// first of the leftmost leaf, which has no head (D17); `None` if the
    /// tree is empty.
    pub(super) fn first_leaf_kv(self) -> Option<Kv<NodeRef<BorrowType, K, V, S, Leaf>>> {
        let mut node = self;
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => return leaf.first_kv(),
                ForceResult::Internal(internal) => internal.first_edge().descend(),
            };
        }
    }

    /// The entry with the largest key in the tree under this root: the last
    /// of the rightmost leaf; `None` if the tree is empty.
    pub(super) fn last_leaf_kv(self) -> Option<Kv<NodeRef<BorrowType, K, V, S, Leaf>>> {
        let mut node = self;
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => return leaf.last_kv(),
                ForceResult::Internal(internal) => internal.last_edge().descend(),
            };
        }
    }
}
