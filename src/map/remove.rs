//! Removing from a tree: the entry comes out where a path to it ends
//! (`node/path.rs`): out of a leaf, or out of an internal node as a
//! separator. Then, going back up the path, each node fixes the child it
//! leads to if the removal left that child underfull, by merging it with a
//! sibling or moving one entry over from it (D22). A merge takes a key out
//! of the node itself, for its parent to check in turn; anything else ends
//! the fixing. A root left without keys gives way to its only child.
//!
//! A separator's value is the head of the leftmost leaf right of it (D17).
//! Removing the separator takes that head out, and the leaf's first key goes
//! up to take the separator's place, with its value as the new head: values
//! never leave their leaf.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{Edge, ForceResult, Internal, NodeMut, Subtree};
use crate::node::inline_keys::SearchKey;
use crate::node::page::PageByKind;
use crate::node::path::{NodeEnd, PathMut};

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Removes the entry whose key equals `key` and returns it, with the key
    /// as stored, or `None` if there is none.
    ///
    /// Every `Ord` call happens in the search, before anything changes, so a
    /// panicking `Ord` leaves the tree unchanged. So does a panicking
    /// `InlineKey`: the key's inline key is computed first. Every page a
    /// merge frees, and the root's if a merge empties it, goes to `release`.
    pub(crate) fn remove_entry<Q>(
        &mut self,
        key: &Q,
        release: impl FnMut(PageByKind<K, V, S>),
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut path = PathMut::new(self);
        if path.search(&SearchKey::new(key)) {
            Some(path.remove(release))
        } else {
            None
        }
    }

    /// Removes the entry with the smallest key and returns it, or `None` if
    /// the tree is empty. Calls no `Ord`; frees pages as
    /// [`Self::remove_entry`] does.
    pub(crate) fn pop_first(&mut self, release: impl FnMut(PageByKind<K, V, S>)) -> Option<(K, V)> {
        let mut path = PathMut::new(self);
        if path.descend_to_first_key() {
            Some(path.remove(release))
        } else {
            None
        }
    }

    /// Removes the entry with the largest key and returns it, or `None` if
    /// the tree is empty. Calls no `Ord`; frees pages as
    /// [`Self::remove_entry`] does.
    pub(crate) fn pop_last(&mut self, release: impl FnMut(PageByKind<K, V, S>)) -> Option<(K, V)> {
        let mut path = PathMut::new(self);
        if path.descend_to_last_key() {
            Some(path.remove(release))
        } else {
            None
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PathMut<'_, K, V, S> {
    /// Removes the entry the path ends at and returns it, fixing the nodes up
    /// the path, and leaves the path detached; frees pages as
    /// [`Subtree::remove_entry`] does. Compares no keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn remove(&mut self, mut release: impl FnMut(PageByKind<K, V, S>)) -> (K, V) {
        let separator_height = self.height();
        let separator_index = self.index();
        if separator_height == 0 {
            let ForceResult::Leaf(leaf) = self.node_mut().force() else {
                unreachable!("a node at height 0 is a leaf");
            };
            let (entry, _) = leaf.kv_at(separator_index).remove();
            self.fix_up_the_path(0, &mut release, |_| {});
            return entry;
        }
        // The separator's value is the head of the leftmost leaf right of it.
        self.set_index(separator_index + 1);
        self.descend_to_leaf(NodeEnd::First);
        let ForceResult::Leaf(leaf) = self.node_mut().force() else {
            unreachable!("a path down to a leaf ends at a leaf");
        };
        let (value, successor) = leaf
            .head()
            .and_then(|head| head.remove_with_first_key())
            .expect("the leaf right of a separator has a head and keys");
        let mut successor = Some(successor);
        let mut key = None;
        self.fix_up_the_path(separator_height, &mut release, |node| {
            let successor = successor.take().expect("the separator is replaced once");
            key = Some(node.kv_at(separator_index).replace_key(successor));
        });
        (key.expect("the path goes up past the separator"), value)
    }

    /// Goes up the path from its bottom node, which just lost a key, and in
    /// each node fixes the child the path came from if it is underfull, as
    /// long as the child lost a key; then detaches the path, popping a root
    /// left without keys. At `must_reach`, it goes on regardless, and lets
    /// `at_must_reach` change that node before it fixes its child.
    fn fix_up_the_path(
        &mut self,
        must_reach: usize,
        release: &mut impl FnMut(PageByKind<K, V, S>),
        mut at_must_reach: impl FnMut(NodeMut<'_, K, V, S, Internal>),
    ) {
        let mut child_lost_a_key = true;
        while self.height() < self.root_height() && (child_lost_a_key || self.height() < must_reach)
        {
            self.ascend();
            let (height, index) = (self.height(), self.index());
            let ForceResult::Internal(mut node) = self.node_mut().force() else {
                unreachable!("a node above another is internal");
            };
            if height == must_reach {
                at_must_reach(node.reborrow_mut());
            }
            if child_lost_a_key {
                child_lost_a_key = node.edge_at(index).fix_underfull_child(release);
            }
        }
        self.detach_popping_an_empty_root(release);
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'_, K, V, S, Internal>> {
    /// Fixes the child at this edge if a removal left it underfull, one key
    /// short of its `min_len` (D22): merges it with a sibling, the left one
    /// if it has one, or, if the two don't fit in one node with the separator
    /// between them, moves one entry over from the sibling. The page a merge
    /// frees goes to `release`. Returns whether this node lost a key, which
    /// only a merge takes.
    ///
    /// A sibling too full to merge with holds more than `capacity - min_len`
    /// keys, so it can spare one; after at most `min_len` such steals in a
    /// row, the two fit.
    ///
    /// # Panics
    ///
    /// If the node has no keys, so that the child has no sibling: a node
    /// that a removal went through holds a key until it fixes its child,
    /// which is the only time it can lose one.
    fn fix_underfull_child(self, release: &mut impl FnMut(PageByKind<K, V, S>)) -> bool {
        if !self.reborrow().descend().is_underfull() {
            return false;
        }
        let (separator, child_is_right) = match self.left_kv() {
            Ok(left_separator) => (left_separator, true),
            Err(edge) => match edge.right_kv() {
                Ok(right_separator) => (right_separator, false),
                Err(_) => unreachable!("an internal node a removal went through has a key"),
            },
        };
        let mut context = match separator.into_balancing_context().can_merge() {
            Ok(mergeable) => {
                release(mergeable.merge().1);
                return true;
            }
            Err(context) => context,
        };
        let stolen = if child_is_right {
            context.steal_left()
        } else {
            context.steal_right()
        };
        stolen.expect("a sibling too full to merge with can spare a key");
        false
    }
}
