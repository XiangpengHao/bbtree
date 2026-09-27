//! Walks over a tree in key order, from both ends at once (D23): the core of
//! every iterator. Each end is a cursor: a path from the root to a position
//! between two entries of a leaf (`path.rs`). Moving a cursor never calls
//! `Ord` (D11).
//!
//! Within the leaf, the next entry is beside the position. Past the leaf's
//! last entry, it is the separator of the lowest node on the path with a key
//! right of it, whose value is the head of the leftmost leaf right of that
//! key (D17); the cursor goes up the path to that node and down to that leaf.
//! Moving backward mirrors this: before the leaf's first entry, the previous
//! one is the separator of the lowest node on the path with a key left of
//! it, whose value is this leaf's head, and the cursor goes on from the end
//! of the rightmost leaf left of that key.
//!
//! A cursor changes in place, field by field, and is never rebuilt and
//! copied over: a copy reads the cursor with wide loads right after narrow
//! stores to it, which the CPU can't forward, so the copy waits for the
//! stores to retire, and with them every older cache miss. Over many small
//! maps, that serialized their misses (D23).
//!
//! A [`DyingWalk`] takes the tree apart as it goes, for `IntoIter` and for
//! dropping a tree: it moves each entry out of its slot, with nothing
//! shifting and nothing rebalanced, and frees each node once its ends are
//! done with it.
//!
//! # Safety argument
//!
//! Besides the path module itself, this is the one module of the tree's
//! path code (D14) with `unsafe` code. A walk with [`ValMut`] hands out
//! values as `&mut V`, and one with [`Dying`] moves entries out, which is
//! sound only if it hands out each entry at most once. Every walk ensures
//! that by construction:
//!
//! - its ends start at the tree's first and last positions, or at a range's
//!   bounds with the front not after the back, which making the range checks;
//! - an end moves only past the entry it hands out, one at a time, toward the
//!   other end, and never back;
//! - a [`FullWalk`] hands out an entry only while its count of the entries
//!   between the ends is above 0, and a [`RangeWalk`] only while its ends
//!   have not met; so an end never hands out an entry the other end has.
//!
//! The count starts at the map's length, which is its number of entries
//! whatever `Ord` does, and the positions are structural, so neither depends
//! on `Ord` being consistent.
//!
//! A [`DyingWalk`] also frees nodes, each once, and never one it still
//! reaches:
//!
//! - an end frees a node when it goes up out of it, past its last edge (its
//!   first, going backward): every entry and child of the node then lies on
//!   the end's side of where it stands, handed out or freed, so neither end
//!   comes back to the node, nor reaches it through the path, which no
//!   longer holds it;
//! - an end goes up out of a leaf only after handing out what it needs from
//!   it: going backward, the leaf's head is the separator's value;
//! - once no entry is left between the ends, they stand at the same position,
//!   or only one of them moved, and the nodes not yet freed are exactly those
//!   on that end's path, which the walk frees last, from the bottom up.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::ops::Bound;

use core::mem::needs_drop;

use super::handle::{
    CopyableBorrow, Dying, ForceResult, Head, Immut, Internal, Kv, Leaf, LeafOrInternal, NodeRef,
    Subtree, ValMut,
};
use super::inline_keys::SearchKey;
use super::page::PageByKind;
use super::path::{NodeEnd, Path};
use crate::inline_key::InlineKeyStrategy;

/// A handle to a tree's root, which a walk starts from.
pub(crate) type Root<B, K, V, S> = NodeRef<B, K, V, S, LeafOrInternal>;

/// A borrow type that walks can hand out entries with: what it hands out
/// for an entry.
pub(crate) trait WalkBorrow<K, V, S>: CopyableBorrow + Sized {
    type Item;

    /// The entry at `position`, as this borrow type hands it out.
    ///
    /// # Safety
    ///
    /// The walk hands out the entry at `position` only this once.
    unsafe fn item(position: EntryPosition<Self, K, V, S>) -> Self::Item;
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> WalkBorrow<K, V, S> for Immut<'a> {
    type Item = (&'a K, &'a V);

    unsafe fn item(position: EntryPosition<Self, K, V, S>) -> (&'a K, &'a V) {
        match position {
            EntryPosition::InLeaf(kv) => kv.into_key_value(),
            EntryPosition::Separator(key, value) => (key.into_key(), value.into_value()),
        }
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> WalkBorrow<K, V, S> for ValMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);

