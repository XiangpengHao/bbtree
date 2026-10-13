//! Building a tree from entries in ascending key order, comparing no keys:
//! [`TreeBuilder`]. Each node fills up before the next one starts beside it,
//! so the tree comes out with full nodes but along its right border, which
//! `finish` tops up past the minimum (D22) from the full left siblings
//! (`cut.rs`). No node is split in half, as inserting the same entries
//! one by one would.
//!
//! The tree is well formed all along, but for the right border, whose nodes
//! may hold fewer keys than the minimum, down to none: if building stops
//! short, by a panic, the tree is taken apart as any other is.
//!
//! Nodes on the right border have what every key of the tree shares as
//! their prefix, having no fence on the right (D50); each key appended
//! shrinks that first, if it shares less. A node that closes, full, gets
//! what its fences share, and its keys' windows past it (`prefix.rs`), once
//! the next leaf hangs after it, off the border; so do the left siblings
//! that `finish` takes entries from.

#![forbid(unsafe_code)]

use super::node::Node;
use super::node::inline_keys::{InlinedKey, KeyInliner};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, PageByKind};
use super::path::{LastEdge, PathMut};
use super::root::Root;
use crate::inline_key::InlineKeyStrategy;

/// A tree being built from entries in ascending key order.
pub(crate) struct TreeBuilder<K, V, S: InlineKeyStrategy<K>> {
    /// No tree until the first entry.
    tree: Root<K, V, S>,
    /// The entries pushed so far.
    length: usize,
    /// The entries `extend` has taken from its iterators: those pushed, and
    /// each one a panicking `InlineKey` dropped while it was being placed.
    taken: usize,
}

impl<K, V, S: InlineKeyStrategy<K>> TreeBuilder<K, V, S> {
    pub(crate) const fn new() -> Self {
        Self {
            tree: Root::EMPTY,
            length: 0,
            taken: 0,
        }
    }

    /// A builder that appends to `tree`, a well-formed tree, as to one it
    /// built: into its rightmost leaf while it has room, then into new nodes
    /// along its right border, whose prefixes are what every key of the
    /// tree shares, in any tree (D50). It counts only the entries it
    /// appends.
    pub(crate) fn continuing(tree: Root<K, V, S>) -> Self {
        Self {
            tree,
            length: 0,
            taken: 0,
        }
    }

    /// Fits the prefixes of the nodes beside the tree's right border to
    /// their fences, reading keys: for a tree [`Self::continuing`] took
    /// from a cut, whose border took entries from them.
    pub(crate) fn fit_border_siblings(&mut self) {
        if self.tree.has_tree() {
            self.tree.fit_border_siblings::<LastEdge>();
        }
    }

    /// Appends the entries, whose keys must be greater than every key so far
    /// and ascend. The order is not checked: entries out of order make a
    /// tree whose searches give wrong answers, but that is memory-safe. Each
    /// key's inline key is computed here (D18), and again when its node
    /// closes with a prefix.
    ///
    /// The entries go into the rightmost leaf while it has room, found once
    /// per leaf and once per call: callers append runs, not single entries.
    ///
    /// An `InlineKey` that panics, computing an entry's inline key or the
    /// prefix of a node the entry closes, drops the entry, which
    /// [`Self::dropped`] counts, and leaves the tree well formed.
    pub(crate) fn extend(&mut self, entries: impl IntoIterator<Item = (K, V)>) {
        let mut entries = entries.into_iter().inspect(|_| self.taken += 1);
        let Some(mut next) = entries.next() else {
            return;
        };
        loop {
            let tree = &mut self.tree;
            tree.page_or_new();
            tree.cover_key(&next.0);
            // To tell a key that shares less without the tree, whose
            // rightmost leaf is changed below.
            let shared = tree.shared_prefix().clone();
            let leaf = Self::rightmost_leaf(tree.top_mut());
            let prefix_len = leaf.prefix_len();
            let leaf_is_full = loop {
                let len = leaf.len();
                if len == Leaf::<K, V, S>::capacity() {
                    break true;
                }
                let (key, value) = next;
                leaf.insert_fit(len, InlinedKey::new(key, prefix_len), value);
                self.length += 1;
                next = match entries.next() {
                    Some(entry) => entry,
                    None => return,
                };
                if shared.len_shared_with::<K, S>(&next.0) < shared.len() {
                    break false;
                }
            };
            if !leaf_is_full {
                continue;
            }
            let (key, value) = next;
            // It goes up into the right border, whose prefix is what every
            // key shares.
            let top = Self::start_leaf(tree, InlinedKey::new(key, shared.len()), value);
            self.length += 1;
            Self::fit_closed_nodes(tree, top);
            next = match entries.next() {
                Some(entry) => entry,
                None => return,
            };
        }
    }

