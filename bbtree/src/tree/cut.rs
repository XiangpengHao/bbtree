//! Cutting a tree in two at a key: [`Tree::split_off_at`], for the map's
//! `split_off`.
//!
//! A search finds the leaf edge on the given side of the key ([`KeySide`]),
//! where the cut goes. For a key that is a separator, the edge before it is
//! the end of the rightmost leaf left of it. Up the node stack from there,
//! each node is cut at the node stack's edge (`Leaf::cut_off_at`,
//! `Internal::cut_off_at`). It keeps what lies left of the node stack; what
//! lies right goes into a new node, after the part cut off the node below,
//! as its first child. The new nodes make a tree as tall as the old one,
//! with the keys past the cut.
//!
//! The nodes on the node stack become the borders of the two trees, whose
//! prefix is what every key of the tree shares (D50). So before the cut,
//! their prefixes go back to the root's, from the root down, reading no key.
//! Both trees keep what the old tree's keys shared; their keys share at
//! least that.
//!
//! Along the cut, nodes on both sides may hold fewer keys than their
//! minimum, down to none. [`Tree::fix_border`] brings them back, from the
//! top down, along the left tree's right border and the right tree's left
//! border. A separator left at the start of the right tree, with nothing
//! left of it, comes down that border into the tree's first leaf, by a merge
//! or a move. It brings its value, which was its head (D17).
//!
//! The nodes beside the borders, which entries moved over from, then fit
//! their prefixes to their new fences, reading keys. They do so only once
//! both trees are counted and where a panic can't lose them. So the cut
//! hands out the right tree only through [`Cut::finish`], which puts it
//! there and fits both.
//!
//! The cut touches the nodes along the node stack and the two borders:
//! O(log n) of them. The two maps' lengths come from counting the entries of
//! the shorter tree, a node at a time, as std's do ([`Cut::lengths`]).
//!
//! # Borders
//!
//! [`Tree::fix_border`] brings the nodes along one edge of a tree up to
//! their minimum (D22), from the top down. A tree cut in two needs it along
//! both sides of the cut. A tree built from the left (`build.rs`) needs it
//! along its right edge. There, nodes may hold any number of keys, down to
//! none, while every other node holds its minimum.
//!
//! It reads no key. The nodes beside the border that give it entries keep
//! their prefixes, which still fit their narrower fences, until the
//! operation fits them once it is done ([`Tree::refit_border_siblings`]).

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::node::inline_keys::SearchKey;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::FreshPage;
use super::node::siblings::Siblings;
use super::node::{Child, Node};
use super::node_stack::{FirstEdge, KeySide, LastEdge, NodeEnd, NodeStackMut};
use super::root::Tree;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Cuts the tree at the leaf edge on `side` of `key`. Keeps what lies
    /// before the cut, and returns what lies after it in a [`Cut`]. Both
    /// trees have their borders along the cut brought up to their minimum
    /// ([`Self::fix_border`]). The `Cut` hands out the part after once it
    /// fits the nodes beside those borders.
    ///
    /// Every `Ord` call happens first, in the search, before anything
    /// changes. Pages come from and go back to `alloc`, the map's allocator,
    /// which owns both trees' pages.
    pub(crate) fn split_off_at<Q>(
        &mut self,
        key: &Q,
        side: KeySide,
        alloc: &impl Allocator,
    ) -> Cut<K, V, S>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut stack = NodeStackMut::new(self);
        stack.search_to_edge(&SearchKey::new(key), side);
        let upper = stack.cut(alloc);
        // As tall as the tree; every key of both parts shares what the
        // tree's keys did (D50).
        let mut upper = Tree::from_node(upper, self.shared_prefix().clone(), alloc);
        self.fix_border::<LastEdge>(alloc);
        upper.fix_border::<FirstEdge>(alloc);
        Cut { upper }
    }
}

/// The part after a cut ([`Tree::split_off_at`]). It comes out only once
/// the nodes beside both borders along the cut, which gave the borders
/// entries, fit their prefixes to their new fences. Until then those nodes
/// keep prefixes that still fit, which searches allow, but shorter than
/// what their fences share.
#[must_use = "the part after the cut comes out only through `finish`"]
pub(crate) struct Cut<K, V, S> {
    upper: Tree<K, V, S>,
}

impl<K, V, S: InlineKeyStrategy<K>> Cut<K, V, S> {
    /// The number of entries before the cut, in `lower`, and after it, of
    /// `length` in all: counts the entries of the shorter tree, a node at a
    /// time, as std's map does.
    pub(crate) fn lengths(&self, lower: &Tree<K, V, S>, length: usize) -> (usize, usize) {
        if lower.leaf_depth() < self.upper.leaf_depth() {
            let lower_length = lower.root().count_entries();
            (lower_length, length - lower_length)
        } else {
            let upper_length = self.upper.root().count_entries();
            (length - upper_length, upper_length)
        }
    }

