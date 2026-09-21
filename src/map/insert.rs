//! Inserting into a tree: a recursive descent whose call stack is the path
//! back up (D11). Each level searches its node on the way down; on the way
//! back up, a child that split hands its separator key and right half to its
//! parent, and a root that split grows a new root above it. A separator's
//! value stays in the right half, as the head of its leftmost leaf (D17), and
//! its inline key goes up with it (D18).

#![forbid(unsafe_code)]

use core::mem;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{
    EdgeRoom, ForceResult, Insertion, Internal, Leaf, LeafOrInternal, NodeKind, NodeMut,
    SearchResult, Subtree,
};
use crate::node::inline_keys::{InlinedKey, SearchKey};
use crate::node::page::FreshPage;

impl<K: Ord, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Inserts `key`/`value` and returns `None`; or, if an equal key is
    /// there, keeps that key, drops `key`, replaces its value and returns the
    /// old one, as std's `BTreeMap::insert` does. For a separator key, the
    /// value replaced is a leaf's head (D17).
    ///
    /// Every page a split needs comes from `new_leaf_page` or
    /// `new_internal_page`, by the kind of node that splits (D21), asked for
    /// at the time of the split; a new root's page comes from
    /// `new_internal_page`.
    ///
    /// The key's inline key is computed once, first, and every `Ord` call
    /// happens on the way down, before anything changes, so a panicking
    /// `InlineKey` or `Ord` leaves the tree unchanged. The page sources are
    /// called on the way back up, possibly after the leaf has changed: the
    /// global allocator's `handle_alloc_error` aborts by default, and a
    /// panicking page source would leave a memory-safe tree that is missing
    /// entries.
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        mut new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        mut new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
    ) -> Option<V> {
        let key = InlinedKey::new(key);
        match self
            .borrow_mut()
            .insert_below(key, value, &mut new_leaf_page, &mut new_internal_page)
        {
            InsertOutcome::Replaced(old_value) => Some(old_value),
            InsertOutcome::Fit => None,
            InsertOutcome::Split { separator, right } => {
                let root = self.push_internal_level(new_internal_page());
                let EdgeRoom::Fits(edge) = root.first_edge().check_room() else {
                    unreachable!("a new root has no keys");
                };
                edge.insert_fit(separator, right);
                None
            }
        }
    }
}

/// What inserting into a subtree leaves for the parent of its root to do.
enum InsertOutcome<K, V, S> {
    /// Nothing: the key was there, and this is its old value.
    Replaced(V),
    /// Nothing: the new entry is in the subtree, under the same root.
    Fit,
    /// The new entry is in the subtree, whose root split: the parent takes
    /// the `separator` key, with its inline key, and `right` as the edge to
    /// its right, which holds the separator's value.
    Split {
        separator: InlinedKey<K, S>,
        right: Subtree<K, V, S>,
    },
}

impl<K, V, S, Kind: NodeKind> From<Insertion<'_, K, V, S, Kind>> for InsertOutcome<K, V, S> {
    fn from(insertion: Insertion<'_, K, V, S, Kind>) -> Self {
        match insertion {
            Insertion::Fit(_) => Self::Fit,
            Insertion::Split(split) => Self::Split {
                separator: split.separator,
                right: split.right.forget_type(),
            },
        }
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> NodeMut<'_, K, V, S, LeafOrInternal> {
    /// Inserts `key`/`value` into the subtree under this node: searches this
    /// node, inserts into the leaf or recurses into the child the search
    /// leads to, and on the way back up inserts a split child's separator
    /// and right half here.
    fn insert_below(
        self,
        key: InlinedKey<K, S>,
        value: V,
        new_leaf_page: &mut impl FnMut() -> FreshPage<K, V, S, Leaf>,
        new_internal_page: &mut impl FnMut() -> FreshPage<K, V, S, Internal>,
    ) -> InsertOutcome<K, V, S> {
        let search = self.search_expecting_append(&SearchKey::for_inlined(&key));
        match search {
            SearchResult::Found(kv) => {
                // As in std, the new key is dropped before the value changes:
                // if its `Drop` panics, the map is unchanged.
                drop(key);
                InsertOutcome::Replaced(mem::replace(kv.into_value_mut(), value))
            }
            SearchResult::GoDown(edge) => match edge.force() {
                ForceResult::Leaf(edge) => edge.insert(key, value, new_leaf_page).into(),
                ForceResult::Internal(mut edge) => {
                    let child = edge.reborrow_mut().descend();
                    match child.insert_below(key, value, new_leaf_page, new_internal_page) {
                        InsertOutcome::Split { separator, right } => {
                            edge.insert(separator, right, new_internal_page).into()
                        }
                        done => done,
                    }
                }
            },
        }
    }
}
