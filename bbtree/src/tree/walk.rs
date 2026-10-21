//! Walks over a tree in key order, from both ends at once (D23): the core of
//! every iterator. Each end is a cursor: a node stack from the root to a
//! position between two entries of a leaf (`node_stack.rs`). Moving a cursor
//! never calls `Ord` (D11).
//!
//! Within a leaf, the next entry is beside the position. After a leaf's last
//! entry comes the nearest ancestor separator to its right; its value is
//! stored as the next leaf's head (D17). The cursor climbs to it, then
//! descends to that leaf. Moving backward mirrors this: before a leaf's
//! first entry comes the nearest ancestor separator to its left, whose value
//! is this leaf's head. The cursor climbs to it, then descends to the end of
//! the previous leaf.
//!
//! Consuming folds visit the rest of each leaf in one counted loop and
//! update the cursor once per leaf; the other end's position bounds the last
//! run. This drops the per-entry cursor bookkeeping and lets the compiler
//! vectorize simple reductions. Crossing a leaf boundary uses the same code
//! as `next`.
//!
//! A cursor changes in place, field by field, and is never rebuilt and
//! copied over. A copy's wide loads can't take their data from the narrow
//! stores just before them, so they wait for those stores to retire, and
//! with them every older cache miss (D23).
//!
//! A [`DyingWalk`] takes the tree apart as it goes, for `IntoIter` and for
//! dropping a tree. It moves each entry out of its slot, shifts and
//! rebalances nothing, and frees each node once its ends are done with it.
//!
//! # Safety argument
//!
//! A walk with [`ValMut`] hands out values as `&mut V`, and one with
//! [`Dying`] moves entries out. Both are sound only if the walk hands out
//! each entry at most once, which every walk ensures by construction:
//!
//! - its ends start at the tree's first and last positions, or at a range's
//!   bounds with the front not after the back, which [`RangeWalk::new`]
//!   checks;
//! - an end moves only toward the other, never back. A step hands out the
//!   entry it crosses; a consuming fold hands out each entry of a crossed
//!   run once. A panicking fold consumes its walk, so it cannot resume at
//!   a partly visited run;
//! - a [`FullWalk`] hands out an entry only while its count of the entries
//!   between the ends is above 0. A [`RangeWalk`] stops where its attached
//!   ends meet, or at the tree's boundary when the opposite, unbounded end
//!   has not started. An end never hands out an entry the other end has.
//!   During a cached leaf run, the run's interval records where the end is
//!   while its node stack lags behind; the walk reconciles the node stack
//!   before the other end can move;
//! - a [`BorrowedWalk`] steps and folds as a `RangeWalk`, and takes every
//!   entry it steps across off its count: one per step, a key run's length
//!   per run.
//!
//! The count starts at the map's length, which is its number of entries
//! whatever `Ord` does, and the positions are structural, so neither depends
//! on `Ord` being consistent.
//!
//! A [`DyingWalk`] also frees nodes, each once, and never one it still
//! reaches:
//!
//! - an end frees a node when it goes up out of it, past its last edge (its
//!   first, going backward). Every entry and child of the node then lies
//!   behind the end, handed out or freed. So neither end comes back to the
//!   node, and the end's node stack no longer holds it;
//! - an end goes up out of a leaf only after handing out what it needs from
//!   it: going backward, the leaf's head is the separator's value;
//! - once no entry is left between the ends, they stand at the same
//!   position, or only one of them has moved. The nodes not yet freed are
//!   then exactly those on that end's node stack, which the walk frees last,
//!   from the bottom up.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::ops::Bound;

use core::marker::PhantomData;
use core::mem::needs_drop;

use super::node::inline_keys::SearchKey;
use super::node::internal::Internal;
use super::node::leaf::{KeyRun, Leaf, LeafRun};
use super::node::page::{NodePtr, PageByType};
use super::node::root_page::RootPage;
use super::node::{EntrySlots, Node};
use super::node_stack::{
    CopyableBorrow, Dying, FirstEdge, Immut, KeySide, LastEdge, NodeEnd, NodeStack, ValMut,
};
use super::shared_prefix::SharedPrefix;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// A borrow type a walk hands out entries with, and what it hands out for
/// each entry.
pub(crate) trait WalkBorrow<K, V>: CopyableBorrow + Sized {
    type Item;

    /// The entry in `slots`, as this borrow type hands it out.
    ///
    /// # Safety
    ///
    /// The slots are an entry of the walk's tree, initialized, which the
    /// walk hands out only this once.
    unsafe fn item(slots: EntrySlots<K, V>) -> Self::Item;
}

impl<'a, K: 'a, V: 'a> WalkBorrow<K, V> for Immut<'a> {
    type Item = (&'a K, &'a V);

    unsafe fn item(slots: EntrySlots<K, V>) -> (&'a K, &'a V) {
        // SAFETY: the slots are initialized and shared for the walk's
        // borrow (caller).
        unsafe { (slots.key.as_ref(), slots.value.as_ref()) }
    }
}

