//! Inserting into a tree.
//!
//! An insertion that goes through a node stack, as one into a full leaf
//! does, remembers the tree's last leaf if the new entry ends up last in it
//! (D45). A greater key then appends there directly while the leaf has room.
//! Otherwise a search down the tree finds the key, or the leaf edge where it
//! belongs. An insertion that fits its leaf needs no ancestors.
//!
//! If the leaf is full, a second search records a node stack for the split.
//! Going back up it, each node takes its split child's separator and right
//! half, and splits in turn if it is full. A root that splits raises the
//! tree: the root node moves down a level, under a new root in the root
//! page. A separator's value stays in the right half, as its head (D17), and
//! with inline keys so does its key (D49). Its inline key goes up, moved
//! back to its new node's prefix length (D18).
//!
//! Once the entry is in and counted, the halves of every split fit their
//! prefixes to their new fences (`prefix.rs`). The node stack then goes back
//! down to the new entry, through whichever half of each split took it, so
//! that a vacant entry can hand it out, or keep it as an occupied one.

#![forbid(unsafe_code)]

use core::mem;

use super::node::inline_keys::SearchKey;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, PageAllocator};
use super::node::{Insertion, Node};
use super::node_stack::{MAX_DEPTH, NodeStackMut};
use super::prefix::PrefixedKey;
use super::root::Tree;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Inserts `key`/`value` and returns `None`, counting the entry in
    /// `length`; or, if an equal key is there, keeps that key, drops `key`,
    /// replaces its value and returns the old one, as std's
    /// `BTreeMap::insert` does. For a separator key, the value replaced is
    /// its head (D17).
    ///
    /// Every page a split needs comes from `alloc`, the map's allocator, at
    /// the time of the split. So do the page the root node moves to when the
    /// tree grows a level, and the tree's root page if it has none.
    ///
    /// Panics in user code:
    ///
    /// - `Ord` runs only while checking an append or searching. `InlineKey`
    ///   runs only while checking an append, checking what the key shares
    ///   with every key (D50), searching, or computing the key's inline key.
    ///   All of these run before anything changes but the shared prefix and
    ///   the border prefixes that follow it, so a panic there leaves the
    ///   tree with the same entries.
    /// - After a split, the split nodes' prefixes fit their new fences
    ///   (`prefix.rs`), which calls `InlineKey` again once the entry is in
    ///   and counted. A panic then leaves a valid tree that has the entry.
    /// - `alloc` is called on the way back up, possibly after the leaf has
    ///   changed; it doesn't unwind (D8).
    #[inline(always)]
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        alloc: &impl PageAllocator,
        length: &mut usize,
    ) -> Option<V>
    where
        K: Ord,
    {
        let key = match self.rightmost_append(key) {
            Ok(append) => {
                append.insert(value);
                *length += 1;
                return None;
            }
            Err(key) => key,
        };
        // Shrink first: the search skips every node's prefix unchecked.
        let key = self.shrink_shared_prefix_for(key, alloc);
        match self
            .root_mut()
            .search_for_insert(&SearchKey::new(key.key()))
        {
            Ok(found) => {
                // Keep std's drop-before-replacement behavior on this path
                // too: a panicking key destructor leaves the value intact.
                drop(key);
                return Some(mem::replace(found.value_mut(), value));
            }
            Err((leaf, edge)) => {
                if leaf.len() < Leaf::<K, V, S>::capacity() {
                    // Compute the inline key before the first change.
                    leaf.insert_fit(edge, key.window_at(leaf.prefix_len()), value);
                    *length += 1;
                    return None;
                }
            }
        }
        self.insert_with_stack(key, value, alloc, length)
    }

    /// Inserts into a full leaf, which needs a node stack to split. Outlined
    /// to keep that node stack, and the machine stack it takes, out of
    /// fitting inserts. It searches again rather than trusting the first
    /// result: an inconsistent `Ord` may now find an existing key or a
    /// different leaf with room, which this handles too.
    #[inline(never)]
    pub(in crate::tree) fn insert_with_stack(
        &mut self,
        key: PrefixedKey<K>,
        value: V,
        alloc: &impl PageAllocator,
        length: &mut usize,
    ) -> Option<V>
    where
        K: Ord,
    {
        let mut stack = NodeStackMut::new(self);
        if stack.search(&SearchKey::new(key.key())) {
            // As in std, the new key is dropped before the value changes: if
            // its `Drop` panics, the map is unchanged.
            drop(key);
            Some(mem::replace(stack.value_mut(), value))
        } else {
            stack.insert_prefixed(key, value, alloc, length);
            stack.finish_insertion();
            None
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> NodeStackMut<'a, K, V, S> {
    /// Makes a new tree in `tree`, which has none, from `alloc`, and returns
    /// a node stack at the only edge of its root, an empty leaf. A vacant
    /// entry of a map without a tree inserts there ([`Self::insert`]).
    pub(crate) fn at_new_root(tree: &'a mut Tree<K, V, S>, alloc: &impl PageAllocator) -> Self {
        debug_assert!(!tree.has_root(), "a new tree in place of one");
        *tree = Tree::new(alloc);
        let mut stack = Self::new(tree);
        stack.start_at_root();
        stack
    }

    /// Inserts `key` and `value` at the leaf edge the node stack ends at. It
    /// counts the entry in `length`, fits the prefixes of the nodes that
    /// split, and moves the node stack to the new entry. Pages come as for
    /// [`Tree::insert`]. Compares no keys. First, it shrinks what every key
    /// of the tree shares to what `key` shares too
    /// ([`Self::shrink_shared_prefix_for`]).
    ///
    /// # Panics
    ///
    /// If the node stack ends at no leaf edge.
    #[inline]
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        alloc: &impl PageAllocator,
        length: &mut usize,
    ) {
        let key = self.shrink_shared_prefix_for(key, alloc);
        self.insert_prefixed(key, value, alloc, length);
    }

    /// [`Self::insert`] of a key that shares what every key of the tree
    /// shares already: for [`Tree::insert`], which shrank it for the key
    /// before it searched, and a latched tree's insert into a full leaf
    /// (`tree/latched/insert.rs`).
    #[inline]
    pub(in crate::tree) fn insert_prefixed(
        &mut self,
        key: PrefixedKey<K>,
        value: V,
        alloc: &impl PageAllocator,
        length: &mut usize,
    ) {
        let index = self.index();
        let leaf = self.leaf();
        assert!(index <= leaf.len(), "edge index out of bounds");
        let prefix_len = leaf.prefix_len();
        if leaf.len() < Leaf::<K, V, S>::capacity() {
            // Compute the inline key before the first change, as a split
            // does, so a panic leaves the tree unchanged.
            leaf.insert_fit(index, key.window_at(prefix_len), value);
            *length += 1;
        } else {
            self.insert_splitting(key, value, alloc, length);
        }
    }

    /// Keeps allocation, split propagation and its temporary arrays out
    /// of the ordinary insertion's stack frame and instruction stream.
    #[inline(never)]
    fn insert_splitting(
        &mut self,
        key: PrefixedKey<K>,
        value: V,
        alloc: &impl PageAllocator,
        length: &mut usize,
    ) {
        let index = self.index();
        let leaf = self.leaf();
        // The key's window at the leaf's prefix length, which both halves
        // of a split keep: the last `InlineKey` call before the first change.
        // The leaf is full: `NodeStackMut::insert` comes here only then.
        let key = key.window_at(leaf.prefix_len());
        let split = leaf.split_insert(index, key, value, FreshPage::allocate(alloc), alloc);
        // Both arrays are indexed by how many levels the split has climbed
        // from the leaf, which a new root above doesn't change. Here, in each
        // node that split, the index leading to the new entry in the half
        // that took it: in the leaf, the entry's own; above, the edge to the
        // child that holds it.
        let mut indexes_in_halves = [0; MAX_DEPTH + 1];
        indexes_in_halves[0] = split.inserted.index();
        // Above each node that split, where the separator between its halves
        // went, in the node stack's node there.
        let mut separator_indexes = [0; MAX_DEPTH + 1];
        let mut carried = (
            split.separator,
            Node::Leaf(split.right),
            split.inserted.is_right(),
        );
        let mut climbed = 0;
        let levels_climbed = loop {
            let (separator, right, entry_is_right) = carried;
            let node = self.ascend_raising(alloc);
            climbed += 1;
            let index = self.index();
            // The new entry is under the edge left of the separator, or right
            // of it if it went into the right half.
            let beside_separator =
                |separator_index: usize| separator_index + usize::from(entry_is_right);
            match node.insert(index, separator, right, alloc) {
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
