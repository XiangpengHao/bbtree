//! Walks over a tree in key order, from both ends at once (D23): the core of
//! every iterator. Each end is a cursor: a position between two entries,
//! kept as the edge indexes that lead to it from the root, with the leaf and
//! its parent at hand. Moving a cursor never calls `Ord` (D11).
//!
//! A position is always an edge of a leaf. Within the leaf, the next entry is
//! beside it. Past the leaf's last entry, it is the separator of the lowest
//! ancestor that has a key right of the path, whose value is the head of the
//! leftmost leaf right of that key (D17); the cursor descends to that leaf and
//! starts there. Moving backward mirrors this: before the leaf's first entry,
//! the previous one is the separator of the lowest ancestor with a key left of
//! the path, whose value is this leaf's head, and the cursor goes on from the
//! end of the rightmost leaf left of that key.
//!
//! The parent answers most of those trips up the tree: only when the path's
//! edge is the parent's last (or first) does the cursor walk down again from
//! the root, by its edge indexes, to find the separator further up.
//!
//! A cursor changes in place, field by field, and is never rebuilt and
//! copied over: a copy reads the cursor with wide loads right after narrow
//! stores to it, which the CPU can't forward, so the copy waits for the
//! stores to retire, and with them every older cache miss. Over many small
//! maps, that serialized their misses (D23).
//!
//! # Safety argument
//!
//! This is the tree's path module (D14), the one module besides `page.rs`,
//! `slots.rs` and `handle.rs` with `unsafe` code. A walk with [`ValMut`]
//! hands out values as `&mut V`, which
//! is sound only if it hands out each value at most once. Every walk ensures
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

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::ops::Bound;

use super::handle::{
    CopyableBorrow, Edge, ForceResult, Head, Immut, Internal, Kv, Leaf, LeafOrInternal, NodeKind,
    NodeRef, SearchResult, ValMut,
};
use super::inline_keys::SearchKey;
use crate::inline_key::InlineKeyStrategy;