    unsafe fn item(position: EntryPosition<Self, K, V, S>) -> (&'a K, &'a mut V) {
        // SAFETY: the caller hands out the entry, and so its value, only
        // this once.
        unsafe {
            match position {
                EntryPosition::InLeaf(kv) => kv.into_key_value_mut(),
                EntryPosition::Separator(key, value) => (key.into_key(), value.into_value_mut()),
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> WalkBorrow<K, V, S> for Dying<K, V> {
    type Item = (K, V);

    unsafe fn item(position: EntryPosition<Self, K, V, S>) -> (K, V) {
        // SAFETY: the caller hands out the entry only this once, so its key
        // and value are moved out once, from nodes not yet freed: a walk
        // frees a node only once past it (module safety argument).
        unsafe {
            match position {
                EntryPosition::InLeaf(kv) => kv.take_key_value(),
                EntryPosition::Separator(key, head) => (key.take_key(), head.take_value()),
            }
        }
    }
}

/// A node handle, of any kind, as a cursor hands over the nodes it leaves.
type Node<B, K, V, S> = NodeRef<B, K, V, S, LeafOrInternal>;

/// An entry a cursor moved over.
pub(crate) enum EntryPosition<B, K, V, S> {
    /// An entry of a leaf, beside its value.
    InLeaf(Kv<NodeRef<B, K, V, S, Leaf>>),
    /// A separator, whose value is the head of the leftmost leaf right of it
    /// (D17).
    Separator(
        Kv<NodeRef<B, K, V, S, Internal>>,
        Head<NodeRef<B, K, V, S, Leaf>>,
    ),
}

/// Which way a cursor moves through the entries.
#[derive(Clone, Copy)]
enum Direction {
    /// Toward greater keys.
    Forward,
    /// Toward smaller keys.
    Backward,
}

impl Direction {
    /// The end of a node where a walk in this direction enters it.
    fn entry_end(self) -> NodeEnd {
        match self {
            Self::Forward => NodeEnd::First,
            Self::Backward => NodeEnd::Last,
        }
    }
}

/// Where a cursor stands relative to a key it looked up.
#[derive(Clone, Copy)]
enum KeySide {
    /// Just before the key, or where it would be.
    Before,
    /// Just after the key, or where it would be.
    After,
}

/// One end of a walk: a path to a position between two entries of a leaf,
/// or, until it starts, a detached path, in which case its first move starts
/// it at the first or last position of the tree, by the direction of the
/// move. Not starting writes no path.
///
/// Invariant, once started (its path attached): the path ends at an edge of
/// a leaf of the walk's tree.
struct Cursor<B, K, V, S> {
    path: Path<B, K, V, S>,
}

impl<B: CopyableBorrow, K, V, S> Clone for Cursor<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for Cursor<B, K, V, S> {}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> Cursor<B, K, V, S> {
    /// A cursor that has not started.
    #[inline(always)]
    fn unstarted() -> Self {
        Self {
            path: Path::detached(),
        }
    }

    /// Starts at the position just before or just after `key` in the tree
    /// under `root`, or, if no entry has that key, where it would be. A
    /// separator stands between the end of the leaf left of it and the start
    /// of the leaf right of it (D17).
    fn start_beside_key<Q>(
        &mut self,
        root: Root<B, K, V, S>,
        key: &SearchKey<'_, K, S, Q>,
        side: KeySide,
    ) where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.path.attach(root);
        if !self.path.search(key) {
            return;
        }
        let index = self.path.index();
        let direction = match side {
            KeySide::Before => Direction::Backward,
            KeySide::After => Direction::Forward,
        };
        self.path.set_index(match side {
            KeySide::Before => index,
            KeySide::After => index + 1,
        });
        self.path.descend_to_leaf(direction.entry_end());
    }

    /// Starts at the first position of the tree under `root` going forward,
    /// or at its last going backward.
    fn start_at_end(&mut self, root: Root<B, K, V, S>, direction: Direction) {
        self.path.attach(root);
        let end = direction.entry_end();
        let root_edge = match end {
            NodeEnd::First => 0,
            NodeEnd::Last => self.path.node(self.path.height()).len(),
        };
        self.path.set_index(root_edge);
        self.path.descend_to_leaf(end);
    }

    /// The leaf the cursor is in. It has started.
    fn leaf(&self) -> NodeRef<B, K, V, S, Leaf> {
        match self.path.node(0).force() {
            ForceResult::Leaf(leaf) => leaf,
            ForceResult::Internal(_) => unreachable!("a started cursor is in a leaf"),
        }
    }

    /// The key at `index` of the node at `height` on the path, a separator.
    fn separator(&self, height: usize, index: usize) -> Kv<NodeRef<B, K, V, S, Internal>> {
        match self.path.node(height).force() {
            ForceResult::Internal(node) => node.kv_at(index),
            ForceResult::Leaf(_) => unreachable!("a node above the leaves is internal"),
        }
    }

    /// Moves over the next entry and hands it out, or returns `None`,
    /// staying put, at the end of the tree. A cursor that has not started
    /// starts at the first position of the tree under `root`.
    ///
    /// Past a leaf's end, it goes up the path to the separator after it,
    /// handing `leave` each node it goes up out of, from the leaf: the nodes
    /// whose last edge it has passed, which it never comes back to.
    ///
    /// # Safety
    ///
    /// The walk hands out the next entry only this once (module safety
    /// argument).
    unsafe fn next(
        &mut self,
        root: Root<B, K, V, S>,
        leave: &mut impl FnMut(Node<B, K, V, S>),
    ) -> Option<B::Item>
    where
        B: WalkBorrow<K, V, S>,
    {
        if !self.path.is_attached() {
            self.start_at_end(root, Direction::Forward);
        }
        let index = self.path.index();
        if let Ok(kv) = self.leaf().edge_at(index).right_kv() {
            self.path.set_index(index + 1);
            // SAFETY: forwarded from the caller.
            return Some(unsafe { B::item(EntryPosition::InLeaf(kv)) });
        }
        let height = (1..=self.path.root_height())
            .find(|&height| self.path.index_at(height) < self.path.node(height).len())?;
        self.ascend_leaving(height, leave);
        let separator = self.separator(height, self.path.index());
        self.path.set_index(separator.index() + 1);
        self.path.descend_to_leaf(NodeEnd::First);
        let head = self
            .leaf()
            .head()
            .expect("a leaf right of a separator has a head");
        // SAFETY: forwarded from the caller.
        Some(unsafe { B::item(EntryPosition::Separator(separator, head)) })
    }

    /// Moves back over the previous entry and hands it out, or returns
    /// `None`, staying put, at the start of the tree. A cursor that has not
    /// started starts at the last position of the tree under `root`.
    ///
    /// Before a leaf's start, it hands out the separator before it, whose
    /// value is the leaf's head, and only then goes up the path to it,
    /// handing `leave` each node it goes up out of, from the leaf: the nodes
    /// whose first edge it has passed, which it never comes back to.
    ///
    /// # Safety
    ///
    /// The walk hands out the previous entry only this once (module safety
    /// argument).
    unsafe fn next_back(
        &mut self,
        root: Root<B, K, V, S>,
        leave: &mut impl FnMut(Node<B, K, V, S>),
    ) -> Option<B::Item>
    where
        B: WalkBorrow<K, V, S>,
    {
        if !self.path.is_attached() {
            self.start_at_end(root, Direction::Backward);
        }
        let index = self.path.index();
        let leaf = self.leaf();
        if let Ok(kv) = leaf.edge_at(index).left_kv() {
            self.path.set_index(index - 1);
            // SAFETY: forwarded from the caller.
            return Some(unsafe { B::item(EntryPosition::InLeaf(kv)) });
        }
        let height =
            (1..=self.path.root_height()).find(|&height| self.path.index_at(height) > 0)?;
        let head = leaf.head().expect("a leaf right of a separator has a head");
        let separator = self.separator(height, self.path.index_at(height) - 1);
        let separator_index = separator.index();
        // SAFETY: forwarded from the caller.
        let item = unsafe { B::item(EntryPosition::Separator(separator, head)) };
        self.ascend_leaving(height, leave);
        self.path.set_index(separator_index);
        self.path.descend_to_leaf(NodeEnd::Last);
        Some(item)
    }

    /// Moves past the rest of the cursor's leaf, and the separator after it,
    /// to the start of the next leaf, handing nothing out and handing `leave`
    /// the nodes it goes up out of, as [`Self::next`] does; returns `false`,
    /// staying put, at the last leaf. A cursor that has not started starts at
    /// the first position of the tree under `root`.
    fn skip_to_next_leaf(
        &mut self,
        root: Root<B, K, V, S>,
        leave: &mut impl FnMut(Node<B, K, V, S>),
    ) -> bool {
        if !self.path.is_attached() {
            self.start_at_end(root, Direction::Forward);
        }
        let Some(height) = (1..=self.path.root_height())
            .find(|&height| self.path.index_at(height) < self.path.node(height).len())
        else {
            return false;
        };
        self.ascend_leaving(height, leave);
        let separator_index = self.path.index();
        self.path.set_index(separator_index + 1);
        self.path.descend_to_leaf(NodeEnd::First);
        true
    }

    /// Goes up the path to `height`, handing `leave` each node it goes up
    /// out of, once off the path.
    fn ascend_leaving(&mut self, height: usize, leave: &mut impl FnMut(Node<B, K, V, S>)) {
        while self.path.height() < height {
            let node = self.path.node(self.path.height());
            self.path.ascend();
            leave(node);
        }
    }

    /// The same position, with shared access for as long as `self` is
    /// borrowed.
    fn reborrow(&self) -> Cursor<Immut<'_>, K, V, S> {
        Cursor {
            path: self.path.reborrow(),
        }
    }
}

/// The two ends of a walk over the tree under `root`.
struct Ends<B, K, V, S> {
    root: Root<B, K, V, S>,
    front: Cursor<B, K, V, S>,
    back: Cursor<B, K, V, S>,
}

impl<B: CopyableBorrow, K, V, S> Clone for Ends<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for Ends<B, K, V, S> {}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> Ends<B, K, V, S> {
    /// Ends that have not started, in the tree under `root`.
    fn unstarted(root: Root<B, K, V, S>) -> Self {
        Self {
            root,
            front: Cursor::unstarted(),
            back: Cursor::unstarted(),
        }
    }

    /// The same ends, with shared access for as long as `self` is borrowed.
    fn reborrow(&self) -> Ends<Immut<'_>, K, V, S> {
        Ends {
            root: self.root.reborrow(),
            front: self.front.reborrow(),
            back: self.back.reborrow(),
        }
    }
}

/// A walk over every entry of a tree, which counts the entries left between
/// its ends. An end that has not started starts at the first or last entry
/// the first time it moves, so the walk costs nothing until then, and one end
/// never moves the other.
pub(crate) struct FullWalk<B, K, V, S> {
    /// `None` for a map without a tree.
    ends: Option<Ends<B, K, V, S>>,
    /// The entries between the ends.
    length: usize,
}

impl<K, V, S> Clone for FullWalk<Immut<'_>, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            ends: self.ends,
            length: self.length,
        }
    }
}

