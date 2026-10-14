//! Per-node prefix stripping (`doc/05-fence-key.md`): a node with inline keys
//! skips the leading bytes that its two fences share, and stores each key's
//! window past them.
//!
//! A node's fences are the separators beside it in its ancestors: the key
//! left of its edge in the nearest ancestor where it is not the first edge,
//! and the key right of it in the nearest where it is not the last. Nothing
//! stores them. A node's prefix length is what they share; every key in the
//! node lies between them, so it shares that prefix, whatever is inserted.
//! Along the tree's left and right borders, where a fence is missing, it is
//! what every key of the tree shares, which the tree keeps with its root and
//! a search checks first (D50): a key outside it is outside every node.
//!
//! A child's fences lie within its parent's, so its prefix is never shorter
//! than its parent's: for keys a ≤ b ≤ c, what a and c share is the shorter
//! of what a and b share and what b and c share.
//!
//! A prefix changes only when a fence does, which only a separator change
//! does: a split adds one, a merge takes one out, a move between siblings or
//! a removal replaces one, and a cut leaves nodes without one. The nodes
//! whose fences changed then read their keys for their new windows
//! ([`Child::refit_prefix`]). Along the borders, a key that doesn't share
//! what the tree's keys share shrinks it, and the border nodes with it,
//! reading no key ([`Tree::shrink_shared_prefix_for`]).
//!
//! An operation changes the tree's shape without calling user encodings,
//! so that a panicking `InlineKey` can't stop it halfway. A prefix may have
//! to shrink during it: it shrinks to the parent's, the shift of a window
//! filled from the parent's separator ([`Window::moved_back_to`]), which
//! bounds the node (`tree/node/mod.rs`). Once it is done, and its entry
//! counted, the nodes whose fences changed are fit to them: the halves of
//! each split ([`NodeStackMut::refit_split_halves`]), the nodes a removal
//! merged or moved entries between (`remove.rs`), the siblings a cut's
//! borders took entries from ([`Tree::refit_border_siblings`]), the spines
//! below a join's separator (`join.rs`), and a built tree's nodes as they
//! close (`build.rs`), down a border ([`NodeStackMut::refit_border_below`]).
//! A removed separator's successor is the one change read first
//! ([`NodeStackMut::refit_left_of_separator`]).
//!
//! [`Window::moved_back_to`]: super::node::inline_keys::Window::moved_back_to

#![forbid(unsafe_code)]

