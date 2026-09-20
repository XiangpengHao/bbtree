//! Inserting into a tree: a recursive descent whose call stack is the path
//! back up (D11). Each level searches its node on the way down; on the way
//! back up, a child that split hands its separator and right half to its
//! parent, and a root that split grows a new root above it.

#![forbid(unsafe_code)]

use core::mem;

use crate::node::handle::{
    EdgeRoom, ForceResult, Insertion, LeafOrInternal, NodeKind, NodeMut, SearchResult, Subtree,
};
use crate::node::page::FreshPage;

impl<K: Ord, V> Subtree<K, V> {
    /// Inserts `key`/`value` and returns `None`; or, if an equal key is
    /// there, keeps that key, drops `key`, replaces its value and returns the
    /// old one, as std's `BTreeMap::insert` does.
    ///
    /// Every page a split needs comes from `new_page`, asked for at the time
    /// of the split.
    ///
    /// Every `Ord` call happens on the way down, before anything changes, so
    /// a panicking `Ord` leaves the tree unchanged. `new_page` is called on
    /// the way back up, possibly after the leaf has changed: the global
    /// allocator's `handle_alloc_error` aborts by default, and a panicking
    /// `new_page` would leave a memory-safe tree that is missing entries.
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        mut new_page: impl FnMut() -> FreshPage<K, V>,
    ) -> Option<V> {
        match self.borrow_mut().insert_below(key, value, &mut new_page) {
            InsertOutcome::Replaced(old_value) => Some(old_value),
            InsertOutcome::Fit => None,
            InsertOutcome::Split {
                separator: (key, value),
                right,
            } => {
                let root = self.push_internal_level(new_page());
                let EdgeRoom::Fits(edge) = root.first_edge().check_room() else {
                    unreachable!("a new root has no keys");
                };
                edge.insert_fit(key, value, right);
                None
            }
        }
    }
}

/// What inserting into a subtree leaves for the parent of its root to do.
enum InsertOutcome<K, V> {
    /// Nothing: the key was there, and this is its old value.
    Replaced(V),
    /// Nothing: the new entry is in the subtree, under the same root.
    Fit,
    /// The new entry is in the subtree, whose root split: the parent takes
    /// `separator`, with `right` as the edge to its right.
    Split {
        separator: (K, V),
        right: Subtree<K, V>,
    },
}

impl<K, V, Kind: NodeKind> From<Insertion<'_, K, V, Kind>> for InsertOutcome<K, V> {
    fn from(insertion: Insertion<'_, K, V, Kind>) -> Self {
        match insertion {
            Insertion::Fit(_) => Self::Fit,
            Insertion::Split(split) => Self::Split {
                separator: split.separator,
                right: split.right.forget_type(),
            },
        }
    }
}

impl<K: Ord, V> NodeMut<'_, K, V, LeafOrInternal> {
    /// Inserts `key`/`value` into the subtree under this node: searches this
    /// node, inserts into the leaf or recurses into the child the search
    /// leads to, and on the way back up inserts a split child's separator
    /// and right half here.
    fn insert_below(
        self,
        key: K,
        value: V,
        new_page: &mut impl FnMut() -> FreshPage<K, V>,
    ) -> InsertOutcome<K, V> {
        match self.search_expecting_append(&key) {
            SearchResult::Found(kv) => {
                // As in std, the new key is dropped before the value changes:
                // if its `Drop` panics, the map is unchanged.
                drop(key);
                InsertOutcome::Replaced(mem::replace(kv.into_value_mut(), value))
            }
            SearchResult::GoDown(edge) => match edge.force() {
                ForceResult::Leaf(edge) => edge.insert(key, value, new_page).into(),
                ForceResult::Internal(mut edge) => {
                    let child = edge.reborrow_mut().descend();
                    match child.insert_below(key, value, new_page) {
                        InsertOutcome::Split {
                            separator: (key, value),
                            right,
                        } => edge.insert(key, value, right, new_page).into(),
                        done => done,
                    }
                }
            },
        }
    }
}
