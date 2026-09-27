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
//! # Safety argument
//!
//! Besides the path module itself, this is the one module of the tree's
//! path code (D14) with `unsafe` code. A walk with [`ValMut`] hands out
//! values as `&mut V`, which is sound only if it hands out each value at
//! most once. Every walk ensures that by construction:
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

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::ops::Bound;

use super::handle::{
    CopyableBorrow, ForceResult, Head, Immut, Internal, Kv, Leaf, LeafOrInternal, NodeRef, ValMut,
};
use super::inline_keys::SearchKey;
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

    /// Moves over the next entry and returns it, or returns `None`, staying
    /// put, at the end of the tree. A cursor that has not started starts at
    /// the first position of the tree under `root`.
    fn next(&mut self, root: Root<B, K, V, S>) -> Option<EntryPosition<B, K, V, S>> {
        if !self.path.is_attached() {
            self.start_at_end(root, Direction::Forward);
        }
        let index = self.path.index();
        if let Ok(kv) = self.leaf().edge_at(index).right_kv() {
            self.path.set_index(index + 1);
            return Some(EntryPosition::InLeaf(kv));
        }
        let height = (1..=self.path.root_height())
            .find(|&height| self.path.index_at(height) < self.path.node(height).len())?;
        while self.path.height() < height {
            self.path.ascend();
        }
        let separator = self.separator(height, self.path.index());
        self.path.set_index(separator.index() + 1);
        self.path.descend_to_leaf(NodeEnd::First);
        let head = self
            .leaf()
            .head()
            .expect("a leaf right of a separator has a head");
        Some(EntryPosition::Separator(separator, head))
    }

    /// Moves back over the previous entry and returns it, or returns `None`,
    /// staying put, at the start of the tree. A cursor that has not started
    /// starts at the last position of the tree under `root`.
    fn next_back(&mut self, root: Root<B, K, V, S>) -> Option<EntryPosition<B, K, V, S>> {
        if !self.path.is_attached() {
            self.start_at_end(root, Direction::Backward);
        }
        let index = self.path.index();
        let leaf = self.leaf();
        if let Ok(kv) = leaf.edge_at(index).left_kv() {
            self.path.set_index(index - 1);
            return Some(EntryPosition::InLeaf(kv));
        }
        let height =
            (1..=self.path.root_height()).find(|&height| self.path.index_at(height) > 0)?;
        let head = leaf.head().expect("a leaf right of a separator has a head");
        while self.path.height() < height {
            self.path.ascend();
        }
        let separator = self.separator(height, self.path.index() - 1);
        self.path.set_index(separator.index());
        self.path.descend_to_leaf(NodeEnd::Last);
        Some(EntryPosition::Separator(separator, head))
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
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        let ends = self.ends.as_mut()?;
        let position = ends.front.next(ends.root)?;
        // SAFETY: an entry was left between the ends, so the front end moved
        // past one the back end has not (module safety argument).
        Some(unsafe { B::item(position) })
    }

    /// The entry before the back end, which moves back past it, if any is
    /// left.
    pub(crate) fn next_back(&mut self) -> Option<B::Item> {
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        let ends = self.ends.as_mut()?;
        let position = ends.back.next_back(ends.root)?;
        // SAFETY: an entry was left between the ends, so the back end moved
        // past one the front end has not (module safety argument).
        Some(unsafe { B::item(position) })
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
        let position = ends.front.next(ends.root)?;
        // SAFETY: the front end was before the back end, so it moved past an
        // entry the back end has not (module safety argument).
        Some(unsafe { B::item(position) })
    }

    /// The entry before the back end, which moves back past it, if any is
    /// left.
    pub(crate) fn next_back(&mut self) -> Option<B::Item> {
        let ends = self.ends_apart()?;
        let position = ends.back.next_back(ends.root)?;
        // SAFETY: the back end was after the front end, so it moved past an
        // entry the front end has not (module safety argument).
        Some(unsafe { B::item(position) })
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed.
    pub(crate) fn reborrow(&self) -> RangeWalk<Immut<'_>, K, V, S> {
        RangeWalk {
            ends: self.ends.as_ref().map(Ends::reborrow),
        }
    }
}