/// The most levels above the leaves that a tree can have. A non-root
/// internal node has at least 3 children (D13, D22) and a leaf takes at
/// least 1 KB (D15), so a tree one level taller would need more than 2^64
/// bytes of leaves.
const MAX_HEIGHT: usize = 34;

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
    /// The edge where a walk in this direction enters a node of `len` keys:
    /// its first, or its last.
    fn entry_edge(self, len: usize) -> usize {
        match self {
            Self::Forward => 0,
            Self::Backward => len,
        }
    }

    /// The key next to `edge` in this direction, if any.
    fn key_beyond<B: CopyableBorrow, K, V, S, Kind: NodeKind>(
        self,
        edge: Edge<NodeRef<B, K, V, S, Kind>>,
    ) -> Option<Kv<NodeRef<B, K, V, S, Kind>>> {
        match self {
            Self::Forward => edge.right_kv().ok(),
            Self::Backward => edge.left_kv().ok(),
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

/// One end of a walk: a position between two entries of a leaf, or not
/// started yet, in which case its first move starts it at the first or last
/// position of the tree, by the direction of the move.
///
/// Invariant, once started: `edges[0]` is an edge of `leaf`, and for every
/// height `h` from 1 to the root's, `edges[h]` is the edge of the node at
/// height `h` that leads toward `leaf`: walking down from the root by them
/// reaches it. `parent` is the node at height 1, if the root is not the leaf
/// itself. Entries above the root's height are 0. A node holds at most 1023
/// keys (`layout.rs`), so every edge index fits in a `u16`.
struct LeafCursor<B, K, V, S> {
    /// `None` until the cursor starts.
    leaf: Option<NodeRef<B, K, V, S, Leaf>>,
    parent: Option<NodeRef<B, K, V, S, Internal>>,
    edges: [u16; MAX_HEIGHT + 1],
}

impl<B: CopyableBorrow, K, V, S> Clone for LeafCursor<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for LeafCursor<B, K, V, S> {}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> LeafCursor<B, K, V, S> {
    /// A cursor that has not started.
    const UNSTARTED: Self = Self {
        leaf: None,
        parent: None,
        edges: [0; MAX_HEIGHT + 1],
    };

    /// Moves to the position just before or just after `key` in the tree
    /// under `root`, or, if no entry has that key, where it would be. A
    /// separator stands between the end of the leaf left of it and the start
    /// of the leaf right of it (D17).
    fn move_beside_key<Q>(
        &mut self,
        root: Root<B, K, V, S>,
        key: &SearchKey<'_, K, S, Q>,
        side: KeySide,
    ) where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut parent = None;
        let mut node = root;
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => {
                    let edge = match (leaf.search(key), side) {
                        (SearchResult::Found(kv), KeySide::Before) => kv.left_edge(),
                        (SearchResult::Found(kv), KeySide::After) => kv.right_edge(),
                        (SearchResult::GoDown(edge), _) => edge,
                    };
                    self.edges[0] = edge.index() as u16;
                    self.parent = parent;
                    self.leaf = Some(leaf);
                    return;
                }
                ForceResult::Internal(internal) => match internal.search(key) {
                    SearchResult::Found(separator) => {
                        match side {
                            KeySide::Before => {
                                self.descend(separator.left_edge(), Direction::Backward)
                            }
                            KeySide::After => {
                                self.descend(separator.right_edge(), Direction::Forward)
                            }
                        };
                        return;
                    }
                    SearchResult::GoDown(edge) => {
                        self.edges[internal.height()] = edge.index() as u16;
                        parent = Some(internal);
                        edge.descend()
                    }
                },
            };
        }
    }

    /// Moves over the next entry and returns it, or returns `None`, staying
    /// put, at the end of the tree under `root`. A cursor that has not
    /// started starts at the first position.
    fn next(&mut self, root: Root<B, K, V, S>) -> Option<EntryPosition<B, K, V, S>> {
        let leaf = match self.leaf {
            Some(leaf) => leaf,
            None => self.move_to_end(root, Direction::Forward),
        };
        if let Ok(kv) = leaf.edge_at(usize::from(self.edges[0])).right_kv() {
            self.edges[0] += 1;
            return Some(EntryPosition::InLeaf(kv));
        }
        let separator = self.separator_beyond_leaf(root, Direction::Forward)?;
        let leaf = self.descend(separator.right_edge(), Direction::Forward);
        let head = leaf.head().expect("a leaf right of a separator has a head");
        Some(EntryPosition::Separator(separator, head))
    }

    /// Moves back over the previous entry and returns it, or returns `None`,
    /// staying put, at the start of the tree under `root`. A cursor that has
    /// not started starts at the last position.
    fn next_back(&mut self, root: Root<B, K, V, S>) -> Option<EntryPosition<B, K, V, S>> {
        let leaf = match self.leaf {
            Some(leaf) => leaf,
            None => self.move_to_end(root, Direction::Backward),
        };
        if let Ok(kv) = leaf.edge_at(usize::from(self.edges[0])).left_kv() {
            self.edges[0] -= 1;
            return Some(EntryPosition::InLeaf(kv));
        }
        let separator = self.separator_beyond_leaf(root, Direction::Backward)?;
        let head = leaf.head().expect("a leaf right of a separator has a head");
        self.descend(separator.left_edge(), Direction::Backward);
        Some(EntryPosition::Separator(separator, head))
    }

    /// How this position compares with `other`, a position in the same tree:
    /// in key order, by the edge indexes from the root down. Both cursors
    /// have started.
    fn cmp_position(&self, other: &Self) -> Ordering {
        self.edges.iter().rev().cmp(other.edges.iter().rev())
    }

    /// Whether this position is `other`, a position in the same tree. Both
    /// cursors have started. Checks the leaf's edge first, which tells apart
    /// most positions.
    fn is_at(&self, other: &Self) -> bool {
        self.edges[0] == other.edges[0] && self.edges == other.edges
    }

    /// The same position, with shared access for as long as `self` is
    /// borrowed.
    fn reborrow(&self) -> LeafCursor<Immut<'_>, K, V, S> {
        LeafCursor {
            leaf: self.leaf.as_ref().map(NodeRef::reborrow),
            parent: self.parent.as_ref().map(NodeRef::reborrow),
            edges: self.edges,
        }
    }

    /// Moves to the start of the tree under `root` going forward, or to its
    /// end going backward, and returns the leaf it is in.
    fn move_to_end(
        &mut self,
        root: Root<B, K, V, S>,
        direction: Direction,
    ) -> NodeRef<B, K, V, S, Leaf> {
        match root.force() {
            ForceResult::Leaf(leaf) => {
                self.edges[0] = direction.entry_edge(leaf.len()) as u16;
                self.parent = None;
                self.leaf = Some(leaf);
                leaf
            }
            ForceResult::Internal(internal) => {
                let edge = internal.edge_at(direction.entry_edge(internal.len()));
                self.descend(edge, direction)
            }
        }
    }

    /// Takes `edge` and goes on down to a leaf, along first edges to the
    /// leaf's start going forward, or along last edges to its end going
    /// backward; returns the leaf. The edges above `edge` stay.
    fn descend(
        &mut self,
        edge: Edge<NodeRef<B, K, V, S, Internal>>,
        direction: Direction,
    ) -> NodeRef<B, K, V, S, Leaf> {
        let mut parent = edge.into_node();
        self.edges[parent.height()] = edge.index() as u16;
        let mut node = edge.descend();
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => {
                    self.edges[0] = direction.entry_edge(leaf.len()) as u16;
                    self.parent = Some(parent);
                    self.leaf = Some(leaf);
                    return leaf;
                }
                ForceResult::Internal(internal) => {
                    let edge = internal.edge_at(direction.entry_edge(internal.len()));
                    self.edges[internal.height()] = edge.index() as u16;
                    parent = internal;
                    edge.descend()
                }
            };
        }
    }

    /// The separator next to the path in `direction` in the lowest internal
    /// node that has one: the next entry that way once the leaf's own are
    /// passed. `None` at the start or end of the tree.
    ///
    /// The parent is checked first. Only if the path leaves it by its last
    /// edge (or first, going backward) does the search walk down again from
    /// `root`, by the edge indexes, to the nodes above the parent.
    fn separator_beyond_leaf(
        &self,
        root: Root<B, K, V, S>,
        direction: Direction,
    ) -> Option<Kv<NodeRef<B, K, V, S, Internal>>> {
        let parent = self.parent?;
        let parent_edge = parent.edge_at(usize::from(self.edges[1]));
        if let Some(separator) = direction.key_beyond(parent_edge) {
            return Some(separator);
        }
        let mut lowest = None;
        let mut node = root;
        while let ForceResult::Internal(internal) = node.force()
            && internal.height() > 1
        {
            let edge = internal.edge_at(usize::from(self.edges[internal.height()]));
            lowest = direction.key_beyond(edge).or(lowest);
            node = edge.descend();
        }
        lowest
    }
}