use super::node::inline_keys::{KeyInliner, Window, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::NodePtr;
use super::node::{Child, Node};
use super::node_stack::{FirstEdge, LastEdge, MAX_DEPTH, NodeEnd, NodeStackMut};
use super::root::Tree;
use super::shared_prefix::SharedPrefix;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> NodeStackMut<'_, K, V, S> {
    /// Fits the prefixes of the nodes an insert split to their fences, from
    /// the leaves up, so that each node's sees its children's; the node stack,
    /// which ends at the new entry, ends there again afterwards.
    ///
    /// `separator_indexes[climbed]`, for each `climbed` from 1 to
    /// `levels_climbed` levels up the node stack from its bottom, is where
    /// the node stack's node there holds the separator between the two
    /// halves of the node below it that split.
    ///
    /// Calls `InlineKey`: an insert does this last, once its entry is in and
    /// counted, so that a panic leaves a map that has it.
    pub(in crate::tree) fn refit_split_halves(
        &mut self,
        separator_indexes: &[usize; MAX_DEPTH + 1],
        levels_climbed: usize,
    ) {
        if !NodePtr::<K, V, S>::has_inline_keys() {
            return;
        }
        let mut indexes = [0; MAX_DEPTH + 1];
        indexes[0] = self.index();
        for climbed in 1..=levels_climbed {
            self.ascend();
            indexes[climbed] = self.index();
            let separator_index = separator_indexes[climbed];
            self.refit_children_to_fences([separator_index, separator_index + 1]);
        }
        for climbed in (0..levels_climbed).rev() {
            self.descend();
            self.set_index(indexes[climbed]);
        }
    }

    /// Fits the prefixes of the bottom node's children at `edges`, an
    /// internal node's, to what their fences share, reading the fences
    /// unless the child's own inline keys tell they share no more
    /// ([`Child::keys_differ_past_prefix`]), and, if a prefix changes, the
    /// child's keys.
    pub(in crate::tree) fn refit_children_to_fences<const N: usize>(&mut self, edges: [usize; N]) {
        if !NodePtr::<K, V, S>::has_inline_keys() {
            return;
        }
        if self.internal().has_leaf_children() {
            self.refit_children_of_kind::<Leaf<K, V, S>, N>(edges);
        } else {
            self.refit_children_of_kind::<Internal<K, V, S>, N>(edges);
        }
    }

    /// [`Self::refit_children_to_fences`], for children of kind `C`, as
    /// decided once for the bottom node.
    fn refit_children_of_kind<C: Child<K, V, S>, const N: usize>(&mut self, edges: [usize; N]) {
        let shared_len = self.tree().shared_prefix().len();
        let (low, high) = self.fences();
        let node = self.internal();
        for edge in edges {
            let child = C::child_of(node, edge);
            if child.keys_differ_past_prefix() {
                continue;
            }
            let child_low = match edge.checked_sub(1) {
                Some(left) => Some(node.separator_key(left)),
                None => low,
            };
            let child_high = (edge < node.len())
                .then(|| node.separator_key(edge))
                .or(high);
            // Well-ordered encodings already imply this floor. Keep the
            // numerical parent/child invariant even when a user encoding
            // lies: later structural moves must only move windows
            // backwards, without invoking that encoding.
            let prefix_len = KeyInliner::fence_prefix_len::<S>(child_low, child_high, shared_len)
                .max(node.prefix_len());
            child.refit_prefix(prefix_len);
        }
    }

    /// Fits the prefixes of the nodes below the bottom node's child at
    /// `edge`, down that child's `End` border, from the leaf up, so that
    /// each node's sees its children's; the child itself is the caller's to
    /// fit, with its sibling if that changed too. The node stack ends at the
    /// bottom node again, at `edge`.
    pub(in crate::tree) fn refit_border_below<End: NodeEnd>(&mut self, edge: usize) {
        if !NodePtr::<K, V, S>::has_inline_keys() {
            return;
        }
        let start_depth = self.depth();
        self.set_index(edge);
        self.descend_to_leaf::<End>();
        self.ascend();
        while self.depth() > start_depth {
            let child = self.index();
            self.refit_children_to_fences([child]);
            self.ascend();
        }
    }

    /// Before the separator the node stack ends at, `s`, in an internal node,
    /// gives way to its successor, `s'`, the first key of the leftmost leaf
    /// right of it (D17): shrinks the prefixes of the nodes along the right
    /// border under the edge left of `s`, whose upper fence `s'` becomes, to
    /// what they will share with it, from the top down, reading keys. Their
    /// fences are still what they were, which a shorter prefix fits too, so
    /// a removal does this before it changes anything.
    ///
    /// A node there has fences `low` and `s`, so its new prefix is the
    /// shorter of its own and what `s` and `s'` share: `low ≤ s ≤ s'`.
    pub(in crate::tree) fn refit_left_of_separator(&mut self) {
        if !NodePtr::<K, V, S>::has_inline_keys() {
            return;
        }
        let index = self.index();
        let node = self.internal();
        let prefix_len = {
            let successor = node
                .head_leaf(index)
                .keys()
                .first()
                .expect("the leaf right of a separator has keys");
            // Arbitrary user encodings may report fewer shared bytes than
            // the parent's current prefix. Shrinking the border below it
            // would break the numerical invariant used by key movement.
            let separator = node.separator_key(index);
            KeyInliner::common_prefix_len::<S>(separator, successor).max(node.prefix_len())
        };
        let mut left = node.child(index);
        loop {
            left = match left {
                Node::Leaf(leaf) => {
                    leaf.shrink_prefix_to(prefix_len);
                    break;
                }
                Node::Internal(internal) => {
                    internal.shrink_prefix_to(prefix_len);
                    internal.child(internal.len())
                }
            };
        }
    }

    /// Moves the prefix of every node on the node stack back to the root's,
    /// what every key of the tree shares (D50), from the root down, each
    /// filled from its parent's key beside the node stack, which is one of its
    /// fences, once the parent's is the root's too. For a cut along the node
    /// stack (`cut.rs`), whose nodes become the borders of two trees, both of
    /// whose keys share what this tree's do. Reads no key; no user encoding
    /// runs. The node stack stays where it is.
    pub(super) fn move_prefixes_back_to_root(&mut self) {
        if !NodePtr::<K, V, S>::has_inline_keys() {
            return;
        }
        debug_assert_eq!(
            self.node_at(0).prefix_len(),
            self.tree().shared_prefix().len(),
            "the root's prefix is what every key shares"
        );
        // Each node above the bottom, its own prefix moved back already,
        // fills its child's on the node stack.
        for depth in 0..self.depth() {
            let fill = self
                .internal_at(depth)
                .fence_window(self.index_at(depth))
                .expect("a node on a node stack down holds a key");
            match self.node_at(depth + 1) {
                Node::Leaf(leaf) => leaf.move_prefix_back_to(fill),
                Node::Internal(internal) => internal.move_prefix_back_to(fill),
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Fits the prefix of the node beside each node along the tree's `End`
    /// border to its fences, from the root down: after
    /// [`Tree::fix_border`], which moves entries over from those nodes,
    /// into the border, without reading a key. The border's own nodes keep
    /// the root's fence on that side, and so its prefix (D50).
    pub(super) fn refit_border_siblings<End: NodeEnd>(&mut self) {
        if !NodePtr::<K, V, S>::has_inline_keys() {
            return;
        }
        let mut stack = NodeStackMut::new(self);
        stack.start_at_root();
        while !stack.ends_at_leaf() {
            let len = stack.node().len();
            debug_assert!(len > 0, "a node on a fixed border holds a key");
            let sibling = if End::IS_LAST { len - 1 } else { 1 };
            stack.refit_children_to_fences([sibling]);
            stack.set_index(End::edge(stack.node()));
            stack.descend();
        }
    }
}

/// A key that shares what every key of its tree shares (D50): what a
/// search that skips each node's prefix unchecked needs, and what an
/// insert computes the key's window from. Only the tree makes one, by
/// shrinking what its keys share to what the key shares too
/// ([`Tree::shrink_shared_prefix_for`]), or by checking it
/// ([`Self::checked`]).
pub(in crate::tree) struct PrefixedKey<K>(K);

impl<K> PrefixedKey<K> {
    /// `key`, if it shares `shared`, what every key of a tree shares; or
    /// else `key` back.
    pub(in crate::tree) fn checked<S: InlineKeyStrategy<K>>(
        shared: &SharedPrefix,
        key: K,
    ) -> Result<Self, K> {
        if shared.is_shared_by::<K, S>(&key) {
            Ok(Self(key))
        } else {
            Err(key)
        }
    }

    pub(in crate::tree) fn key(&self) -> &K {
        &self.0
    }

    /// The key with its window at `prefix_len`, the prefix length of the
    /// node it goes into, whose fences it lies between.
    pub(in crate::tree) fn window_at<S: InlineKeyStrategy<K>>(
        self,
        prefix_len: usize,
    ) -> WindowedKey<K, S> {
        WindowedKey::new(self.0, prefix_len)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Shrinks what every key of the tree shares to what `key` shares too,
    /// before the tree takes it (D50), and returns `key`, which shares it
    /// now: a tree whose keys share bytes `key` lacks shrinks to what `key`
    /// shares with them, and so do the nodes that take the root's fences,
    /// along its borders. Without a tree, there is a new one from `alloc`
    /// first. An empty tree, whose prefix nothing bounds yet, takes `key`'s
    /// first bytes, and its root leaf takes them as its prefix. Calls
    /// `InlineKey` with [`WithInlineKey`](crate::WithInlineKey), before it
    /// changes anything but making the tree.
    ///
    /// Every insert does this, and almost every one changes nothing: the
    /// check is inline, and making the tree or starting or shrinking the
    /// prefix is not.
    #[inline]
    pub(in crate::tree) fn shrink_shared_prefix_for(
        &mut self,
        key: K,
        alloc: &impl Allocator,
    ) -> PrefixedKey<K> {
        let root_page = self.root_page_or_new(alloc);
        if NodePtr::<K, V, S>::has_inline_keys() {
            if let Node::Leaf(root) = root_page.node()
                && root.len() == 0
            {
                self.start_shared_prefix(root, &key);
            } else {
                let len = root_page.shared_prefix().len_shared_with::<K, S>(&key);
                self.shrink_shared_prefix_to(len);
            }
        }
        PrefixedKey(key)
    }

    /// Shrinks what every key of this tree and of `right` shares to what
    /// both share with `key` (D50), and returns `key` with its window there:
    /// the separator that joins the two trees (`join.rs`). Calls `InlineKey`
    /// before it changes anything.
    pub(in crate::tree) fn shrink_shared_prefixes_for_separator(
        &mut self,
        right: &mut Self,
        key: K,
    ) -> WindowedKey<K, S> {
        let shared_with_key = |tree: &Self| tree.shared_prefix().len_shared_with::<K, S>(&key);
        let len = shared_with_key(self).min(shared_with_key(right));
        let separator = WindowedKey::new(key, len);
        self.shrink_shared_prefix_to(len);
        right.shrink_shared_prefix_to(len);
        separator
    }

    /// Makes `key`'s first bytes what every key of the tree, which has none
    /// yet, shares, and the prefix of `root`, its empty root leaf.
    #[cold]
    #[inline(never)]
    fn start_shared_prefix(&mut self, root: Leaf<K, V, S>, key: &K) {
        let shared = SharedPrefix::of_key::<K, S>(key);
        root.set_empty_prefix_len(shared.len());
        *self.shared_prefix_mut() = shared;
    }

    /// Shrinks what every key of the tree shares to its first `len` bytes,
    /// if there are more, and the nodes along its borders with it: before
    /// the tree takes a key that shares only those, or joins another tree,
    /// around a separator, whose keys do. Reads no key and calls no
    /// `InlineKey`.
    #[inline]
    fn shrink_shared_prefix_to(&mut self, len: usize) {
        if len < self.shared_prefix().len() {
            self.shrink_borders_to(len);
        }
    }

    /// [`Self::shrink_shared_prefix_to`], for a `len` shorter than what
    /// every key shares: rare, as the prefix only ever shrinks.
    #[cold]
    #[inline(never)]
    fn shrink_borders_to(&mut self, len: usize) {
        let fill = self.shared_prefix().window_at(len);
        self.shared_prefix_mut().shrink_to(len);
        self.move_borders_back_to(fill);
    }

    /// Moves the prefix of every node along the tree's left and right
    /// borders back to `fill`'s, filled from it: the window there of what
    /// every key of the tree shares, before it shrank.
    fn move_borders_back_to(&mut self, fill: Window) {
        self.move_border_back_to::<FirstEdge>(fill);
        self.move_border_back_to::<LastEdge>(fill);
    }

    /// [`Self::move_borders_back_to`], along the `End` border.
    fn move_border_back_to<End: NodeEnd>(&mut self, fill: Window) {
        let mut node = self.root_mut();
        loop {
            node = match node {
                Node::Leaf(leaf) => {
                    leaf.move_prefix_back_to(fill);
                    break;
                }
                Node::Internal(internal) => {
                    internal.move_prefix_back_to(fill);
                    internal.child(End::edge(node))
                }
            };
        }
    }
}
