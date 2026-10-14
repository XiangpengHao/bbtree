//! Joining two trees whose keys don't overlap into one, around a separator
//! entry between them, comparing no keys: [`Tree::join`]. The inverse of a
//! cut (`cut.rs`): it touches the nodes along one border of the taller tree
//! and the two spines beside the separator, O(log n) of them.
//!
//! The shorter tree joins the taller one's border on its side: the right
//! border, for a shorter right tree, or the left one. Each tree's leaves are
//! as deep as its border is long, which a walk down it counts. The border
//! node whose leaves are as deep below it as the shorter tree's are below
//! its root absorbs the shorter root around the separator if all of it fits
//! in one node, as a merge would. Otherwise the shorter root hangs beside
//! it, under the separator, in the node one level up, which splits up the
//! border if full, as an insert's nodes do. Only roots may
//! hold fewer keys than a node's minimum (D22), so one of the two roots now
//! side by side may: it takes keys over from the other, which, too full to
//! share a node with it, can spare them. No merge happens, so no node above
//! loses a key. Whichever way, the separator's value is the head of the
//! right tree's first leaf, the leftmost leaf right of it (D17), and where
//! separator keys are heads, so is its key (D49).
//!
//! Prefixes (05-fence-key.md): every node that changes is on the taller
//! tree's border or was a root, with what both trees' keys share as its
//! prefix (D50), or beside them. The split halves and the sibling that gave
//! keys over, beside the border, and the two spines below the separator,
//! whose nodes gain it as a fence, fit their prefixes last, reading keys,
//! once the tree is whole.

#![forbid(unsafe_code)]

use core::mem;