impl<B: WalkBorrow<K, V, S>, K, V, S: InlineKeyStrategy<K>> FullWalk<B, K, V, S> {
    /// A walk over no entries.
    pub(crate) const EMPTY: Self = Self {
        ends: None,
        length: 0,
    };

    /// A walk over the `length` entries of the tree under `root`, if any.
    pub(crate) fn new(root: Option<Root<B, K, V, S>>, length: usize) -> Self {
        Self {
            ends: root.map(Ends::unstarted),
            length,
        }
    }

    /// The entries left between the ends.
    pub(crate) fn len(&self) -> usize {
        self.length
    }

    /// The entry after the front end, which moves past it, if any is left.
    pub(crate) fn next(&mut self) -> Option<B::Item> {
        self.next_leaving(&mut |_| {})
    }

    /// The entry before the back end, which moves back past it, if any is
    /// left.
    pub(crate) fn next_back(&mut self) -> Option<B::Item> {
        self.next_back_leaving(&mut |_| {})
    }

    /// [`Self::next`], handing `leave` the nodes the front end goes up out
    /// of, which neither end comes back to (module safety argument).
    fn next_leaving(&mut self, leave: &mut impl FnMut(Node<B, K, V, S>)) -> Option<B::Item> {
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        let ends = self.ends.as_mut()?;
        // SAFETY: an entry was left between the ends, so the front end moves
        // past one the back end has not (module safety argument).
        unsafe { ends.front.next(ends.root, leave) }
    }

