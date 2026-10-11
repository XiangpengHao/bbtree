//! Joining two trees whose keys don't overlap into one, around a separator
//! entry between them, comparing no keys: [`Subtree::join`]. The inverse of
//! a cut (`map/split_off.rs`): it touches the
//! nodes along one border of the taller tree, down to the shorter one's
//! height, and the two spines beside the separator, O(log n) of them.
//!
//! The shorter tree joins the taller one's border on its side: the right
//! border, for a shorter right tree, or the left one. At the shorter tree's
//! height, the border node absorbs the shorter root around the separator if
//! all of it fits in one node, as a merge would. Otherwise the shorter root
//! hangs beside it, under the separator, in the node one level up, which
//! splits up the border if full, as an insert's nodes do. Only roots may
//! hold fewer keys than a node's minimum (D22), so one of the two roots now
//! side by side may: it takes keys over from the other, which, too full to
//! share a node with it, can spare them. No merge happens, so no node above
//! loses a key. Whichever way, the separator's value is the head of the
//! right tree's first leaf, the leftmost leaf right of it (D17).
//!
//! Prefixes (05-fence-key.md): every node that changes is on the taller
//! tree's border or was a root, with prefix 0, or beside them. The split
//! halves and the sibling that gave keys over, beside the border, and the
//! two spines below the separator, whose nodes gain it as a fence, fit their
//! prefixes last, from the top down, reading keys, once the tree is whole.

#![forbid(unsafe_code)]

use core::mem;

use super::handle::{
    BalancingContext, ForceResult, Insertion, Internal, Leaf, Subtree, TrailingSlots,
};
use super::inline_keys::InlinedKey;
use super::page::{FreshPage, PageByKind};
use super::path::{FirstEdge, LastEdge, NodeEnd, PathMut};
use crate::inline_key::InlineKeyStrategy;

/// Where a join left its separator: in the node at `height` on the border
/// it joined, or, where keys moved over, in that node's child at
/// `child_edge`, one level down; at `index` there.
struct SeparatorPlace {
    height: usize,
    child_edge: Option<usize>,
    index: usize,
    /// Whether the shorter root hung beside the border rather than being
    /// absorbed into it: then nodes beside the border changed.
    hung: bool,
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Joins this tree, `separator` and `right`, whose keys ascend in that
    /// order and which both have entries, into this tree. Compares no keys;
    /// keys out of order make a tree whose searches give wrong answers, but
    /// that is memory-safe. Pages come from and go to the global allocator.
    ///
    /// `InlineKey` is called first, for the separator, before anything
    /// changes: a panic then drops the separator and leaves both trees as
    /// they were. It is called last again, to fit the prefixes, once the
    /// tree is whole: a panic then leaves it valid.
    pub(crate) fn join(&mut self, separator: (K, V), mut right: Self) {
        let (key, value) = separator;
        let separator = InlinedKey::new(key, 0);
        let mut node = right.borrow_mut();
        loop {
            node = match node.force() {
                ForceResult::Leaf(mut leaf) => {
                    let old_head = leaf.replace_head(value);
                    assert!(old_head.is_none(), "a first leaf has no head");
                    break;
                }
                ForceResult::Internal(internal) => internal.first_edge().descend(),
            };
        }
        if self.height() >= right.height() {
            self.join_at_border::<LastEdge>(separator, right);
        } else {
            let left = mem::replace(self, right);
            self.join_at_border::<FirstEdge>(separator, left);
        }
    }

    /// Joins `shorter`, a tree no taller than this one, beside this tree's
    /// `End` border, with `separator` between them: after this tree's last
    /// entry for [`LastEdge`], before its first for [`FirstEdge`]. The
    /// separator's value is already the head of the leftmost leaf right of
    /// it (D17).
    fn join_at_border<End: NodeEnd>(&mut self, separator: InlinedKey<K, S>, shorter: Self) {
        let place = {
            let mut path = PathMut::new(self);
            Self::descend_border::<End>(&mut path, shorter.height());
            if End::IS_LAST {
                Self::absorb_or_hang_right(&mut path, separator, shorter)
            } else {
                Self::absorb_or_hang_left(&mut path, separator, shorter)
            }
        };
        self.fit_separator_spines::<End>(&place);
        if place.hung {
            self.fit_border_siblings::<End>();
        }
    }

    /// Starts `path` at the root and takes it down `End`'s border to
    /// `height`.
    fn descend_border<End: NodeEnd>(path: &mut PathMut<'_, K, V, S>, height: usize) {
        path.start_at_root();
        while path.height() > height {
            let border = End::edge(&path.view());
            path.set_index(border);
            path.descend();
        }
    }

