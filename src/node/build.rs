//! Building a tree from entries in ascending key order, comparing no keys:
//! [`TreeBuilder`]. Each node fills up before the next one starts beside it,
//! so the tree comes out with full nodes but along its right border, which
//! `finish` tops up past the minimum (D22) from the full left siblings
//! (`border.rs`). No node is split in half, as inserting the same entries
//! one by one would.
//!
//! The tree is well formed all along, but for the right border, whose nodes
//! may hold fewer keys than the minimum, down to none: if building stops
//! short, by a panic, the tree is taken apart as any other is.
//!
//! Nodes on the right border have prefix 0, having no fence on the right.
//! A node that closes, full, gets what its fences share, and its keys'
//! windows past it (`prefix.rs`); so do the left siblings that `finish`
//! takes entries from.

#![forbid(unsafe_code)]

use super::handle::{EdgeRoom, ForceResult, Internal, Leaf, LeafOrInternal, NodeMut, Subtree};
use super::inline_keys::{InlinedKey, KeyInliner};
use super::page::{FreshPage, PageByKind};
use super::path::{MAX_HEIGHT, NodeEnd};
use crate::inline_key::InlineKeyStrategy;

/// A tree being built from entries in ascending key order.
pub(crate) struct TreeBuilder<K, V, S: InlineKeyStrategy<K>> {
    /// `None` until the first entry.
    tree: Option<Subtree<K, V, S>>,
    /// The entries pushed so far.
    length: usize,
}

impl<K, V, S: InlineKeyStrategy<K>> TreeBuilder<K, V, S> {
    pub(crate) const fn new() -> Self {
        Self {
            tree: None,
            length: 0,
        }
    }

    /// The entries pushed so far.
    pub(crate) fn len(&self) -> usize {
        self.length
    }

    /// Appends `key`/`value`; see [`Self::extend`].
    pub(crate) fn push(&mut self, key: K, value: V) {
        self.extend(core::iter::once((key, value)));
    }

    /// Appends the entries, whose keys must be greater than every key so far
    /// and ascend. The order is not checked: entries out of order make a
    /// tree whose searches give wrong answers, but that is memory-safe. Each
    /// key's inline key is computed here (D18), and again when its node
    /// closes with a prefix.
    ///
    /// The entries go into the rightmost leaf while it has room, found once
    /// per leaf.
    pub(crate) fn extend(&mut self, entries: impl IntoIterator<Item = (K, V)>) {
        let mut entries = entries.into_iter();
        let Some(mut next) = entries.next() else {
            return;
        };
        loop {
            let tree = self
                .tree
                .get_or_insert_with(|| Subtree::new_leaf(FreshPage::allocate()).forget_type());
            let mut leaf = Self::rightmost_leaf(tree.borrow_mut());
            let prefix_len = leaf.prefix_len();
            while let EdgeRoom::Fits(edge) = leaf.reborrow_mut().last_edge().check_room() {
                let (key, value) = next;
                edge.insert_fit(InlinedKey::new(key, prefix_len), value);
                self.length += 1;
                next = match entries.next() {
                    Some(entry) => entry,
                    None => return,
                };
            }
            let (key, value) = next;
            Self::fit_closing_nodes(tree, &key);
            // It goes up into the right border, where prefixes are 0.
            Self::start_leaf(tree, InlinedKey::new(key, 0), value);
            self.length += 1;
            next = match entries.next() {
                Some(entry) => entry,
                None => return,
            };
        }
    }

    /// The tree, with its right border topped up past the minimum from the
    /// full left siblings (`border.rs`), whose prefixes then fit their new
    /// fences, and its number of entries; `None` if nothing was pushed.
    pub(crate) fn finish(mut self) -> (Option<Subtree<K, V, S>>, usize) {
        let mut tree = self.tree.take();
        if let Some(tree) = &mut tree {
            tree.fix_border(NodeEnd::Last, &mut PageByKind::deallocate);
            tree.fit_border_siblings(NodeEnd::Last);
        }
        (tree, self.length)
    }

