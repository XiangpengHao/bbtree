//! Removing from a tree: the entry comes out where a node stack to it ends
//! (`node_stack.rs`): out of a leaf, or out of an internal node as a
//! separator. Then, going back up the node stack, each node fixes the child it
//! leads to if the removal left that child underfull, by merging it with a
//! sibling or moving one entry over from it (D22). A merge takes a
//! separator out of the node itself, for its parent to check in turn;
//! anything else ends the fixing. A root left without separators lowers the
//! tree: its only child moves up into the root's page.
//!
//! A separator's value is the head of the leftmost leaf right of it (D17),
//! and where separator keys are heads, so is its key (D49). Removing the
//! separator takes that head out, and the leaf's first key takes the
//! separator's place, with its value as the new head: values never leave
//! their leaf. The key goes up where internal nodes keep keys, and becomes
//! the head's key where they don't, its window going up alone.
//!
//! Prefixes (`prefix.rs`): the fixing reads no key, and gives the nodes
//! it changes their parent's prefix. Once the entry is out and counted, the
//! nodes whose fences changed are fit to them: at each node fixed, the
//! child the node stack leads to and the sibling that gave it an entry, if one
//! did; for a separator, also the nodes along the left border of the
//! subtree right of it, whose lower fence its successor becomes. Those along
//! the right border of the subtree left of it, whose upper fence it becomes,
//! are fit before anything changes.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::node::inline_keys::{SearchKey, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::{Child, Node};
use super::node_stack::{FirstEdge, MAX_DEPTH, NodeStackMut};
use super::root::Tree;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Removes the entry whose key equals `key` and returns it, with the key
    /// as stored, counting it out of `length`; or `None` if there is none.
    ///
    /// Every `Ord` call happens in the search, before anything changes, so a
    /// panicking `Ord` leaves the tree unchanged. `InlineKey` is called
    /// before anything changes too, and last, once the entry is out and
    /// counted: a panic there leaves a tree without it, still valid. Every
    /// page a merge frees, and the root's if a merge empties it, goes back
    /// to `alloc`, the map's allocator.
    pub(crate) fn remove_entry<Q>(
        &mut self,
        key: &Q,
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if !self.has_root() {
            return None;
        }
        let mut stack = NodeStackMut::new(self);
        if stack.search(&SearchKey::new(key)) {
            Some(stack.remove(alloc, length))
        } else {
            None
        }
    }

    /// Removes the entry with the smallest key and returns it, counting it
    /// out of `length`, or `None` if the tree is empty. Calls no `Ord`;
    /// frees pages as [`Self::remove_entry`] does.
    pub(crate) fn pop_first(
        &mut self,
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> Option<(K, V)> {
        if !self.has_root() {
            return None;
        }
        let mut stack = NodeStackMut::new(self);
        if stack.descend_to_first_key() {
            Some(stack.remove(alloc, length))
        } else {
            None
        }
    }

    /// Removes the entry with the largest key and returns it, counting it
    /// out of `length`, or `None` if the tree is empty. Calls no `Ord`;
    /// frees pages as [`Self::remove_entry`] does.
    pub(crate) fn pop_last(
        &mut self,
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> Option<(K, V)> {
        if !self.has_root() {
            return None;
        }
        let mut stack = NodeStackMut::new(self);
        if stack.descend_to_last_key() {
            Some(stack.remove(alloc, length))
        } else {
            None
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeStackMut<'_, K, V, S> {
    /// Removes the entry the node stack ends at and returns it, counting it
    /// out of `length`, fixing the nodes up the node stack, and leaves the
    /// node stack detached; frees pages as [`Tree::remove_entry`] does.
    /// Compares no keys.
    ///
    /// # Panics
    ///
    /// If the node stack ends at no key.
    pub(crate) fn remove(&mut self, alloc: &impl Allocator, length: &mut usize) -> (K, V) {
        let entry_depth = self.depth();
        let entry_index = self.index();
        let (entry, changed) = if let Node::Leaf(leaf) = self.node() {
            let entry = leaf.remove(entry_index);
            (entry, self.fix_up_the_stack(None, alloc, |_| {}))
        } else {
            self.refit_left_of_separator();
            // The separator's value is the head of the leftmost leaf right
            // of it.
            self.set_index(entry_index + 1);
            self.descend_to_leaf::<FirstEdge>();
            let succession = self
                .leaf()
                .take_head_and_first_key()
                .expect("the leaf right of a separator has keys");
            let mut successor = Some(succession.successor);
            let mut replaced = None;
            let changed = self.fix_up_the_stack(Some(entry_depth), alloc, |node| {
                let successor = successor.take().expect("the separator is replaced once");
                replaced = Some(node.replace_separator_from_below(entry_index, successor));
            });
            let replaced = replaced.expect("the node stack goes up past the separator");
            let key = (succession.removed_key)
                .or_else(|| replaced.into_windowed_key().map(WindowedKey::into_key))
                .expect("a removed separator's key is in its head or its node");
            ((key, succession.value), changed)
        };
        *length -= 1;
        self.refit_changed_fences(&changed);
        self.detach_lowering_an_empty_root(alloc);
        entry
    }

    /// Goes up the node stack from its bottom node, a leaf, which just lost a
    /// key, and in each node fixes the child the node stack came from if it is
    /// underfull, as long as the child lost a key; up to depth `must_reach`,
    /// if given, it goes on regardless, and lets `at_must_reach` change that
    /// node before it fixes its child. Returns where the fences it changed
    /// are; every node from `must_reach` down counts as changed.
    fn fix_up_the_stack(
        &mut self,
        must_reach: Option<usize>,
        alloc: &impl Allocator,
        mut at_must_reach: impl FnMut(Internal<K, V, S>),
    ) -> ChangedFences {
        let mut changed = ChangedFences::new(self.depth(), must_reach);
        let mut child_lost_a_key = true;
        while !self.is_at_root()
            && (child_lost_a_key || must_reach.is_some_and(|depth| self.depth() > depth))
        {
            let node = self.ascend();
            let (depth, index) = (self.depth(), self.index());
            changed.indexes[depth] = index;
            if Some(depth) == must_reach {
                at_must_reach(node);
            }
            if child_lost_a_key {
                let fix = if node.has_leaf_children() {
                    node.fix_underfull_child::<Leaf<K, V, S>>(index, alloc)
                } else {
                    node.fix_underfull_child::<Internal<K, V, S>>(index, alloc)
                };
                child_lost_a_key = matches!(
                    fix,
                    ChildFix::MergedIntoLeft { .. } | ChildFix::MergedRightIn
                );
                changed.record(depth, fix);
            }
        }
        changed
    }

    /// Fits the prefixes of the nodes whose fences a removal changed to them,
    /// from the bottom up, as `changed` tells; the node stack, which ends
    /// where the fixing stopped, ends at `changed`'s top depth afterwards.
    /// Calls `InlineKey`.
    fn refit_changed_fences(&mut self, changed: &ChangedFences) {
        let Some(top_depth) = changed.top_depth else {
            return;
        };
        while self.depth() + 1 < changed.leaf_depth {
            self.set_index(changed.indexes[self.depth()]);
            self.descend();
        }
        loop {
            let depth = self.depth();
            let child = changed.indexes[depth];
            self.set_index(child);
            match changed.donors[depth] {
                Some(donor) => self.refit_children_to_fences([child, donor]),
                None => self.refit_children_to_fences([child]),
            }
            if depth == top_depth {
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
    MergedRightIn,
    /// It took an entry from its left sibling, at its front.
    TookFromLeft,
    /// It took an entry from its right sibling, at its end.
    TookFromRight,
}

/// Where a removal's fixing changed fences, for fitting the prefixes there
/// once the removal is done.
struct ChangedFences {
    /// At each depth the fixing went up to, the edge to the child it came
    /// from, kept up to date as fixes move entries between that child and
    /// its siblings: the way back down.
    indexes: [usize; MAX_DEPTH + 1],
    /// At each depth, the edge to the sibling that gave the child an entry,
    /// if one did.
    donors: [Option<usize>; MAX_DEPTH + 1],
    /// The depth of the leaf the fixing went up from, which it doesn't
    /// change: the root lowers only once it is done.
    leaf_depth: usize,
    /// The child of every node from depth `top_depth` down to the leaf's parent
    /// has new fences; none does if `None`.
    top_depth: Option<usize>,
}

impl ChangedFences {
    /// Nothing fixed yet, up from a leaf at `leaf_depth`, with the children
    /// of the nodes from `top_depth` down counting as changed.
    fn new(leaf_depth: usize, top_depth: Option<usize>) -> Self {
        Self {
            indexes: [0; MAX_DEPTH + 1],
            donors: [None; MAX_DEPTH + 1],
            leaf_depth,
            top_depth,
        }
    }

    /// Records what fixing the child of the node at `depth` did;
    /// `indexes[depth]` is the edge to it until then.
    fn record(&mut self, depth: usize, fix: ChildFix) {
        let child = self.indexes[depth];
        match fix {
            ChildFix::Nothing => return,
            ChildFix::MergedIntoLeft { left_len } => {
                self.indexes[depth] = child - 1;
                // The child's edges follow the left sibling's keys and the
                // separator between them.
                self.indexes[depth + 1] += left_len + 1;
            }
            ChildFix::MergedRightIn => {}
            ChildFix::TookFromLeft => {
                self.indexes[depth + 1] += 1;
                self.donors[depth] = Some(child - 1);
            }
            ChildFix::TookFromRight => self.donors[depth] = Some(child + 1),
        }
        self.top_depth = Some(
            self.top_depth
                .map_or(depth, |top_depth| top_depth.min(depth)),
        );
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// Fixes the child at edge `index` if a removal left it underfull, one
    /// entry short of its minimum (D22): merges it with a sibling, the left
    /// one if it has one, or, if the two don't fit in one node with the
    /// separator between them, moves one entry over from the sibling. The
    /// page a merge frees goes back to `alloc`. Only a merge takes a separator
    /// from this node.
    ///
    /// A sibling too full to merge with holds more than `capacity - min_len`
    /// entries, so it can spare one.
    ///
    /// The children are of kind `C`, which the caller decided once for the
    /// node ([`Internal::has_leaf_children`]).
    ///
    /// # Panics
    ///
    /// If the node has no separators, so that the child has no sibling: a
    /// node that a removal went through holds a separator until it fixes its
    /// child, which is the only time it can lose one.
    fn fix_underfull_child<C: Child<K, V, S>>(
        self,
        index: usize,
        alloc: &impl Allocator,
    ) -> ChildFix {
        if C::child_of(self, index).len() >= C::min_len() {
            return ChildFix::Nothing;
        }
        let (separator_index, child_is_right) = if index > 0 {
            (index - 1, true)
        } else if index < self.len() {
            (index, false)
        } else {
            unreachable!("an internal node a removal went through has a separator")
        };
        let mut siblings = self.siblings::<C>(separator_index);
        let left_len = siblings.left_len();
        if siblings.can_merge() {
            siblings.merge().deallocate(alloc);
            return if child_is_right {
                ChildFix::MergedIntoLeft { left_len }
            } else {
                ChildFix::MergedRightIn
            };
        }
        let stolen = if child_is_right {
            siblings.steal_left()
        } else {
            siblings.steal_right()
        };
        stolen.expect("a sibling too full to merge with can spare an entry");
        if child_is_right {
            ChildFix::TookFromLeft
        } else {
            ChildFix::TookFromRight
        }
    }
}
