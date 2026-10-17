//! Visiting a range of a latched tree's entries, in key order or in
//! reverse, one leaf at a time (`doc/09-concurrency.md`, Scans): the
//! caller's closure runs on each entry while its leaf is latched shared.
//!
//! - **The first leaf** is where the bound the scan starts from belongs
//!   (`descent.rs`), or the tree's first leaf, down first edges, or its last
//!   in reverse, down last edges.
//! - **The next leaf**, the held leaf's neighbor on the scan's side, right
//!   in key order, left in reverse, is found from the internal nodes the
//!   descent kept: the nearest one above with an edge on that side of the
//!   path, then the edges nearest the held leaf down, each read
//!   optimistically and checked hand over hand. The scan latches it before
//!   it lets go of the leaf it has, which can't split meanwhile, and it is
//!   that leaf's neighbor if every node kept from there down is at the
//!   version read.
//! - **Otherwise** the scan searches again for the last key it visited,
//!   still holding that key's leaf, so that the key stays where it is, and
//!   goes on past it: no key is cloned.
//!
//! No deadlock: a thread waits for a leaf while it holds one only in a
//! scan, to latch it shared, which waits only while a thread holds it
//! exclusively; and a thread holding leaves exclusively takes every other
//! node without waiting (`held.rs`), but for a page off a free list, which
//! a thread that reached it through a stale pointer holds a moment, waiting
//! for nothing (`pages.rs`). So scans in either direction, and writers,
//! form no cycle.
//!
//! A scan is not a snapshot: an entry inserted or removed while it runs may
//! or may not be visited. An entry present for the whole scan is visited
//! exactly once, as the scan visits every entry of a leaf while it holds it,
//! and moves only to keys past it.
//!
//! With each entry the closure gets its leaf's [`LeafStamp`]: whether a
//! thread has changed the leaf since tells a caller that copied entries
//! whether its copies are still the leaf's (`map/range.rs`). A scan
//! that the closure stops can keep where it stopped ([`ScanResume`]): the
//! leaf, its stamp and the place in it. The next scan latches that leaf
//! again and, if its stamp is current, goes on from there without a
//! descent. Past that leaf it descends once, from its last key, unless the
//! resume keeps the path too, in a box its owner asks for once it scans
//! on: a cursor that only seeks stays a few words.

use alloc::boxed::Box;
use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::ops::{Bound, ControlFlow};
use core::{mem, ptr};

use super::super::node::Node;
use super::super::node::inline_keys::SearchKey;
use super::super::node::internal::Internal;
use super::super::node::latch::{Latched, NodeLatch, Version};
use super::super::node::leaf::Leaf;
use super::super::node::optimistic::Routed;
use super::LatchedTree;
use super::descent::{DescentPath, HeldLeaf, PathLevel, Shared, TreePath};
use crate::inline_key::InlineKeyStrategy;

/// A leaf as a scan saw it: its latch, and the version it was at while the
/// scan held it. Equal stamps are of one leaf at one version.
#[derive(Clone, Copy)]
pub(crate) struct LeafStamp<'t> {
    latch: &'t NodeLatch,
    version: Version,
}

impl<'t> LeafStamp<'t> {
    /// The stamp of `leaf`, which the caller holds.
    fn of<K, V, S: InlineKeyStrategy<K>>(leaf: Leaf<K, V, Latched<S>>) -> Self {
        let latch = leaf.latch();
        Self {
            latch,
            version: latch.read_version(),
        }
    }

    /// Whether no thread has changed the leaf since: what the scan read of
    /// it, the leaf holds still.
    pub(crate) fn is_current(self) -> bool {
        self.latch.is_unchanged(self.version)
    }
}

impl PartialEq for LeafStamp<'_> {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.latch, other.latch) && self.version == other.version
    }
}