impl<'a, K: 'a, V: 'a> WalkBorrow<K, V> for ValMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);

    unsafe fn item(mut slots: EntrySlots<K, V>) -> (&'a K, &'a mut V) {
        // SAFETY: the slots are initialized and apart; the key stays shared,
        // and the caller hands out the entry, and so its value, only this
        // once.
        unsafe { (slots.key.as_ref(), slots.value.as_mut()) }
    }
}

impl<K, V> WalkBorrow<K, V> for Dying<K, V> {
    type Item = (K, V);

    unsafe fn item(slots: EntrySlots<K, V>) -> (K, V) {
        // SAFETY: the caller hands out the entry only this once, so its key
        // and value are moved out once. Its node is not yet freed: a walk
        // frees a node only once past it (module safety argument).
        unsafe { (slots.key.read(), slots.value.read()) }
    }
}

/// One end of a walk: a node stack to a position between two entries of a
/// leaf. Until the cursor starts, its node stack is detached, and its first
/// move starts it at the tree's first or last position, by the move's
/// direction. An unstarted cursor writes no node stack.
///
/// Invariant, once started (its node stack attached): the node stack ends at
/// an edge of a leaf of the walk's tree.
struct Cursor<B, K, V, S> {
    stack: NodeStack<B, K, V, S>,
}

impl<B: CopyableBorrow, K, V, S> Clone for Cursor<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for Cursor<B, K, V, S> {}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> Cursor<B, K, V, S> {
    #[inline(always)]
    fn unstarted() -> Self {
        Self {
            stack: NodeStack::detached(),
        }
    }

