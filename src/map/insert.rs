//! Inserting into a tree: the entry goes in at the leaf edge where a path to
//! it ends (`node/path.rs`), which a search for the key found. If the leaf
//! is full, it splits, and going back up the path, each node takes its split
//! child's separator key and right half, splitting in turn if it is full; a
//! root that splits grows a new root above it. A separator's value stays in
//! the right half, as the head of its leftmost leaf (D17), and its inline key
//! goes up with it (D18).
//!
//! The path then goes back down to the new entry, through whichever half of
//! each split took it, so that a vacant entry can hand it out, or keep it as
//! an occupied one.

#![forbid(unsafe_code)]

use core::mem;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{ForceResult, Insertion, Internal, Leaf, Subtree};
use crate::node::inline_keys::{InlinedKey, SearchKey};
use crate::node::page::FreshPage;
use crate::node::path::{MAX_HEIGHT, PathMut};

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
    /// happens in the search, before anything changes, so a panicking
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
        let key = InlinedKey::new(key);
        let mut path = PathMut::new(self);
        if path.search_expecting_append(&SearchKey::for_inlined(&key)) {
            // As in std, the new key is dropped before the value changes: if
            // its `Drop` panics, the map is unchanged.
            drop(key);
            Some(mem::replace(path.into_value_mut(), value))
        } else {
            path.insert(key, value, new_leaf_page, new_internal_page);
            None
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> PathMut<'a, K, V, S> {
    /// Inserts `key`, with its inline key, and `value` at the leaf edge the
    /// path ends at, and moves the path to the new entry; pages come as for
    /// [`Subtree::insert`]. Compares no keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no leaf edge.
    pub(crate) fn insert(
        &mut self,
        key: InlinedKey<K, S>,
        value: V,
        mut new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        mut new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
    ) {
        let index = self.index();
        let ForceResult::Leaf(leaf) = self.node_mut().force() else {
            panic!("inserting above the leaves");
        };
        let split = match leaf.edge_at(index).insert(key, value, &mut new_leaf_page) {
            // The new key is at the index of the edge it went in at.
            Insertion::Fit(_) => return,
            Insertion::Split(split) => split,
        };
        // In each node that split, from the leaf up, the index that leads to
        // the new entry in the half that took it: its key's in the leaf, and
        // above, the edge to the child that holds it.
        let mut indexes_in_halves = [0; MAX_HEIGHT + 1];
        indexes_in_halves[0] = split.inserted.index();
        let mut carried = (
            split.separator,
            split.right.forget_type(),
            split.inserted.is_right(),
        );
        loop {
            let (separator, right, entry_is_right) = carried;
            if self.height() == self.root_height() {
                self.push_root(new_internal_page());
            } else {
                self.ascend();
            }
            let (height, index) = (self.height(), self.index());
            let ForceResult::Internal(node) = self.node_mut().force() else {
                unreachable!("a node above another is internal");
            };
            // The new entry is under the edge left of the separator, or right
            // of it if it went into the right half.
            let beside_separator =
                |separator_index: usize| separator_index + usize::from(entry_is_right);
            match node
                .edge_at(index)
                .insert(separator, right, &mut new_internal_page)
            {
                Insertion::Fit(kv) => {
                    let index = beside_separator(kv.index());
                    self.set_index(index);
                    break;
                }
                Insertion::Split(split) => {
                    indexes_in_halves[height] = beside_separator(split.inserted.index());
                    carried = (
                        split.separator,
                        split.right.forget_type(),
                        split.inserted.is_right(),
                    );
                }
            }
        }
        // Back down to the new entry, through the halves that took it.
        while self.height() > 0 {
            self.descend();
            self.set_index(indexes_in_halves[self.height()]);
        }
    }
}
