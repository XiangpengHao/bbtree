//! Removing from a tree: a recursive descent whose call stack is the path
//! back up (D11), as for inserts (`insert.rs`). Each level searches its node
//! on the way down, and the entry comes out of a leaf, or out of an internal
//! node as a separator. On the way back up, each level fixes the child it
//! descended into if the removal left it underfull, by merging it with a
//! sibling or moving one entry over from it (D22); a merge takes a key out of
//! the level itself, for its parent to check in turn. A root left without
//! keys gives way to its only child.
//!
//! A separator's value is the head of the leftmost leaf right of it (D17).
//! Removing the separator takes that head out, and the leaf's first key goes
//! up to take the separator's place, with its value as the new head: values
//! never leave their leaf.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::position::Position;
use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{
    Edge, ForceResult, Internal, Kv, Leaf, LeafOrInternal, NodeMut, SearchResult, Subtree,
};
use crate::node::inline_keys::{InlinedKey, SearchKey};
use crate::node::page::PageByKind;

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Removes the entry whose key equals `key` and returns it, with the key
    /// as stored, or `None` if there is none.
    ///
    /// Every `Ord` call happens on the way down, before anything changes, so
    /// a panicking `Ord` leaves the tree unchanged. So does a panicking
    /// `InlineKey`: the key's inline key is computed first. Every page a
    /// merge frees, and the root's if a merge empties it, goes to `release`.
    pub(crate) fn remove_entry<Q>(
        &mut self,
        key: &Q,
        release: impl FnMut(PageByKind<K, V, S>),
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.remove(&SearchKey::new(key), release)
    }

    /// Removes the entry at `position`, which a search found, and returns
    /// it, comparing no keys; frees pages as [`Self::remove_entry`] does.
    pub(super) fn remove_at(
        &mut self,
        position: &Position,
        release: impl FnMut(PageByKind<K, V, S>),
    ) -> (K, V) {
        self.remove(position, release)
            .expect("a position found in the tree holds an entry")
    }

    /// Removes the entry with the smallest key and returns it, or `None` if
    /// the tree is empty. Calls no `Ord`; frees pages as
    /// [`Self::remove_entry`] does.
    pub(crate) fn pop_first(&mut self, release: impl FnMut(PageByKind<K, V, S>)) -> Option<(K, V)> {
        self.remove(&TreeEnd::First, release)
    }

    /// Removes the entry with the largest key and returns it, or `None` if
    /// the tree is empty. Calls no `Ord`; frees pages as
    /// [`Self::remove_entry`] does.
    pub(crate) fn pop_last(&mut self, release: impl FnMut(PageByKind<K, V, S>)) -> Option<(K, V)> {
        self.remove(&TreeEnd::Last, release)
    }

    fn remove(
        &mut self,
        target: &impl RemovalTarget<K, S>,
        mut release: impl FnMut(PageByKind<K, V, S>),
    ) -> Option<(K, V)> {
        let removed = self.borrow_mut().remove_below(target, &mut release)?;
        if self.height() > 0 && Self::borrow(self).len() == 0 {
            release(self.pop_internal_level().into());
        }
        Some(removed)
    }
}

/// Which entry a removal takes out of a tree: how to find it in each node on
/// the way down.
trait RemovalTarget<K, S> {
    /// The target's key/value in `leaf`, if it is there.
    fn find_in_leaf<'a, V>(
        &self,
        leaf: NodeMut<'a, K, V, S, Leaf>,
    ) -> Option<Kv<NodeMut<'a, K, V, S, Leaf>>>;

    /// The target's separator in `node`, or the edge to the child that would
    /// hold the target.
    fn find_in_internal<'a, V>(
        &self,
        node: NodeMut<'a, K, V, S, Internal>,
    ) -> SearchResult<NodeMut<'a, K, V, S, Internal>>;
}

/// The entry whose key equals the searched one, if there is one.
impl<K, S, Q> RemovalTarget<K, S> for SearchKey<'_, K, S, Q>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    Q: Ord + ?Sized,
{
    fn find_in_leaf<'a, V>(
        &self,
        leaf: NodeMut<'a, K, V, S, Leaf>,
    ) -> Option<Kv<NodeMut<'a, K, V, S, Leaf>>> {
        match leaf.search(self) {
            SearchResult::Found(kv) => Some(kv),
            SearchResult::GoDown(_) => None,
        }
    }

    fn find_in_internal<'a, V>(
        &self,
        node: NodeMut<'a, K, V, S, Internal>,
    ) -> SearchResult<NodeMut<'a, K, V, S, Internal>> {
        node.search(self)
    }
}

/// The entry at a position a search found, by the position's indexes,
/// comparing no keys.
impl<K, S: InlineKeyStrategy<K>> RemovalTarget<K, S> for Position {
    fn find_in_leaf<'a, V>(
        &self,
        leaf: NodeMut<'a, K, V, S, Leaf>,
    ) -> Option<Kv<NodeMut<'a, K, V, S, Leaf>>> {
        Some(leaf.kv_at(self.index_at(0)))
    }

    fn find_in_internal<'a, V>(
        &self,
        node: NodeMut<'a, K, V, S, Internal>,
    ) -> SearchResult<NodeMut<'a, K, V, S, Internal>> {
        if node.height() == self.height() {
            SearchResult::Found(node.kv_at(self.index_at(self.height())))
        } else {
            SearchResult::GoDown(self.edge_in(node))
        }
    }
}