    /// Starts at the position just before or just after `key` in the tree
    /// under `top`, whose keys share `shared` (D50); see
    /// [`NodeStack::search_to_edge`].
    fn start_beside_key<Q>(
        &mut self,
        top: WalkTop<K, V, S>,
        key: &SearchKey<'_, K, S, Q>,
        side: KeySide,
        shared: &SharedPrefix,
    ) where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.stack.attach(top.node());
        self.stack.search_to_edge(key, side, shared);
    }

    /// Unless the cursor has started, starts it at the `End` of the tree
    /// under `top`: the first position for a walk forward, the last for one
    /// backward.
    fn ensure_started<End: NodeEnd>(&mut self, top: WalkTop<K, V, S>) {
        if !self.stack.is_attached() {
            self.stack.attach(top.node());
            self.stack.descend_to_end::<End>();
        }
    }

    /// The leaf and the edge of it the cursor stands at. Only for a cursor
    /// that has started, which every caller is moving.
    #[inline(always)]
    fn leaf_edge(&self) -> (Leaf<K, V, S>, usize) {
        // SAFETY: the cursor has started (precondition, kept by every caller
        // by hand), so its node stack is attached and ends at an edge of a
        // leaf (type invariant once started).
        unsafe { self.stack.leaf_edge() }
    }

    /// Moves the cursor to edge `index` of its leaf, unchecked. Only for a
    /// cursor that has started, which every caller is moving.
    #[inline(always)]
    fn set_leaf_index(&mut self, index: usize) {
        // SAFETY: as for `leaf_edge`.
        unsafe { self.stack.set_leaf_index(index) }
    }

    /// Folds `count` entries beside this cursor within its current leaf.
    /// The node stack moves once for the whole contiguous run, leaving a
    /// simple counted loop over the entries instead of a cursor step per
    /// entry.
    ///
    /// # Safety
    ///
    /// The cursor is attached. These entries are between the walk's ends
    /// and have not been handed out before. `count` does not cross a leaf
    /// boundary in the direction of `End`.
    unsafe fn fold_leaf<End: NodeEnd, A>(
        &mut self,
        count: usize,
        mut accumulator: A,
        fold: &mut impl FnMut(A, B::Item) -> A,
    ) -> A
    where
        B: WalkBorrow<K, V>,
    {
        let (leaf, index) = self.leaf_edge();
        let (start, end) = if End::IS_LAST {
            self.set_leaf_index(index - count);
            (index - count, index)
        } else {
            self.set_leaf_index(index + count);
            (index, index + count)
        };
        for offset in 0..count {
            let index = if End::IS_LAST {
                end - offset - 1
            } else {
                start + offset
            };
            // SAFETY: this loop visits each of the caller's unvisited
            // entries once, each below the leaf's length. The node stack has
            // already moved past all of them.
            let item = unsafe { B::item(leaf.entry_slots(index)) };
            accumulator = fold(accumulator, item);
        }
        accumulator
    }

    /// Moves over the next entry and hands it out, or returns `None`,
    /// staying put, at the end of the tree. A cursor that has not started
    /// starts at the first position of the tree under `top`.
    ///
    /// Past a leaf's end, it climbs the node stack to the separator after the
    /// leaf. It hands `leave` each node it climbs out of, the leaf first:
    /// nodes whose last edge it has passed, which it never comes back to.
    ///
    /// # Safety
    ///
    /// The walk hands out the next entry only this once (module safety
    /// argument).
    #[inline]
    unsafe fn next(
        &mut self,
        top: WalkTop<K, V, S>,
        leave: &mut impl FnMut(Node<K, V, S>),
    ) -> Option<B::Item>
    where
        B: WalkBorrow<K, V>,
    {
        self.ensure_started::<FirstEdge>(top);
        let (leaf, index) = self.leaf_edge();
        if index < leaf.len() {
            self.set_leaf_index(index + 1);
            // SAFETY: the entry is below the leaf's length; forwarded from
            // the caller.
            return Some(unsafe { B::item(leaf.entry_slots(index)) });
        }
        // SAFETY: the cursor is at its leaf's last edge, and the caller
        // permits handing out the next entry.
        unsafe { self.next_separator(leave) }
    }

    /// Crosses a leaf boundary and hands out the separator there. Keeping the
    /// ancestor search outside the per-entry path avoids duplicating it in
    /// every inlined scan loop.
    ///
    /// # Safety
    ///
    /// The cursor is at its leaf's last edge. If there is a next entry,
    /// the walk hands it out only this once.
    #[inline(never)]
    unsafe fn next_separator(&mut self, leave: &mut impl FnMut(Node<K, V, S>)) -> Option<B::Item>
    where
        B: WalkBorrow<K, V>,
    {
        let (node, index) = self.cross_to_next_leaf(leave)?;
        let (head_leaf, _) = self.leaf_edge();
        // SAFETY: separator `index` is in the node, and its entry is
        // initialized; forwarded from the caller.
        Some(unsafe { B::item(node.separator_entry_slots(index, head_leaf)) })
    }

    /// Moves back over the previous entry and hands it out, or returns
    /// `None`, staying put, at the start of the tree. A cursor that has not
    /// started starts at the last position of the tree under `top`.
    ///
    /// Before a leaf's start, it hands out the separator before the leaf,
    /// whose value is the leaf's head, and only then climbs the node stack to
    /// it. It hands `leave` each node it climbs out of, the leaf first: nodes
    /// whose first edge it has passed, which it never comes back to.
    ///
    /// # Safety
    ///
    /// The walk hands out the previous entry only this once (module safety
    /// argument).
    #[inline]
    unsafe fn next_back(
        &mut self,
        top: WalkTop<K, V, S>,
        leave: &mut impl FnMut(Node<K, V, S>),
    ) -> Option<B::Item>
    where
        B: WalkBorrow<K, V>,
    {
        self.ensure_started::<LastEdge>(top);
        let (leaf, index) = self.leaf_edge();
        if index > 0 {
            self.set_leaf_index(index - 1);
            // SAFETY: the entry is below the leaf's length; forwarded from
            // the caller.
            return Some(unsafe { B::item(leaf.entry_slots(index - 1)) });
        }
        // SAFETY: the cursor is at its leaf's first edge, and the caller
        // permits handing out the previous entry.
        unsafe { self.next_back_separator(leave) }
    }

    /// The reverse boundary crossing; see [`Self::next_separator`].
    ///
    /// # Safety
    ///
    /// The cursor is at its leaf's first edge. If there is a previous
    /// entry, the walk hands it out only this once.
    #[inline(never)]
    unsafe fn next_back_separator(
        &mut self,
        leave: &mut impl FnMut(Node<K, V, S>),
    ) -> Option<B::Item>
    where
        B: WalkBorrow<K, V>,
    {
        let depth = (0..self.stack.depth())
            .rev()
            .find(|&depth| self.stack.index_at(depth) > 0)?;
        let (head_leaf, _) = self.leaf_edge();
        let (node, separator_index) = (
            self.stack.internal_at(depth),
            self.stack.index_at(depth) - 1,
        );
        // SAFETY: separator `separator_index` is in the node, and its entry
        // is initialized; forwarded from the caller.
        let item = unsafe { B::item(node.separator_entry_slots(separator_index, head_leaf)) };
        self.ascend_leaving(depth, leave);
        self.stack.set_index(separator_index);
        self.stack.descend_to_leaf::<LastEdge>();
        Some(item)
    }

    /// Moves past the rest of the cursor's leaf and the separator after it,
    /// to the start of the next leaf. It hands nothing out, and hands `leave`
    /// the nodes it climbs out of, as [`Self::next`] does. Returns `false`,
    /// staying put, at the last leaf. An unstarted cursor first starts at the
    /// first position of the tree under `top`.
    fn skip_to_next_leaf(
        &mut self,
        top: WalkTop<K, V, S>,
        leave: &mut impl FnMut(Node<K, V, S>),
    ) -> bool {
        self.ensure_started::<FirstEdge>(top);
        self.cross_to_next_leaf(leave).is_some()
    }

    /// Goes from the end of the cursor's leaf to the start of the next leaf,
    /// handing `leave` each node it climbs out of, as [`Self::next`] does.
    /// Returns the internal node and index of the separator it crossed;
    /// `None`, staying put, at the last leaf.
    fn cross_to_next_leaf(
        &mut self,
        leave: &mut impl FnMut(Node<K, V, S>),
    ) -> Option<(Internal<K, V, S>, usize)> {
        let depth = (0..self.stack.depth())
            .rev()
            .find(|&depth| self.stack.index_at(depth) < self.stack.internal_at(depth).len())?;
        self.ascend_leaving(depth, leave);
        let (node, index) = (self.stack.internal_at(depth), self.stack.index());
        self.stack.set_index(index + 1);
        self.stack.descend_to_leaf::<FirstEdge>();
        Some((node, index))
    }

    /// Climbs the node stack from the cursor's leaf to `depth`, above it. It
    /// hands `leave` each node it climbs out of, once the node is off the
    /// node stack: the leaf, then the internal nodes above it.
    fn ascend_leaving(&mut self, depth: usize, leave: &mut impl FnMut(Node<K, V, S>)) {
        let (leaf, _) = self.leaf_edge();
        self.stack.ascend();
        leave(Node::Leaf(leaf));
        while self.stack.depth() > depth {
            let node = self.stack.internal_at(self.stack.depth());
            self.stack.ascend();
            leave(Node::Internal(node));
        }
    }

    /// The same position, with shared access for as long as `self` is
    /// borrowed.
    fn reborrow(&self) -> Cursor<Immut<'_>, K, V, S> {
        Cursor {
            stack: self.stack.reborrow(),
        }
    }
}

