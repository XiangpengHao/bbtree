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
use super::node::inline_keys::WindowedKey;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::FreshPage;
use super::path::{LastEdge, PathMut};
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
    /// The entries `extend` has taken from its iterators: those pushed, and
    /// each one a panicking `InlineKey` dropped while it was being placed.
    taken: usize,
    /// The allocator of the tree's pages.
    alloc: A,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> TreeBuilder<K, V, S, A> {
    pub(crate) const fn new(alloc: A) -> Self {
        Self::continuing(Tree::EMPTY, alloc)
    }

    /// A builder that appends to `tree`, a well-formed tree on pages from
    /// `alloc`, as to one it built: into its rightmost leaf while it has
    /// room, then into new nodes along its right border, whose prefixes are
    /// what every key of the tree shares, in any tree (D50). It counts only
    /// the entries it appends.
    pub(crate) const fn continuing(tree: Tree<K, V, S>, alloc: A) -> Self {
        Self {
            tree,
            length: 0,
            taken: 0,
            alloc,
        }
    }

    /// The tree being built: for a cut to finish in (`cut.rs`), when
    /// [`Self::continuing`] took the part before it.
    pub(crate) fn tree_mut(&mut self) -> &mut Tree<K, V, S> {
        &mut self.tree
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
    /// An `InlineKey` that panics computing an entry's inline key drops the
    /// entry, which [`Self::dropped`] counts; one that panics fitting the
    /// prefix of a node the entry closes leaves the entry placed. Either
    /// way the tree stays well formed.
    pub(crate) fn extend(&mut self, entries: impl IntoIterator<Item = (K, V)>) {
        let mut entries = entries.into_iter().inspect(|_| self.taken += 1);
        let Some(mut next) = entries.next() else {
            return;
        };
        loop {
            let tree = &mut self.tree;
            let (key, value) = next;
            // The first entry makes the tree.
            let mut entry = (tree.shrink_shared_prefix_for(key, &self.alloc), value);
            // To tell a key that shares less without the tree, whose
            // rightmost leaf is changed below.
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
            let mut path = tree.start_leaf(key.window_at(shared.len()), value, &self.alloc);
            self.length += 1;
            // Fits the nodes the new leaf closed, off the tree's border now:
            // the right border of the subtree left of the separator, which
            // no key joins again, whose upper fence the separator is.
            let edge = path.index();
            path.refit_border_below::<LastEdge>(edge);
            path.refit_children_to_fences([edge]);
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
    pub(crate) fn finish(mut self) -> (Tree<K, V, S>, usize) {
        if self.tree.has_root() {
            self.tree.fix_border::<LastEdge>(&self.alloc);
            self.tree.refit_border_siblings::<LastEdge>();
        }
        (self.tree.take(), self.length)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Starts a new rightmost leaf after a full one, with `value` as its
    /// head, and `separator`'s key where separator keys are heads (D49), and
    /// hangs it after `separator`: in the rightmost internal node one level
    /// up if it has room; or else in a new node there, which hangs one level
    /// further up the same way; or in the root, which the tree raises for
    /// it. Returns the path up the right border it came, ending at the node
    /// that took the separator, at the edge left of it, which leads down to
    /// the nodes the new leaf closed. Pages come from `alloc`.
    fn start_leaf(
        &mut self,
        separator: WindowedKey<K, S>,
        value: V,
        alloc: &impl Allocator,
    ) -> PathMut<'_, K, V, S> {
        let shared_len = self.shared_prefix().len();
        let leaf = Leaf::new(FreshPage::allocate(alloc), shared_len);
        let separator = leaf.put_separator_head(separator, value);
        let mut child = Node::Leaf(leaf);
        let mut path = PathMut::new(self);
        path.descend_to_last_key();
        loop {
            let node = path.ascend_raising(alloc);
            let len = node.len();
            if len < Internal::<K, V, S>::capacity() {
                node.insert_fit(len, separator, child);
                return path;
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