    /// At `path`'s node, on this tree's right border at `shorter`'s height:
    /// absorbs `shorter`'s root around `separator` if they fit in one node,
    /// or else hangs it right of the node, under the separator, one level up.
    fn absorb_or_hang_right(
        path: &mut PathMut<'_, K, V, S>,
        separator: InlinedKey<K, S>,
        shorter: Self,
    ) -> SeparatorPlace {
        let height = path.height();
        let index = path.view().len();
        let absorbed = match path.node_mut().force() {
            ForceResult::Leaf(mut node) => node.absorb(separator, shorter),
            ForceResult::Internal(mut node) => node.absorb(separator, shorter),
        };
        let (separator, shorter) = match absorbed {
            Ok(page) => {
                PageByKind::deallocate(page);
                return SeparatorPlace {
                    height,
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
        Self::descend_border::<LastEdge>(path, height + 1);
        let index = path.view().len() - 1;
        Self::stock_beside_separator(path, height + 1, index)
    }

    /// At `path`'s node, on this tree's left border at `shorter`'s height,
    /// below the root: puts `shorter`'s root in its place and absorbs it
    /// into that root around `separator` if they fit in one node, or else
    /// hangs it right of `shorter`'s root, under the separator, one level up.
    fn absorb_or_hang_left(
        path: &mut PathMut<'_, K, V, S>,
        separator: InlinedKey<K, S>,
        shorter: Self,
    ) -> SeparatorPlace {
        let height = path.height();
        let index = Subtree::borrow(&shorter).len();
        path.ascend();
        let displaced = path.internal_mut().edge_at(0).replace_child(shorter);
        path.descend();
        let absorbed = match path.node_mut().force() {
            ForceResult::Leaf(mut node) => node.absorb(separator, displaced),
            ForceResult::Internal(mut node) => node.absorb(separator, displaced),
        };
        let (separator, displaced) = match absorbed {
            Ok(page) => {
                PageByKind::deallocate(page);
                return SeparatorPlace {
                    height,
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
        Self::descend_border::<FirstEdge>(path, height + 1);
        Self::stock_beside_separator(path, height + 1, 0)
    }

    /// Hangs `right`, a tree at `path`'s node's height, right of that node,
    /// under `separator`, in the node one level up: under a new root if the
    /// node is the root; splitting up the path while full, each split's
    /// right half hung right of its left half the same way.
    fn hang_beside(path: &mut PathMut<'_, K, V, S>, separator: InlinedKey<K, S>, right: Self) {
        let mut carried = (separator, right);
        loop {
            let (separator, right) = carried;
            if path.height() == path.root_height() {
                path.push_root(FreshPage::allocate());
            } else {
                path.ascend();
            }
            let index = path.index();
            match path
                .internal_mut()
                .edge_at(index)
                .insert(separator, right, FreshPage::allocate)
            {
                Insertion::Fit(_) => return,
                Insertion::Split(split) => carried = (split.separator, split.right.forget_type()),
            }
        }
    }

    /// Brings whichever of the two children beside the separator, at
    /// `index` in `path`'s node at `height`, holds fewer keys than its
    /// minimum, a root before, up to it, taking keys over from the other;
    /// returns where the separator is then. The two didn't fit in one node,
    /// so at most one is short, and the other can spare what it needs.
    fn stock_beside_separator(
        path: &mut PathMut<'_, K, V, S>,
        height: usize,
        index: usize,
    ) -> SeparatorPlace {
        let kv = path.internal_mut().kv_at(index);
        if height == 1 {
            Self::stock_children::<Leaf>(kv.into_balancing_context(), height, index)
        } else {
            Self::stock_children::<Internal>(kv.into_balancing_context(), height, index)
        }
    }

    /// [`Self::stock_beside_separator`], for children of kind `ChildKind`.
    fn stock_children<ChildKind: TrailingSlots>(
        mut context: BalancingContext<'_, K, V, S, ChildKind>,
        height: usize,
        index: usize,
    ) -> SeparatorPlace {
        let min_len = context.child_min_len();
        if context.right_child_len() < min_len {
            let steals = min_len - context.right_child_len();
            for _ in 0..steals {
                context
                    .steal_left()
                    .expect("a child too full to share a node can spare keys");
            }
            // Each move puts the parent's separator at the right child's
            // front: the joined one, moved first, ends after the others.
            return SeparatorPlace {
                height: height - 1,
                child_edge: Some(index + 1),
                index: steals - 1,
                hung: true,
            };
        }
        if context.left_child_len() < min_len {
            let left_len = context.left_child_len();
            for _ in 0..min_len - left_len {
                context
                    .steal_right()
                    .expect("a child too full to share a node can spare keys");
            }
            // The joined separator goes to the end of the left child first.
            return SeparatorPlace {
                height: height - 1,
                child_edge: Some(index),
                index: left_len,
                hung: true,
            };
        }
        SeparatorPlace {
            height,
            child_edge: None,
            index,
            hung: true,
        }
    }

    /// Fits the prefixes of the two spines below the separator at `place`,
    /// whose fences the separator joined (05-fence-key.md): the right border
    /// of the child left of it and the left border of the child right of
    /// it, from the leaves up, and then those two children: an internal
    /// node's prefix grows no further than its children's.
    fn fit_separator_spines<End: NodeEnd>(&mut self, place: &SeparatorPlace) {
        if place.height == 0 {
            return;
        }
        let mut path = PathMut::new(self);
        let node_height = place.height + usize::from(place.child_edge.is_some());
        Self::descend_border::<End>(&mut path, node_height);
        if let Some(edge) = place.child_edge {
            path.set_index(edge);
            path.descend();
        }
        let index = place.index;
        Self::fit_spine_below::<LastEdge>(&mut path, index);
        Self::fit_spine_below::<FirstEdge>(&mut path, index + 1);
        path.fit_children_to_fences([index, index + 1]);
    }

    /// Fits the nodes below the child at `edge` of `path`'s node down that
    /// child's `End` border, from the leaf up; the path ends at the node it
    /// started at.
    fn fit_spine_below<End: NodeEnd>(path: &mut PathMut<'_, K, V, S>, edge: usize) {
        let top = path.height();
        path.set_index(edge);
        path.descend();
        while path.height() > 0 {
            let border = End::edge(&path.view());
            path.set_index(border);
            path.descend();
        }
        path.ascend();
        while path.height() < top {
            let child = path.index();
            path.fit_children_to_fences([child]);
            path.ascend();
        }
    }
}
