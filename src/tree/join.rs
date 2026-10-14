//! Joining two trees whose keys don't overlap into one, around a separator
//! entry between them, comparing no keys: [`Tree::join`]. The inverse of
//! a cut (`cut.rs`): it touches the
//! nodes along one border of the taller tree and the two spines beside the
//! separator, O(log n) of them.
//!
//! The shorter tree joins the taller one's border on its side: the right
//! border, for a shorter right tree, or the left one. Each tree's leaves are
//! as deep as its border is long, which a walk down it counts. The border
//! node whose leaves are as deep below it as the shorter tree's are below
//! its root absorbs the shorter root around the separator if all of it fits
//! in one node, as a merge would. Otherwise the shorter root
//! hangs beside it, under the separator, in the node one level up, which
//! splits up the border if full, as an insert's nodes do. Only roots may
//! hold fewer keys than a node's minimum (D22), so one of the two roots now
//! side by side may: it takes keys over from the other, which, too full to
//! share a node with it, can spare them. No merge happens, so no node above
//! loses a key. Whichever way, the separator's value is the head of the
//! right tree's first leaf, the leftmost leaf right of it (D17), and where
//! separator keys are heads, so is its key (D49).
//!
//! Prefixes (05-fence-key.md): every node that changes is on the taller
//! tree's border or was a root, with what both trees' keys share as its
//! prefix (D50), or beside them. The split
//! halves and the sibling that gave keys over, beside the border, and the
//! two spines below the separator, whose nodes gain it as a fence, fit their
//! prefixes last, from the top down, reading keys, once the tree is whole.

#![forbid(unsafe_code)]

use core::mem;