    /// Puts the part after the cut into `upper`, a tree without a root page.
    /// Then fits the prefixes of the nodes beside the borders along the cut,
    /// in `lower` (the part before it) and in `upper`, reading keys. Both
    /// trees should be counted and where a panic can't lose them.
    pub(crate) fn finish(self, lower: &mut Tree<K, V, S>, upper: &mut Tree<K, V, S>) {
        debug_assert!(!upper.has_root(), "the part after a cut replaced a tree");
        *upper = self.upper;
        lower.refit_border_siblings::<LastEdge>();
        upper.refit_border_siblings::<FirstEdge>();
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeStackMut<'_, K, V, S> {
    /// Cuts the tree along the node stack, which ends at a leaf edge. Each
    /// node on it, from the leaf up, keeps what lies left of the node stack;
    /// what lies right goes into a new node, after the part cut off the node
    /// below. Returns the top of the new nodes, as tall as the root, on plain
    /// pages from `alloc`; nothing owns them until the caller makes the top
    /// a tree's root. Nodes on either side of the cut may be left with fewer
    /// keys than their minimum, down to none. Compares no keys.
    ///
    /// The nodes on the node stack become the two trees' borders, so first
    /// their prefixes move back to what every key shares (D50).
    ///
    /// # Panics
    ///
    /// If the node stack ends at no leaf edge.
    fn cut(mut self, alloc: &impl Allocator) -> Node<K, V, S> {
        self.move_prefixes_back_to_root();
        let index = self.index();
        let mut right = Node::Leaf(self.leaf().cut_off_at(index, FreshPage::allocate(alloc)));
        while !self.is_at_root() {
            let node = self.ascend();
            let index = self.index();
            right = Node::Internal(node.cut_off_at(index, right, FreshPage::allocate(alloc)));
        }
        right
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// The number of entries under this node, leaf entries and separators,
    /// counted a node at a time.
    fn count_entries(self) -> usize {
        match self {
            Node::Leaf(leaf) => leaf.len(),
            Node::Internal(internal) => {
                let below: usize = (0..=internal.len())
                    .map(|index| internal.child(index).count_entries())
                    .sum();
                internal.len() + below
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Makes every node along the tree's `End` edge (its last children for
    /// [`LastEdge`], its first for [`FirstEdge`]) hold at least its minimum,
    /// from the root down. Each node merges with its sibling or takes keys
    /// over from it. The tree lowers while its root is left without keys,
    /// until the root has some or is a leaf. Every other node must hold its
    /// minimum already. Emptied pages go back to `alloc`.
    ///
    /// Going down, each node on the edge is brought to one key more than its
    /// minimum, so that a merge one level down, which takes one of its keys,
    /// still leaves it at its minimum.
    pub(in crate::tree) fn fix_border<End: NodeEnd>(&mut self, alloc: &impl Allocator) {
        self.lower_while_root_empty(alloc);
        if let Node::Internal(mut node) = self.root_mut() {
            loop {
                let len = node.len();
                assert!(len > 0, "a node on the border holds a key once stocked");
                let separator = if End::IS_LAST { len - 1 } else { 0 };
                if node.has_leaf_children() {
                    node.siblings::<Leaf<K, V, S>>(separator)
                        .stock_border_child::<End>(alloc);
                    break;
                }
                node = node
                    .siblings::<Internal<K, V, S>>(separator)
                    .stock_border_child::<End>(alloc);
            }
        }
        self.lower_while_root_empty(alloc);
    }

    /// Lowers the tree while its root is internal without keys, until the
    /// root has some or is a leaf ([`Tree::lower`]), giving each page
    /// emptied back to `alloc`.
    pub(super) fn lower_while_root_empty(&mut self, alloc: &impl Allocator) {
        while matches!(self.root(), Node::Internal(root) if root.len() == 0) {
            self.lower().deallocate(alloc);
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, C: Child<K, V, S>> Siblings<K, V, S, C> {
    /// Brings the child on the `End` side (the right one for [`LastEdge`])
    /// to at least one key more than its minimum. Merges the two children if
    /// they fit in one node with their separator. Otherwise it moves keys
    /// over from the other child, which can spare them: it is too full to
    /// merge and holds its minimum. Returns the child on the `End` side,
    /// merged or not.
    fn stock_border_child<End: NodeEnd>(mut self, alloc: &impl Allocator) -> C {
        let target = C::min_len() + 1;
        let border_len = |siblings: &Self| {
            if End::IS_LAST {
                siblings.right_len()
            } else {
                siblings.left_len()
            }
        };
        if border_len(&self) < target {
            if self.can_merge() {
                let merged = self.left();
                self.merge().deallocate(alloc);
                return merged;
            }
            while border_len(&self) < target {
                if End::IS_LAST {
                    self.steal_left()
                } else {
                    self.steal_right()
                }
                .expect("a sibling too full to merge with can spare a key");
            }
        }
        self.border_child::<End>()
    }

    /// The child on the `End` side.
    fn border_child<End: NodeEnd>(&self) -> C {
        if End::IS_LAST {
            self.right()
        } else {
            self.left()
        }
    }
}
