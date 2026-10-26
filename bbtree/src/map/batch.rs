//! Lookups and inserts of a fixed batch of keys, which overlap the keys'
//! waits for memory (doc 12): each key descends in its own lane, and no lane
//! holds a latch while another runs (`tree/latched/batch.rs`). Arrays keep
//! the batch on the stack and its results in input order, repeated keys
//! included.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::mem;

use super::BBTreeMap;
use crate::allocator::{Allocator, InsertError};
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> BBTreeMap<K, V, S, A> {
    /// Runs `f` on each key's entry, while no thread changes it, and returns
    /// what `f` returned, in input order, as separate calls to
    /// [`Self::read`] would: `None` for a key without one. Keys may repeat,
    /// and an empty batch returns an empty array.
    ///
    /// `f` runs once per entry found, in the order the lookups reach their
    /// entries, not the input order.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, &str>::new();
    /// map.insert(1, "one")?;
    /// map.insert(2, "two")?;
    /// assert_eq!(
    ///     map.read_many([&2, &3, &2], |_, value| value.len()),
    ///     [Some(3), None, Some(3)]
    /// );
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    ///
    /// # Deadlocks
    ///
    /// As [`Self::read`]'s closure, `f` may deadlock if it calls into the
    /// map.
    #[inline]
    pub fn read_many<Q, R, const N: usize>(
        &self,
        keys: [&Q; N],
        f: impl FnMut(&K, &V) -> R,
    ) -> [Option<R>; N]
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.read_many(keys, f)
    }

    /// A copy of each key's value, in input order, as separate calls to
    /// [`Self::get_copied`] would give, using the same shared-latched snapshots.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, u64>::new();
    /// map.insert(1, 10)?;
    /// assert_eq!(map.get_many_copied([&1, &2, &1]), [Some(10), None, Some(10)]);
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    #[inline]
    pub fn get_many_copied<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<V>; N]
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        V: Copy,
    {
        self.tree.get_many_copied(keys)
    }

    /// Inserts `entries` in input order, and returns each replaced value, or
    /// the entry back ([`InsertError`]), as separate calls to
    /// [`Self::insert`] would: a key repeated in the batch replaces the value
    /// its earlier entry inserted, and keeps that key. Each key descends in
    /// its own lane first, then the entries go in one after another, each
    /// into the leaf its lane found unless another thread changed that leaf
    /// meanwhile.
    ///
    /// Not atomic: other threads may see some of the batch's entries before
    /// the rest, and an entry the allocator had no page for doesn't stop
    /// the rest.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, &str>::new();
    /// let [one, again] = map.insert_many([(1, "one"), (1, "again")]);
    /// assert_eq!((one?, again?), (None, Some("one")));
    /// assert_eq!(map.get_copied(&1), Some("again"));
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    #[inline]
    pub fn insert_many<const N: usize>(
        &self,
        entries: [(K, V); N],
    ) -> [Result<Option<V>, InsertError<K, V>>; N]
    where
        K: Ord,
    {
        self.update_or_insert_many(entries, |_, stored, value| mem::replace(stored, value))
    }

    /// For each entry, runs `f` on the stored entry of its key, with its
    /// value, or inserts it if the map holds no equal key, and returns what
    /// `f` returned, `None` for an entry inserted, or the entry back
    /// ([`InsertError`]): as separate calls to [`Self::update_or_insert`]
    /// in input order would. A key repeated in the batch runs `f` on the
    /// entry its earlier one inserted. The entries go in as
    /// [`Self::insert_many`]'s do, and no more atomically.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let counts = BBTreeMap::<&str, u64>::new();
    /// let added = counts.update_or_insert_many([("a", 1), ("b", 1), ("a", 1)], |_, count, one| {
    ///     *count += one;
    ///     *count
    /// });
    /// assert_eq!(added.map(Result::unwrap), [None, None, Some(2)]);
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    ///
    /// # Deadlocks
    ///
    /// As [`Self::update_or_insert`]'s closure, `f` may deadlock if it
    /// calls into the map.
    #[inline]
    pub fn update_or_insert_many<R, const N: usize>(
        &self,
        entries: [(K, V); N],
        f: impl FnMut(&K, &mut V, V) -> R,
    ) -> [Result<Option<R>, InsertError<K, V>>; N]
    where
        K: Ord,
    {
        self.tree.update_or_insert_many(entries, f, &self.alloc)
    }

    /// Clones of each key's value, in input order, as separate calls to
    /// [`Self::get_cloned`] would.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, &str>::new();
    /// map.insert(1, "one")?;
    /// assert_eq!(map.get_many_cloned([&1, &2]), [Some("one"), None]);
    /// # Ok::<(), bbtree::AllocError>(())
    /// ```
    #[inline]
    pub fn get_many_cloned<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<V>; N]
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        V: Clone,
    {
        self.read_many(keys, |_, value| value.clone())
    }
}
