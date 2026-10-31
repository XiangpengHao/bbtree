//! Inserting into a tree.
//!
//! An insertion that goes through a node stack, as one into a full leaf
//! does, remembers the tree's last leaf if the new entry ends up last in it
//! (D45). A greater key then appends there directly while the leaf has room.
//! Otherwise a search down the tree finds the key, or the leaf edge where it
//! belongs. An insertion that fits its leaf needs no ancestors.
//!
//! If the leaf is full, a second search records a node stack for the split.
//! The split's pages come from the map's allocator first, before the tree
//! changes ([`SplitPages`], `reserved_pages.rs`): an allocator that refuses
//! one leaves the tree holding what it held. Going back up the node stack,
//! each node takes its split child's separator and right half, and splits
//! in turn if it is full. A root that splits raises the tree: the root node
//! moves down a level, under a new root in the root page. A separator's
//! value stays in the right half, as its head (D17), and with inline keys
//! so does its key (D49). Its inline key goes up, moved back to its new
//! node's prefix length (D18).
//!
//! Once the entry is in and counted, the halves of every split fit their
//! prefixes to their new fences (`prefix.rs`). The node stack then goes back
//! down to the new entry, through whichever half of each split took it, so
//! that a vacant entry can hand it out, or keep it as an occupied one.

#![forbid(unsafe_code)]

use core::iter;
use core::mem;

use super::node::inline_keys::SearchKey;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, PageAllocator};
use super::node::{Insertion, Node};
use super::node_stack::{MAX_DEPTH, NodeStackMut};
use super::prefix::PrefixedKey;
use super::reserved_pages::ReservedPages;
use super::root::Tree;
use crate::allocator::{Allocator, InsertError, PageKind};
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// [`Self::try_insert`], calling
    /// [`handle_alloc_error`](alloc::alloc::handle_alloc_error) if `alloc`
    /// refuses a page, as std's collections do.
    #[inline(always)]
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> Option<V>
    where
        K: Ord,
    {
        (self.try_insert(key, value, alloc, length))
            .unwrap_or_else(|error| error.handle_alloc_error())
    }

    /// Inserts `key`/`value` and returns `None`, counting the entry in
    /// `length`; or, if an equal key is there, keeps that key, drops `key`,
    /// replaces its value and returns the old one, as std's
    /// `BTreeMap::insert` does. For a separator key, the value replaced is
    /// its head (D17).
    ///
    /// Every page a split needs comes from `alloc`, the map's allocator,
    /// before the tree changes. So do the page the root node moves to when
    /// the tree grows a level, and the tree's root page if it has none. If
    /// `alloc` refuses one, the error holds the entry, and the tree the
    /// entries it held; what every key shares may be shorter, `key`'s too.
    ///
    /// User code runs between changes, and inside one only behind an
    /// abort-on-unwind guard (D48): `Ord` and `InlineKey` before the leaf
    /// changes, and `InlineKey` again once the entry is in and counted,
    /// fitting split nodes' prefixes (`prefix.rs`). `alloc` doesn't unwind
    /// (D8).
    #[inline(always)]
    pub(crate) fn try_insert(
        &mut self,
        key: K,
        value: V,
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> Result<Option<V>, InsertError<K, V>>
    where
        K: Ord,
    {
        let key = match self.rightmost_append(key) {
            Ok(append) => {
                append.insert(value);
                *length += 1;
                return Ok(None);
            }
            Err(key) => key,
        };
        if !self.has_root() {
            match Self::try_new(alloc) {
                Ok(tree) => *self = tree,
                Err(layout) => return Err(InsertError::new(key, value, layout)),
            }
        }
        // Shrink first: the search skips every node's prefix unchecked.
        let key = self.shrink_shared_prefix_for(key, alloc);
        match self
            .root_mut()
            .search_for_insert(&SearchKey::new(key.key()))
        {
            Ok(found) => {
                // As std's does, drop the new key before replacing the value.
                drop(key);
                return Ok(Some(mem::replace(found.value_mut(), value)));
            }
            Err((leaf, edge)) => {
                if leaf.len() < Leaf::<K, V, S>::capacity() {
                    // Compute the inline key before the first change.
                    leaf.insert_fit(edge, key.window_at(leaf.prefix_len()), value);
                    *length += 1;
                    return Ok(None);
                }
            }
        }
        self.insert_with_stack(key, value, alloc, length)
    }

    /// Inserts into a full leaf, which needs a node stack to split. Outlined
    /// to keep that node stack, and the machine stack it takes, out of
    /// fitting inserts. It searches again rather than trusting the first
    /// result: an inconsistent `Ord` may now find an existing key or a
    /// different leaf with room, which this handles too. Then it takes the
    /// pages the split needs from `alloc`, or gives the entry back.
    #[inline(never)]
    pub(in crate::tree) fn insert_with_stack(
        &mut self,
        key: PrefixedKey<K>,
        value: V,
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> Result<Option<V>, InsertError<K, V>>
    where
        K: Ord,
    {
        let mut stack = NodeStackMut::new(self);
        if stack.search(&SearchKey::new(key.key())) {
            // As in std, the new key is dropped before the value changes.
            drop(key);
            return Ok(Some(mem::replace(stack.value_mut(), value)));
        }
        let pages = match ReservedPages::reserve::<K, V, S>(alloc, stack.split_pages()) {
            Ok(pages) => pages,
            Err(layout) => return Err(InsertError::new(key.into_key(), value, layout)),
        };
        stack.insert_prefixed(key, value, &pages, length);
        stack.finish_insertion();
        Ok(None)
    }
}

/// The pages a split that inserts into a full leaf takes: a leaf's, for its
/// right half; an internal node's for each internal node above it that is
/// full too, and splits; and if the root splits, one of the root's kind for
/// the root's node, which moves out of the root page as the tree grows.
#[derive(Clone, Copy)]
pub(in crate::tree) struct SplitPages {
    leaves: usize,
    internals: usize,
}

impl SplitPages {
    /// No split: the leaf has room.
    const NONE: Self = Self {
        leaves: 0,
        internals: 0,
    };

    /// The pages for a full leaf `depth` levels below the root, whose lowest
    /// ancestor with room, if any, is at level `room`, the root's 0.
    pub(in crate::tree) fn of_full_leaf(depth: usize, room: Option<usize>) -> Self {
        let splits = depth - room.map_or(0, |level| level + 1);
        let (leaves, internals) = match (room, depth) {
            (None, 0) => (2, 0),
            (None, _) => (1, splits + 1),
            (Some(_), _) => (1, splits),
        };
        Self { leaves, internals }
    }

    /// Each page's kind, the leaves' first.
    pub(in crate::tree) fn kinds(self) -> impl Iterator<Item = PageKind> {
        iter::repeat_n(PageKind::Leaf, self.leaves)
            .chain(iter::repeat_n(PageKind::Internal, self.internals))
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> NodeStackMut<'a, K, V, S> {
    /// The pages an insert at the leaf edge the node stack ends at takes.
    fn split_pages(&self) -> SplitPages {
        if self.leaf().len() < Leaf::<K, V, S>::capacity() {
            return SplitPages::NONE;
        }
        let depth = self.depth();
        let room = (0..depth)
            .rev()
            .find(|&level| self.internal_at(level).len() < Internal::<K, V, S>::capacity());
        SplitPages::of_full_leaf(depth, room)
    }

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
            // does (D48).
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
        let split = leaf.split_insert(index, key, value, FreshPage::allocate(alloc));
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
