//! Inserting into a tree: the entry goes in at the leaf edge where a path to
//! it ends (`node/path.rs`), which a search for the key found. If the leaf
//! is full, it splits, and going back up the path, each node takes its split
//! child's separator key and right half, splitting in turn if it is full; a
//! root that splits grows a new root above it. A separator's value stays in
//! the right half, as the head of its leftmost leaf (D17), and its inline key
//! goes up with it (D18), moved back to its new node's prefix length. Once
//! the entry is in and counted, the halves of every split fit their prefixes
//! to their new fences (`node/prefix.rs`).
//!
//! The path then goes back down to the new entry, through whichever half of
//! each split took it, so that a vacant entry can hand it out, or keep it as
//! an occupied one.

#![forbid(unsafe_code)]

use core::mem;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{Insertion, Internal, Leaf, Subtree};
use crate::node::inline_keys::{InlinedKey, SearchKey};
use crate::node::page::FreshPage;
use crate::node::path::{MAX_HEIGHT, PathMut};

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Inserts `key`/`value` and returns `None`, counting the entry in
    /// `length`; or, if an equal key is there, keeps that key, drops `key`,
    /// replaces its value and returns the old one, as std's
    /// `BTreeMap::insert` does. For a separator key, the value replaced is a
    /// leaf's head (D17).
    ///
    /// Every page a split needs comes from `new_leaf_page` or
    /// `new_internal_page`, by the kind of node that splits (D21), asked for
    /// at the time of the split; a new root's page comes from
    /// `new_internal_page`.
    ///
    /// Every `Ord` call happens in the search, and the key's inline key is
    /// computed next, before anything changes, so a panicking `Ord` or
    /// `InlineKey` there leaves the tree unchanged. After a split, the split
    /// nodes' prefixes fit their new fences, which calls `InlineKey` again,
    /// once the entry is in and counted (`node/prefix.rs`): a panic then
    /// leaves a valid tree that has it. The page sources are called on the way back up,
    /// possibly after the leaf has changed: the global allocator's
    /// `handle_alloc_error` aborts by default, and a panicking page source
    /// would leave a memory-safe tree that is missing entries.
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
        length: &mut usize,
    ) -> Option<V>
    where
        K: Ord,
    {
        let mut path = PathMut::new(self);
        if path.search_expecting_append(&SearchKey::new(&key)) {
            // As in std, the new key is dropped before the value changes: if
            // its `Drop` panics, the map is unchanged.
            drop(key);
            Some(mem::replace(path.into_value_mut(), value))
        } else {
            path.insert(key, value, new_leaf_page, new_internal_page, length);
            None
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> PathMut<'a, K, V, S> {
    /// Inserts `key` and `value` at the leaf edge the path ends at, counts
    /// the entry in `length`, fits the prefixes of the nodes that split,
    /// and moves the path to the new entry; pages come as for
    /// [`Subtree::insert`]. Compares no keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no leaf edge.
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        mut new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        mut new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
        length: &mut usize,
    ) {
        let index = self.index();
        let leaf = self.leaf_mut();
        // The key's window at the leaf's prefix length, which both halves
        // of a split keep: the last `InlineKey` call before the first change.
        let key = InlinedKey::new(key, leaf.prefix_len());
        let split = match leaf.edge_at(index).insert(key, value, &mut new_leaf_page) {
            // The new key is at the index of the edge it went in at.
            Insertion::Fit(_) => {
                *length += 1;
                return;
            }
            Insertion::Split(split) => split,
        };
        // In each node that split, from the leaf up, the index that leads to
        // the new entry in the half that took it: its key's in the leaf, and
        // above, the edge to the child that holds it.
        let mut indexes_in_halves = [0; MAX_HEIGHT + 1];
        indexes_in_halves[0] = split.inserted.index();
        // Above each node that split, where the separator between its halves
        // went, in the node the path goes through.
        let mut separators = [0; MAX_HEIGHT + 1];
        let mut carried = (
            split.separator,
            split.right.forget_type(),
            split.inserted.is_right(),
        );
        let top = loop {
            let (separator, right, entry_is_right) = carried;
            if self.height() == self.root_height() {
                self.push_root(new_internal_page());
            } else {
                self.ascend();
            }
            let (height, index) = (self.height(), self.index());
            let node = self.internal_mut();
            // The new entry is under the edge left of the separator, or right
            // of it if it went into the right half.
            let beside_separator =
                |separator_index: usize| separator_index + usize::from(entry_is_right);
            match node
                .edge_at(index)
                .insert(separator, right, &mut new_internal_page)
            {
                Insertion::Fit(kv) => {
                    let separator_index = kv.index();
                    separators[height] = separator_index;
                    self.set_index(beside_separator(separator_index));
                    break height;
                }
                Insertion::Split(split) => {
                    separators[height] = split.inserted.index();
                    indexes_in_halves[height] = beside_separator(split.inserted.index());
                    carried = (
                        split.separator,
                        split.right.forget_type(),
                        split.inserted.is_right(),
                    );
                }
            }
        };
        // Back down to the new entry, through the halves that took it.
        while self.height() > 0 {
            self.descend();
            self.set_index(indexes_in_halves[self.height()]);
        }
        *length += 1;
        self.fit_split_halves(&separators, top);
    }
}
