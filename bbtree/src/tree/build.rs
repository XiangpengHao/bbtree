//! Building a tree from entries in ascending key order, comparing no keys:
//! [`TreeBuilder`]. Each node fills up before the next one starts beside it,
//! so every node is full except along the right border, which `finish` tops
//! up past the minimum (D22) from the full left siblings (`cut.rs`). No node
//! is split in half, as inserting the same entries one by one would do.
//!
//! The tree is well formed throughout, except that the right border's nodes
//! may hold fewer keys than the minimum, down to none. If building stops
//! short by a panic, the tree is taken apart as any other is.
//!
//! Nodes on the right border have no right fence, so their prefix is what
//! every key of the tree shares (D50); a key that shares less shrinks that
//! first. A full node closes, leaving the border, once the next leaf starts
//! after it. It then gets what its fences share as its prefix, and its keys'
//! windows past it (`prefix.rs`). So do the left siblings that `finish`
//! takes entries from.

#![forbid(unsafe_code)]

use super::node::Node;
use super::node::inline_keys::WindowedKey;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::FreshPage;
use super::node_stack::{LastEdge, NodeStackMut};
use super::prefix::PrefixedKey;
use super::root::Tree;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// A tree being built from entries in ascending key order, on pages from
/// `alloc`.
pub(crate) struct TreeBuilder<K, V, S: InlineKeyStrategy<K>, A: Allocator> {
    /// No tree until the first entry.
    tree: Tree<K, V, S>,
    /// The entries pushed so far.
    length: usize,
    alloc: A,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> TreeBuilder<K, V, S, A> {
    pub(crate) const fn new(alloc: A) -> Self {
        Self {
            tree: Tree::EMPTY,
            length: 0,
            alloc,
        }
    }

    /// Appends the entries, whose keys must be greater than every key so far
    /// and ascend. The order is not checked: entries out of order make a
    /// tree whose searches give wrong answers, but that is memory-safe. Each
    /// key's inline key is computed here (D18), and again when its node
    /// closes with a prefix.
    ///
    /// The entries go into the rightmost leaf while it has room. The leaf is
    /// found once per leaf and once per call: callers append runs, not
    /// single entries.
    pub(crate) fn extend(&mut self, entries: impl IntoIterator<Item = (K, V)>) {
        let mut entries = entries.into_iter();
        let Some(mut next) = entries.next() else {
            return;
        };
        loop {
            let tree = &mut self.tree;
            let (key, value) = next;
            // The first entry makes the tree.
            let mut entry = (tree.shrink_shared_prefix_for(key, &self.alloc), value);
            // A copy, to check keys against without the tree, whose
            // rightmost leaf changes below.
            let shared = tree.shared_prefix().clone();
            // On the right border, whose prefix is what every key shares.
            let leaf = tree.root_mut().border_leaf::<LastEdge>();
            // The entry past the full leaf, or one whose key shares less.
            let past_leaf = loop {
                let len = leaf.len();
                if len == Leaf::<K, V, S>::capacity() {
                    break Ok(entry);
                }
                let (key, value) = entry;
                leaf.insert_fit(len, key.window_at(shared.len()), value);
                self.length += 1;
                let Some((key, value)) = entries.next() else {
                    return;
                };
                match PrefixedKey::checked::<S>(&shared, key) {
                    Ok(key) => entry = (key, value),
                    Err(key) => break Err((key, value)),
                }
            };
            let (key, value) = match past_leaf {
                Ok(entry) => entry,
                Err(sharing_less) => {
                    next = sharing_less;
                    continue;
                }
            };
            // It goes up into the right border, whose prefix is what every
            // key shares.
            let mut stack = tree.start_leaf(key.window_at(shared.len()), value, &self.alloc);
            self.length += 1;
            // Fit the nodes the new leaf closed, now off the tree's border:
            // the right border of the subtree left of the separator. The
            // separator is their upper fence, and no key joins them again.
            let edge = stack.index();
            stack.refit_border_below::<LastEdge>(edge);
            stack.refit_children_to_fences([edge]);
            next = match entries.next() {
                Some(entry) => entry,
                None => return,
            };
        }
    }

    /// The tree and its number of entries; no tree if nothing was pushed.
    /// Its right border is topped up past the minimum from the full left
    /// siblings (`cut.rs`), whose prefixes then fit their new fences.
    ///
    /// Fitting reads keys while the builder still holds the tree: a
    /// panicking `InlineKey` then takes the tree apart, as for any unfinished
    /// tree, where a tree held anywhere else would leak.
    pub(crate) fn finish(mut self) -> (Tree<K, V, S>, usize) {
        if self.tree.has_root() {
            self.tree.fix_border::<LastEdge>(&self.alloc);
            self.tree.refit_border_siblings::<LastEdge>();
        }
        (self.tree.take(), self.length)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Starts a new rightmost leaf after a full one. Its head holds `value`,
    /// and with inline keys `separator`'s key (D49). The leaf goes in after
    /// `separator`:
    ///
    /// - in the rightmost internal node one level up, if it has room;
    /// - or else in a new node there, which goes in one level further up the
    ///   same way;
    /// - or in the root, which the tree raises for it.
    ///
    /// Returns the node stack up the right border, ending at the node that
    /// took the separator, at the edge left of it, which leads down to the
    /// nodes the new leaf closed. Pages come from `alloc`.
    fn start_leaf(
        &mut self,
        separator: WindowedKey<K, S>,
        value: V,
        alloc: &impl Allocator,
    ) -> NodeStackMut<'_, K, V, S> {
        let shared_len = self.shared_prefix().len();
        let leaf = Leaf::new(FreshPage::allocate(alloc), shared_len);
        let separator = leaf.put_separator_head(separator, value);
        let mut child = Node::Leaf(leaf);
        let mut stack = NodeStackMut::new(self);
        stack.descend_to_last_key();
        loop {
            let node = stack.ascend_raising(alloc);
            let len = node.len();
            if len < Internal::<K, V, S>::capacity() {
                node.insert_fit(len, separator, child);
                return stack;
            }
            // Its new levels along the right border take what every key
            // shares.
            child = Node::Internal(Internal::new(FreshPage::allocate(alloc), child, shared_len));
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for TreeBuilder<K, V, S, A> {
    /// Takes apart a tree that was not finished.
    fn drop(&mut self) {
        self.tree.take().destroy(&self.alloc);
    }
}
