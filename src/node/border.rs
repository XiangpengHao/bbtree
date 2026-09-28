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

use super::handle::{BalancingContext, ForceResult, LeafOrInternal, NodeMut, Subtree};
use super::page::PageByKind;
use super::path::NodeEnd;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Makes every node along the tree's `end` edge, its last children for
    /// [`NodeEnd::Last`] or its first for [`NodeEnd::First`], hold at least
    /// its minimum, by merging it with its sibling or moving keys over from
    /// it, from the root down; and gives way to the only child of a root left
    /// without keys, until the root has some or is a leaf. Every other node
    /// must hold its minimum already. Merged pages go to `release`.
    ///
    /// Going down, each node on the edge is brought to one key more than its
    /// minimum, so that a merge one level down, which takes one of its keys,
    /// leaves it at its minimum still.
    pub(crate) fn fix_border(
        &mut self,
        end: NodeEnd,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) {
        self.pop_empty_roots(release);
        let mut node = self.borrow_mut();
        while let ForceResult::Internal(internal) = node.force() {
            let min_len = match end {
                NodeEnd::First => internal.reborrow().first_edge().descend().min_len(),
                NodeEnd::Last => internal.reborrow().last_edge().descend().min_len(),
            };
            let separator = match end {
                NodeEnd::First => internal.first_kv(),
                NodeEnd::Last => internal.last_kv(),
            }
            .expect("a node on the border holds a key once stocked");
            node = separator
                .into_balancing_context()
                .stock_border_child(end, min_len + 1, release);
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

impl<'a, K, V, S: InlineKeyStrategy<K>> BalancingContext<'a, K, V, S> {
    /// Brings the child on the `end` side, the right one for
    /// [`NodeEnd::Last`], to at least `target` keys: merges the two children
    /// if they fit in one node with their separator, or else moves keys over
    /// from the other child, which, too full to merge, can spare them, as
    /// long as `target` is at most one more than the minimum and the other
    /// child holds its minimum. Returns the child on the `end` side, merged
    /// or not.
    fn stock_border_child(
        self,
        end: NodeEnd,
        target: usize,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) -> NodeMut<'a, K, V, S, LeafOrInternal> {
        let border_len = |context: &Self| match end {
            NodeEnd::First => context.left_child_len(),
            NodeEnd::Last => context.right_child_len(),
        };
        if border_len(&self) < target {
            match self.can_merge() {
                Ok(mergeable) => {
                    let (edge, page) = mergeable.merge();
                    release(page);
                    return edge.descend();
                }
                Err(mut context) => {
                    while border_len(&context) < target {
                        match end {
                            NodeEnd::First => context.steal_right(),
                            NodeEnd::Last => context.steal_left(),
                        }
                        .expect("a sibling too full to merge with can spare a key");
                    }
                    return context.into_border_child(end);
                }
            }
        }
        self.into_border_child(end)
    }

    /// The child on the `end` side.
    fn into_border_child(self, end: NodeEnd) -> NodeMut<'a, K, V, S, LeafOrInternal> {
        match end {
            NodeEnd::First => self.into_left_child(),
            NodeEnd::Last => self.into_right_child(),
        }
    }
}