/// The top node of a walk's tree, where each end starts: a tagged pointer,
/// 8 bytes, where a [`Node`] takes 16.
struct WalkTop<K, V, S> {
    node: NodePtr<K, V, S>,
}

impl<K, V, S> Clone for WalkTop<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for WalkTop<K, V, S> {}

impl<K, V, S> WalkTop<K, V, S> {
    /// The top of the tree in `root_page`: its root node.
    fn of_root_page(root_page: RootPage<K, V, S>) -> Self
    where
        S: InlineKeyStrategy<K>,
    {
        Self {
            node: root_page.node().node_ptr(),
        }
    }

    #[inline(always)]
    fn node(self) -> Node<K, V, S> {
        Node::at(self.node)
    }
}

/// The two ends of a walk over the tree under `top`.
struct Ends<B, K, V, S> {
    top: WalkTop<K, V, S>,
    front: Cursor<B, K, V, S>,
    back: Cursor<B, K, V, S>,
}

impl<B: CopyableBorrow, K, V, S> Clone for Ends<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for Ends<B, K, V, S> {}

// SAFETY: the ends have the access their borrow type gives to the tree, as
// their node stacks do, and nothing more: see the `Send` and `Sync` impls of
// `NodeStack`, whose bounds these are.
unsafe impl<B, K, V, S> Send for Ends<B, K, V, S> where NodeStack<B, K, V, S>: Send {}
// SAFETY: as for `Send`.
unsafe impl<B, K, V, S> Sync for Ends<B, K, V, S> where NodeStack<B, K, V, S>: Sync {}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> Ends<B, K, V, S> {
    /// Ends that have not started, in the tree under `top`.
    fn unstarted(top: WalkTop<K, V, S>) -> Self {
        Self {
            top,
            front: Cursor::unstarted(),
            back: Cursor::unstarted(),
        }
    }

    /// The same ends, with shared access for as long as `self` is borrowed.
    fn reborrow(&self) -> Ends<Immut<'_>, K, V, S> {
        Ends {
            top: self.top,
            front: self.front.reborrow(),
            back: self.back.reborrow(),
        }
    }
}

/// A borrowed whole-map walk: a range walk over all of it, with an exact count
/// of the entries left, which steps from either end take off.
pub(crate) struct BorrowedWalk<B, K, V, S> {
    range: RangeWalk<B, K, V, S>,
    length: usize,
}

impl<K, V, S> Clone for BorrowedWalk<Immut<'_>, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            range: self.range.clone(),
            length: self.length,
        }
    }
}

impl<B: WalkBorrow<K, V>, K, V, S: InlineKeyStrategy<K>> BorrowedWalk<B, K, V, S> {
    pub(crate) const EMPTY: Self = Self {
        range: RangeWalk::EMPTY,
        length: 0,
    };

