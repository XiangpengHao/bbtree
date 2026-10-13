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

use super::node::handle::{EdgeRoom, ForceResult, Leaf, LeafOrInternal, NodeMut, Subtree};
use super::node::inline_keys::{InlinedKey, KeyInliner};
use super::node::page::{FreshPage, PageByKind};
use super::path::{LastEdge, PathMut};
use crate::inline_key::InlineKeyStrategy;

/// A tree being built from entries in ascending key order.
pub(crate) struct TreeBuilder<K, V, S: InlineKeyStrategy<K>> {
    /// `None` until the first entry.
    tree: Option<Subtree<K, V, S>>,
    /// The entries pushed so far.
    length: usize,
    /// The entries `extend` has taken from its iterators: those pushed, and
    /// each one a panicking `InlineKey` dropped while it was being placed.
    taken: usize,
}

impl<K, V, S: InlineKeyStrategy<K>> TreeBuilder<K, V, S> {
    pub(crate) const fn new() -> Self {
        Self {
            tree: None,
            length: 0,
            taken: 0,
        }
    }

    /// A builder that appends to `tree`, a well-formed tree, as to one it
    /// built: into its rightmost leaf while it has room, then into new nodes
    /// along its right border, whose prefixes are what every key of the
    /// tree shares, in any tree (D50). It counts only the entries it
    /// appends.
    pub(crate) fn continuing(tree: Subtree<K, V, S>) -> Self {
        Self {
            tree: Some(tree),
            length: 0,
            taken: 0,
        }
    }

    /// Fits the prefixes of the nodes beside the tree's right border to
    /// their fences, reading keys: for a tree [`Self::continuing`] took
    /// from a cut, whose border took entries from them.
    pub(crate) fn fit_border_siblings(&mut self) {
        if let Some(tree) = &mut self.tree {
            tree.fit_border_siblings::<LastEdge>();
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
            let tree = self
                .tree
                .get_or_insert_with(|| Subtree::new_leaf(FreshPage::allocate()).forget_type());
            tree.cover_key(&next.0);
            // To tell a key that shares less without the tree, whose
            // rightmost leaf is lent below.
            let shared = tree.shared_prefix().clone();
            let mut leaf = Self::rightmost_leaf(tree.borrow_mut());
            let prefix_len = leaf.prefix_len();
            let leaf_is_full = loop {
                let EdgeRoom::Fits(edge) = leaf.reborrow_mut().last_edge().check_room() else {
                    break true;
                };
                let (key, value) = next;
                edge.insert_fit(InlinedKey::new(key, prefix_len), value);
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
    /// fences, and its number of entries; `None` if nothing was pushed.
    ///
    /// Fitting reads keys while the builder still holds the tree: a
    /// panicking `InlineKey` takes it apart, as it does any tree not
    /// finished, where a tree held anywhere else would leak.
    pub(crate) fn finish(mut self) -> (Option<Subtree<K, V, S>>, usize) {
        if let Some(tree) = &mut self.tree {
            tree.fix_border::<LastEdge>(&mut PageByKind::deallocate);
            tree.fit_border_siblings::<LastEdge>();
        }
        (self.tree.take(), self.length)
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
    fn fit_closed_nodes(tree: &mut Subtree<K, V, S>, top: usize) {
        if KeyInliner::<K>::resolve::<S>().is_none() {
            return;
        }
        let mut path = PathMut::new(tree);
        path.start_at_root();
        while path.height() > top {
            path.set_index(path.view().len());
            path.descend();
        }
        // The edge left of the separator, the node's last key.
        path.set_index(path.view().len() - 1);
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
    /// one level further up the same way; or under a new root. Returns the
    /// height of the node that took the separator.
    fn start_leaf(tree: &mut Subtree<K, V, S>, separator: InlinedKey<K, S>, value: V) -> usize {
        let shared = tree.shared_prefix().clone();
        let mut leaf = Subtree::new_leaf(FreshPage::allocate());
        leaf.borrow_mut().set_empty_prefix_len(shared.len());
        let separator = leaf.borrow_mut().put_separator_head(separator, value);
        let mut child = leaf.forget_type();
        // Its new levels along the right border take what every key shares.
        *child.shared_prefix_mut() = shared;
        let mut path = PathMut::new(tree);
        path.descend_to_last_key();
        while path.height() < path.root_height() {
            path.ascend();
            if let EdgeRoom::Fits(edge) = path.internal_mut().last_edge().check_room() {
                edge.insert_fit(separator, child);
                return path.height();
            }
            child.push_internal_level(FreshPage::allocate());
        }
        path.push_root(FreshPage::allocate());
        let EdgeRoom::Fits(edge) = path.internal_mut().last_edge().check_room() else {
            unreachable!("a new root has room");
        };
        edge.insert_fit(separator, child);
        path.height()
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
