//! Inserting into a tree: an owner may remember the last leaf after a split,
//! allowing a greater key to append directly while it has room. Otherwise a
//! downward search finds the entry or vacant leaf edge. A fitting insertion
//! needs no ancestors. If the leaf is full, a
//! second search records its path (`path.rs`) for splitting. Going
//! back up the path, each node takes its split child's separator and right
//! half, splitting in turn if it is full; a root that splits raises the
//! tree, moving the root node down a level, under the root. A separator's value stays in the right half, as the head
//! of its leftmost leaf (D17), and so does its key where separator keys are
//! heads (D49); its inline key goes up with it (D18), moved back to its new
//! node's prefix length. Once
//! the entry is in and counted, the halves of every split fit their prefixes
//! to their new fences (`prefix.rs`).
//!
//! The path then goes back down to the new entry, through whichever half of
//! each split took it, so that a vacant entry can hand it out, or keep it as
//! an occupied one.

#![forbid(unsafe_code)]

use core::mem;

use super::node::inline_keys::{SearchKey, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::FreshPage;
use super::node::{Insertion, Node};
use super::path::{MAX_DEPTH, PathMut};
use super::root::Tree;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Inserts `key`/`value` and returns `None`, counting the entry in
    /// `length`; or, if an equal key is there, keeps that key, drops `key`,
    /// replaces its value and returns the old one, as std's
    /// `BTreeMap::insert` does. For a separator key, the value replaced is a
    /// leaf's head (D17).
    ///
    /// Every page a split needs comes from `new_leaf_page` or
    /// `new_internal_page`, by the kind of node that splits (D21), asked for
    /// at the time of the split; so does the page the root node moves to
    /// when the tree grows a level. Without a tree, a new one's root page
    /// comes from the global allocator.
    ///
    /// Every `Ord` call happens while validating an append or searching,
    /// and every `InlineKey` call while validating an append, checking what
    /// the key shares with every key (D50), searching, or computing its
    /// inline key next, before anything changes but the shared prefix and
    /// the border prefixes that follow it, so a panicking `Ord` or
    /// `InlineKey` there leaves the tree with the same entries. After a split, the split
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
        new_leaf_page: impl FnMut() -> FreshPage<Leaf<K, V, S>>,
        new_internal_page: impl FnMut() -> FreshPage<Internal<K, V, S>>,
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
        self.ensure_root_page();
        self.cover_key(&key);
        match self.root_mut().search_for_insert(&SearchKey::new(&key)) {
            Ok(found) => {
                // Keep std's drop-before-replacement behavior on this path
                // too: a panicking key destructor leaves the value intact.
                drop(key);
                return Some(mem::replace(found.value_mut(), value));
            }
            Err((leaf, edge)) => {
                let prefix_len = leaf.prefix_len();
                if leaf.len() < Leaf::<K, V, S>::capacity() {
                    // Compute the summary before the first mutation.
                    leaf.insert_fit(edge, WindowedKey::new(key, prefix_len), value);
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
        new_leaf_page: impl FnMut() -> FreshPage<Leaf<K, V, S>>,
        new_internal_page: impl FnMut() -> FreshPage<Internal<K, V, S>>,
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
    /// [`Tree::insert`]. Compares no keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no leaf edge.
    #[inline]
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        new_leaf_page: impl FnMut() -> FreshPage<Leaf<K, V, S>>,
        new_internal_page: impl FnMut() -> FreshPage<Internal<K, V, S>>,
        length: &mut usize,
    ) {
        self.cover_key(&key);
        let index = self.index();
        let leaf = self.leaf();
        assert!(index <= leaf.len(), "edge index out of bounds");
        let prefix_len = leaf.prefix_len();
        if leaf.len() < Leaf::<K, V, S>::capacity() {
            // Compute the inline key before the first change, as on the
            // split path, so a panic leaves the tree unchanged.
            leaf.insert_fit(index, WindowedKey::new(key, prefix_len), value);
            *length += 1;
        } else {
            self.insert_splitting(key, value, new_leaf_page, new_internal_page, length);
        }
    }

    /// Keeps allocation, split propagation and its temporary arrays out
    /// of the ordinary insertion's stack frame and instruction stream.
    #[inline(never)]
    fn insert_splitting(
        &mut self,
        key: K,
        value: V,
        mut new_leaf_page: impl FnMut() -> FreshPage<Leaf<K, V, S>>,
        mut new_internal_page: impl FnMut() -> FreshPage<Internal<K, V, S>>,
        length: &mut usize,
    ) {
        let index = self.index();
        let leaf = self.leaf();
        // The key's window at the leaf's prefix length, which both halves
        // of a split keep: the last `InlineKey` call before the first change.
        let key = WindowedKey::new(key, leaf.prefix_len());
        let split = match leaf.insert(index, key, value, &mut new_leaf_page) {
            // The new key is at the index of the edge it went in at.
            Insertion::Fit(_) => {
                *length += 1;
                return;
            }
            Insertion::Split(split) => split,
        };
        // Both arrays go by how many levels the split has climbed from the
        // leaf, which a new root above doesn't change. In each node that
        // split, the index that leads to the new entry in the half that took
        // it: its key's in the leaf, and above, the edge to the child that
        // holds it.
        let mut indexes_in_halves = [0; MAX_DEPTH + 1];
        indexes_in_halves[0] = split.inserted.index();
        // Above each node that split, where the separator between its halves
        // went, in the node the path goes through.
        let mut separator_indexes = [0; MAX_DEPTH + 1];
        let mut carried = (
            split.separator,
            Node::Leaf(split.right),
            split.inserted.is_right(),
        );
        let mut climbed = 0;
        let levels_climbed = loop {
            let (separator, right, entry_is_right) = carried;
            let node = self.ascend_raising(&mut new_leaf_page, &mut new_internal_page);
            climbed += 1;
            let index = self.index();
            // The new entry is under the edge left of the separator, or right
            // of it if it went into the right half.
            let beside_separator =
                |separator_index: usize| separator_index + usize::from(entry_is_right);
            match node.insert(index, separator, right, &mut new_internal_page) {
                Insertion::Fit(separator_index) => {
                    separator_indexes[climbed] = separator_index;
                    self.set_index(beside_separator(separator_index));
                    break climbed;
                }
                Insertion::Split(split) => {
                    separator_indexes[climbed] = split.inserted.index();
                    indexes_in_halves[climbed] = beside_separator(split.inserted.index());
                    carried = (
                        split.separator,
                        Node::Internal(split.right),
                        split.inserted.is_right(),
                    );
                }
            }
        };
        // Back down to the new entry, through the halves that took it.
        for climbed in (0..levels_climbed).rev() {
            self.descend();
            self.set_index(indexes_in_halves[climbed]);
        }
        *length += 1;
        self.refit_split_halves(&separator_indexes, levels_climbed);
    }
}
