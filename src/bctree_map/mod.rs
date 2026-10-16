//! [`BCTreeMap`]: an ordered map that threads share, on the same nodes as
//! [`BBTreeMap`](crate::BBTreeMap) (`doc/09-concurrency.md`).

#![forbid(unsafe_code)]

mod batch;
mod range;
#[cfg(test)]
mod tests;

pub use self::range::RangeCloned;

use core::borrow::Borrow;
use core::fmt;
use core::ops::{Bound, ControlFlow, RangeBounds};

use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::latched::{LatchedTree, ReadOrInsert, Replace};

/// An ordered map that threads share, through `&self`: a B+-tree whose
/// internal nodes are read optimistically, and whose leaves are latched
/// (`doc/09-concurrency.md`).
///
/// Nothing it hands out outlives a call: an operation that exposes an entry
/// runs the caller's closure while the entry's leaf is latched, and returns
/// what the closure returned ([`Self::read`], [`Self::update`]). No user
/// takes a latch by hand, or holds a guard.
///
/// The keys need inline keys (`InlineKey`): the std integers and byte
/// strings have them under [`Auto`]; other keys implement the trait and use
/// [`WithInlineKey`](crate::WithInlineKey).
///
/// It iterates by [`Self::scan`] and [`Self::scan_rev`], which run a
/// closure on each entry of a range, in key order or in reverse, and by
/// [`Self::range_cloned`], an iterator over clones of the entries, which
/// holds nothing between calls.
///
/// # Deadlocks
///
/// A closure that calls into the same map may deadlock: [`Self::update`]'s,
/// [`Self::get_or_insert`]'s and [`Self::remove_if`]'s on any key of its
/// entry's leaf, [`Self::read`]'s and the scans' if a writer waits for that
/// leaf meanwhile.
///
/// It is `Sync` if its keys and values are `Send` and `Sync`, as threads
/// move them in, read them, change them and drop them through `&self`, and
/// its allocator is `Sync`.
pub struct BCTreeMap<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator = Global> {
    tree: LatchedTree<K, V, S>,
    /// Where the tree's pages come from and go back to.
    alloc: A,
}

impl<K, V> BCTreeMap<K, V> {
    /// Makes a new, empty `BCTreeMap`, with the default strategy, [`Auto`],
    /// and the global allocator.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under [`Auto`].
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl<K, V> Default for BCTreeMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BCTreeMap<K, V, S> {
    /// Makes a new, empty `BCTreeMap` with strategy `S`, as
    /// [`BBTreeMap::with_strategy`](crate::BBTreeMap::with_strategy) does.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under `S`.
    pub fn with_strategy(strategy: S) -> Self {
        let _ = strategy;
        Self::new_in(Global)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> BCTreeMap<K, V, S, A> {
    /// Makes a new, empty `BCTreeMap` whose pages come from `alloc`.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under `S`.
    pub fn new_in(alloc: A) -> Self {
        Self {
            tree: LatchedTree::new(),
            alloc,
        }
    }

    /// Runs `f` on the entry whose key equals `key`, while no thread
    /// changes it, and returns what `f` returned; `None` without one.
    #[inline]
    pub fn read<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.read(key, f)
    }

    /// Runs `f` on the entry whose key equals `key`, its value exclusive,
    /// and returns what `f` returned; `None` without one.
    #[inline]
    pub fn update<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &mut V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.update(key, f)
    }

    /// A clone of the value of `key`'s entry; `None` without one.
    #[inline]
    pub fn get_cloned<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        V: Clone,
    {
        self.read(key, |_, value| value.clone())
    }

    /// Whether the map holds an entry whose key equals `key`.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.read(key, |_, _| ()).is_some()
    }

    /// Runs `f` on every entry whose key lies in `range`, in key order, each
    /// while no thread changes it, until `f` breaks; returns what `f` broke
    /// with, or `None` if it never did.
    ///
    /// Not a snapshot: an entry inserted or removed while the scan runs may
    /// or may not be visited; an entry present for the whole scan is visited
    /// exactly once.
    ///
    /// # Panics
    ///
    /// As std's `BTreeMap::range`: if `range` starts after it ends, or
    /// starts and ends at the same key, both excluded.
    #[inline]
    pub fn scan<Q, R>(
        &self,
        range: impl RangeBounds<Q>,
        mut f: impl FnMut(&K, &V) -> ControlFlow<R>,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let (start, end) = Self::checked_bounds(&range);
        self.tree.scan(start, end, |key, value, _| f(key, value))
    }