/// A place between a leaf's entries, which run from its head, if it has
/// one (D17), through its keys: how many entries come before it. A scan
/// in key order goes on from the entry after it, one in reverse from the
/// entry before it.
#[derive(Clone, Copy)]
struct LeafCursor(usize);

/// The order a scan visits entries in: [`Ascending`] or [`Descending`]. A
/// type, so that a scan's code holds no choice between them.
trait ScanOrder {
    /// How a key that the scan meets after a bound compares with it.
    const PAST: KeyOrdering;

    /// The bound the scan starts from, and the one it stops at, of a range
    /// from `start` to `end`.
    fn from_to<T>(start: Bound<T>, end: Bound<T>) -> (Bound<T>, Bound<T>);

    /// The edge of an internal node with `len` separators where the scan
    /// enters the node's entries: its first, or its last.
    fn entered_edge(len: usize) -> usize;

    /// The edge next to `routed`'s, on the scan's side, if the node has
    /// one.
    fn neighbor_edge(routed: Routed) -> Option<usize>;

    /// Where the scan starts in a leaf with `entry_count` entries that it
    /// visits whole.
    fn whole_leaf(entry_count: usize) -> LeafCursor;

    /// Where the scan starts from a key at `position` among a leaf's
    /// entries, the leaf's entry there if `found`, or the place it would
    /// take otherwise: visiting that entry if `inclusive`.
    fn at_key(position: usize, found: bool, inclusive: bool) -> LeafCursor;

    /// The entries of `leaf`, which the caller holds, that the scan visits
    /// from `cursor` on, in its order.
    fn entries<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
        cursor: LeafCursor,
    ) -> impl Iterator<Item = (&'a K, &'a V)>;

    /// Every entry of `leaf`, which the caller holds, in the scan's order:
    /// [`Self::entries`] from [`Self::whole_leaf`], but where no key's place
    /// waits on the header's head flag. A computed first index would make
    /// every entry load in the leaf wait for the header's line, and the
    /// hardware prefetcher with them: one memory latency a leaf, a quarter
    /// of a walk of 17-entry leaves (Tonbo's memtable, 2026-10-17).
    fn whole_entries<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
    ) -> impl Iterator<Item = (&'a K, &'a V)>;

    /// The key of `leaf`, which the caller holds, that the scan visits
    /// last: `None` for a leaf without one, as only an empty tree's root
    /// is.
    fn last_visited<'a, K, V, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
    ) -> Option<&'a K>;

    /// Where the scan is in a leaf once it visited `visited` entries from
    /// `cursor`.
    fn past(cursor: LeafCursor, visited: usize) -> LeafCursor;
}

/// Key order.
enum Ascending {}

/// Reverse key order.
enum Descending {}

impl ScanOrder for Ascending {
    const PAST: KeyOrdering = KeyOrdering::Greater;

    fn from_to<T>(start: Bound<T>, end: Bound<T>) -> (Bound<T>, Bound<T>) {
        (start, end)
    }

    fn entered_edge(_len: usize) -> usize {
        0
    }

    fn neighbor_edge(routed: Routed) -> Option<usize> {
        (routed.edge < routed.len).then_some(routed.edge + 1)
    }

    fn whole_leaf(_entry_count: usize) -> LeafCursor {
        LeafCursor(0)
    }

    fn at_key(position: usize, found: bool, inclusive: bool) -> LeafCursor {
        LeafCursor(position + usize::from(found && !inclusive))
    }