    /// [`Self::next_back`], handing `leave` the nodes the back end goes up
    /// out of, which neither end comes back to (module safety argument).
    fn next_back_leaving(&mut self, leave: &mut impl FnMut(Node<B, K, V, S>)) -> Option<B::Item> {
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        let ends = self.ends.as_mut()?;
        // SAFETY: an entry was left between the ends, so the back end moves
        // past one the front end has not (module safety argument).
        unsafe { ends.back.next_back(ends.root, leave) }
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed.
    pub(crate) fn reborrow(&self) -> FullWalk<Immut<'_>, K, V, S> {
        FullWalk {
            ends: self.ends.as_ref().map(Ends::reborrow),
            length: self.length,
        }
    }
}

/// A walk over the entries between two bounds, which stops where its ends
/// meet. Both ends start when the walk is made, by searching for the bounds.
pub(crate) struct RangeWalk<B, K, V, S> {
    /// `None` for a map without a tree.
    ends: Option<Ends<B, K, V, S>>,
}

impl<K, V, S> Clone for RangeWalk<Immut<'_>, K, V, S> {
    fn clone(&self) -> Self {
        Self { ends: self.ends }
    }
}

impl<B: WalkBorrow<K, V, S>, K, V, S: InlineKeyStrategy<K>> RangeWalk<B, K, V, S> {
    /// A walk over no entries.
    pub(crate) const EMPTY: Self = Self { ends: None };

