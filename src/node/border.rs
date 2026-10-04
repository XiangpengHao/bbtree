//! Bringing the nodes along one edge of a tree up to their minimum (D22),
//! from the top down: [`Subtree::fix_border`]. A tree cut in two
//! (`map/split_off.rs`) needs it along both sides of the cut, and a tree
//! built from the left (`build.rs`) along its right edge: there, nodes may
//! hold any number of keys, down to none, where every other node holds its
//! minimum.
//!
//! It reads no key: the nodes beside the border that give it entries keep
//! their prefixes, which still fit their narrower fences, until the caller
//! fits them, once its operation is done ([`Subtree::fit_border_siblings`]).

#![forbid(unsafe_code)]

use super::handle::{
    BalancingContext, ForceResult, Internal, Leaf, NodeMut, Subtree, TrailingSlots,
};
use super::page::PageByKind;
use super::path::NodeEnd;
#[cfg(doc)]
use super::path::{FirstEdge, LastEdge};
use crate::inline_key::InlineKeyStrategy;

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