    fn entries<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
        cursor: LeafCursor,
    ) -> impl Iterator<Item = (&'a K, &'a V)> {
        let has_head = leaf.has_head();
        let head = (has_head && cursor.0 == 0).then(|| (leaf.head_key(), leaf.head_value()));
        let first_key = cursor.0 - usize::from(has_head && cursor.0 > 0);
        let keys = (first_key..leaf.len()).map(move |index| leaf.key_value(index));
        head.into_iter().chain(keys)
    }

    fn whole_entries<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
    ) -> impl Iterator<Item = (&'a K, &'a V)> {
        let head = leaf
            .has_head()
            .then(|| (leaf.head_key(), leaf.head_value()));
        // SAFETY: each index is below the length of the leaf, which the
        // caller holds, so that it doesn't change.
        let keys = (0..leaf.len()).map(move |index| unsafe { leaf.key_value_unchecked(index) });
        head.into_iter().chain(keys)
    }

    fn last_visited<'a, K, V, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
    ) -> Option<&'a K> {
        match leaf.len().checked_sub(1) {
            Some(last) => Some(leaf.key(last)),
            None => leaf.has_head().then(|| leaf.head_key()),
        }
    }

    fn past(cursor: LeafCursor, visited: usize) -> LeafCursor {
        LeafCursor(cursor.0 + visited)
    }
}

impl ScanOrder for Descending {
    const PAST: KeyOrdering = KeyOrdering::Less;

    fn from_to<T>(start: Bound<T>, end: Bound<T>) -> (Bound<T>, Bound<T>) {
        (end, start)
    }

    fn entered_edge(len: usize) -> usize {
        len
    }

    fn neighbor_edge(routed: Routed) -> Option<usize> {
        routed.edge.checked_sub(1)
    }

    fn whole_leaf(entry_count: usize) -> LeafCursor {
        LeafCursor(entry_count)
    }

    fn at_key(position: usize, found: bool, inclusive: bool) -> LeafCursor {
        LeafCursor(position + usize::from(found && inclusive))
    }

    fn entries<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
        cursor: LeafCursor,
    ) -> impl Iterator<Item = (&'a K, &'a V)> {
        let has_head = leaf.has_head();
        let head = (has_head && cursor.0 > 0).then(|| (leaf.head_key(), leaf.head_value()));
        let key_end = cursor.0 - usize::from(has_head && cursor.0 > 0);
        let keys = (0..key_end).rev().map(move |index| leaf.key_value(index));
        keys.chain(head)
    }

    fn whole_entries<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
    ) -> impl Iterator<Item = (&'a K, &'a V)> {
        let head = leaf
            .has_head()
            .then(|| (leaf.head_key(), leaf.head_value()));
        // SAFETY: each index is below the length of the leaf, which the
        // caller holds, so that it doesn't change.
        let keys = (0..leaf.len())
            .rev()
            .map(move |index| unsafe { leaf.key_value_unchecked(index) });
        keys.chain(head)
    }

    fn last_visited<'a, K, V, S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
    ) -> Option<&'a K> {
        if leaf.has_head() {
            Some(leaf.head_key())
        } else {
            (leaf.len() > 0).then(|| leaf.key(0))
        }
    }

    fn past(cursor: LeafCursor, visited: usize) -> LeafCursor {
        LeafCursor(cursor.0 - visited)
    }
}

impl LeafCursor {
    /// Where a scan in order `O` starts in `leaf` from `key`, visiting the
    /// key's entry if `inclusive`; `at_head` if the key is the leaf's head,
    /// which the descent found.
    fn at<O, K, V, S, Q>(
        leaf: Leaf<K, V, Latched<S>>,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        at_head: bool,
        inclusive: bool,
    ) -> Self
    where
        O: ScanOrder,
        S: InlineKeyStrategy<K>,
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if at_head {
            return O::at_key(0, true, inclusive);
        }
        let before = usize::from(leaf.has_head());
        match leaf.search(key) {
            Ok(index) => O::at_key(before + index, true, inclusive),
            Err(edge) => O::at_key(before + edge, false, inclusive),
        }
    }
}

/// Where a scan the closure stopped stopped, kept for a later scan to go
/// on from: the leaf, its stamp then, and the place past the last entry
/// visited; and, once kept ([`Self::keep_path`]), the internal nodes down
/// to the leaf, in place. The later scan goes on from there only while the
/// stamp is current, the leaf unchanged.
pub(crate) struct ScanResume<'t, K, V, S: InlineKeyStrategy<K>> {
    stopped: Option<Stopped<'t, K, V, S>>,
    path: Option<Box<TreePath<K, V, S>>>,
    /// Whether `path` leads to the leaf `stopped` names.
    path_leads_there: bool,
}

