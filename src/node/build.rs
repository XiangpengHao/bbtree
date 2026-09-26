//! Building a tree from entries in ascending key order, comparing no keys:
//! [`TreeBuilder`]. Each node fills up before the next one starts beside it,
//! so the tree comes out with full nodes but along its right border, which
//! `finish` tops up from the full left siblings to the minimum (D22). No
//! node is split in half, as inserting the same entries one by one would.
//!
//! The tree is well formed all along, but for the right border, whose nodes
//! may hold fewer keys than the minimum, down to none: if building stops
//! short, by a panic, the tree is taken apart as any other is.

#![forbid(unsafe_code)]

use super::handle::{EdgeRoom, ForceResult, Internal, Leaf, LeafOrInternal, NodeMut, Subtree};
use super::inline_keys::InlinedKey;
use super::page::{FreshPage, PageByKind};
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
    /// key's inline key is computed here (D18).
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
            while let EdgeRoom::Fits(edge) = leaf.reborrow_mut().last_edge().check_room() {
                let (key, value) = next;
                edge.insert_fit(InlinedKey::new(key), value);
                self.length += 1;
                next = match entries.next() {
                    Some(entry) => entry,
                    None => return,
                };
            }
            let (key, value) = next;
            Self::start_leaf(tree, InlinedKey::new(key), value);
            self.length += 1;
            next = match entries.next() {
                Some(entry) => entry,
                None => return,
            };
        }
    }

    /// The tree, with its right border topped up to the minimum, and its
    /// number of entries; `None` if nothing was pushed.
    pub(crate) fn finish(mut self) -> (Option<Subtree<K, V, S>>, usize) {
        let mut tree = self.tree.take();
        if let Some(tree) = &mut tree {
            Self::fill_right_border(tree.borrow_mut());
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

    /// Tops up each node of the right border under `node` to its minimum,
    /// from the top down, by moving entries over from its left sibling,
    /// which is full: a node on the border started only once its left
    /// sibling was full, and gives at most a minimum's worth, far less than
    /// half.
    fn fill_right_border(mut node: NodeMut<'_, K, V, S, LeafOrInternal>) {
        while let ForceResult::Internal(internal) = node.force() {
            let last_child = internal.reborrow().last_edge().descend();
            let min_len = last_child.min_len();
            let separator = internal
                .last_kv()
                .expect("a node on the right border holds a key once topped up");
            let mut context = separator.into_balancing_context();
            while context.right_child_len() < min_len {
                context
                    .steal_left()
                    .expect("a full left sibling can spare a key");
            }
            node = context.into_right_child();
        }
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
