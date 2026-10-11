//! Cutting a map in two at a key, as std's map does: `split_off`.
//!
//! A search finds the leaf edge just before the key, where the cut goes; for
//! a key that is a separator, that is the end of the rightmost leaf left of
//! it. Up the path from there, each node is cut at the path's edge
//! (`NodeMut::cut_off_at`): it keeps what lies left of the path, and what
//! lies right of it goes into a new node, after the part cut off from the
//! node below as its first child. The new nodes make a tree as tall as the
//! old one, with the keys from the key on.
//!
//! The nodes on the path become the borders of the two trees, where every
//! prefix is 0 (05-fence-key.md): before the cut, their prefixes go back to
//! 0, from the root down, reading no key.
//!
//! Along the cut, nodes on both sides may hold fewer keys than their
//! minimum, down to none. Fixing the left tree's right border and the right
//! tree's left border, from the top down, brings them back
//! (`node/border.rs`). A separator left at the start of the right tree, with
//! nothing left of it, comes down along that border, by a merge or a move,
//! into the tree's first leaf, with its value, the head of the leaf right of
//! it (D17). Once both maps are counted, the nodes beside the borders, which
//! entries moved over from, fit their prefixes to their new fences, reading
//! keys.
//!
//! That touches the nodes along the path and the two borders: O(log n) of
//! them. The two maps' lengths come from counting the entries of the shorter
//! tree, a node at a time, as std's do.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::BBTreeMap;
use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{ForceResult, Immut, LeafOrInternal, NodeRef, Subtree};
use crate::node::inline_keys::SearchKey;
use crate::node::page::{FreshPage, PageByKind};
use crate::node::path::{FirstEdge, KeySide, LastEdge, PathMut};

impl<K: Ord, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Splits the collection into two at the given key. Returns everything
    /// after the given key, including the key.
    ///
    /// Every `Ord` call happens first, before any entry moves. `InlineKey`
    /// is called last, once both maps are counted: if it panics, both stay
    /// valid. Both maps keep a tree, even an empty one, as std's do.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "a");
    /// a.insert(2, "b");
    /// a.insert(3, "c");
    /// a.insert(17, "d");
    /// a.insert(41, "e");
    ///
    /// let b = a.split_off(&3);
    ///
    /// assert_eq!(a.len(), 2);
    /// assert_eq!(b.len(), 3);
    ///
    /// assert_eq!(a[&1], "a");
    /// assert_eq!(a[&2], "b");
    ///
    /// assert_eq!(b[&3], "c");
    /// assert_eq!(b[&17], "d");
    /// assert_eq!(b[&41], "e");
    /// ```
    pub fn split_off<Q>(&mut self, key: &Q) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.is_empty() {
            return Self::empty();
        }
        let length = self.length;
        let lower = self.root.as_mut().expect("a map with entries has a tree");
        let upper = lower.split_off_at(key, KeySide::Before);
        let (lower_length, upper_length) = if lower.height() < upper.height() {
            let lower_length = Subtree::borrow(lower).count_entries();
            (lower_length, length - lower_length)
        } else {
            let upper_length = Subtree::borrow(&upper).count_entries();
            (length - upper_length, upper_length)
        };
        self.length = lower_length;
        let mut upper = Self {
            root: Some(upper),
            length: upper_length,
        };
        if let Some(root) = &mut self.root {
            root.fit_border_siblings::<LastEdge>();
        }
        if let Some(root) = &mut upper.root {
            root.fit_border_siblings::<FirstEdge>();
        }
        upper
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Cuts the tree at the leaf edge on `side` of `key`: keeps what lies
    /// before it, and returns what lies after, both with their borders along
    /// the cut brought up to their minimum (`node/border.rs`). The nodes
    /// beside those borders, which gave them entries, keep prefixes that
    /// still fit their narrower fences: fitting them with
    /// `fit_border_siblings`, which reads keys, is the caller's business,
    /// once both trees are where a panic can't lose them.
    ///
    /// Every `Ord` call happens first, in the search, before anything
    /// changes.
    pub(crate) fn split_off_at<Q>(&mut self, key: &Q, side: KeySide) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut path = PathMut::new(self);
        path.search_to_edge(&SearchKey::new(key), side);
        path.clear_prefixes();
        let mut upper = path.cut();
        let release = &mut PageByKind::deallocate;
        self.fix_border::<LastEdge>(release);
        upper.fix_border::<FirstEdge>(release);
        upper
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PathMut<'_, K, V, S> {
    /// Cuts the tree along the path, which ends at a leaf edge: each node on
    /// it, from the leaf up, keeps what lies left of the path, and what lies
    /// right of it goes into a new node, after the part cut off from the
    /// node below. Returns the tree of the new nodes, as tall as this one,
    /// on pages from the global allocator. Nodes on either side of the cut
    /// may be left with fewer keys than their minimum, down to none. Compares
    /// no keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no leaf edge.
    fn cut(mut self) -> Subtree<K, V, S> {
        let index = self.index();
        let mut right = self
            .leaf_mut()
            .cut_off_at(index, FreshPage::allocate())
            .forget_type();
        while self.height() < self.root_height() {
            self.ascend();
            let index = self.index();
            right = self
                .internal_mut()
                .cut_off_at(index, right, FreshPage::allocate())
                .forget_type();
        }
        right
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeRef<Immut<'_>, K, V, S, LeafOrInternal> {
    /// The number of entries in the subtree under this node, leaf entries
    /// and separators, counted a node at a time.
    fn count_entries(self) -> usize {
        match self.force() {
            ForceResult::Leaf(leaf) => leaf.len(),
            ForceResult::Internal(internal) => {
                let below: usize = (0..=internal.len())
                    .map(|index| internal.edge_at(index).descend().count_entries())
                    .sum();
                internal.len() + below
            }
        }
    }
}
