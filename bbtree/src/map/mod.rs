//! [`BBTreeMap`]: an ordered map that threads share, on the same nodes as
//! [`unsync::BBTreeMap`](crate::unsync::BBTreeMap)
//! (`design/09-concurrency.md`).

#![forbid(unsafe_code)]

mod batch;
#[cfg(all(test, shuttle))]
mod batch_read_linearizability_review;
#[cfg(all(test, shuttle))]
mod batch_write_linearizability_review;
#[cfg(test)]
mod linearizability_review;
mod owner;
mod range;
#[cfg(all(test, shuttle))]
mod shuttle_tests;
#[cfg(test)]
mod tests;
mod with_leaf;

pub use self::owner::{IntoIter, IterMut};
pub use self::range::RangeCloned;

use core::borrow::Borrow;
use core::fmt;
use core::mem;
use core::ops::{Bound, ControlFlow, RangeBounds};

use crate::allocator::{Allocator, Global, InsertError};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::latched::{LatchedTree, ReadOrInsert, UpdateOrInsert};

/// An ordered map that threads share, through `&self`: a B+-tree whose
/// internal nodes are read optimistically, and whose leaves are latched
/// (`design/09-concurrency.md`).
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
/// closure on each entry of a range, in key order or in reverse.
/// [`Self::range_cloned`] is an iterator over clones of the entries, which
/// holds nothing between calls.
///
/// # Deadlocks
///
/// A closure that calls into the same map may deadlock: [`Self::update`]'s,
/// [`Self::update_or_insert`]'s, [`Self::get_or_insert`]'s and
/// [`Self::remove_if`]'s on any key of its entry's leaf, [`Self::read`]'s
/// and the scans' if a writer waits for that leaf meanwhile.
///
/// It is `Sync` if its keys and values are `Send` and `Sync`, as threads
/// move them in, read them, change them and drop them through `&self`, and
/// its allocator is `Sync`.
pub struct BBTreeMap<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator = Global> {
    tree: LatchedTree<K, V, S>,
    /// Where the tree's pages come from and go back to.
    alloc: A,
}

impl<K, V> BBTreeMap<K, V> {
    /// Makes a new, empty `BBTreeMap`, with the default strategy, [`Auto`],
    /// and the global allocator.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under [`Auto`].
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl<K, V> Default for BBTreeMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Makes a new, empty `BBTreeMap` with strategy `S`, as
    /// [`BBTreeMap::with_strategy`](crate::unsync::BBTreeMap::with_strategy) does.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under `S`.
    pub fn with_strategy(strategy: S) -> Self {
        let _ = strategy;
        Self::new_in(Global)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> BBTreeMap<K, V, S, A> {
    /// Makes a new, empty `BBTreeMap` whose pages come from `alloc`.
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

    /// Whether the map holds no entry, at some moment during the call.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, u64>::new();
    /// assert!(map.is_empty());
    /// map.insert(1, 10)?;
    /// assert!(!map.is_empty());
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    pub fn is_empty(&self) -> bool
    where
        K: Ord,
    {
        self.scan::<K, ()>(.., |_, _| ControlFlow::Break(()))
            .is_none()
    }

    /// A copy of the value of `key`'s entry; `None` without one.
    ///
    /// Mostly without a latch: it copies the value, then checks that no
    /// thread changed its leaf meanwhile, and looks again, latched, if one
    /// did. So a lookup writes nothing to the map, and threads that look up
    /// the same keys share their cache lines. This works for a key the leaf
    /// tells apart without following a pointer. That is a std integer or
    /// byte array under [`Auto`], or a std byte string that ends within the
    /// bytes a leaf keeps of each key, about 8 past what its keys share. Any
    /// other key's entry it copies with its leaf latched, as [`Self::read`]
    /// does.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, u64>::new();
    /// map.insert(1, 10)?;
    /// assert_eq!(map.get_copied(&1), Some(10));
    /// assert_eq!(map.get_copied(&2), None);
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    #[inline]
    pub fn get_copied<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        V: Copy,
    {
        self.tree.get_copied(key)
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

    /// Whether the map holds an entry whose key equals `key`: without a
    /// latch where [`Self::get_copied`] takes none, and for a key the leaf's
    /// inline keys rule out.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.contains_key(key)
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
        self.tree
            .scan(start, end, None, |key, value, _| f(key, value))
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
            .scan_rev(start, end, None, |key, value, _| f(key, value))
    }

    /// An iterator over clones of the entries whose keys lie in `range`: in
    /// key order, and in reverse from its back ([`DoubleEndedIterator`]).
    ///
    /// It holds no latch between calls. It clones a few entries of a leaf at
    /// a time, and hands them out while no thread has changed the leaf;
    /// otherwise it looks again past the last key it handed out. So, like a
    /// scan, it is not a snapshot: it hands out an entry other threads
    /// insert ahead of it, and no entry they removed before it got there.
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
                panic!("range start and end are equal and excluded in BBTreeMap")
            }
            (
                Bound::Included(start) | Bound::Excluded(start),
                Bound::Included(end) | Bound::Excluded(end),
            ) if start > end => panic!("range start is greater than range end in BBTreeMap"),
            _ => {}
        }
        (start, end)
    }

    /// Inserts `key` and `value`, and returns `None`; or, if the map holds
    /// an equal key, keeps that key, drops `key`, and returns the value it
    /// replaces, as std's `BTreeMap::insert` does.
    ///
    /// # Errors
    ///
    /// If the allocator has no page for a node the insert makes, the map is
    /// unchanged, and the entry comes back ([`InsertError`]).
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, &str>::new();
    /// assert_eq!(map.insert(1, "one")?, None);
    /// assert_eq!(map.insert(1, "again")?, Some("one"));
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    #[inline]
    pub fn insert(&self, key: K, value: V) -> Result<Option<V>, InsertError<K, V>>
    where
        K: Ord,
    {
        self.update_or_insert(key, value, |_, stored, value| mem::replace(stored, value))
    }

    /// Runs `f` on the entry whose key equals `key`, its value exclusive,
    /// with `value`, which `f` takes, and returns what `f` returned; or, if
    /// the map holds none, inserts `key` and `value`, and returns `None`. A
    /// held entry keeps its key, and `key` drops before `f` runs. `f` runs
    /// while no other thread reads or changes the entry, so the change and
    /// the insert are one step to other threads: threads that add to the
    /// same key's value this way miss none of each other's additions.
    ///
    /// [`Self::insert`] is the `f` that replaces the value.
    ///
    /// # Errors
    ///
    /// As [`Self::insert`]'s; `f` doesn't run then.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let counts = BBTreeMap::<&str, u64>::new();
    /// for word in ["a", "b", "a"] {
    ///     counts.update_or_insert(word, 1, |_, count, one| *count += one)?;
    /// }
    /// assert_eq!((counts.get_copied(&"a"), counts.get_copied(&"b")), (Some(2), Some(1)));
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    #[inline]
    pub fn update_or_insert<R>(
        &self,
        key: K,
        value: V,
        f: impl FnOnce(&K, &mut V, V) -> R,
    ) -> Result<Option<R>, InsertError<K, V>>
    where
        K: Ord,
    {
        self.tree.insert(key, value, UpdateOrInsert(f), &self.alloc)
    }

    /// Runs `f` on the entry whose key equals `key`, inserting `key` and
    /// `value` first if the map holds none, and returns what `f` returned.
    /// `f` sees the entry as the map stores it; if the map held one already,
    /// it drops `key` and `value`. `f` runs while no other thread reads or
    /// changes the entry; on a new one, just before it goes in.
    ///
    /// # Errors
    ///
    /// As [`Self::insert`]'s; `f` doesn't run then.
    #[inline]
    pub fn get_or_insert<R>(
        &self,
        key: K,
        value: V,
        f: impl FnOnce(&K, &V) -> R,
    ) -> Result<R, InsertError<K, V>>
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
        self.tree
            .remove_entry(key, |_, _| true, |_, _, _| {}, &self.alloc)
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
        self.tree
            .remove_entry(key, condition, |_, _, _| {}, &self.alloc)
    }
}

