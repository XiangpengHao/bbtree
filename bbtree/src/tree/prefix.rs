//! Per-node prefix stripping (D26, `design/05-fence-key.md`): a node with
//! inline keys skips the leading bytes its two fences share, and stores each
//! key's window past them.
//!
//! A node's fences are the separators beside it in its ancestors. The low fence
//! is the key left of the node's edge in the nearest ancestor where that edge
//! is not the first. The high fence is the key right of it in the nearest
//! ancestor where it is not the last. Nothing stores them. A node's prefix
//! length is what its fences share. Every key in the node lies between them, so
//! it shares that prefix, whatever is inserted. Along the tree's left and right
//! borders, where a fence is missing, the prefix is what every key of the tree
//! shares. The tree keeps that with its root, and a search checks it first
//! (D50): a key outside it is outside every node.
//!
//! A child's fences lie within its parent's, so its prefix is never shorter
//! than its parent's (D44). For keys a ≤ b ≤ c, what a and c share is the
//! shorter of what a and b share and what b and c share.
//!
//! A prefix changes only when a fence does, and only a separator change moves a
//! fence. A split adds a separator, a merge takes one out, a move between
//! siblings or a removal replaces one, and a cut leaves nodes without one. The
//! nodes whose fences changed then read their keys for their new windows
//! ([`Child::refit_prefix`]). A key that lacks what the tree's keys share
//! shrinks the shared prefix, and the border nodes' prefixes with it, reading
//! no key ([`Tree::shrink_shared_prefix_for`]).
//!
//! An operation changes the tree's shape without calling user encodings,
//! but behind an abort-on-unwind guard (a key's hash tag, and the window an
//! internal node of 4-byte inline keys reads again from its key), so a
//! panicking `InlineKey` can't stop it halfway. A prefix that must shrink
//! meanwhile shrinks to the parent's: its windows shift back, filled from
//! the parent's separator that bounds the node ([`Window::moved_back_to`],
//! `tree/node/mod.rs`). Once the operation is done and its entry counted,
//! the nodes whose fences changed are fit to them:
//!
//! - the halves of each split ([`NodeStackMut::refit_split_halves`]);
//! - the nodes a removal merged or moved entries between (`remove.rs`);
//! - the siblings a cut's borders took entries from
//!   ([`Tree::refit_border_siblings`]);
//! - a built tree's nodes as they close, down a border (`build.rs`,
//!   [`NodeStackMut::refit_border_below`]).
//!
//! A removed separator's successor is the one change read first
//! ([`NodeStackMut::refit_left_of_separator`]).
//!
//! [`Window::moved_back_to`]: super::node::inline_keys::Window::moved_back_to

#![forbid(unsafe_code)]

use super::node::inline_keys::KeyInliner;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::NodePtr;
use super::node::{Child, Node, NodeEnd};
use super::node_stack::{MAX_DEPTH, NodeStackMut};
use super::root::Tree;
use crate::strategy::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> NodeStackMut<'_, K, V, S> {
    /// Fits the prefixes of the nodes an insert split to their fences, from
    /// the leaves up, so each node's fit sees its children's. The node stack
    /// ends at the new entry before and after.
    ///
    /// For each `climbed` from 1 to `levels_climbed`, the node `climbed`
    /// levels up the node stack holds, at `separator_indexes[climbed]`, the
    /// separator between the two halves of its child that split.
    ///
    /// Calls `InlineKey`: an insert does this last, once its entry is in and
    /// counted (D48).
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

    /// Fits the prefixes of the children at `edges` of the bottom node, an
    /// internal node, to what their fences share. It reads the fences unless
    /// the child's own inline keys show they share no more
    /// ([`Child::keys_differ_past_prefix`]). If a prefix changes, it reads
    /// the child's keys too.
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
            // lies (D44): later structural moves must only move windows
            // backwards, without computing prefixes from that encoding.
            let prefix_len = KeyInliner::fence_prefix_len::<S>(child_low, child_high, shared_len)
                .max(node.prefix_len());
            child.refit_prefix(prefix_len);
        }
    }

    /// Fits the prefixes of the nodes below the bottom node's child at
    /// `edge`, down that child's `End` border, from the leaf up so each
    /// node's fit sees its children's. The child itself is the caller's to
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

    /// Prepares the separator the node stack ends at, `s` in an internal
    /// node, to give way to its successor `s'`, the first key of its head's
    /// leaf (D17). `s'` becomes the upper fence of the nodes along the right
    /// border under the edge left of `s`. This shrinks their prefixes, from
    /// the top down, to what they will share with `s'`, reading keys. A
    /// shorter prefix still fits their current fences, so a removal does
    /// this before it changes anything.
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
            // would break the numerical invariant used by key movement
            // (D44).
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
    /// what every key of the tree shares (D50), from the root down. Each
    /// node's windows are filled from its parent's key beside the node
    /// stack, one of its fences, once the parent's prefix is the root's too.
    /// For a cut along the node stack (`cut.rs`): its nodes become the
    /// borders of two trees, whose keys both share what this tree's do.
    /// Reads no key, and no user encoding runs. The node stack stays where
    /// it is.
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
    /// border to its fences, from the root down. Runs after
    /// [`Tree::fix_border`], which moves entries from those nodes into the
    /// border without reading a key. The border's own nodes keep the root's
    /// fence on that side, and so its prefix (D50).
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
