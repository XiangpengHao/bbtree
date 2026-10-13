//! Inserting into a tree: an owner may remember the last leaf after a split,
//! allowing a greater key to append directly while it has room. Otherwise a
//! downward search finds the entry or vacant leaf edge. A fitting insertion
//! needs no ancestors. If the leaf is full, a
//! second search records its path (`path.rs`) for splitting. Going
//! back up the path, each node takes its split child's separator key and
//! right half, splitting in turn if it is full; a
//! root that splits grows a new root above it. A separator's value stays in
//! the right half, as the head of its leftmost leaf (D17), and its inline key
//! goes up with it (D18), moved back to its new node's prefix length. Once
//! the entry is in and counted, the halves of every split fit their prefixes
//! to their new fences (`prefix.rs`).
//!
//! The path then goes back down to the new entry, through whichever half of
//! each split took it, so that a vacant entry can hand it out, or keep it as
//! an occupied one.

#![forbid(unsafe_code)]

use core::mem;

use super::node::handle::{Edge, EdgeRoom, Insertion, Internal, Leaf, NodeMut, Subtree};
use super::node::inline_keys::{InlinedKey, SearchKey};
use super::node::page::FreshPage;
use super::path::{MAX_HEIGHT, PathMut};
use crate::inline_key::InlineKeyStrategy;

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
    /// Every `Ord` call happens while validating an append or searching,
    /// and the key's inline key is computed next, before anything changes,
    /// so a panicking `Ord` or
    /// `InlineKey` there leaves the tree unchanged. After a split, the split
    /// nodes' prefixes fit their new fences, which calls `InlineKey` again,
    /// once the entry is in and counted (`prefix.rs`): a panic then
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
        if let Some(append) = self.rightmost_append(&key) {
            append.insert(key, value);
            *length += 1;
            return None;
        }
        match self
            .borrow_mut()
            .search_tree_for_insert(&SearchKey::new(&key))
        {
            Ok(kv) => {
                // Keep std's drop-before-replacement behavior on this path
                // too: a panicking key destructor leaves the value intact.
                drop(key);
                return Some(mem::replace(kv.into_value_mut(), value));
            }
            Err(edge) => {
                let prefix_len = edge.reborrow().into_node().prefix_len();
                if let EdgeRoom::Fits(edge) = edge.check_room() {
                    // Compute the summary before the first mutation.
                    edge.insert_fit(InlinedKey::new(key, prefix_len), value);
                    *length += 1;
                    return None;
                }
            }
        }
        self.insert_with_path(key, value, new_leaf_page, new_internal_page, length)
    }

    /// A full leaf needs ancestors to split. Keep that path and its stack
    /// space out of fitting inserts. Search again rather than trusting the
    /// first result: even an inconsistent `Ord` may now find an existing key
    /// or a different leaf with room, both handled by the ordinary path.
    #[inline(never)]
    fn insert_with_path(
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
        if path.search(&SearchKey::new(&key)) {
            // As in std, the new key is dropped before the value changes: if
            // its `Drop` panics, the map is unchanged.
            drop(key);
            Some(mem::replace(path.value_mut(), value))
        } else {
            path.insert(key, value, new_leaf_page, new_internal_page, length);
            path.finish_insertion();
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
    #[inline]
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        new_leaf_page: impl FnMut() -> FreshPage<K, V, S, Leaf>,
        new_internal_page: impl FnMut() -> FreshPage<K, V, S, Internal>,
        length: &mut usize,
    ) {
        let index = self.index();
        let leaf = self.leaf_mut();
        let prefix_len = leaf.prefix_len();
        match leaf.edge_at(index).check_room() {
            EdgeRoom::Fits(edge) => {
                // Compute the inline key before the first change, as on
                // the split path, so a panic leaves the tree unchanged.
                edge.insert_fit(InlinedKey::new(key, prefix_len), value);
                *length += 1;
            }
            EdgeRoom::Full(_) => {
                self.insert_splitting(key, value, new_leaf_page, new_internal_page, length);
            }
        }
    }

    /// Keeps allocation, split propagation and its temporary arrays out
    /// of the ordinary insertion's stack frame and instruction stream.
    #[inline(never)]
    fn insert_splitting(
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

impl<'a, K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'a, K, V, S, Leaf>> {
    /// Inserts `key`, with its inline key, and `value` at this edge,
    /// splitting the leaf first if it is full, with the right half in a page
    /// from `new_page`.
    pub(crate) fn insert(
        self,
        key: InlinedKey<K, S>,
        value: V,
        new_page: impl FnOnce() -> FreshPage<K, V, S, Leaf>,
    ) -> Insertion<'a, K, V, S, Leaf> {
        match self.check_room() {
            EdgeRoom::Fits(edge) => Insertion::Fit(edge.insert_fit(key, value)),
            EdgeRoom::Full(edge) => Insertion::Split(edge.split_and_insert(new_page(), key, value)),
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'a, K, V, S, Internal>> {
    /// Inserts separator `key`, with its inline key, at this edge with
    /// `right_child` as the edge to its right, splitting the node first if it
    /// is full, with the right half in a page from `new_page`. This is how a
    /// child's split lands in its parent; the separator's value is already in
    /// `right_child`, as the head of its leftmost leaf (D17).
    ///
    /// # Panics
    ///
    /// If `right_child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn insert(
        self,
        key: InlinedKey<K, S>,
        right_child: Subtree<K, V, S>,
        new_page: impl FnOnce() -> FreshPage<K, V, S, Internal>,
    ) -> Insertion<'a, K, V, S, Internal> {
        match self.check_room() {
            EdgeRoom::Fits(edge) => Insertion::Fit(edge.insert_fit(key, right_child)),
            EdgeRoom::Full(edge) => {
                Insertion::Split(edge.split_and_insert(new_page(), key, right_child))
            }
        }
    }
}