    /// How many entries `extend` took and dropped, each one a panicking
    /// `InlineKey` stopped it from placing: a caller that counts the entries
    /// it hands over, rather than those pushed, subtracts these.
    pub(crate) fn dropped(&self) -> usize {
        self.taken - self.length
    }

    /// The tree, with its right border topped up past the minimum from the
    /// full left siblings (`cut.rs`), whose prefixes then fit their new
    /// fences, and its number of entries; no tree if nothing was pushed.
    ///
    /// Fitting reads keys while the builder still holds the tree: a
    /// panicking `InlineKey` takes it apart, as it does any tree not
    /// finished, where a tree held anywhere else would leak.
    pub(crate) fn finish(mut self) -> (Root<K, V, S>, usize) {
        if self.tree.has_tree() {
            self.tree
                .fix_border::<LastEdge>(&mut PageByKind::deallocate);
            self.tree.fit_border_siblings::<LastEdge>();
        }
        (self.tree.take(), self.length)
    }

    /// The rightmost leaf under `node`.
    fn rightmost_leaf(mut node: Node<K, V, S>) -> Leaf<K, V, S> {
        loop {
            node = match node {
                Node::Leaf(leaf) => return leaf,
                Node::Internal(internal) => internal.child(internal.len()),
            };
        }
    }

    /// Fits the prefixes of the nodes that a new leaf closed, which
    /// [`Self::start_leaf`] hung after a separator in the node at height
    /// `top`: the right border of the subtree left of that separator, full,
    /// which no key joins again, whose upper fence the separator is from now
    /// on. From the leaf up, so that each node's prefix sees its children's.
    ///
    /// Reads keys only once the new leaf hangs beside them: until then they
    /// are on the tree's right border, where a prefix must be 0, and a
    /// panicking `InlineKey` would leave one there that searches past the
    /// last key misread. Off the border, a prefix it leaves short of the
    /// fences is one searches allow (`prefix.rs`).
    fn fit_closed_nodes(tree: &mut Root<K, V, S>, top: usize) {
        if KeyInliner::<K>::resolve::<S>().is_none() {
            return;
        }
        let mut path = PathMut::new(tree);
        path.start_at_root();
        while path.height() > top {
            path.set_index(path.node().len());
            path.descend();
        }
        // The edge left of the separator, the node's last key.
        path.set_index(path.node().len() - 1);
        path.descend_to_leaf::<LastEdge>();
        while path.height() < top {
            path.ascend();
            let edge = path.index();
            path.fit_children_to_fences([edge]);
        }
    }

    /// Starts a new rightmost leaf after a full one, with `value` as its
    /// head, and `separator`'s key where separator keys are heads (D49), and
    /// hangs it in `tree` after `separator`: in the rightmost internal node
    /// one level up if it has room; or else in a new node there, which hangs
    /// one level further up the same way; or in the root, which the tree
    /// raises for it. Returns the height of the node that took the
    /// separator.
    fn start_leaf(tree: &mut Root<K, V, S>, separator: InlinedKey<K, S>, value: V) -> usize {
        let shared_len = tree.shared_prefix().len();
        let leaf = Leaf::new(FreshPage::allocate());
        leaf.set_empty_prefix_len(shared_len);
        let separator = leaf.put_separator_head(separator, value);
        let mut child = Node::Leaf(leaf);
        let mut path = PathMut::new(tree);
        path.descend_to_last_key();
        while path.height() < path.root_height() {
            path.ascend();
            let node = path.internal();
            let len = node.len();
            if len < Internal::<K, V, S>::capacity() {
                node.insert_fit(len, separator, child);
                return path.height();
            }
            // Its new levels along the right border take what every key
            // shares.
            let level = Internal::new(FreshPage::allocate(), child);
            level.set_empty_prefix_len(shared_len);
            child = Node::Internal(level);
        }
        path.push_root(FreshPage::allocate, FreshPage::allocate);
        path.internal().insert_fit(0, separator, child);
        path.height()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for TreeBuilder<K, V, S> {
    /// Takes apart a tree that was not finished.
    fn drop(&mut self) {
        self.tree.take().destroy(PageByKind::deallocate);
    }
}
