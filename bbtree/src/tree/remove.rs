//! Removing from a tree. The entry comes out where a node stack to it ends: out
//! of a leaf, or out of an internal node as a separator. Going back up the node
//! stack, each node fixes the child it leads to if the removal left that child
//! underfull. It merges the child with a sibling, or moves one entry over from
//! the sibling (D22). A merge takes a separator out of the node itself, for its
//! parent to check in turn; anything else ends the fixing. A root left without
//! separators lowers the tree: its only child moves up into the root page.
//!
//! Removing a separator takes its head out. The first key of the head's
//! leaf, the separator's successor, takes the separator's place, and the
//! successor's value becomes the new head: values never leave their leaf
//! (D17). Without inline keys, the successor's key goes up; with them, it
//! becomes the head's key, and only its window goes up (D49).
//!
//! Prefixes (`prefix.rs`): the fixing reads no key, and gives the nodes it
//! changes their parent's prefix. Once the entry is out and counted, and the
//! tree lowered if its root lost its last separator, the nodes whose fences
//! changed are fit to them:
//!
//! - at each node fixed, the child the node stack leads to, and the sibling
//!   that gave it an entry, if one did;
//! - for a separator, the nodes along the left border of the subtree right
//!   of it, whose lower fence the successor becomes.
//!
//! The nodes along the right border of the subtree left of a removed
//! separator, whose upper fence the successor becomes, are fit before
//! anything changes.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::node::inline_keys::{SearchKey, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::PageAllocator;
use super::node::{Child, Node};
use super::node_stack::{FirstEdge, MAX_DEPTH, NodeStackMut};
use super::root::Tree;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Removes the entry whose key equals `key` and returns it, with the key
    /// as stored, counting it out of `length`; or `None` if there is none.
    ///
    /// Every `Ord` call happens in the search, before anything changes, so a
    /// panicking `Ord` leaves the tree unchanged. `InlineKey` runs before
    /// anything changes too, and last, once the entry is out and counted: a
    /// panic in that last call leaves a valid tree without the entry. Every
    /// page a merge frees, and the one freed when an emptied root lowers the
    /// tree, goes back to `alloc`, the map's allocator.
    pub(crate) fn remove_entry<Q>(
        &mut self,
        key: &Q,
        alloc: &impl PageAllocator,
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
        alloc: &impl PageAllocator,
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
        alloc: &impl PageAllocator,
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
    /// out of `length`. Fixes the nodes up the node stack, and leaves the
    /// node stack detached. Frees pages as [`Tree::remove_entry`] does.
    /// Compares no keys.
    ///
    /// # Panics
    ///
    /// If the node stack ends at no key.
    pub(crate) fn remove(&mut self, alloc: &impl PageAllocator, length: &mut usize) -> (K, V) {
        // Most removals take an entry out of a leaf that keeps its minimum,
        // or is the root: nothing to fix, fit or lower, as in the latched
        // tree (`latched/remove.rs`).
        if let Node::Leaf(leaf) = self.node()
            && (leaf.keeps_its_minimum() || self.is_at_root())
        {
            let entry = leaf.remove(self.index());
            *length -= 1;
            self.detach();
            return entry;
        }
        let (entry, mut changed) = self.take_out(alloc, length);
        self.lower_and_refit_changed_fences(&mut changed, alloc);
        self.detach();
        entry
    }

    /// Removes the entry the node stack ends at, as [`Self::remove`] does, and
    /// ends at the entry after it, wherever the fixing moved it; detached past
    /// the tree's last entry.
    pub(crate) fn remove_and_go_to_next(
        &mut self,
        alloc: &impl PageAllocator,
        length: &mut usize,
    ) -> (K, V) {
        // A leaf that needs no fixing keeps the path to its successor. Its
        // next key shifts into this index; past its end, the next slot leads
        // to the separator on its right, or detaches past the tree's last.
        if let Node::Leaf(leaf) = self.node()
            && (leaf.keeps_its_minimum() || self.is_at_root())
        {
            let index = self.index();
            let entry = leaf.remove(index);
            *length -= 1;
            self.go_to_entry_at_slot(index + 1);
            return entry;
        }
        let (entry, mut changed) = self.take_out(alloc, length);
        // The way down to the next entry: as the node stack leads above the
        // nodes the fixing changed, and as the fixing tracked it from there.
        for (depth, edge) in changed.indexes.iter_mut().enumerate().take(self.depth()) {
            *edge = self.index_at(depth);
        }
        self.lower_and_refit_changed_fences(&mut changed, alloc);
        let (slot, edges) = changed.indexes[..=changed.leaf_depth]
            .split_last()
            .expect("the way down ends at the leaf");
        self.start_at_root();
        for &edge in edges {
            self.set_index(edge);
            self.descend();
        }
        self.go_to_entry_at_slot(*slot);
        entry
    }

    /// Takes out the entry the node stack ends at, counting it out of `length`,
    /// and fixes the nodes up the node stack; returns it, and where the fixing
    /// changed fences, for [`Self::remove`] to refit.
    fn take_out(
        &mut self,
        alloc: &impl PageAllocator,
        length: &mut usize,
    ) -> ((K, V), ChangedFences) {
        let entry_depth = self.depth();
        let entry_index = self.index();
        let (entry, changed) = if let Node::Leaf(leaf) = self.node() {
            let entry = leaf.remove(entry_index);
            // The next entry takes the removed one's place.
            (
                entry,
                self.fix_up_the_stack(entry_index + 1, None, alloc, |_| {}),
            )
        } else {
            self.refit_left_of_separator();
            // Go down to the separator's head.
            self.set_index(entry_index + 1);
            self.descend_to_leaf::<FirstEdge>();
            let succession = self
                .leaf()
                .take_head_and_first_key()
                .expect("the leaf right of a separator has keys");
            let mut successor = Some(succession.successor);
            let mut replaced = None;
            // The next entry, the successor, takes the separator's place, just
            // left of the leaf.
            let changed = self.fix_up_the_stack(0, Some(entry_depth), alloc, |node| {
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
        (entry, changed)
    }

    /// Climbs the node stack from its bottom node, a leaf that just lost a
    /// key. In each node, it fixes the child the node stack came from while
    /// that child is underfull. If `must_reach` is given, it climbs to that
    /// depth regardless, and lets `at_must_reach` change that node before
    /// fixing its child. Returns where the fences it changed are, counting
    /// every node from `must_reach` down as changed, and where the entry at
    /// `next_slot` of the leaf ([`NodeStackMut::go_to_entry_at_slot`]) ends
    /// up.
    ///
    /// It decides whether to climb from the node it is at, and reads no
    /// node above the last one it changes: a latched tree's removal holds
    /// just the nodes it changes (`tree/latched/remove.rs`).
    fn fix_up_the_stack(
        &mut self,
        next_slot: usize,
        must_reach: Option<usize>,
        alloc: &impl PageAllocator,
        mut at_must_reach: impl FnMut(Internal<K, V, S>),
    ) -> ChangedFences {
        let mut changed = ChangedFences::new(self.depth(), next_slot, must_reach);
        let mut child_is_underfull = self.leaf().is_underfull();
        while !self.is_at_root()
            && (child_is_underfull || must_reach.is_some_and(|depth| self.depth() > depth))
        {
            let node = self.ascend();
            let (depth, index) = (self.depth(), self.index());
            changed.indexes[depth] = index;
            if Some(depth) == must_reach {
                at_must_reach(node);
            }
            if child_is_underfull {
                let fix = if node.has_leaf_children() {
                    node.fix_underfull_child::<Leaf<K, V, S>>(index, alloc)
                } else {
                    node.fix_underfull_child::<Internal<K, V, S>>(index, alloc)
                };
                let merged = matches!(
                    fix,
                    ChildFix::MergedIntoLeft { .. } | ChildFix::MergedRightIn
                );
                changed.record(depth, fix);
                child_is_underfull = merged && node.is_underfull();
            }
        }
        changed
    }

    /// Lowers the tree if the fixing took the root's last separator, then
    /// fits the prefixes of the nodes whose fences a removal changed, from
    /// the bottom up, as `changed` tells. The node stack ends where the
    /// fixing stopped, and afterwards at `changed`'s top depth. Calls
    /// `InlineKey`, once the tree has lowered: a panic there leaves no
    /// internal node without a separator, which the next removal through it
    /// would fix the child of (D48).
    fn lower_and_refit_changed_fences(
        &mut self,
        changed: &mut ChangedFences,
        alloc: &impl PageAllocator,
    ) {
        if self.lower_an_emptied_root(changed.reached_root(), alloc) {
            changed.lower();
        }
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

/// Where a removal's fixing changed fences, for fitting the prefixes there once
/// the removal is done, and where the entry after the removed one went.
struct ChangedFences {
    /// The way back down. At each depth the fixing climbed to, the edge to
    /// the child it came from; at the leaf's depth, the slot of the entry
    /// after the removed one ([`NodeStackMut::go_to_entry_at_slot`]). Fixes
    /// keep it up to date as they move entries between a child and its
    /// siblings.
    indexes: [usize; MAX_DEPTH + 1],
    /// At each depth, the edge to the sibling that gave the child an entry,
    /// if one did.
    donors: [Option<usize>; MAX_DEPTH + 1],
    /// The depth of the leaf the fixing went up from; one less once the
    /// tree lowers ([`Self::lower`]).
    leaf_depth: usize,
    /// The child of every node from depth `top_depth` down to the leaf's parent
    /// has new fences; none does if `None`.
    top_depth: Option<usize>,
}

impl ChangedFences {
    /// Whether the fixing changed the root's children: only then may it
    /// have taken the root's last separator.
    fn reached_root(&self) -> bool {
        self.top_depth == Some(0)
    }

    /// The tree lowered, its root's only child moving up into the root:
    /// each node is a depth higher. That child took the root's prefix when
    /// it merged (`Siblings::merge`), what every key shares (D50), so it is
    /// fit as the root; the nodes below it are fit as before.
    fn lower(&mut self) {
        self.indexes.copy_within(1.., 0);
        self.donors.copy_within(1.., 0);
        self.leaf_depth -= 1;
        self.top_depth = (self.leaf_depth > 0).then_some(0);
    }

    /// Nothing fixed yet, up from a leaf at `leaf_depth`, whose entry after the
    /// removed one is at `next_slot`, with the children of the nodes from
    /// `top_depth` down counting as changed.
    fn new(leaf_depth: usize, next_slot: usize, top_depth: Option<usize>) -> Self {
        let mut indexes = [0; MAX_DEPTH + 1];
        indexes[leaf_depth] = next_slot;
        Self {
            indexes,
            donors: [None; MAX_DEPTH + 1],
            leaf_depth,
            top_depth,
        }
    }

    /// Records what fixing the child of the node at `depth` did;
    /// `indexes[depth]` is the edge to it until then. A leaf's slots shift as
    /// its edges do: its head's separator comes before its keys.
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
    /// The edge of the sibling that fixing an underfull child at edge
    /// `index` pairs it with: its left one if it has one, else its right one.
    pub(in crate::tree) fn fixing_sibling(index: usize) -> usize {
        if index > 0 { index - 1 } else { index + 1 }
    }

    /// Fixes the child at edge `index` if a removal left it underfull, one
    /// entry short of its minimum (D22). It pairs the child with a sibling,
    /// the left one if there is one. If the two fit in one node with the
    /// separator between them, they merge, and the freed page goes back to
    /// `alloc`. Otherwise one entry moves over from the sibling. Only a merge
    /// takes a separator from this node.
    ///
    /// A sibling too full to merge with holds more than `capacity - min_len`
    /// entries, so it can spare one.
    ///
    /// The children are of kind `C`, which the caller decided once for the
    /// node ([`Internal::has_leaf_children`]).
    ///
    /// # Panics
    ///
    /// If the node has no separators, so that the child has no sibling. A
    /// node that a removal went through holds a separator until it fixes its
    /// child, which is the only time it can lose one.
    fn fix_underfull_child<C: Child<K, V, S>>(
        self,
        index: usize,
        alloc: &impl PageAllocator,
    ) -> ChildFix {
        if C::child_of(self, index).len() >= C::min_len() {
            return ChildFix::Nothing;
        }
        let sibling = Self::fixing_sibling(index);
        assert!(
            sibling <= self.len(),
            "an internal node a removal went through has a separator"
        );
        let (separator_index, child_is_right) = (index.min(sibling), sibling < index);
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
