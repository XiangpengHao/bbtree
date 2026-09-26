//! Inserting into a tree: a recursive descent whose call stack is the path
//! back up (D11). Each level finds its edge on the way down: by searching for
//! the key, or by a [`Position`] that a search found earlier, for an entry,
//! without comparing keys. On the way back up, a child that split hands its
//! separator key and right half to its parent, and a root that split grows a
//! new root above it. A separator's value stays in the right half, as the
//! head of its leftmost leaf (D17), and its inline key goes up with it
//! (D18).
//!
//! On the way back up, each level also records where the new entry ended
//! up, so that a vacant entry can hand out its value afterward. A split
//! takes the new entry into one of its halves; a level that took a split
//! child's separator holds the new entry under the edge left of that
//! separator, or right of it if the entry went to the child's right half.

#![forbid(unsafe_code)]

use core::mem;

use super::position::Position;
use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{
    EdgeRoom, ForceResult, Insertion, InsertionSide, Internal, Leaf, LeafOrInternal, NodeKind,
    NodeMut, SearchResult, Subtree,
};
use crate::node::inline_keys::{InlinedKey, SearchKey};
use crate::node::page::FreshPage;

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
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
        new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
    ) -> Option<V>
    where
        K: Ord,
    {
        self.insert_toward(
            &ByKey,
            InlinedKey::new(key),
            value,
            new_leaf_page,
            new_internal_page,
            &mut Unrecorded,
        )
    }

    /// Inserts `key`/`value` at `position`, an edge of a leaf where a search
    /// found the key belongs, without comparing keys, and moves `position`
    /// to the new entry, wherever the splits took it. Pages come as for
    /// [`Self::insert`].
    pub(super) fn insert_at(
        &mut self,
        position: &mut Position,
        key: InlinedKey<K, S>,
        value: V,
        new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
    ) {
        let target = *position;
        let replaced = self.insert_toward(
            &target,
            key,
            value,
            new_leaf_page,
            new_internal_page,
            position,
        );
        debug_assert!(replaced.is_none(), "a position for a new key held a key");
    }

    /// Inserts toward `target`, and records in `placed` where the new entry
    /// ended up, unless the key was there; see [`Self::insert`].
    fn insert_toward(
        &mut self,
        target: &impl InsertionTarget<K, S>,
        key: InlinedKey<K, S>,
        value: V,
        mut new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        mut new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
        placed: &mut impl Placement,
    ) -> Option<V> {
        let outcome = self.borrow_mut().insert_below(
            target,
            key,
            value,
            &mut new_leaf_page,
            &mut new_internal_page,
            placed,
        );
        match outcome {
            InsertOutcome::Replaced(old_value) => Some(old_value),
            InsertOutcome::Fit => None,
            InsertOutcome::Split {
                separator,
                right,
                new_entry_in_right,
            } => {
                let root = self.push_internal_level(new_internal_page());
                placed.record(root.height(), usize::from(new_entry_in_right));
                let EdgeRoom::Fits(edge) = root.first_edge().check_room() else {
                    unreachable!("a new root has no keys");
                };
                edge.insert_fit(separator, right);
                None
            }
        }
    }
}

/// How an insert finds the edge to go down, in each node on the way.
trait InsertionTarget<K, S> {
    fn locate<'a, V, Kind: NodeKind>(
        &self,
        node: NodeMut<'a, K, V, S, Kind>,
        key: &InlinedKey<K, S>,
    ) -> SearchResult<NodeMut<'a, K, V, S, Kind>>;
}

/// By searching for the key, as `insert` does: an equal key is found.
struct ByKey;

impl<K: Ord, S: InlineKeyStrategy<K>> InsertionTarget<K, S> for ByKey {
    fn locate<'a, V, Kind: NodeKind>(
        &self,
        node: NodeMut<'a, K, V, S, Kind>,
        key: &InlinedKey<K, S>,
    ) -> SearchResult<NodeMut<'a, K, V, S, Kind>> {
        node.search_expecting_append(&SearchKey::for_inlined(key))
    }
}

/// By the edges of a position a search found for the key, comparing nothing.
impl<K, S: InlineKeyStrategy<K>> InsertionTarget<K, S> for Position {
    fn locate<'a, V, Kind: NodeKind>(
        &self,
        node: NodeMut<'a, K, V, S, Kind>,
        _key: &InlinedKey<K, S>,
    ) -> SearchResult<NodeMut<'a, K, V, S, Kind>> {
        SearchResult::GoDown(self.edge_in(node))
    }
}