    /// A walk over the entries of the tree under `root` within the bounds,
    /// with std's panics for bounds out of order (D1).
    ///
    /// # Panics
    ///
    /// If `start > end`, if `start == end` and both are excluded, or if `Ord`
    /// is so inconsistent that the start is found after the end.
    pub(crate) fn new<Q>(root: Root<B, K, V, S>, start: Bound<&Q>, end: Bound<&Q>) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match (start, end) {
            (Bound::Excluded(start), Bound::Excluded(end)) if start == end => {
                panic!("range start and end are equal and excluded in BTreeMap")
            }
            (
                Bound::Included(start) | Bound::Excluded(start),
                Bound::Included(end) | Bound::Excluded(end),
            ) if start > end => panic!("range start is greater than range end in BTreeMap"),
            _ => {}
        }
        // The ends start where they stay: positioning them writes their
        // paths, which a copy right after would wait on (`path.rs`).
        let mut walk = Self {
            ends: Some(Ends::unstarted(root)),
        };
        let ends = walk.ends.as_mut().expect("just made");
        match start {
            Bound::Included(key) => {
                ends.front
                    .start_beside_key(root, &SearchKey::new(key), KeySide::Before);
            }
            Bound::Excluded(key) => {
                ends.front
                    .start_beside_key(root, &SearchKey::new(key), KeySide::After);
            }
            Bound::Unbounded => ends.front.start_at_end(root, Direction::Forward),
        }
        match end {
            Bound::Included(key) => {
                ends.back
                    .start_beside_key(root, &SearchKey::new(key), KeySide::After);
            }
            Bound::Excluded(key) => {
                ends.back
                    .start_beside_key(root, &SearchKey::new(key), KeySide::Before);
            }
            Bound::Unbounded => ends.back.start_at_end(root, Direction::Backward),
        }
        if ends.front.path.cmp_position(&ends.back.path) == Ordering::Greater {
            panic!("Ord is ill-defined in BTreeMap range");
        }
        walk
    }

    /// The ends, unless no entry is left between them.
    fn ends_apart(&mut self) -> Option<&mut Ends<B, K, V, S>> {
        self.ends
            .as_mut()
            .filter(|ends| !ends.front.path.is_at(&ends.back.path))
    }

    /// The entry after the front end, which moves past it, if any is left.
    pub(crate) fn next(&mut self) -> Option<B::Item> {
        let ends = self.ends_apart()?;
        // SAFETY: the front end is before the back end, so it moves past an
        // entry the back end has not (module safety argument).
        unsafe { ends.front.next(ends.root, &mut |_| {}) }
    }

    /// The entry before the back end, which moves back past it, if any is
    /// left.
    pub(crate) fn next_back(&mut self) -> Option<B::Item> {
        let ends = self.ends_apart()?;
        // SAFETY: the back end is after the front end, so it moves past an
        // entry the front end has not (module safety argument).
        unsafe { ends.back.next_back(ends.root, &mut |_| {}) }
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed.
    pub(crate) fn reborrow(&self) -> RangeWalk<Immut<'_>, K, V, S> {
        RangeWalk {
            ends: self.ends.as_ref().map(Ends::reborrow),
        }
    }
}

/// A walk that takes a tree apart: it moves every entry out, in key order
/// from either end, and frees each node, its page going to `release`, once
/// its ends are done with it (module safety argument). Nothing shifts and
/// nothing is rebalanced, as nothing but the walk sees the tree again.
///
/// Dropping it drops the entries left between its ends in ascending order,
/// each key before its value, as std's `BTreeMap` does (D1), then frees the
/// nodes left. If one of their `Drop`s panics, the rest are still dropped,
/// in the same order, and every node freed, before the panic goes on; a
/// second panic aborts, as any panic during unwinding does.
pub(crate) struct DyingWalk<K, V, S: InlineKeyStrategy<K>, R: FnMut(PageByKind<K, V, S>)> {
    walk: FullWalk<Dying<K, V>, K, V, S>,
    release: R,
}

