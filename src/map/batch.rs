//! Lookups and inserts of a fixed batch of keys, which overlap the keys'
//! waits for memory (doc 12): each key descends in its own lane, and no lane
//! holds a latch while another runs (`tree/latched/batch.rs`). Arrays keep
//! the batch on the stack and its results in input order, repeated keys
//! included.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::BBTreeMap;
use crate::allocator::Allocator;
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
    /// map.insert(1, "one");
    /// map.insert(2, "two");
    /// assert_eq!(
    ///     map.read_many([&2, &3, &2], |_, value| value.len()),
    ///     [Some(3), None, Some(3)]
    /// );
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
    /// [`Self::get_copied`] would give, and like it mostly without a latch.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, u64>::new();
    /// map.insert(1, 10);
    /// assert_eq!(map.get_many_copied([&1, &2, &1]), [Some(10), None, Some(10)]);
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

    /// Inserts `entries` in input order, and returns each replaced value, as
    /// separate calls to [`Self::insert`] would: a key repeated in the batch
    /// replaces the value its earlier entry inserted, and keeps that key.
    /// Each key descends in its own lane first, then the entries go in one
    /// after another, each into the leaf its lane found unless another
    /// thread changed that leaf meanwhile.
    ///
    /// Not atomic: other threads may see some of the batch's entries before
    /// the rest.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, &str>::new();
    /// assert_eq!(map.insert_many([(1, "one"), (1, "again")]), [None, Some("one")]);
    /// assert_eq!(map.get_copied(&1), Some("again"));
    /// ```
    #[inline]
    pub fn insert_many<const N: usize>(&self, entries: [(K, V); N]) -> [Option<V>; N]
    where
        K: Ord,
    {
        self.tree.insert_many(entries, &self.alloc)
    }

    /// Clones of each key's value, in input order, as separate calls to
    /// [`Self::get_cloned`] would.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, &str>::new();
    /// map.insert(1, "one");
    /// assert_eq!(map.get_many_cloned([&1, &2]), [Some("one"), None]);
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