use super::node::inline_keys::{Separator, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, PageByType};
use super::node::siblings::Siblings;
use super::node::{Child, Insertion, Node};
use super::path::{FirstEdge, Immut, LastEdge, NodeEnd, Path, PathMut};
use super::root::Tree;
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
    /// that is memory-safe. Pages come from and go to the global allocator.
    ///
    /// The caller computes the separator's window, which calls `InlineKey`,
    /// while it still holds `right`: a panic then leaves both trees with
    /// their owners. `InlineKey` is called again, last, to fit the
    /// prefixes, once the tree is whole: a panic then leaves it valid, with
    /// this tree's owner.
    pub(crate) fn join(&mut self, separator: WindowedKey<K, S>, value: V, mut right: Self) {
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
            self.join_at_border::<LastEdge>(separator, right, right_depth);
        } else {
            let left = mem::replace(self, right);
            self.join_at_border::<FirstEdge>(separator, left, left_depth);
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
    ) {
        // The shorter root hangs under an edge, on a plain page.
        let shorter = shorter.into_node();
        let hung = {
            let mut path = PathMut::new(self);
            Self::descend_border::<End>(&mut path, shorter_depth);
            let place = if End::IS_LAST {
                Self::absorb_or_hang_right(&mut path, separator, shorter, shorter_depth)
            } else {
                Self::absorb_or_hang_left(&mut path, separator, shorter, shorter_depth)
            };
            Self::refit_separator_spines(&mut path, &place);
            place.hung
        };
        if hung {
            self.refit_border_siblings::<End>();
        }
    }

    /// Starts `path` at the root and takes it down `End`'s border to the
    /// node whose leaves lie `subtree_depth` below it: down to the border's
    /// leaf, and back up that many levels.
    fn descend_border<End: NodeEnd>(path: &mut PathMut<'_, K, V, S>, subtree_depth: usize) {
        path.start_at_root();
        path.set_index(End::edge(path.node()));
        path.descend_to_leaf::<End>();
        for _ in 0..subtree_depth {
            path.ascend();
        }
    }

    /// At `path`'s node, on this tree's right border, whose leaves lie as
    /// deep below it as `shorter`'s, `shorter_depth`: absorbs `shorter`'s
    /// root around `separator` if they fit in one node, or else hangs it
    /// right of the node, under the separator, one level up.
    fn absorb_or_hang_right(
        path: &mut PathMut<'_, K, V, S>,
        separator: Separator<K, S>,
        shorter: Node<K, V, S>,
        shorter_depth: usize,
    ) -> SeparatorPlace {
        let index = path.node().len();
        let absorbed = path.node().absorb(separator, shorter);
        let (separator, shorter) = match absorbed {
            Ok(page) => {
                PageByType::deallocate(page);
                return SeparatorPlace {
                    child_edge: None,
                    index,
                    hung: false,
                };
            }
            Err(unabsorbed) => unabsorbed,
        };
        Self::hang_beside(path, separator, shorter);
        // Inserted at the last edge, the separator is the last key of the
        // border node one level up, in the right half if it split.
        Self::descend_border::<LastEdge>(path, shorter_depth + 1);
        let index = path.node().len() - 1;
        Self::stock_beside_separator(path, index)
    }

    /// At `path`'s node, on this tree's left border, whose leaves lie as
    /// deep below it as `shorter`'s, `shorter_depth`, below the root: puts
    /// `shorter`'s root in its place and absorbs it into that root around
    /// `separator` if they fit in one node, or else hangs it right of
    /// `shorter`'s root, under the separator, one level up.
    fn absorb_or_hang_left(
        path: &mut PathMut<'_, K, V, S>,
        separator: Separator<K, S>,
        shorter: Node<K, V, S>,
        shorter_depth: usize,
    ) -> SeparatorPlace {
        let index = shorter.len();
        let displaced = path.ascend().replace_child(0, shorter);
        path.descend();
        let absorbed = path.node().absorb(separator, displaced);
        let (separator, displaced) = match absorbed {
            Ok(page) => {
                PageByType::deallocate(page);
                return SeparatorPlace {
                    child_edge: None,
                    index,
                    hung: false,
                };
            }
            Err(unabsorbed) => unabsorbed,
        };
        Self::hang_beside(path, separator, displaced);
        // Inserted at the first edge, the separator is the first key of the
        // border node one level up, in the left half if it split.
        Self::descend_border::<FirstEdge>(path, shorter_depth + 1);
        Self::stock_beside_separator(path, 0)
    }

    /// Hangs `right`, a tree as deep as `path`'s node's subtree, right of that node,
    /// under `separator`, in the node one level up: under a new root if the
    /// node is the root, which the tree raises for it; splitting up the
    /// path while full, each split's right half hung right of its left half
    /// the same way.
    fn hang_beside(
        path: &mut PathMut<'_, K, V, S>,
        separator: Separator<K, S>,
        right: Node<K, V, S>,
    ) {
        let mut carried = (separator, right);
        loop {
            let (separator, right) = carried;
            let node = path.ascend_raising(FreshPage::allocate, FreshPage::allocate);
            let index = path.index();
            match node.insert(index, separator, right, FreshPage::allocate) {
                Insertion::Fit(_) => return,
                Insertion::Split(split) => {
                    carried = (split.separator, Node::Internal(split.right));
                }
            }
        }
    }

    /// Brings whichever of the two children beside the separator, at
    /// `index` in `path`'s node, holds fewer keys than its minimum, a root
    /// before, up to it, taking keys over from the other; returns where the
    /// separator is then. The two didn't fit in one node, so at most one is
    /// short, and the other can spare what it needs.
    fn stock_beside_separator(path: &mut PathMut<'_, K, V, S>, index: usize) -> SeparatorPlace {
        let node = path.internal();
        if node.has_leaf_children() {
            Self::stock_children(node.siblings::<Leaf<K, V, S>>(index), index)
        } else {
            Self::stock_children(node.siblings::<Internal<K, V, S>>(index), index)
        }
    }

    /// [`Self::stock_beside_separator`], for children of kind `C`.
    fn stock_children<C: Child<K, V, S>>(
        mut siblings: Siblings<K, V, S, C>,
        index: usize,
    ) -> SeparatorPlace {
        let min_len = C::min_len();
        if siblings.right_len() < min_len {
            let steals = min_len - siblings.right_len();
            for _ in 0..steals {
                siblings
                    .steal_left()
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
        if siblings.left_len() < min_len {
            let left_len = siblings.left_len();
            for _ in 0..min_len - left_len {
                siblings
                    .steal_right()
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

    /// Fits the prefixes of the two spines below the separator at `place`,
    /// whose fences the separator joined (05-fence-key.md): the right border
    /// of the child left of it and the left border of the child right of
    /// it, from the leaves up, and then those two children: an internal
    /// node's prefix grows no further than its children's. `path` ends at
    /// the node `place` is in, and goes to its child there if keys moved
    /// over; a separator in a leaf has nothing below it.
    fn refit_separator_spines(path: &mut PathMut<'_, K, V, S>, place: &SeparatorPlace) {
        if let Some(edge) = place.child_edge {
            path.set_index(edge);
            path.descend();
        }
        if path.ends_at_leaf() {
            return;
        }
        let index = place.index;
        Self::refit_spine_below::<LastEdge>(path, index);
        Self::refit_spine_below::<FirstEdge>(path, index + 1);
        path.refit_children_to_fences([index, index + 1]);
    }

    /// Fits the nodes below the child at `edge` of `path`'s node down that
    /// child's `End` border, from the leaf up; the path ends at the node it
    /// started at.
    fn refit_spine_below<End: NodeEnd>(path: &mut PathMut<'_, K, V, S>, edge: usize) {
        let start_depth = path.depth();
        path.set_index(edge);
        path.descend_to_leaf::<End>();
        path.ascend();
        while path.depth() > start_depth {
            let child = path.index();
            path.refit_children_to_fences([child]);
            path.ascend();
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
    /// a panic then drops their entries rather than leaking them.
    pub(crate) fn concat(left: Self, right: Self) -> Self {
        let (left, right) = (Self::destroy_if_empty(left), Self::destroy_if_empty(right));
        if !left.has_root() {
            return right;
        }
        if !right.has_root() {
            return left;
        }
        let (mut left, mut right) = (DetachedTree(left), DetachedTree(right));
        let mut left_length = 1;
        let (key, value) = (left.0)
            .pop_last(PageByType::deallocate, &mut left_length)
            .expect("a tree with entries has a last one");
        left.0 = Self::destroy_if_empty(left.0.take());
        if !left.0.has_root() {
            let mut right_length = 0;
            let mut path = PathMut::new(&mut right.0);
            path.start_at_root();
            path.descend_to_leaf::<FirstEdge>();
            path.insert(
                key,
                value,
                FreshPage::allocate,
                FreshPage::allocate,
                &mut right_length,
            );
            return right.0.take();
        }
        // Both trees' keys, and the separator, share what both trees' keys
        // share with the separator (D50).
        let shared_with_key =
            |tree: &DetachedTree<K, V, S>| tree.0.shared_prefix().len_shared_with::<K, S>(&key);
        let shared_len = shared_with_key(&left).min(shared_with_key(&right));
        let separator = WindowedKey::new(key, shared_len);
        let mut right = right.0.take();
        left.0.shrink_shared_prefix_to(shared_len);
        right.shrink_shared_prefix_to(shared_len);
        left.0.join(separator, value, right);
        left.0.take()
    }

    /// `tree`, if it has entries; otherwise it is taken apart, and no tree
    /// is left.
    fn destroy_if_empty(tree: Self) -> Self {
        if tree
            .root_page()
            .is_some_and(|root_page| root_page.root().len() > 0)
        {
            return tree;
        }
        tree.destroy(PageByType::deallocate);
        Self::EMPTY
    }
}

/// A tree held apart from any map, as the pieces [`Tree::concat`] joins
/// are, and the entries after an `extract_if`'s range while its walk lasts:
/// taken apart if dropped.
pub(crate) struct DetachedTree<K, V, S: InlineKeyStrategy<K>>(Tree<K, V, S>);

impl<K, V, S: InlineKeyStrategy<K>> DetachedTree<K, V, S> {
    pub(crate) fn new(tree: Tree<K, V, S>) -> Self {
        Self(tree)
    }

    pub(crate) fn tree_mut(&mut self) -> &mut Tree<K, V, S> {
        &mut self.0
    }

    pub(crate) fn take(&mut self) -> Tree<K, V, S> {
        self.0.take()
    }

    /// The first entry, for `Debug` to print where std's walk would.
    pub(crate) fn first_entry(&self) -> Option<(&K, &V)> {
        let root_page = self.0.root_page()?;
        let mut path = Path::<Immut<'_>, K, V, S>::detached();
        path.attach(root_page.root());
        if path.descend_to_first_key() {
            Some(path.key_value())
        } else {
            None
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for DetachedTree<K, V, S> {
    fn drop(&mut self) {
        self.0.take().destroy(PageByType::deallocate);
    }
}