/// The two ends of a walk over the tree under `root`.
struct Ends<B, K, V, S> {
    /// `None` for a map without a tree.
    root: Option<Root<B, K, V, S>>,
    front: LeafCursor<B, K, V, S>,
    back: LeafCursor<B, K, V, S>,
}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> Ends<B, K, V, S> {
    const EMPTY: Self = Self {
        root: None,
        front: LeafCursor::UNSTARTED,
        back: LeafCursor::UNSTARTED,
    };

    /// Ends that have not started, over the tree under `root`, if any.
    fn unstarted(root: Option<Root<B, K, V, S>>) -> Self {
        Self {
            root,
            front: LeafCursor::UNSTARTED,
            back: LeafCursor::UNSTARTED,
        }
    }

    /// The same ends, with shared access for as long as `self` is borrowed.
    fn reborrow(&self) -> Ends<Immut<'_>, K, V, S> {
        Ends {
            root: self.root.as_ref().map(NodeRef::reborrow),
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
    ends: Ends<B, K, V, S>,
    /// The entries between the ends.
    length: usize,
}

impl<K, V, S> Clone for FullWalk<Immut<'_>, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            ends: Ends {
                root: self.ends.root,
                front: self.ends.front,
                back: self.ends.back,
            },
            length: self.length,
        }
    }
}