#[cfg(any(test, fuzzing))]
impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator> BBTreeMap<K, V, S, A> {
    /// Panics unless the map is a well-formed B+-tree of `length` entries
    /// whose appends go to its last leaf, as
    /// `unsync::BBTreeMap::check_invariants` checks its own. Compiled for
    /// tests and for fuzzing builds (`--cfg fuzzing`, which `cargo fuzz`
    /// sets), where the fuzz targets (`fuzz/`) call it. The map keeps no
    /// length, so the caller says how many entries it holds.
    ///
    /// Calls `Ord` and the strategy's inline-key encoding: a map whose keys
    /// break their contracts fails it.
    pub fn check_invariants(&mut self, length: usize) {
        self.tree.check_invariants(length);
    }
}

#[cfg(test)]
impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator> BBTreeMap<K, V, S, A> {
    /// How many leading bytes every key shares (D50).
    pub(crate) fn shared_prefix_len(&mut self) -> usize {
        self.tree.shared_prefix_len()
    }

    /// How many keys each node holds, level by level from the root.
    #[cfg(shuttle)]
    pub(crate) fn node_lengths(&mut self) -> std::vec::Vec<std::vec::Vec<usize>> {
        self.tree.node_lengths()
    }

    /// The keys of each leaf, in key order.
    #[cfg(shuttle)]
    pub(crate) fn leaf_keys(&mut self) -> std::vec::Vec<std::vec::Vec<K>>
    where
        K: Clone,
    {
        self.tree.leaf_keys()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for BBTreeMap<K, V, S, A> {
    fn drop(&mut self) {
        self.tree.destroy(&self.alloc);
    }
}

/// Prints the entries in key order, as a scan visits them: not a snapshot
/// while other threads change the map.
impl<K, V, S, A> fmt::Debug for BBTreeMap<K, V, S, A>
where
    K: fmt::Debug + Ord,
    V: fmt::Debug,
    S: InlineKeyStrategy<K>,
    A: Allocator,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries = f.debug_map();
        self.scan::<K, ()>(.., |key, value| {
            entries.entry(key, value);
            ControlFlow::Continue(())
        });
        entries.finish()
    }
}
