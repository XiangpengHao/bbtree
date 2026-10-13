//! Cutting a tree in two at a key: [`Subtree::split_off_at`], for the map's
//! `split_off` and `extract_if`.
//!
//! A search finds the leaf edge just before the key, where the cut goes; for
//! a key that is a separator, that is the end of the rightmost leaf left of
//! it. Up the path from there, each node is cut at the path's edge
//! (`NodeMut::cut_off_at`): it keeps what lies left of the path, and what
//! lies right of it goes into a new node, after the part cut off from the
//! node below as its first child. The new nodes make a tree as tall as the
//! old one, with the keys from the key on.
//!
//! The nodes on the path become the borders of the two trees, where every
//! prefix is 0 (05-fence-key.md): before the cut, their prefixes go back to
//! 0, from the root down, reading no key.
//!
//! Along the cut, nodes on both sides may hold fewer keys than their
//! minimum, down to none. Fixing the left tree's right border and the right
//! tree's left border, from the top down, brings them back
//! ([`Subtree::fix_border`]). A separator left at the start of the right
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
//! from the top down: [`Subtree::fix_border`]. A tree cut in two needs it
//! along both sides of the cut, and a tree built from the left (`build.rs`)
//! along its right edge: there, nodes may hold any number of keys, down to
//! none, where every other node holds its minimum.
//!
//! It reads no key: the nodes beside the border that give it entries keep
//! their prefixes, which still fit their narrower fences, until the caller
//! fits them, once its operation is done ([`Subtree::fit_border_siblings`]).

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::node::handle::{
    BalancingContext, ForceResult, Immut, Internal, Leaf, LeafOrInternal, NodeMut, NodeRef,
    Subtree, TrailingSlots,
};
use super::node::inline_keys::SearchKey;
use super::node::page::{FreshPage, PageByKind};
use super::path::{FirstEdge, KeySide, LastEdge, NodeEnd, PathMut};
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
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
        let mut upper = path.cut();
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
    /// node below. Returns the tree of the new nodes, as tall as this one,
    /// on pages from the global allocator. Nodes on either side of the cut
    /// may be left with fewer keys than their minimum, down to none. Compares
    /// no keys.
    ///
    /// # Panics
    ///
    /// If the path ends at no leaf edge.
    fn cut(mut self) -> Subtree<K, V, S> {
        let index = self.index();
        let mut right = self
            .leaf_mut()
            .cut_off_at(index, FreshPage::allocate())
            .forget_type();
        while self.height() < self.root_height() {
            self.ascend();
            let index = self.index();
            right = self
                .internal_mut()
                .cut_off_at(index, right, FreshPage::allocate())
                .forget_type();
        }
        right
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeRef<Immut<'_>, K, V, S, LeafOrInternal> {
    /// The number of entries in the subtree under this node, leaf entries
    /// and separators, counted a node at a time.
    pub(crate) fn count_entries(self) -> usize {
        match self.force() {
            ForceResult::Leaf(leaf) => leaf.len(),
            ForceResult::Internal(internal) => {
                let below: usize = (0..=internal.len())
                    .map(|index| internal.edge_at(index).descend().count_entries())
                    .sum();
                internal.len() + below
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Makes every node along the tree's `End` edge, its last children for
    /// [`LastEdge`] or its first for [`FirstEdge`], hold at least
    /// its minimum, by merging it with its sibling or moving keys over from
    /// it, from the root down; and gives way to the only child of a root left
    /// without keys, until the root has some or is a leaf. Every other node
    /// must hold its minimum already. Merged pages go to `release`.
    ///
    /// Going down, each node on the edge is brought to one key more than its
    /// minimum, so that a merge one level down, which takes one of its keys,
    /// leaves it at its minimum still.
    pub(crate) fn fix_border<End: NodeEnd>(
        &mut self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) {
        self.pop_empty_roots(release);
        if let ForceResult::Internal(mut node) = self.borrow_mut().force() {
            loop {
                let leaf_children = node.has_leaf_children();
                let separator = if End::IS_LAST {
                    node.last_kv()
                } else {
                    node.first_kv()
                }
                .expect("a node on the border holds a key once stocked");
                if leaf_children {
                    separator
                        .into_balancing_context::<Leaf>()
                        .stock_border_child::<End>(release);
                    break;
                }
                node = separator
                    .into_balancing_context::<Internal>()
                    .stock_border_child::<End>(release);
            }
        }
        self.pop_empty_roots(release);
    }

    /// Gives way to the only child of a root without keys, until the root
    /// has some or is a leaf, handing each old root's page to `release`.
    fn pop_empty_roots(&mut self, release: &mut impl FnMut(PageByKind<K, V, S>)) {
        while self.height() > 0 && Subtree::borrow(self).len() == 0 {
            release(self.pop_internal_level().into());
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, ChildKind: TrailingSlots>
    BalancingContext<'a, K, V, S, ChildKind>
{
    /// Brings the child on the `End` side, the right one for
    /// [`LastEdge`], to at least one key more than its minimum: merges
    /// the two children if they fit in one node with their separator, or
    /// else moves keys over from the other child, which, too full to merge
    /// and holding its minimum, can spare them. Returns the child on the
    /// `End` side, merged or not.
    fn stock_border_child<End: NodeEnd>(
        self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) -> NodeMut<'a, K, V, S, ChildKind> {
        let target = self.child_min_len() + 1;
        let border_len = |context: &Self| {
            if End::IS_LAST {
                context.right_child_len()
            } else {
                context.left_child_len()
            }
        };
        if border_len(&self) < target {
            match self.can_merge() {
                Ok(mergeable) => {
                    let (edge, page) = mergeable.merge();
                    release(page);
                    return edge.descend_as::<ChildKind>();
                }
                Err(mut context) => {
                    while border_len(&context) < target {
                        if End::IS_LAST {
                            context.steal_left()
                        } else {
                            context.steal_right()
                        }
                        .expect("a sibling too full to merge with can spare a key");
                    }
                    return context.into_border_child::<End>();
                }
            }
        }
        self.into_border_child::<End>()
    }

    /// The child on the `End` side.
    fn into_border_child<End: NodeEnd>(self) -> NodeMut<'a, K, V, S, ChildKind> {
        if End::IS_LAST {
            self.into_right_child()
        } else {
            self.into_left_child()
        }
    }
}