    /// A lazy walk over the `length` entries of the tree in `root_page`, if
    /// any. It does not search for either end, so constructing it needs no
    /// `Ord` bound.
    pub(crate) fn new(root_page: Option<RootPage<K, V, S>>, length: usize) -> Self {
        Self {
            range: RangeWalk {
                ends: root_page.map(|root_page| Ends::unstarted(WalkTop::of_root_page(root_page))),
                run: None,
            },
            length,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.length
    }

    pub(crate) fn next(&mut self) -> Option<B::Item> {
        let item = self.range.next()?;
        self.length -= 1;
        Some(item)
    }

    pub(crate) fn next_back(&mut self) -> Option<B::Item> {
        let item = self.range.next_back()?;
        self.length -= 1;
        Some(item)
    }

    pub(crate) fn fold<A>(self, accumulator: A, fold: impl FnMut(A, B::Item) -> A) -> A {
        self.range.fold(accumulator, fold)
    }

    pub(crate) fn rfold<A>(self, accumulator: A, fold: impl FnMut(A, B::Item) -> A) -> A {
        self.range.rfold(accumulator, fold)
    }

    pub(crate) fn reborrow(&self) -> BorrowedWalk<Immut<'_>, K, V, S> {
        BorrowedWalk {
            range: self.range.reborrow(),
            length: self.length,
        }
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> BorrowedWalk<Immut<'a>, K, V, S> {
    /// The keys after the front end, up to its leaf's end or the back end,
    /// whichever comes first. Where the front end crosses into another leaf,
    /// the next key alone: a separator or the new leaf's first. The walk
    /// moves past all of them at once, so a loop can visit them without it.
    /// `None` once the ends meet.
    pub(crate) fn next_key_run(&mut self) -> Option<KeyRun<'a, K>> {
        let run = self.range.next_key_run()?;
        self.length -= run.len();
        Some(run)
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

impl<B: WalkBorrow<K, V>, K, V, S: InlineKeyStrategy<K>> FullWalk<B, K, V, S> {
    /// A walk over the `length` entries of the tree in `root_page`, if any.
    fn new(root_page: Option<RootPage<K, V, S>>, length: usize) -> Self {
        Self::from_top(root_page.map(WalkTop::of_root_page), length)
    }

    /// A walk over the `length` entries of the tree under `top`, if any.
    fn from_top(top: Option<WalkTop<K, V, S>>, length: usize) -> Self {
        Self {
            ends: top.map(Ends::unstarted),
            length,
        }
    }

    /// The entries left between the ends.
    fn len(&self) -> usize {
        self.length
    }

    /// The entry after the front end, which moves past it, if any is left.
    pub(crate) fn next(&mut self) -> Option<B::Item> {
        self.next_leaving(&mut |_| {})
    }

    /// [`Self::next`], handing `leave` the nodes the front end goes up out
    /// of, which neither end comes back to (module safety argument).
    fn next_leaving(&mut self, leave: &mut impl FnMut(Node<K, V, S>)) -> Option<B::Item> {
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        let ends = self.ends.as_mut()?;
        // SAFETY: an entry was left between the ends, so the front end moves
        // past one the back end has not (module safety argument).
        unsafe { ends.front.next(ends.top, leave) }
    }

    /// Moves back one entry, handing `leave` the nodes the back end goes
    /// up out of, which neither end comes back to (module safety argument).
    fn next_back_leaving(&mut self, leave: &mut impl FnMut(Node<K, V, S>)) -> Option<B::Item> {
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        let ends = self.ends.as_mut()?;
        // SAFETY: an entry was left between the ends, so the back end moves
        // past one the front end has not (module safety argument).
        unsafe { ends.back.next_back(ends.top, leave) }
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed.
    fn reborrow(&self) -> FullWalk<Immut<'_>, K, V, S> {
        FullWalk {
            ends: self.ends.as_ref().map(Ends::reborrow),
            length: self.length,
        }
    }
}

/// A walk over the entries between two bounds, which stops where its ends
/// meet. Bounded ends start by searching when the walk is made. Unbounded
/// ends start at the tree's first or last position only when consumed.
pub(crate) struct RangeWalk<B, K, V, S> {
    /// `None` for a map without a tree.
    ends: Option<Ends<B, K, V, S>>,
    /// The run of the end that moved last, within its leaf, bounded by the
    /// other end where that is in the same leaf. Its cursor's node stack may
    /// lag behind its progress within this leaf; `finish_run` reconciles them
    /// before any operation reads that node stack or moves the opposite end.
    run: Option<RangeLeafRun<B, K, V, S>>,
}

/// A run of one leaf's entries between a range walk's ends, with the
/// walk's borrow.
struct RangeLeafRun<B, K, V, S> {
    entries: LeafRun<K, V, S>,
    reverse: bool,
    _borrow: PhantomData<B>,
}

impl<B: CopyableBorrow, K, V, S> Clone for RangeLeafRun<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for RangeLeafRun<B, K, V, S> {}

// SAFETY: the run's pointers give the access the walk's borrow type gives
// to the leaf they point into, and nothing more: see the `Send` and `Sync`
// impls of `NodeStack`, whose bounds these are.
unsafe impl<B, K, V, S> Send for RangeLeafRun<B, K, V, S> where NodeStack<B, K, V, S>: Send {}
// SAFETY: as for `Send`.
unsafe impl<B, K, V, S> Sync for RangeLeafRun<B, K, V, S> where NodeStack<B, K, V, S>: Sync {}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> RangeLeafRun<B, K, V, S> {
    /// The chosen cursor is attached. The other cursor either bounds this
    /// run in the same leaf, or cannot be reached before a leaf boundary.
    fn new<End: NodeEnd>(ends: &Ends<B, K, V, S>) -> Self {
        let (cursor, other) = if End::IS_LAST {
            (&ends.back, &ends.front)
        } else {
            (&ends.front, &ends.back)
        };
        let (leaf, index) = cursor.leaf_edge();
        let mut limit = if End::IS_LAST { 0 } else { leaf.len() };
        if other.stack.is_attached() {
            let (other_leaf, other_index) = other.leaf_edge();
            if leaf.node_ptr().is(other_leaf.node_ptr()) {
                limit = other_index;
            }
        }
        let interval = if End::IS_LAST {
            limit..index
        } else {
            index..limit
        };
        Self {
            entries: LeafRun::new(leaf, interval),
            reverse: End::IS_LAST,
            _borrow: PhantomData,
        }
    }

    fn next<End: NodeEnd>(&mut self) -> Option<EntrySlots<K, V>> {
        if End::IS_LAST {
            self.entries.next_back()
        } else {
            self.entries.next()
        }
    }
}

impl<K, V, S> Clone for RangeWalk<Immut<'_>, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            ends: self.ends,
            run: self.run,
        }
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> RangeWalk<Immut<'a>, K, V, S> {
    /// See [`BorrowedWalk::next_key_run`].
    fn next_key_run(&mut self) -> Option<KeyRun<'a, K>> {
        if let Some(run) = &mut self.run
            && !run.reverse
        {
            // SAFETY: the run's keys are shared for the walk's borrow, `'a`.
            let keys = unsafe { run.entries.take_keys() };
            if keys.len() > 0 {
                return Some(keys);
            }
        }
        let (key, _) = self.step_outside_run::<FirstEdge>()?;
        Some(KeyRun::one(key))
    }
}