impl<K, V, S: InlineKeyStrategy<K>, R: FnMut(PageByKind<K, V, S>)> DyingWalk<K, V, S, R> {
    /// A walk taking apart `tree`, of `length` entries, if any.
    ///
    /// A walk that only its front moves needs no count: given `usize::MAX`,
    /// it ends where the tree does.
    pub(crate) fn new(tree: Option<Subtree<K, V, S>>, length: usize, release: R) -> Self {
        Self {
            walk: FullWalk::new(tree.map(Subtree::into_dying), length),
            release,
        }
    }

    /// The entries left between the ends.
    pub(crate) fn len(&self) -> usize {
        self.walk.len()
    }

    /// Moves out the entry after the front end, if any is left.
    pub(crate) fn next(&mut self) -> Option<(K, V)> {
        let release = &mut self.release;
        self.walk.next_leaving(&mut |node| {
            // SAFETY: the walk is done with every node it leaves (module
            // safety argument), so each is freed once.
            release(unsafe { node.free() });
        })
    }

    /// Moves out the entry before the back end, if any is left.
    pub(crate) fn next_back(&mut self) -> Option<(K, V)> {
        let release = &mut self.release;
        self.walk.next_back_leaving(&mut |node| {
            // SAFETY: as for `next`.
            release(unsafe { node.free() });
        })
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed. They are all still in their slots: only those outside the
    /// ends have been moved out.
    pub(crate) fn remaining(&self) -> FullWalk<Immut<'_>, K, V, S> {
        self.walk.reborrow()
    }

    /// Moves out and drops the entries left, in ascending order, then frees
    /// the nodes left.
    ///
    /// Entries without drop glue are not moved out: the front end skips them
    /// a leaf at a time, unless the back end has moved, which it must not
    /// pass. The order of their drops is unobservable.
    fn drop_rest(&mut self) {
        let Some(ends) = &mut self.walk.ends else {
            return;
        };
        if !needs_drop::<K>() && !needs_drop::<V>() && !ends.back.path.is_attached() {
            let release = &mut self.release;
            let mut leave = |node: Node<Dying<K, V>, K, V, S>| {
                // SAFETY: as for `next`.
                release(unsafe { node.free() });
            };
            while ends.front.skip_to_next_leaf(ends.root, &mut leave) {}
            self.walk.length = 0;
        } else {
            while let Some(entry) = self.next() {
                drop(entry);
            }
        }
        self.free_remaining();
    }

    /// Frees the nodes the walk has not, once no entry is left between its
    /// ends: those on the path of an end that moved, from the bottom up, or
    /// the root alone, if neither did.
    fn free_remaining(&mut self) {
        let Some(ends) = self.walk.ends.take() else {
            return;
        };
        let path = if ends.front.path.is_attached() {
            ends.front.path
        } else if ends.back.path.is_attached() {
            ends.back.path
        } else {
            let mut path = Path::detached();
            path.attach(ends.root);
            path
        };
        for height in path.height()..=path.root_height() {
            // SAFETY: with no entry left between the ends, every node off this
            // path is freed, and every entry on it moved out or skipped for
            // having no drop glue (module safety argument); each node here
            // is freed once, after its children.
            (self.release)(unsafe { path.node(height).free() });
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, R: FnMut(PageByKind<K, V, S>)> Drop for DyingWalk<K, V, S, R> {
    fn drop(&mut self) {
        let rest = DropRest(self);
        // A panicking `Drop` unwinds through `rest`, whose own `Drop` goes on
        // with what is left.
        rest.0.drop_rest();
    }
}

/// The rest of a [`DyingWalk`] being dropped, which dropping this drops.
struct DropRest<'w, K, V, S: InlineKeyStrategy<K>, R: FnMut(PageByKind<K, V, S>)>(
    &'w mut DyingWalk<K, V, S, R>,
);

impl<K, V, S: InlineKeyStrategy<K>, R: FnMut(PageByKind<K, V, S>)> Drop
    for DropRest<'_, K, V, S, R>
{
    fn drop(&mut self) {
        self.0.drop_rest();
    }
}
