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

use super::handle::{
    EdgeRoom, ForceResult, Immut, Leaf, LeafOrInternal, NodeMut, NodeRef, Subtree,
};
use super::inline_keys::{InlinedKey, KeyInliner};
use super::page::{FreshPage, PageByKind, PageKind};
use super::path::{LastEdge, MAX_HEIGHT, PathMut};
use super::prefix::PrefixGrowth;
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
    /// along its right border, whose prefixes are 0 in any tree. It counts
    /// only the entries it appends.
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

    /// How many entries `extend` took and dropped, each one a panicking
    /// `InlineKey` stopped it from placing: a caller that counts the entries
    /// it hands over, rather than those pushed, subtracts these.
    pub(crate) fn dropped(&self) -> usize {
        self.taken - self.length
    }

    /// The tree, with its right border topped up past the minimum from the
    /// full left siblings (`border.rs`), whose prefixes then fit their new
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
            let height = node.height();
            let ((is_full, last_key), child) = match node.force() {
                ForceResult::Leaf(leaf) => (Self::closing_state(leaf), None),
                ForceResult::Internal(internal) => (
                    Self::closing_state(internal),
                    Some(internal.last_edge().descend()),
                ),
            };
            if is_full {
                prefix_lens[height] = Some(inliner.fence_prefix_len(low, Some(first_key)));
            }
            if last_key.is_some() {
                low = last_key;
            }
            match child {
                Some(child) => node = child,
                None => break,
            }
        }
        // From the leaf up, while full, so that each node's prefix sees its
        // children's.
        let mut path = PathMut::new(tree);
        path.descend_to_last_key();
        if !Self::refit_if_full(path.leaf_mut(), prefix_lens[0]) {
            return;
        }
        while path.height() < path.root_height() {
            path.ascend();
            let height = path.height();
            if !Self::refit_if_full(path.internal_mut(), prefix_lens[height]) {
                return;
            }
        }
    }

    /// Whether `node`, a rightmost node, is full, and its last key.
    fn closing_state<'t, Kind: PageKind>(
        node: NodeRef<Immut<'t>, K, V, S, Kind>,
    ) -> (bool, Option<&'t K>) {
        (node.is_full(), node.last_kv().map(|last| last.into_key()))
    }

    /// Fits the prefix of `node`, a rightmost node a new leaf closes, to
    /// `prefix_len`, what its fences share, if it is full; returns whether
    /// it was.
    fn refit_if_full<Kind: PrefixGrowth>(
        mut node: NodeMut<'_, K, V, S, Kind>,
        prefix_len: Option<usize>,
    ) -> bool {
        if !node.is_full() {
            return false;
        }
        if let Some(prefix_len) = prefix_len {
            node.refit_prefix(prefix_len);
        }
        true
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
        let mut path = PathMut::new(tree);
        path.descend_to_last_key();
        while path.height() < path.root_height() {
            path.ascend();
            if let EdgeRoom::Fits(edge) = path.internal_mut().last_edge().check_room() {
                edge.insert_fit(separator, child);
                return;
            }
            child.push_internal_level(FreshPage::allocate());
        }
        path.push_root(FreshPage::allocate());
        let EdgeRoom::Fits(edge) = path.internal_mut().last_edge().check_room() else {
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