impl<B: WalkBorrow<K, V>, K, V, S: InlineKeyStrategy<K>> RangeWalk<B, K, V, S> {
    /// A walk over no entries.
    pub(crate) const EMPTY: Self = Self {
        ends: None,
        run: None,
    };

    /// A walk over the entries of the tree in `root_page` within the bounds,
    /// with std's panics for bounds out of order (D1).
    ///
    /// # Panics
    ///
    /// If `start > end`, if `start == end` and both are excluded, or if `Ord`
    /// is so inconsistent that the start is found after the end.
    pub(crate) fn new<Q>(root_page: RootPage<K, V, S>, start: Bound<&Q>, end: Bound<&Q>) -> Self
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
        // node stacks, which a copy right after would wait on
        // (`node_stack.rs`).
        let (top, shared) = (WalkTop::of_root_page(root_page), root_page.shared_prefix());
        let mut walk = Self {
            ends: Some(Ends::unstarted(top)),
            run: None,
        };
        let ends = walk.ends.as_mut().expect("just made");
        if let Some((key, side)) = KeySide::of_start(start) {
            ends.front
                .start_beside_key(top, &SearchKey::new(key), side, shared);
        }
        if let Some((key, side)) = KeySide::of_end(end) {
            ends.back
                .start_beside_key(top, &SearchKey::new(key), side, shared);
        }
        if ends.front.stack.is_attached()
            && ends.back.stack.is_attached()
            && ends.front.stack.cmp_position(&ends.back.stack) == Ordering::Greater
        {
            panic!("Ord is ill-defined in BTreeMap range");
        }
        walk
    }

    /// Starts the `End` end, then returns the ends, or `None` once they have
    /// met. Starting first matters when a walk switches direction after
    /// exhausting a range whose other end is unbounded and has not started.
    fn ends_apart<End: NodeEnd>(&mut self) -> Option<&mut Ends<B, K, V, S>> {
        let ends = self.ends.as_mut()?;
        let top = ends.top;
        let cursor = if End::IS_LAST {
            &mut ends.back
        } else {
            &mut ends.front
        };
        cursor.ensure_started::<End>(top);
        if ends.front.stack.is_at(&ends.back.stack) {
            None
        } else {
            Some(ends)
        }
    }

    /// The entry after the front end, which moves past it, if any is left.
    pub(crate) fn next(&mut self) -> Option<B::Item> {
        self.step::<FirstEdge>()
    }

    /// The entry before the back end, which moves back past it, if any is
    /// left.
    pub(crate) fn next_back(&mut self) -> Option<B::Item> {
        self.step::<LastEdge>()
    }

    #[inline]
    fn step<End: NodeEnd>(&mut self) -> Option<B::Item> {
        if let Some(run) = &mut self.run
            && run.reverse == End::IS_LAST
            && let Some(entry) = run.next::<End>()
        {
            // SAFETY: the run stays between the ends and advances once
            // per entry. Moving the other end first discards this run.
            return Some(unsafe { B::item(entry) });
        }
        self.step_outside_run::<End>()
    }

    /// Reconciles a partly consumed run before reading either node stack. The
    /// cursor has stayed in the same leaf, so only its leaf index changed.
    fn finish_run(&mut self) {
        if let Some(run) = self.run.take() {
            let ends = self.ends.as_mut().expect("a run belongs to a tree");
            let cursor = if run.reverse {
                &mut ends.back
            } else {
                &mut ends.front
            };
            // SAFETY: a cached run never changes its cursor's leaf; this
            // is the same allocation used to bound and start the run.
            let index = unsafe { run.entries.edge_index(cursor.leaf_edge().0, run.reverse) };
            cursor.set_leaf_index(index);
        }
    }

    /// Attaches, changes direction, or crosses a leaf boundary once per
    /// run. The ordinary step within a leaf never needs either node stack.
    #[inline(never)]
    fn step_outside_run<End: NodeEnd>(&mut self) -> Option<B::Item> {
        self.finish_run();
        let ends = self.ends_apart::<End>()?;
        let mut run = RangeLeafRun::new::<End>(ends);
        if let Some(entry) = run.next::<End>() {
            self.run = Some(run);
            // SAFETY: the ends are apart, and this newly bounded run
            // contains only entries neither end has handed out.
            return Some(unsafe { B::item(entry) });
        }
        // SAFETY: the ends are apart but the leaf run is empty, so its
        // cursor stands at a leaf boundary. Any separator crossed is
        // still between the ends; an unbounded end stops at the tree edge.
        let item = unsafe {
            if End::IS_LAST {
                ends.back.next_back_separator(&mut |_| {})
            } else {
                ends.front.next_separator(&mut |_| {})
            }
        }?;
        self.run = Some(RangeLeafRun::new::<End>(ends));
        Some(item)
    }

    pub(crate) fn fold<A>(self, accumulator: A, fold: impl FnMut(A, B::Item) -> A) -> A {
        self.fold_from::<FirstEdge, A>(accumulator, fold)
    }

    pub(crate) fn rfold<A>(self, accumulator: A, fold: impl FnMut(A, B::Item) -> A) -> A {
        self.fold_from::<LastEdge, A>(accumulator, fold)
    }

    /// Checks whether the other end is in this leaf once per run. When it
    /// is, its edge bounds the final run, including an empty range.
    fn fold_from<End: NodeEnd, A>(
        mut self,
        mut accumulator: A,
        mut fold: impl FnMut(A, B::Item) -> A,
    ) -> A {
        self.finish_run();
        let Some(ends) = self.ends.as_mut() else {
            return accumulator;
        };
        ends.front.ensure_started::<FirstEdge>(ends.top);
        ends.back.ensure_started::<LastEdge>(ends.top);
        let (cursor, other) = if End::IS_LAST {
            (&mut ends.back, &ends.front)
        } else {
            (&mut ends.front, &ends.back)
        };
        let (other_leaf, other_index) = other.leaf_edge();
        loop {
            let (leaf, index) = cursor.leaf_edge();
            let same_leaf = leaf.node_ptr().is(other_leaf.node_ptr());
            let limit = if same_leaf {
                other_index
            } else if End::IS_LAST {
                0
            } else {
                leaf.len()
            };
            let count = if End::IS_LAST {
                index - limit
            } else {
                limit - index
            };
            // SAFETY: both ends are attached and in order. This run stops
            // at either the leaf boundary or the other end, whichever
            // comes first, so it visits only entries between the ends.
            accumulator = unsafe { cursor.fold_leaf::<End, A>(count, accumulator, &mut fold) };
            if same_leaf {
                break;
            }
            // SAFETY: the ends are in different leaves. The separator
            // across this boundary remains between them.
            let item = unsafe {
                if End::IS_LAST {
                    cursor.next_back_separator(&mut |_| {})
                } else {
                    cursor.next_separator(&mut |_| {})
                }
            }
            .expect("different leaves have a separator between them");
            accumulator = fold(accumulator, item);
        }
        accumulator
    }

    /// The entries left, with shared access for as long as `self` is
    /// borrowed.
    pub(crate) fn reborrow(&self) -> RangeWalk<Immut<'_>, K, V, S> {
        RangeWalk {
            ends: self.ends.as_ref().map(Ends::reborrow),
            run: self.run.map(|run| RangeLeafRun {
                entries: run.entries,
                reverse: run.reverse,
                _borrow: PhantomData,
            }),
        }
    }
}