/// An end of the tree, found without comparing keys.
enum TreeEnd {
    /// The entry with the smallest key: the first of the leftmost leaf,
    /// which has no head (D17).
    First,
    /// The entry with the largest key: the last of the rightmost leaf.
    Last,
}

impl<K, S: InlineKeyStrategy<K>> RemovalTarget<K, S> for TreeEnd {
    fn find_in_leaf<'a, V>(
        &self,
        leaf: NodeMut<'a, K, V, S, Leaf>,
    ) -> Option<Kv<NodeMut<'a, K, V, S, Leaf>>> {
        match self {
            Self::First => leaf.first_kv(),
            Self::Last => leaf.last_kv(),
        }
    }

    fn find_in_internal<'a, V>(
        &self,
        node: NodeMut<'a, K, V, S, Internal>,
    ) -> SearchResult<NodeMut<'a, K, V, S, Internal>> {
        match self {
            Self::First => SearchResult::GoDown(node.first_edge()),
            Self::Last => SearchResult::GoDown(node.last_edge()),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeMut<'_, K, V, S, LeafOrInternal> {
    /// Removes the target from the subtree under this node and returns it,
    /// fixing every underfull node below this one on the way back up; this
    /// node itself may be left underfull, for its parent to fix.
    fn remove_below(
        self,
        target: &impl RemovalTarget<K, S>,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) -> Option<(K, V)> {
        match self.force() {
            ForceResult::Leaf(leaf) => target.find_in_leaf(leaf).map(|kv| kv.remove().0),
            ForceResult::Internal(node) => match target.find_in_internal(node) {
                SearchResult::Found(separator) => Some(separator.remove_separator(release)),
                SearchResult::GoDown(mut edge) => {
                    let removed = edge
                        .reborrow_mut()
                        .descend()
                        .remove_below(target, release)?;
                    edge.fix_underfull_child(release);
                    Some(removed)
                }
            },
        }
    }

    /// Takes the head out of the leftmost leaf under this node, along with
    /// that leaf's first key, whose value becomes the head, fixing every
    /// underfull node below this one on the way back up. Returns the head
    /// and the key, with its inline key.
    ///
    /// # Panics
    ///
    /// If the leftmost leaf has no head or no keys, which never happens below
    /// a separator in a tree built by the node operations: only the tree's
    /// leftmost leaf has no head, and only a root has no keys (D17, D22).
    fn remove_leftmost_head(
        self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) -> (V, InlinedKey<K, S>) {
        match self.force() {
            ForceResult::Leaf(leaf) => leaf
                .head()
                .and_then(|head| head.remove_with_first_key())
                .expect("the leaf right of a separator has a head and keys"),
            ForceResult::Internal(node) => {
                let mut edge = node.first_edge();
                let removed = edge.reborrow_mut().descend().remove_leftmost_head(release);
                edge.fix_underfull_child(release);
                removed
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Kv<NodeMut<'_, K, V, S, Internal>> {
    /// Removes this separator and its value, the head of the leftmost leaf
    /// right of it (D17), and returns both. That leaf's first key takes the
    /// separator's place, its value becoming the head; then the child right
    /// of the separator is fixed if it ended up underfull.
    fn remove_separator(mut self, release: &mut impl FnMut(PageByKind<K, V, S>)) -> (K, V) {
        let (value, successor) = self
            .reborrow_mut()
            .right_edge()
            .descend()
            .remove_leftmost_head(release);
        let key = self.replace_key(successor);
        self.right_edge().fix_underfull_child(release);
        (key, value)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'_, K, V, S, Internal>> {
    /// Fixes the child at this edge if a removal left it underfull, one key
    /// short of its `min_len` (D22): merges it with a sibling, the left one
    /// if it has one, or, if the two don't fit in one node with the separator
    /// between them, moves one entry over from the sibling. The page a merge
    /// frees goes to `release`.
    ///
    /// A sibling too full to merge with holds more than `capacity - min_len`
    /// keys, so it can spare one; after at most `min_len` such steals in a
    /// row, the two fit.
    ///
    /// # Panics
    ///
    /// If the node has no keys, so that the child has no sibling: a node
    /// that a removal went through holds a key until it fixes its child,
    /// which is the only time it can lose one.
    fn fix_underfull_child(self, release: &mut impl FnMut(PageByKind<K, V, S>)) {
        if !self.reborrow().descend().is_underfull() {
            return;
        }
        let (separator, child_is_right) = match self.left_kv() {
            Ok(left_separator) => (left_separator, true),
            Err(edge) => match edge.right_kv() {
                Ok(right_separator) => (right_separator, false),
                Err(_) => unreachable!("an internal node a removal went through has a key"),
            },
        };
        let mut context = match separator.into_balancing_context().can_merge() {
            Ok(mergeable) => {
                release(mergeable.merge().1);
                return;
            }
            Err(context) => context,
        };
        let stolen = if child_is_right {
            context.steal_left()
        } else {
            context.steal_right()
        };
        stolen.expect("a sibling too full to merge with can spare a key");
    }
}