use super::node::inline_keys::{Separator, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::NodePtr;
use super::node::siblings::Siblings;
use super::node::{Child, Insertion, Node};
use super::path::{FirstEdge, LastEdge, NodeEnd, PathMut};
use super::root::Tree;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// Where a join left its separator: in the node on the border it joined
/// that a path ends at, or, where keys moved over, in that node's child at
/// `child_edge`, one level down; at `index` there.
struct SeparatorPlace {
    child_edge: Option<usize>,
    index: usize,
    /// Whether the shorter root hung beside the border rather than being
    /// absorbed into it: then nodes beside the border changed.
    hung: bool,
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Joins this tree, the separator, its key `separator` with its window
    /// at the trees' shared prefix and its `value`, and `right`, whose keys
    /// ascend in that order, which both have entries, and whose keys share
    /// the same bytes as this tree's (D50), into this tree. Compares no keys;
    /// keys out of order make a tree whose searches give wrong answers, but
    /// that is memory-safe. Pages come from and go back to `alloc`, the
    /// allocator both trees' pages came from.
    ///
    /// The caller computes the separator's window, which calls `InlineKey`,
    /// while it still holds `right`: a panic then leaves both trees with
    /// their owners. `InlineKey` is called again, last, to fit the
    /// prefixes, once the tree is whole: a panic then leaves it valid, with
    /// this tree's owner.
    pub(in crate::tree) fn join(
        &mut self,
        separator: WindowedKey<K, S>,
        value: V,
        mut right: Self,
        alloc: &impl Allocator,
    ) {
        // Down `right`'s left border, the one the join walks there, counting
        // how deep its leaves lie, to its first leaf, which takes the value.
        let (mut node, mut right_depth) = (right.root_mut(), 0);
        let separator = loop {
            node = match node {
                Node::Leaf(leaf) => break leaf.put_separator_head(separator, value),
                Node::Internal(internal) => {
                    right_depth += 1;
                    internal.child(0)
                }
            };
        };
        let left_depth = self.leaf_depth();
        if left_depth >= right_depth {
            self.join_at_border::<LastEdge>(separator, right, right_depth, alloc);
        } else {
            let left = mem::replace(self, right);
            self.join_at_border::<FirstEdge>(separator, left, left_depth, alloc);
        }
    }

    /// Joins `shorter`, a tree whose leaves lie `shorter_depth` below its
    /// root, no deeper than this tree's, beside this tree's `End` border,
    /// with `separator` between them: after this tree's last entry for
    /// [`LastEdge`], before its first for [`FirstEdge`]. The separator's
    /// value is already the head of the leftmost leaf right of it (D17).
    fn join_at_border<End: NodeEnd>(
        &mut self,
        separator: Separator<K, S>,
        shorter: Self,
        shorter_depth: usize,
        alloc: &impl Allocator,
    ) {
        // The shorter root hangs under an edge, on a plain page.
        let shorter = shorter.into_node(alloc);
        let hung = {
            let mut path = PathMut::new(self);
            path.descend_border::<End>(shorter_depth);
            let place = path.absorb_or_hang::<End>(separator, shorter, shorter_depth, alloc);
            path.refit_separator_spines(&place);
            place.hung
        };
        if hung {
            self.refit_border_siblings::<End>();
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PathMut<'_, K, V, S> {
    /// Starts the path at the root and takes it down `End`'s border to the
    /// node whose leaves lie `subtree_depth` below it: down to the border's
    /// leaf, and back up that many levels.
    fn descend_border<End: NodeEnd>(&mut self, subtree_depth: usize) {
        self.start_at_root();
        self.descend_to_end::<End>();
        for _ in 0..subtree_depth {
            self.ascend();
        }
    }

    /// At the bottom node, on the tree's `End` border, whose leaves lie as
    /// deep below it as `shorter`'s, `shorter_depth`: absorbs `shorter`'s
    /// root around `separator` if they fit in one node, or else hangs the
    /// right one of the two right of the left one, under the separator, one
    /// level up. On the left border, `shorter`'s root first takes the
    /// node's place, as the left one.
    fn absorb_or_hang<End: NodeEnd>(
        &mut self,
        separator: Separator<K, S>,
        shorter: Node<K, V, S>,
        shorter_depth: usize,
        alloc: &impl Allocator,
    ) -> SeparatorPlace {
        let right = if End::IS_LAST {
            shorter
        } else {
            let displaced = self.ascend().replace_child(0, shorter);
            self.descend();
            displaced
        };
        let index = self.node().len();
        let (separator, right) = match self.node().absorb(separator, right) {
            Ok(page) => {
                page.deallocate(alloc);
                return SeparatorPlace {
                    child_edge: None,
                    index,
                    hung: false,
                };
            }
            Err(unabsorbed) => unabsorbed,
        };
        self.hang_beside(separator, right, alloc);
        // Inserted at the border's edge, the separator is the border node's
        // key on that side one level up, in the half on that side if it
        // split.
        self.descend_border::<End>(shorter_depth + 1);
        let index = if End::IS_LAST {
            self.node().len() - 1
        } else {
            0
        };
        self.stock_beside_separator(index)
    }

    /// Hangs `right`, a tree as deep as the bottom node's subtree, right of
    /// that node, under `separator`, in the node one level up: under a new
    /// root if the node is the root, which the tree raises for it;
    /// splitting up the path while full, each split's right half hung
    /// right of its left half the same way, on a page from `alloc`.
    fn hang_beside(
        &mut self,
        separator: Separator<K, S>,
        right: Node<K, V, S>,
        alloc: &impl Allocator,
    ) {
        let mut carried = (separator, right);
        loop {
            let (separator, right) = carried;
            let node = self.ascend_raising(alloc);
            let index = self.index();
            match node.insert(index, separator, right, alloc) {
                Insertion::Fit(_) => return,
                Insertion::Split(split) => {
                    carried = (split.separator, Node::Internal(split.right));
                }
            }
        }
    }

    /// Brings whichever of the two children beside the separator, at
    /// `index` in the bottom node, holds fewer keys than its minimum, a
    /// root before, up to it, taking keys over from the other; returns
    /// where the separator is then. The two didn't fit in one node, so at
    /// most one is short, and the other can spare what it needs.
    fn stock_beside_separator(&mut self, index: usize) -> SeparatorPlace {
        let node = self.internal();
        if node.has_leaf_children() {
            node.siblings::<Leaf<K, V, S>>(index)
                .stock_beside_separator(index)
        } else {
            node.siblings::<Internal<K, V, S>>(index)
                .stock_beside_separator(index)
        }
    }

    /// Fits the prefixes of the two spines below the separator at `place`,
    /// whose fences the separator joined (05-fence-key.md): the right border
    /// of the child left of it and the left border of the child right of
    /// it, from the leaves up, and then those two children: an internal
    /// node's prefix grows no further than its children's. The path ends
    /// at the node `place` is in, and goes to its child there if keys moved
    /// over; a separator in a leaf has nothing below it.
    fn refit_separator_spines(&mut self, place: &SeparatorPlace) {
        if !NodePtr::<K, V, S>::has_inline_keys() {
            return;
        }
        if let Some(edge) = place.child_edge {
            self.set_index(edge);
            self.descend();
        }
        if self.ends_at_leaf() {
            return;
        }
        let index = place.index;
        self.refit_border_below::<LastEdge>(index);
        self.refit_border_below::<FirstEdge>(index + 1);
        self.refit_children_to_fences([index, index + 1]);
    }
}

impl<K, V, S: InlineKeyStrategy<K>, C: Child<K, V, S>> Siblings<K, V, S, C> {
    /// [`PathMut::stock_beside_separator`], for the children beside the
    /// separator at `index` in their parent.
    fn stock_beside_separator(mut self, index: usize) -> SeparatorPlace {
        let min_len = C::min_len();
        if self.right_len() < min_len {
            let steals = min_len - self.right_len();
            for _ in 0..steals {
                self.steal_left()
                    .expect("a child too full to share a node can spare keys");
            }
            // Each move puts the parent's separator at the right child's
            // front: the joined one, moved first, ends after the others.
            return SeparatorPlace {
                child_edge: Some(index + 1),
                index: steals - 1,
                hung: true,
            };
        }
        if self.left_len() < min_len {
            let left_len = self.left_len();
            for _ in 0..min_len - left_len {
                self.steal_right()
                    .expect("a child too full to share a node can spare keys");
            }
            // The joined separator goes to the end of the left child first.
            return SeparatorPlace {
                child_edge: Some(index),
                index: left_len,
                hung: true,
            };
        }
        SeparatorPlace {
            child_edge: None,
            index,
            hung: true,
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// The tree of `left`'s entries and then `right`'s, every one of whose
    /// keys must be greater, joined around `left`'s last entry
    /// ([`Self::join`]); or the one with entries; or no tree if neither has
    /// any. Keys out of order make a tree whose searches give wrong answers,
    /// but that is memory-safe.
    ///
    /// Both trees stay with owners that take them apart while `InlineKey`
    /// runs, as the last entry's removal, its window and the join call it:
    /// a panic then drops their entries rather than leaking them. Pages
    /// come from and go back to `alloc`, the allocator both trees' pages
    /// came from.
    pub(crate) fn concat(left: Self, right: Self, alloc: &impl Allocator) -> Self {
        let left = Self::destroy_if_empty(left, alloc);
        let right = Self::destroy_if_empty(right, alloc);
        if !left.has_root() {
            return right;
        }
        if !right.has_root() {
            return left;
        }
        let mut left = DetachedTree::new(left, alloc);
        let mut right = DetachedTree::new(right, alloc);
        let mut left_length = 1;
        let (key, value) = (left.tree)
            .pop_last(alloc, &mut left_length)
            .expect("a tree with entries has a last one");
        left.tree = Self::destroy_if_empty(left.take(), alloc);
        if !left.tree.has_root() {
            let mut right_length = 0;
            let mut path = PathMut::new(&mut right.tree);
            path.start_at_root();
            path.descend_to_leaf::<FirstEdge>();
            path.cover_key(&key);
            path.insert(key, value, alloc, &mut right_length);
            return right.take();
        }
        // Both trees' keys, and the separator, share what both trees' keys
        // share with the separator (D50).
        let shared_with_key = |tree: &Self| tree.shared_prefix().len_shared_with::<K, S>(&key);
        let shared_len = shared_with_key(&left.tree).min(shared_with_key(&right.tree));
        let separator = WindowedKey::new(key, shared_len);
        let mut right = right.take();
        left.tree.shrink_shared_prefix_to(shared_len);
        right.shrink_shared_prefix_to(shared_len);
        left.tree.join(separator, value, right, alloc);
        left.take()
    }

    /// `tree`, if it has entries; otherwise it is taken apart, its pages
    /// going back to `alloc`, and no tree is left.
    fn destroy_if_empty(tree: Self, alloc: &impl Allocator) -> Self {
        if tree
            .root_page()
            .is_some_and(|root_page| root_page.node().len() > 0)
        {
            return tree;
        }
        tree.destroy(alloc);
        Self::EMPTY
    }
}

/// A tree held apart from any map, as the pieces [`Tree::concat`] joins
/// are, and the entries after an `extract_if`'s range while its walk lasts,
/// with the allocator its pages came from: taken apart if dropped.
pub(crate) struct DetachedTree<K, V, S: InlineKeyStrategy<K>, A: Allocator> {
    tree: Tree<K, V, S>,
    alloc: A,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> DetachedTree<K, V, S, A> {
    pub(crate) fn new(tree: Tree<K, V, S>, alloc: A) -> Self {
        Self { tree, alloc }
    }

    pub(crate) fn tree_mut(&mut self) -> &mut Tree<K, V, S> {
        &mut self.tree
    }

    pub(crate) fn take(&mut self) -> Tree<K, V, S> {
        self.tree.take()
    }

    /// The first entry, for `Debug` to print where std's walk would.
    pub(crate) fn first_key_value(&self) -> Option<(&K, &V)> {
        self.tree.first_key_value()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for DetachedTree<K, V, S, A> {
    fn drop(&mut self) {
        self.tree.take().destroy(&self.alloc);
    }
}
