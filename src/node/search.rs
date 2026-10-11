//! Searches that need no ancestor path. The returned position retains
//! the root's borrow and resolves a separator's value through its right child.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::handle::{
    ForceResult, InsertSearchResult, KvByKind, LeafOrInternal, NodeRef, SearchResult,
};
use super::inline_keys::SearchKey;
use crate::inline_key::InlineKeyStrategy;

impl<B, K, V, S: InlineKeyStrategy<K>> NodeRef<B, K, V, S, LeafOrInternal> {
    /// Searches downward, retaining only the current node. Updates that
    /// need ancestors and range endpoints use `Path::search`; a point lookup
    /// only needs the position where it ends.
    pub(crate) fn search_tree<Q>(
        mut self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> Option<KvByKind<B, K, V, S>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        loop {
            match self.force() {
                ForceResult::Leaf(leaf) => {
                    return leaf.search_point(key).map(ForceResult::Leaf);
                }
                ForceResult::Internal(internal) => match internal.search(key) {
                    SearchResult::Found(kv) => return Some(ForceResult::Internal(kv)),
                    SearchResult::GoDown(edge) => self = edge.descend(),
                },
            }
        }
    }

    /// Searches in key order, returning an existing key or its vacant leaf
    /// edge. Unlike a point search's equality filter, the leaf search must
    /// establish where an absent key belongs. A fitting insertion needs no
    /// ancestors; a full leaf can be searched again with a path for splitting.
    pub(crate) fn search_tree_for_insert<Q>(
        mut self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> InsertSearchResult<B, K, V, S>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        loop {
            match self.force() {
                ForceResult::Leaf(leaf) => {
                    return match leaf.search(key) {
                        SearchResult::Found(kv) => Ok(ForceResult::Leaf(kv)),
                        SearchResult::GoDown(edge) => Err(edge),
                    };
                }
                ForceResult::Internal(internal) => match internal.search(key) {
                    SearchResult::Found(kv) => return Ok(ForceResult::Internal(kv)),
                    SearchResult::GoDown(edge) => self = edge.descend(),
                },
            }
        }
    }
}