/// Where an insert records the new entry's place, one level at a time on
/// the way back up: the index that leads to it in the node at each height.
trait Placement {
    fn record(&mut self, height: usize, index: usize);
}

/// For a vacant entry, which hands out the new value afterward.
impl Placement for Position {
    fn record(&mut self, height: usize, index: usize) {
        self.set_index_at(height, index);
    }
}

/// For `insert`, which doesn't need the place: records nothing, at no cost.
struct Unrecorded;

impl Placement for Unrecorded {
    fn record(&mut self, _height: usize, _index: usize) {}
}

/// What inserting into a subtree leaves for the parent of its root to do.
enum InsertOutcome<K, V, S> {
    /// Nothing: the key was there, and this is its old value.
    Replaced(V),
    /// Nothing: the new entry is in the subtree, under the same root.
    Fit,
    /// The new entry is in the subtree, whose root split: the parent takes
    /// the `separator` key, with its inline key, and `right` as the edge to
    /// its right, which holds the separator's value. The new entry is under
    /// that edge if `new_entry_in_right`, and under the one left of the
    /// separator otherwise.
    Split {
        separator: InlinedKey<K, S>,
        right: Subtree<K, V, S>,
        new_entry_in_right: bool,
    },
}

impl<K, V, S: InlineKeyStrategy<K>> NodeMut<'_, K, V, S, LeafOrInternal> {
    /// Inserts `key`/`value` into the subtree under this node toward
    /// `target`: finds this node's edge, inserts into the leaf or recurses
    /// into the child the edge leads to, and on the way back up inserts a
    /// split child's separator and right half here. Records in `placed`, for
    /// this node's height, the index that leads to the new entry.
    fn insert_below(
        self,
        target: &impl InsertionTarget<K, S>,
        key: InlinedKey<K, S>,
        value: V,
        new_leaf_page: &mut impl FnMut() -> FreshPage<K, V, S, Leaf>,
        new_internal_page: &mut impl FnMut() -> FreshPage<K, V, S, Internal>,
        placed: &mut impl Placement,
    ) -> InsertOutcome<K, V, S> {
        let height = self.height();
        match target.locate(self, &key) {
            SearchResult::Found(kv) => {
                // As in std, the new key is dropped before the value changes:
                // if its `Drop` panics, the map is unchanged.
                drop(key);
                InsertOutcome::Replaced(mem::replace(kv.into_value_mut(), value))
            }
            SearchResult::GoDown(edge) => match edge.force() {
                ForceResult::Leaf(edge) => match edge.insert(key, value, new_leaf_page) {
                    Insertion::Fit(kv) => {
                        placed.record(height, kv.index());
                        InsertOutcome::Fit
                    }
                    Insertion::Split(split) => {
                        placed.record(height, split.inserted.index());
                        InsertOutcome::Split {
                            separator: split.separator,
                            right: split.right.forget_type(),
                            new_entry_in_right: split.inserted.is_right(),
                        }
                    }
                },
                ForceResult::Internal(mut edge) => {
                    let child = edge.reborrow_mut().descend();
                    let child_outcome = child.insert_below(
                        target,
                        key,
                        value,
                        new_leaf_page,
                        new_internal_page,
                        placed,
                    );
                    match child_outcome {
                        InsertOutcome::Fit => {
                            placed.record(height, edge.index());
                            InsertOutcome::Fit
                        }
                        InsertOutcome::Split {
                            separator,
                            right,
                            new_entry_in_right,
                        } => {
                            let beside_separator = |separator_index: usize| {
                                separator_index + usize::from(new_entry_in_right)
                            };
                            match edge.insert(separator, right, new_internal_page) {
                                Insertion::Fit(kv) => {
                                    placed.record(height, beside_separator(kv.index()));
                                    InsertOutcome::Fit
                                }
                                Insertion::Split(split) => {
                                    let inserted: InsertionSide = split.inserted;
                                    placed.record(height, beside_separator(inserted.index()));
                                    InsertOutcome::Split {
                                        separator: split.separator,
                                        right: split.right.forget_type(),
                                        new_entry_in_right: inserted.is_right(),
                                    }
                                }
                            }
                        }
                        replaced => replaced,
                    }
                }
            },
        }
    }
}