    /// The rightmost leaf of the tree under `node`.
    fn rightmost_leaf(
        mut node: NodeMut<'_, K, V, S, LeafOrInternal>,
    ) -> NodeMut<'_, K, V, S, Leaf> {
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => return leaf,
                ForceResult::Internal(internal) => internal.last_edge().descend(),
            };
        }
    }

    /// Fits the prefixes of the nodes that a new leaf starting at
    /// `first_key` closes, from the leaf up: the full rightmost leaf, and
    /// each full rightmost node above it, which no key joins again, whose
    /// upper fence `first_key` is from now on.
    fn fit_closing_nodes(tree: &mut Subtree<K, V, S>, first_key: &K) {
        let Some(inliner) = KeyInliner::<K>::resolve::<S>() else {
            return;
        };
        // What each full node's fences share, from the root down: its lower
        // fence is the last key of the nearest node above it that has one.
        let mut prefix_lens = [None; MAX_HEIGHT + 1];
        let mut low = None;
        let mut node = tree.borrow();
        loop {
            if node.is_full() {
                prefix_lens[node.height()] = Some(inliner.fence_prefix_len(low, Some(first_key)));
            }
            if let Some(last) = node.last_kv() {
                low = Some(last.into_key());
            }
            node = match node.force() {
                ForceResult::Leaf(_) => break,
                ForceResult::Internal(internal) => internal.last_edge().descend(),
            };
        }
        // From the leaf up, while full, so that each node's prefix sees its
        // children's.
        let height = tree.height();
        for (height, prefix_len) in prefix_lens.into_iter().enumerate().take(height + 1) {
            let mut node = if height == 0 {
                Self::rightmost_leaf(tree.borrow_mut()).forget_type()
            } else {
                Self::rightmost_at(tree.borrow_mut(), height).forget_type()
            };
            if !node.is_full() {
                break;
            }
            if let Some(prefix_len) = prefix_len {
                node.refit_prefix(prefix_len);
            }
        }
    }

    /// The rightmost node at `height`, above the leaves, of the tree under
    /// `node`.
    fn rightmost_at(
        mut node: NodeMut<'_, K, V, S, LeafOrInternal>,
        height: usize,
    ) -> NodeMut<'_, K, V, S, Internal> {
        loop {
            let ForceResult::Internal(internal) = node.force() else {
                unreachable!("a height above the leaves is internal");
            };
            if internal.height() == height {
                return internal;
            }
            node = internal.last_edge().descend();
        }
    }

    /// Starts a new rightmost leaf after a full one, with `value` as its
    /// head, and hangs it in `tree` after `separator`: in the rightmost
    /// internal node one level up if it has room; or else in a new node
    /// there, which hangs one level further up the same way; or under a new
    /// root.
    fn start_leaf(tree: &mut Subtree<K, V, S>, separator: InlinedKey<K, S>, value: V) {
        let mut leaf = Subtree::new_leaf(FreshPage::allocate());
        leaf.borrow_mut().replace_head(value);
        let mut child = leaf.forget_type();
        let mut height = 1;
        while height <= tree.height() {
            let node = Self::rightmost_at(tree.borrow_mut(), height);
            if let EdgeRoom::Fits(edge) = node.last_edge().check_room() {
                edge.insert_fit(separator, child);
                return;
            }
            child.push_internal_level(FreshPage::allocate());
            height += 1;
        }
        let root = tree.push_internal_level(FreshPage::allocate());
        let EdgeRoom::Fits(edge) = root.last_edge().check_room() else {
            unreachable!("a new root has room");
        };
        edge.insert_fit(separator, child);
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for TreeBuilder<K, V, S> {
    /// Takes apart a tree that was not finished.
    fn drop(&mut self) {
        if let Some(tree) = self.tree.take() {
            tree.destroy(PageByKind::deallocate);
        }
    }
}
