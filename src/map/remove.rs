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
//!
//! Prefixes (`node/prefix.rs`): the fixing reads no key, and gives the nodes
//! it changes their parent's prefix. Once the entry is out and counted, the
//! nodes whose fences changed are fit to them: at each height fixed, the
//! child the path leads to and the sibling that gave it an entry, if one
//! did; for a separator, also the nodes along the left border of the
//! subtree right of it, whose lower fence its successor becomes. Those along
//! the right border of the subtree left of it, whose upper fence it becomes,
//! are fit before anything changes.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{Edge, Internal, Leaf, NodeMut, Subtree, TrailingSlots};
use crate::node::inline_keys::SearchKey;
use crate::node::page::PageByKind;
use crate::node::path::{FirstEdge, MAX_HEIGHT, PathMut};

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Removes the entry whose key equals `key` and returns it, with the key
    /// as stored, counting it out of `length`; or `None` if there is none.
    ///
    /// Every `Ord` call happens in the search, before anything changes, so a
    /// panicking `Ord` leaves the tree unchanged. `InlineKey` is called
    /// before anything changes too, and last, once the entry is out and
    /// counted: a panic there leaves a tree without it, still valid. Every
    /// page a merge frees, and the root's if a merge empties it, goes to
    /// `release`.
    pub(crate) fn remove_entry<Q>(
        &mut self,
        key: &Q,
        release: impl FnMut(PageByKind<K, V, S>),
        length: &mut usize,
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut path = PathMut::new(self);
        if path.search(&SearchKey::new(key)) {
            Some(path.remove(release, length))
        } else {
            None
        }
    }

    /// Removes the entry with the smallest key and returns it, counting it
    /// out of `length`, or `None` if the tree is empty. Calls no `Ord`;
    /// frees pages as [`Self::remove_entry`] does.
    pub(crate) fn pop_first(
        &mut self,
        release: impl FnMut(PageByKind<K, V, S>),
        length: &mut usize,
    ) -> Option<(K, V)> {
        let mut path = PathMut::new(self);
        if path.descend_to_first_key() {
            Some(path.remove(release, length))
        } else {
            None
        }
    }

    /// Removes the entry with the largest key and returns it, counting it
    /// out of `length`, or `None` if the tree is empty. Calls no `Ord`;
    /// frees pages as [`Self::remove_entry`] does.
    pub(crate) fn pop_last(
        &mut self,
        release: impl FnMut(PageByKind<K, V, S>),
        length: &mut usize,
    ) -> Option<(K, V)> {
        let mut path = PathMut::new(self);
        if path.descend_to_last_key() {
            Some(path.remove(release, length))
        } else {
            None
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PathMut<'_, K, V, S> {
    /// Removes the entry the path ends at and returns it, counting it out of
    /// `length`, fixing the nodes up the path, and leaves the path detached;
    /// frees pages as [`Subtree::remove_entry`] does. Compares no keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn remove(
        &mut self,
        mut release: impl FnMut(PageByKind<K, V, S>),
        length: &mut usize,
    ) -> (K, V) {
        let separator_height = self.height();
        let separator_index = self.index();
        let (entry, changed) = if separator_height == 0 {
            let (entry, _) = self.leaf_mut().kv_at(separator_index).remove();
            (entry, self.fix_up_the_path(0, &mut release, |_| {}))
        } else {
            self.fit_left_of_separator();
            // The separator's value is the head of the leftmost leaf right
            // of it.
            self.set_index(separator_index + 1);
            self.descend_to_leaf::<FirstEdge>();
            let (value, successor) = self
                .leaf_mut()
                .head()
                .and_then(|head| head.remove_with_first_key())
                .expect("the leaf right of a separator has a head and keys");
            let mut successor = Some(successor);
            let mut key = None;
            let changed = self.fix_up_the_path(separator_height, &mut release, |node| {
                let successor = successor.take().expect("the separator is replaced once");
                key = Some(node.kv_at(separator_index).replace_key(successor));
            });
            let key = key.expect("the path goes up past the separator");
            ((key, value), changed)
        };
        *length -= 1;
        self.fit_changed_fences(&changed);
        self.detach_popping_an_empty_root(&mut release);
        entry
    }

    /// Goes up the path from its bottom node, which just lost a key, and in
    /// each node fixes the child the path came from if it is underfull, as
    /// long as the child lost a key; at `must_reach`, it goes on regardless,
    /// and lets `at_must_reach` change that node before it fixes its child.
    /// Returns where the fences it changed are; every height up to
    /// `must_reach` counts as changed.
    fn fix_up_the_path(
        &mut self,
        must_reach: usize,
        release: &mut impl FnMut(PageByKind<K, V, S>),
        mut at_must_reach: impl FnMut(NodeMut<'_, K, V, S, Internal>),
    ) -> ChangedFences {
        let mut changed = ChangedFences::up_to(must_reach);
        let mut child_lost_a_key = true;
        while self.height() < self.root_height() && (child_lost_a_key || self.height() < must_reach)
        {
            self.ascend();
            let (height, index) = (self.height(), self.index());
            changed.indexes[height] = index;
            let mut node = self.internal_mut();
            if height == must_reach {
                at_must_reach(node.reborrow_mut());
            }
            if child_lost_a_key {
                let leaf_children = node.has_leaf_children();
                let child = node.edge_at(index);
                let fix = if leaf_children {
                    child.fix_underfull_child::<Leaf>(release)
                } else {
                    child.fix_underfull_child::<Internal>(release)
                };
                child_lost_a_key =
                    matches!(fix, ChildFix::MergedIntoLeft { .. } | ChildFix::MergedRight);
                changed.record(height, fix);
            }
        }
        changed
    }

    /// Fits the prefixes of the nodes whose fences a removal changed to
    /// them, from the bottom up, as `changed` tells; the path, which ends
    /// where the fixing stopped, ends at `changed`'s top height afterwards.
    /// Calls `InlineKey`.
    fn fit_changed_fences(&mut self, changed: &ChangedFences) {
        if changed.top == 0 {
            return;
        }
        while self.height() > 1 {
            self.set_index(changed.indexes[self.height()]);
            self.descend();
        }
        loop {
            let height = self.height();
            let child = changed.indexes[height];
            self.set_index(child);
            match changed.donors[height] {
                Some(donor) => self.fit_children_to_fences([child, donor]),
                None => self.fit_children_to_fences([child]),
            }
            if height == changed.top {
                return;
            }
            self.ascend();
        }
    }
}

/// What fixing a child that a removal may have left underfull did.
enum ChildFix {
    /// It wasn't underfull.
    Nothing,
    /// It merged into its left sibling, which held `left_len` keys.
    MergedIntoLeft { left_len: usize },
    /// Its right sibling merged into it.
    MergedRight,
    /// It took an entry from its left sibling, at its front.
    TookFromLeft,
    /// It took an entry from its right sibling, at its end.
    TookFromRight,
}

/// Where a removal's fixing changed fences, for fitting the prefixes there
/// once the removal is done.
struct ChangedFences {
    /// At each height the fixing went up to, the edge to the child it came
    /// from, kept up to date as fixes move entries between that child and
    /// its siblings: the way back down.
    indexes: [usize; MAX_HEIGHT + 1],
    /// At each height, the edge to the sibling that gave the child an entry,
    /// if one did.
    donors: [Option<usize>; MAX_HEIGHT + 1],
    /// The child at every height from 1 up to `top` has new fences; none
    /// does if 0.
    top: usize,
}

impl ChangedFences {
    /// Nothing fixed yet, with the children up to height `top` counting as
    /// changed.
    fn up_to(top: usize) -> Self {
        Self {
            indexes: [0; MAX_HEIGHT + 1],
            donors: [None; MAX_HEIGHT + 1],
            top,
        }
    }

    /// Records what fixing the child at `height` did; `indexes[height]` is
    /// the edge to it until then.
    fn record(&mut self, height: usize, fix: ChildFix) {
        let child = self.indexes[height];
        match fix {
            ChildFix::Nothing => return,
            ChildFix::MergedIntoLeft { left_len } => {
                self.indexes[height] = child - 1;
                // The child's edges follow the left sibling's keys and the
                // separator between them.
                self.indexes[height - 1] += left_len + 1;
            }
            ChildFix::MergedRight => {}
            ChildFix::TookFromLeft => {
                self.indexes[height - 1] += 1;
                self.donors[height] = Some(child - 1);
            }
            ChildFix::TookFromRight => self.donors[height] = Some(child + 1),
        }
        self.top = self.top.max(height);
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'_, K, V, S, Internal>> {
    /// Fixes the child at this edge if a removal left it underfull, one key
    /// short of its `min_len` (D22): merges it with a sibling, the left one
    /// if it has one, or, if the two don't fit in one node with the separator
    /// between them, moves one entry over from the sibling. The page a merge
    /// frees goes to `release`. Only a merge takes a key from this node.
    ///
    /// A sibling too full to merge with holds more than `capacity - min_len`
    /// keys, so it can spare one; after at most `min_len` such steals in a
    /// row, the two fit.
    ///
    /// The children are of kind `ChildKind`, which the caller decided once
    /// for the node (`NodeRef::has_leaf_children`).
    ///
    /// # Panics
    ///
    /// If the node has no keys, so that the child has no sibling: a node
    /// that a removal went through holds a key until it fixes its child,
    /// which is the only time it can lose one. If the children are of the
    /// other kind.
    fn fix_underfull_child<ChildKind: TrailingSlots>(
        self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) -> ChildFix {
        if !self.reborrow().descend_as::<ChildKind>().is_underfull() {
            return ChildFix::Nothing;
        }
        let (separator, child_is_right) = match self.left_kv() {
            Ok(left_separator) => (left_separator, true),
            Err(edge) => match edge.right_kv() {
                Ok(right_separator) => (right_separator, false),
                Err(_) => unreachable!("an internal node a removal went through has a key"),
            },
        };
        let context = separator.into_balancing_context::<ChildKind>();
        let left_len = context.left_child_len();
        let mut context = match context.can_merge() {
            Ok(mergeable) => {
                release(mergeable.merge().1);
                return if child_is_right {
                    ChildFix::MergedIntoLeft { left_len }
                } else {
                    ChildFix::MergedRight
                };
            }
            Err(context) => context,
        };
        let stolen = if child_is_right {
            context.steal_left()
        } else {
            context.steal_right()
        };
        stolen.expect("a sibling too full to merge with can spare a key");
        if child_is_right {
            ChildFix::TookFromLeft
        } else {
            ChildFix::TookFromRight
        }
    }
}