/// A leaf a scan stopped in, its stamp, and the place in it.
type Stopped<'t, K, V, S> = (Leaf<K, V, Latched<S>>, LeafStamp<'t>, LeafCursor);

impl<K, V, S: InlineKeyStrategy<K>> ScanResume<'_, K, V, S> {
    pub(crate) fn new() -> Self {
        Self {
            stopped: None,
            path: None,
            path_leads_there: false,
        }
    }

    /// Keeps the path down to where a scan stops from now on, so that a
    /// scan that goes on past that leaf needs no descent: for a caller that
    /// scans on, as one box.
    pub(crate) fn keep_path(&mut self) {
        if self.path.is_none() {
            self.path = Some(Box::new(TreePath::new()));
        }
    }

    /// Forgets where the scan stopped: the next starts from its bound.
    pub(crate) fn forget(&mut self) {
        self.stopped = None;
    }
}

// SAFETY: a `ScanResume` names a leaf page of a tree that outlives it
// (`'t`), which stays a node page while the tree lives (`pages.rs`), and
// reads it only as a scan does, latched and checked by version; the entries
// it leads to are the tree's, which threads share when they are `Send` and
// `Sync`.
unsafe impl<K: Send + Sync, V: Send + Sync, S: InlineKeyStrategy<K>> Send
    for ScanResume<'_, K, V, S>
{
}

// SAFETY: as for `Send`; `&ScanResume` reads nothing.
unsafe impl<K: Send + Sync, V: Send + Sync, S: InlineKeyStrategy<K>> Sync
    for ScanResume<'_, K, V, S>
{
}

/// How visiting a leaf ended.
enum Visit<R> {
    /// The closure broke, with this, and the scan is here in the leaf.
    Broke(R, LeafCursor),
    /// A key past the bound the scan stops at.
    PastEnd,
    /// Every entry of the leaf from the cursor on was visited.
    Exhausted,
}

/// Where the scan goes after a leaf it exhausted.
enum NextLeaf<K, V, S: InlineKeyStrategy<K>> {
    /// The leaf was the tree's last on the scan's side.
    None,
    /// The leaf after it, latched shared, which the path leads to now.
    Latched(HeldLeaf<K, V, S, Shared>),
    /// A node on the way changed: search again.
    StartOver,
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Runs `f` on every entry whose key lies between `start` and `end`, in
    /// key order, each while its leaf is latched shared, with the leaf's
    /// stamp, until `f` breaks: returns what it broke with, or `None` once
    /// past the range.
    pub(crate) fn scan<'t, Q, R>(
        &'t self,
        start: Bound<&Q>,
        end: Bound<&Q>,
        f: impl FnMut(&K, &V, LeafStamp<'t>) -> ControlFlow<R>,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.scan_in::<Ascending, Q, R>(start, end, None, f)
    }

    /// [`Self::scan`] in reverse key order.
    pub(crate) fn scan_rev<'t, Q, R>(
        &'t self,
        start: Bound<&Q>,
        end: Bound<&Q>,
        f: impl FnMut(&K, &V, LeafStamp<'t>) -> ControlFlow<R>,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.scan_in::<Descending, Q, R>(start, end, None, f)
    }

    /// [`Self::scan`], going on where the scan that kept `resume` stopped,
    /// if its leaf is unchanged, else from `start`; keeping in `resume`
    /// where `f` stops this one.
    pub(crate) fn scan_resumable<'t, Q, R>(
        &'t self,
        start: Bound<&Q>,
        end: Bound<&Q>,
        resume: &mut ScanResume<'t, K, V, S>,
        f: impl FnMut(&K, &V, LeafStamp<'t>) -> ControlFlow<R>,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.scan_in::<Ascending, Q, R>(start, end, Some(resume), f)
    }