    /// [`Self::scan`] in reverse key order: from the end of `range` to its
    /// start.
    ///
    /// # Panics
    ///
    /// As [`Self::scan`].
    #[inline]
    pub fn scan_rev<Q, R>(
        &self,
        range: impl RangeBounds<Q>,
        mut f: impl FnMut(&K, &V) -> ControlFlow<R>,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let (start, end) = Self::checked_bounds(&range);
        self.tree
            .scan_rev(start, end, |key, value, _| f(key, value))
    }

    /// An iterator over clones of the entries whose keys lie in `range`: in
    /// key order, and in reverse from its back ([`DoubleEndedIterator`]).
    ///
    /// It holds no latch between calls: it clones a few entries of a leaf
    /// at a time, and hands them out while no thread has changed the leaf,
    /// or else looks again past the last key it handed out. So, like a scan,
    /// it is not a snapshot: it hands out an entry other threads insert
    /// ahead of it, and no entry they removed before it got there.
    ///
    /// # Panics
    ///
    /// As [`Self::scan`].
    pub fn range_cloned<Q, R>(&self, range: R) -> RangeCloned<'_, K, V, S, A, Q, R>
    where
        K: Borrow<Q> + Clone,
        V: Clone,
        Q: Ord + ?Sized,
        R: RangeBounds<Q>,
    {
        Self::checked_bounds(&range);
        RangeCloned::new(self, range)
    }

    /// `range`'s bounds.
    ///
    /// # Panics
    ///
    /// As std's `BTreeMap::range`: if `range` starts after it ends, or
    /// starts and ends at the same key, both excluded.
    fn checked_bounds<Q>(range: &impl RangeBounds<Q>) -> (Bound<&Q>, Bound<&Q>)
    where
        Q: Ord + ?Sized,
    {
        let (start, end) = (range.start_bound(), range.end_bound());
        match (start, end) {
            (Bound::Excluded(start), Bound::Excluded(end)) if start == end => {
                panic!("range start and end are equal and excluded in BCTreeMap")
            }
            (
                Bound::Included(start) | Bound::Excluded(start),
                Bound::Included(end) | Bound::Excluded(end),
            ) if start > end => panic!("range start is greater than range end in BCTreeMap"),
            _ => {}
        }
        (start, end)
    }

    /// Inserts `key` and `value`, and returns `None`; or, if the map holds
    /// an equal key, keeps that key, drops `key`, and returns the value it
    /// replaces, as std's `BTreeMap::insert` does.
    #[inline]
    pub fn insert(&self, key: K, value: V) -> Option<V>
    where
        K: Ord,
    {
        self.tree.insert(key, value, Replace, &self.alloc)
    }

    /// Runs `f` on the entry whose key equals `key`, inserting `key` and
    /// `value` first if the map holds none, and returns what `f` returned:
    /// on the entry as the map stores it, and drops `key` and `value` if it
    /// held one already. `f` runs while no other thread reads or changes the
    /// entry; on a new one, just before it goes in.
    #[inline]
    pub fn get_or_insert<R>(&self, key: K, value: V, f: impl FnOnce(&K, &V) -> R) -> R
    where
        K: Ord,
    {
        self.tree.insert(key, value, ReadOrInsert(f), &self.alloc)
    }

    /// Removes the entry whose key equals `key`, and returns its value;
    /// `None` without one.
    #[inline]
    pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.remove_entry(key).map(|(_, value)| value)
    }

    /// Removes the entry whose key equals `key`, and returns it, with the
    /// key as the map stored it; `None` without one.
    ///
    /// The pages of nodes a removal empties stay with the map, for new
    /// nodes, until it is dropped: a thread may still be reading them.
    #[inline]
    pub fn remove_entry<Q>(&self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.remove_entry(key, |_, _| true, &self.alloc)
    }

    /// Removes the entry whose key equals `key` if `condition` holds of it,
    /// and returns it, with the key as the map stored it; `None` without
    /// one, or if `condition` doesn't hold. `condition` runs while no other
    /// thread reads or changes the entry, again each time the removal tries
    /// again.
    #[inline]
    pub fn remove_if<Q>(&self, key: &Q, condition: impl FnMut(&K, &V) -> bool) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.remove_entry(key, condition, &self.alloc)
    }
}

#[cfg(test)]
impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator> BCTreeMap<K, V, S, A> {
    /// Panics unless the tree is well formed, as a `BBTreeMap`'s, and holds
    /// `length` entries.
    pub(crate) fn check_invariants(&mut self, length: usize) {
        self.tree.check_invariants(length);
    }

    /// How many leading bytes every key shares (D50).
    pub(crate) fn shared_prefix_len(&mut self) -> usize {
        self.tree.shared_prefix_len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for BCTreeMap<K, V, S, A> {
    fn drop(&mut self) {
        self.tree.destroy(&self.alloc);
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> fmt::Debug for BCTreeMap<K, V, S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BCTreeMap").finish_non_exhaustive()
    }
}