impl<B: WalkBorrow<K, V, S>, K, V, S: InlineKeyStrategy<K>> FullWalk<B, K, V, S> {
    /// A walk over no entries.
    pub(crate) const EMPTY: Self = Self {
        ends: Ends::EMPTY,
        length: 0,
    };

    /// A walk over the `length` entries of the tree under `root`, if any.
    pub(crate) fn new(root: Option<Root<B, K, V, S>>, length: usize) -> Self {
        Self {
            ends: Ends::unstarted(root),
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
        let position = self.ends.front.next(self.ends.root?)?;
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
        let position = self.ends.back.next_back(self.ends.root?)?;
        // SAFETY: an entry was left between the ends, so the back end moved
        // past one the front end has not (module safety argument).
        Some(unsafe { B::item(position) })
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed.
    pub(crate) fn reborrow(&self) -> FullWalk<Immut<'_>, K, V, S> {
        FullWalk {
            ends: self.ends.reborrow(),
            length: self.length,
        }
    }
}

/// A walk over the entries between two bounds, which stops where its ends
/// meet. Both ends start when the walk is made, by searching for the bounds.
pub(crate) struct RangeWalk<B, K, V, S> {
    ends: Ends<B, K, V, S>,
}

impl<K, V, S> Clone for RangeWalk<Immut<'_>, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            ends: Ends {
                root: self.ends.root,
                front: self.ends.front,
                back: self.ends.back,
            },
        }
    }
}

impl<B: WalkBorrow<K, V, S>, K, V, S: InlineKeyStrategy<K>> RangeWalk<B, K, V, S> {
    /// A walk over no entries.
    pub(crate) const EMPTY: Self = Self { ends: Ends::EMPTY };

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
        let mut ends = Ends::unstarted(Some(root));
        match start {
            Bound::Included(key) => {
                ends.front
                    .move_beside_key(root, &SearchKey::new(key), KeySide::Before);
            }
            Bound::Excluded(key) => {
                ends.front
                    .move_beside_key(root, &SearchKey::new(key), KeySide::After);
            }
            Bound::Unbounded => {
                ends.front.move_to_end(root, Direction::Forward);
            }
        }
        match end {
            Bound::Included(key) => {
                ends.back
                    .move_beside_key(root, &SearchKey::new(key), KeySide::After);
            }
            Bound::Excluded(key) => {
                ends.back
                    .move_beside_key(root, &SearchKey::new(key), KeySide::Before);
            }
            Bound::Unbounded => {
                ends.back.move_to_end(root, Direction::Backward);
            }
        }
        if ends.front.cmp_position(&ends.back) == Ordering::Greater {
            panic!("Ord is ill-defined in BTreeMap range");
        }
        Self { ends }
    }

    /// Whether no entry is left between the ends.
    fn ends_meet(&self) -> bool {
        self.ends.root.is_none() || self.ends.front.is_at(&self.ends.back)
    }

    /// The entry after the front end, which moves past it, if any is left.
    pub(crate) fn next(&mut self) -> Option<B::Item> {
        if self.ends_meet() {
            return None;
        }
        let position = self.ends.front.next(self.ends.root?)?;
        // SAFETY: the front end was before the back end, so it moved past an
        // entry the back end has not (module safety argument).
        Some(unsafe { B::item(position) })
    }

    /// The entry before the back end, which moves back past it, if any is
    /// left.
    pub(crate) fn next_back(&mut self) -> Option<B::Item> {
        if self.ends_meet() {
            return None;
        }
        let position = self.ends.back.next_back(self.ends.root?)?;
        // SAFETY: the back end was after the front end, so it moved past an
        // entry the front end has not (module safety argument).
        Some(unsafe { B::item(position) })
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed.
    pub(crate) fn reborrow(&self) -> RangeWalk<Immut<'_>, K, V, S> {
        RangeWalk {
            ends: self.ends.reborrow(),
        }
    }
}