    /// [`Self::scan_resumable`] in reverse key order.
    pub(crate) fn scan_rev_resumable<'t, Q, R>(
        &'t self,
        start: Bound<&Q>,
        end: Bound<&Q>,
        resume: &mut ScanResume<'t, K, V, S>,
        f: impl FnMut(&K, &V, LeafStamp<'t>) -> ControlFlow<R>,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.scan_in::<Descending, Q, R>(start, end, Some(resume), f)
    }

    /// [`Self::scan`] in order `O`; from where `resume` says a scan
    /// stopped, if that leaf is unchanged, and keeping there where `f` stops
    /// this one.
    #[inline(always)]
    fn scan_in<'t, O: ScanOrder, Q, R>(
        &'t self,
        start: Bound<&Q>,
        end: Bound<&Q>,
        resume: Option<&mut ScanResume<'t, K, V, S>>,
        f: impl FnMut(&K, &V, LeafStamp<'t>) -> ControlFlow<R>,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let (from, to) = O::from_to(start, end);
        let mut local_path = TreePath::new();
        let Some(resume) = resume else {
            let (held, cursor) = self.scan_start::<O, Q>(from, &mut local_path)?;
            return self
                .scan_on::<O, Q, R>(held, cursor, to, &mut local_path, true, None, f)
                .0;
        };
        let ScanResume {
            stopped,
            path,
            path_leads_there,
        } = resume;
        let leads_there = mem::take(path_leads_there);
        let resumed = stopped.take().and_then(|(leaf, stamp, cursor)| {
            let held = HeldLeaf::lock(leaf);
            stamp.is_current().then_some((held, cursor))
        });
        let boxed = path.is_some();
        let path = path.as_deref_mut().unwrap_or(&mut local_path);
        let (held, cursor, path_kept) = match resumed {
            // Without the path to its leaf, a resumed scan descends again
            // past it.
            Some((held, cursor)) => (held, cursor, boxed && leads_there),
            None => {
                let (held, cursor) = self.scan_start::<O, Q>(from, path)?;
                (held, cursor, true)
            }
        };
        let (result, path_kept) =
            self.scan_on::<O, Q, R>(held, cursor, to, path, path_kept, Some(&mut *stopped), f);
        *path_leads_there = boxed && path_kept && stopped.is_some();
        result
    }