/// A walk that takes a tree apart. It moves every entry out, in key order
/// from either end. It frees each node once its ends are done with it
/// (module safety argument), giving its page back to `alloc`, the allocator
/// it came from. Nothing shifts and nothing is rebalanced: nothing but the
/// walk sees the tree again.
///
/// Dropping it drops the entries left between its ends in ascending order,
/// each key before its value, as std's `BTreeMap` does (D1), then frees the
/// nodes left. If one of their `Drop`s panics, the rest are still dropped,
/// in the same order, and every node freed, before the panic goes on; a
/// second panic aborts, as any panic during unwinding does.
pub(crate) struct DyingWalk<K, V, S: InlineKeyStrategy<K>, A: Allocator> {
    walk: FullWalk<Dying<K, V>, K, V, S>,
    alloc: A,
    /// Whether the tree's top node is in a root page, which is freed as one.
    top_in_root_page: bool,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> DyingWalk<K, V, S, A> {
    /// A walk taking apart the tree in `root_page`, of `length` entries, if
    /// any.
    ///
    /// A walk that only its front moves needs no count: given `usize::MAX`,
    /// it ends where the tree does.
    pub(crate) fn new(root_page: Option<RootPage<K, V, S>>, length: usize, alloc: A) -> Self {
        Self {
            walk: FullWalk::new(root_page, length),
            alloc,
            top_in_root_page: true,
        }
    }

    /// A walk taking apart the nodes under `top`, on plain pages: what is
    /// left of a tree being made (`tree/clone.rs`).
    pub(in crate::tree) fn of_subtree(top: Node<K, V, S>, alloc: A) -> Self {
        let top = WalkTop {
            node: top.node_ptr(),
        };
        Self {
            walk: FullWalk::from_top(Some(top), usize::MAX),
            alloc,
            top_in_root_page: false,
        }
    }

    /// The entries left between the ends.
    pub(crate) fn len(&self) -> usize {
        self.walk.len()
    }

    /// Moves out the entry after the front end, if any is left.
    pub(crate) fn next(&mut self) -> Option<(K, V)> {
        let alloc = &self.alloc;
        self.walk.next_leaving(&mut |node| {
            // SAFETY: the walk is done with every node it leaves (module
            // safety argument), so each is freed once.
            unsafe { Self::release(node, alloc) };
        })
    }

    /// Moves out the entry before the back end, if any is left.
    pub(crate) fn next_back(&mut self) -> Option<(K, V)> {
        let alloc = &self.alloc;
        self.walk.next_back_leaving(&mut |node| {
            // SAFETY: as for `next`.
            unsafe { Self::release(node, alloc) };
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
        if !needs_drop::<K>() && !needs_drop::<V>() && !ends.back.stack.is_attached() {
            let alloc = &self.alloc;
            let mut leave = |node: Node<K, V, S>| {
                // SAFETY: as for `next`.
                unsafe { Self::release(node, alloc) };
            };
            while ends.front.skip_to_next_leaf(ends.top, &mut leave) {}
            self.walk.length = 0;
        } else {
            while let Some(entry) = self.next() {
                drop(entry);
            }
        }
        self.free_remaining();
    }

    /// Gives `node`'s page back to `alloc`, and a store's leaf's heap pages
    /// with it (`node/store.rs`).
    ///
    /// # Safety
    ///
    /// As for [`Node::free`]: the node is on a page of its kind, and nothing
    /// uses it again.
    unsafe fn release(node: Node<K, V, S>, alloc: &A) {
        Self::release_heap(node, alloc);
        // SAFETY: as the caller promises.
        unsafe { node.free() }.deallocate(alloc);
    }

    /// Gives a store's leaf's heap pages back to `alloc`, as the leaf goes;
    /// nothing for other nodes.
    fn release_heap(node: Node<K, V, S>, alloc: &A) {
        #[cfg(feature = "store")]
        if let Node::Leaf(leaf) = node
            && NodePtr::<K, V, S>::is_in_store()
        {
            leaf.free_heap(alloc);
        }
        let _ = (node, alloc);
    }

    /// Frees the nodes the walk has not freed, once no entry is left between
    /// its ends. They are those on the node stack of an end that moved, from
    /// the bottom up, or the top alone if neither did.
    fn free_remaining(&mut self) {
        let Some(ends) = self.walk.ends.take() else {
            return;
        };
        let stack = if ends.front.stack.is_attached() {
            ends.front.stack
        } else if ends.back.stack.is_attached() {
            ends.back.stack
        } else {
            let mut stack = NodeStack::detached();
            stack.attach(ends.top.node());
            stack
        };
        for depth in (1..=stack.depth()).rev() {
            // SAFETY: with no entry left between the ends, every node off this
            // node stack is freed, and every entry on it moved out or skipped
            // for having no drop glue (module safety argument). Each node here
            // is freed once, after its children.
            unsafe { Self::release(stack.node_at(depth), &self.alloc) };
        }
        let top = stack.node_at(0);
        Self::release_heap(top, &self.alloc);
        // SAFETY: as for the nodes below; the top goes last, in its root page
        // if it has one, which nothing reaches anymore.
        let top_page: PageByType<K, V, S> = unsafe {
            if self.top_in_root_page {
                RootPage::of_node(top).free().into()
            } else {
                top.free()
            }
        };
        top_page.deallocate(&self.alloc);
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for DyingWalk<K, V, S, A> {
    fn drop(&mut self) {
        let rest = DropRest(self);
        // A panicking `Drop` unwinds through `rest`, whose own `Drop` goes on
        // with what is left.
        rest.0.drop_rest();
    }
}

/// A guard that drops the rest of a [`DyingWalk`] being dropped, if an
/// entry's `Drop` panics.
struct DropRest<'w, K, V, S: InlineKeyStrategy<K>, A: Allocator>(&'w mut DyingWalk<K, V, S, A>);

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for DropRest<'_, K, V, S, A> {
    fn drop(&mut self) {
        self.0.drop_rest();
    }
}
