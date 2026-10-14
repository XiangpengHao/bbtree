//! Cutting a tree in two at a key: [`Root::split_off_at`], for the map's
//! `split_off` and `extract_if`.
//!
//! A search finds the leaf edge just before the key, where the cut goes; for
//! a key that is a separator, that is the end of the rightmost leaf left of
//! it. Up the path from there, each node is cut at the path's edge
//! (`Leaf::cut_off_at`, `Internal::cut_off_at`): it keeps what lies left of
//! the path, and what
//! lies right of it goes into a new node, after the part cut off from the
//! node below as its first child. The new nodes make a tree as tall as the
//! old one, with the keys from the key on.
//!
//! The nodes on the path become the borders of the two trees, whose prefix
//! is what every key of the tree shares (D50): before the cut, their
//! prefixes go back to the root's, from the root down, reading no key. Both
//! trees keep what the tree's keys shared; their keys share at least that.
//!
//! Along the cut, nodes on both sides may hold fewer keys than their
//! minimum, down to none. Fixing the left tree's right border and the right
//! tree's left border, from the top down, brings them back
//! ([`Root::fix_border`]). A separator left at the start of the right
//! tree, with nothing left of it, comes down along that border, by a merge
//! or a move, into the tree's first leaf, with its value, the head of the
//! leaf right of it (D17). Once both maps are counted, the nodes beside the
//! borders, which entries moved over from, fit their prefixes to their new
//! fences, reading keys.
//!
//! That touches the nodes along the path and the two borders: O(log n) of
//! them. The two maps' lengths come from counting the entries of the shorter
//! tree, a node at a time, as std's do.
//!
//! # Borders
//!
//! Bringing the nodes along one edge of a tree up to their minimum (D22),
//! from the top down: [`Root::fix_border`]. A tree cut in two needs it
//! along both sides of the cut, and a tree built from the left (`build.rs`)
//! along its right edge: there, nodes may hold any number of keys, down to
//! none, where every other node holds its minimum.
//!
//! It reads no key: the nodes beside the border that give it entries keep
//! their prefixes, which still fit their narrower fences, until the caller
//! fits them, once its operation is done ([`Root::fit_border_siblings`]).

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::node::inline_keys::SearchKey;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, PageByKind};
use super::node::siblings::Siblings;
use super::node::{Child, Node};
use super::path::{FirstEdge, KeySide, LastEdge, NodeEnd, PathMut};
use super::root::Root;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Root<K, V, S> {
    /// Cuts the tree at the leaf edge on `side` of `key`: keeps what lies
    /// before it, and returns what lies after, both with their borders along
    /// the cut brought up to their minimum ([`Self::fix_border`]). The nodes
    /// beside those borders, which gave them entries, keep prefixes that
    /// still fit their narrower fences: fitting them with
    /// `fit_border_siblings`, which reads keys, is the caller's business,
    /// once both trees are where a panic can't lose them.
    ///
    /// Every `Ord` call happens first, in the search, before anything
    /// changes.
    pub(crate) fn split_off_at<Q>(&mut self, key: &Q, side: KeySide) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut path = PathMut::new(self);
        path.search_to_edge(&SearchKey::new(key), side);
        path.clear_prefixes();
        let upper = path.cut();
        // As tall as the tree; every key of both parts shares what the
        // tree's keys did (D50).
        let mut upper = Root::from_node(upper, self.shared_prefix().clone());
        let release = &mut PageByKind::deallocate;
        self.fix_border::<LastEdge>(release);
        upper.fix_border::<FirstEdge>(release);
        upper
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PathMut<'_, K, V, S> {
    /// Cuts the tree along the path, which ends at a leaf edge: each node on
    /// it, from the leaf up, keeps what lies left of the path, and what lies
    /// right of it goes into a new node, after the part cut off from the
    /// node below. Returns the top of the new nodes, as tall as the root,
    /// on plain pages from the global allocator, which nothing owns until
    /// the caller makes it a tree's root. Nodes on either side of the cut may be
    /// left with fewer keys than their minimum, down to none. Compares no
    /// keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no leaf edge.
    fn cut(mut self) -> Node<K, V, S> {
        let index = self.index();
        let mut right = Node::Leaf(self.leaf().cut_off_at(index, FreshPage::allocate()));
        while !self.is_at_root() {
            let node = self.ascend();
            let index = self.index();
            right = Node::Internal(node.cut_off_at(index, right, FreshPage::allocate()));
        }
        right
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// The number of entries under this node, leaf entries and separators,
    /// counted a node at a time.
    pub(crate) fn count_entries(self) -> usize {
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

impl<K, V, S: InlineKeyStrategy<K>> Root<K, V, S> {
    /// Makes every node along the tree's `End` edge, its last children for
    /// [`LastEdge`] or its first for [`FirstEdge`], hold at least
    /// its minimum, by merging it with its sibling or moving keys over from
    /// it, from the root down; and lowers the tree while its root is left
    /// without keys, until the root has some or is a leaf. Every other node
    /// must hold its minimum already. Emptied pages go to `release`.
    ///
    /// Going down, each node on the edge is brought to one key more than its
    /// minimum, so that a merge one level down, which takes one of its keys,
    /// leaves it at its minimum still.
    pub(crate) fn fix_border<End: NodeEnd>(
        &mut self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) {
        self.pop_empty_roots(release);
        if let Node::Internal(mut node) = self.top_mut() {
            loop {
                let len = node.len();
                assert!(len > 0, "a node on the border holds a key once stocked");
                let separator = if End::IS_LAST { len - 1 } else { 0 };
                if node.has_leaf_children() {
                    node.siblings::<Leaf<K, V, S>>(separator)
                        .stock_border_child::<End>(release);
                    break;
                }
                node = node
                    .siblings::<Internal<K, V, S>>(separator)
                    .stock_border_child::<End>(release);
            }
        }
        self.pop_empty_roots(release);
    }

    /// Lowers the tree while its root is internal without keys, until the
    /// root has some or is a leaf ([`Root::lower`]), handing each page
    /// emptied to `release`.
    fn pop_empty_roots(&mut self, release: &mut impl FnMut(PageByKind<K, V, S>)) {
        while matches!(self.top(), Node::Internal(root) if root.len() == 0) {
            release(self.lower());
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, C: Child<K, V, S>> Siblings<K, V, S, C> {
    /// Brings the child on the `End` side, the right one for
    /// [`LastEdge`], to at least one key more than its minimum: merges
    /// the two children if they fit in one node with their separator, or
    /// else moves keys over from the other child, which, too full to merge
    /// and holding its minimum, can spare them. Returns the child on the
    /// `End` side, merged or not.
    fn stock_border_child<End: NodeEnd>(
        mut self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) -> C {
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
                release(self.merge());
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