    /// The leaf where a scan in order `O` from `from` starts, latched
    /// shared, `path` keeping the internal nodes down to it, and the place
    /// in it; `None` if the range has no key in the tree.
    #[inline(always)]
    fn scan_start<O: ScanOrder, Q>(
        &self,
        from: Bound<&Q>,
        path: &mut TreePath<K, V, S>,
    ) -> Option<(HeldLeaf<K, V, S, Shared>, LeafCursor)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let at_from = match from {
            Bound::Unbounded => None,
            Bound::Included(key) | Bound::Excluded(key) => {
                let key = SearchKey::new(key);
                match self.descend::<Q, Shared, _>(&key, path) {
                    Ok((held, at_head)) => {
                        let inclusive = matches!(from, Bound::Included(_));
                        let cursor =
                            LeafCursor::at::<O, _, _, _, _>(held.leaf(), &key, at_head, inclusive);
                        Some((held, cursor))
                    }
                    // The bound comes before every key, in the scan's order.
                    Err(outside) if outside == O::PAST.reverse() => None,
                    Err(_) => return None,
                }
            }
        };
        Some(match at_from {
            Some(at_from) => at_from,
            None => {
                let held = self.descend_to_first_leaf::<O>(path);
                let cursor = O::whole_leaf(Self::entry_count(held.leaf()));
                (held, cursor)
            }
        })
    }

    /// [`Self::scan_in`] from `cursor` in `held`, which `path` leads to if
    /// `path_kept`; with whether `path` led to the leaf it stopped in.
    #[expect(
        clippy::too_many_arguments,
        reason = "a scan's state, and where to keep where it stops"
    )]
    #[inline(always)]
    fn scan_on<'t, O: ScanOrder, Q, R>(
        &'t self,
        mut held: HeldLeaf<K, V, S, Shared>,
        mut cursor: LeafCursor,
        to: Bound<&Q>,
        path: &mut TreePath<K, V, S>,
        mut path_kept: bool,
        stopped: Option<&mut Option<Stopped<'t, K, V, S>>>,
        mut f: impl FnMut(&K, &V, LeafStamp<'t>) -> ControlFlow<R>,
    ) -> (Option<R>, bool)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        // The leaves after the first are visited whole.
        let mut whole = false;
        loop {
            let leaf = held.leaf();
            let stamp = LeafStamp::of(leaf);
            let visit = if whole {
                Self::visit::<O, Q, R>(O::whole_entries(leaf), cursor, stamp, to, &mut f)
            } else {
                Self::visit::<O, Q, R>(O::entries(leaf, cursor), cursor, stamp, to, &mut f)
            };
            match visit {
                Visit::Broke(result, past) => {
                    if let Some(stopped) = stopped {
                        *stopped = Some((leaf, stamp, past));
                    }
                    return (Some(result), path_kept);
                }
                Visit::PastEnd => return (None, path_kept),
                Visit::Exhausted => {}
            }
            let next = if path_kept {
                self.next_leaf::<O>(path, &held)
            } else {
                path_kept = true;
                NextLeaf::StartOver
            };
            (held, cursor, whole) = match next {
                NextLeaf::None => return (None, path_kept),
                NextLeaf::Latched(next) => {
                    let cursor = O::whole_leaf(Self::entry_count(next.leaf()));
                    (next, cursor, true)
                }
                NextLeaf::StartOver => {
                    let Some(last) = O::last_visited(held.leaf()) else {
                        return (None, path_kept);
                    };
                    let last = SearchKey::new(Borrow::<Q>::borrow(last));
                    let (next, at_head) = self
                        .descend::<Q, Shared, _>(&last, path)
                        .unwrap_or_else(|_| unreachable!("a key in the tree shares its prefix"));
                    let cursor =
                        LeafCursor::at::<O, _, _, _, _>(next.leaf(), &last, at_head, false);
                    (next, cursor, false)
                }
            };
        }
    }

    /// How many entries `leaf`, which the caller holds, has: its head, if
    /// it has one, and its keys.
    fn entry_count(leaf: Leaf<K, V, Latched<S>>) -> usize {
        usize::from(leaf.has_head()) + leaf.len()
    }

    /// Runs `f` on `entries`, those of a leaf the caller holds from `cursor`
    /// on, in order `O`, up to `to`, with the leaf's stamp.
    fn visit<'a, 't, O: ScanOrder, Q, R>(
        entries: impl Iterator<Item = (&'a K, &'a V)>,
        cursor: LeafCursor,
        stamp: LeafStamp<'t>,
        to: Bound<&Q>,
        f: &mut impl FnMut(&K, &V, LeafStamp<'t>) -> ControlFlow<R>,
    ) -> Visit<R>
    where
        K: Borrow<Q> + 'a,
        V: 'a,
        Q: Ord + ?Sized,
    {
        let past_end = |key: &K| match to {
            Bound::Included(to) => key.borrow().cmp(to) == O::PAST,
            Bound::Excluded(to) => key.borrow().cmp(to) != O::PAST.reverse(),
            Bound::Unbounded => false,
        };
        let mut visited = 0;
        for (key, value) in entries {
            if past_end(key) {
                return Visit::PastEnd;
            }
            visited += 1;
            if let ControlFlow::Break(result) = f(key, value, stamp) {
                return Visit::Broke(result, O::past(cursor, visited));
            }
        }
        Visit::Exhausted
    }

    /// The tree's first leaf in order `O`, latched shared, with `path`
    /// keeping the internal nodes down to it.
    ///
    /// # Panics
    ///
    /// Without a tree.
    fn descend_to_first_leaf<O: ScanOrder>(
        &self,
        path: &mut TreePath<K, V, S>,
    ) -> HeldLeaf<K, V, S, Shared> {
        loop {
            path.clear();
            let root_page = self.root_page().expect("a tree to descend");
            let root = match root_page.node() {
                Node::Leaf(leaf) => {
                    let held = HeldLeaf::lock(leaf);
                    // A split that raised the tree changed the tag first.
                    if self.is_root(leaf) {
                        return held;
                    }
                    continue;
                }
                Node::Internal(internal) => internal,
            };
            let version = root.latch().read_version();
            if let Some(held) = Self::walk_down::<O>(root, version, None, path) {
                return held;
            }
        }
    }

    /// The leaf after the one `path` leads to, in order `O`, which the
    /// caller holds (`_held`): latched shared before that one is let go,
    /// and `path` leads to it then, if every node `path` keeps from the
    /// nearest one with an edge on the scan's side of it down is still at
    /// the version read (module documentation).
    fn next_leaf<O: ScanOrder>(
        &self,
        path: &mut TreePath<K, V, S>,
        _held: &HeldLeaf<K, V, S, Shared>,
    ) -> NextLeaf<K, V, S> {
        let depth = path.depth();
        let Some((level, edge)) = (0..depth)
            .rev()
            .find_map(|level| O::neighbor_edge(path.level(level).routed).map(|edge| (level, edge)))
        else {
            return NextLeaf::None;
        };
        let PathLevel { node, version, .. } = path.level(level);
        let mut next_path = TreePath::new();
        next_path.copy_levels_from(path, level);
        let Some(next) = Self::walk_down::<O>(node, version, Some(edge), &mut next_path) else {
            return NextLeaf::StartOver;
        };
        // The nodes below `node` still end at the held leaf, which can't
        // split while held: the next leaf is its neighbor.
        let unchanged = (level + 1..depth).all(|below| {
            let PathLevel { node, version, .. } = path.level(below);
            node.latch().is_unchanged(version)
        });
        if !unchanged {
            return NextLeaf::StartOver;
        }
        path.copy_from(&next_path);
        NextLeaf::Latched(next)
    }

    /// Walks down from `node`, read at `version`, through edge `edge`, or
    /// the edge where order `O` enters it, then each node below there, to a
    /// leaf, latched shared, recording the internal nodes in `path`, each
    /// checked hand over hand as the descent checks them; `None` if a check
    /// fails.
    fn walk_down<O: ScanOrder>(
        mut node: Internal<K, V, Latched<S>>,
        mut version: Version,
        mut edge: Option<usize>,
        path: &mut TreePath<K, V, S>,
    ) -> Option<HeldLeaf<K, V, S, Shared>> {
        loop {
            let len = node.racing_len();
            let edge_taken = edge.take().unwrap_or_else(|| O::entered_edge(len));
            let child = node.racing_child(edge_taken)?;
            // Followed only once the node's version vouches for it.
            if !node.latch().is_unchanged(version) {
                return None;
            }
            path.record(
                node,
                version,
                Routed {
                    edge: edge_taken,
                    at_separator: false,
                    len,
                },
            );
            match child {
                Node::Leaf(leaf) => {
                    let held = HeldLeaf::lock(leaf);
                    return node.latch().is_unchanged(version).then_some(held);
                }
                Node::Internal(child) => {
                    let child_version = child.latch().read_version();
                    if !node.latch().is_unchanged(version) {
                        return None;
                    }
                    (node, version) = (child, child_version);
                }
            }
        }
    }
}
