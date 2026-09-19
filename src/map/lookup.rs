//! Looking up a key: a loop down from the root, one node search per level.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use crate::node::handle::{ForceResult, Kv, LeafOrInternal, NodeRef, SearchResult};

impl<BorrowType, K, V> NodeRef<BorrowType, K, V, LeafOrInternal> {
    /// The key/value in the subtree under this node whose key equals `key`,
    /// with the access this handle had, or `None` if there is none.
    pub(super) fn search_tree<Q>(self, key: &Q) -> Option<Kv<Self>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut node = self;
        loop {
            node = match node.search(key) {
                SearchResult::Found(kv) => return Some(kv),
                SearchResult::GoDown(edge) => match edge.force() {
                    ForceResult::Leaf(_) => return None,
                    ForceResult::Internal(edge) => edge.descend(),
                },
            };
        }
    }
}
