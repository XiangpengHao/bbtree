//! Operations on a fixed batch of keys, which overlap the keys' waits for
//! memory (doc 12). Arrays keep the batch on the stack and its results in
//! input order, repeated keys included.

#![forbid(unsafe_code)]

use core::borrow::Borrow;

use super::BBTreeMap;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::tree::root::Tree;

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Whether the map is large enough for batches to pay for themselves:
    /// for operations that would batch lookups on their own.
    pub(crate) fn batches(&self) -> bool {
        Tree::<K, V, S>::batches(self.length)
    }

    /// Returns each key's value, in input order, as separate calls to `get`
    /// would. Keys may repeat, and an empty batch returns an empty array.
    /// The borrowed form must order like the stored key type.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::from([(1, "one"), (2, "two")]);
    /// assert_eq!(map.get_many([&2, &3, &2]), [Some(&"two"), None, Some(&"two")]);
    /// ```
    pub fn get_many<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<&V>; N]
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.tree.find_many(keys, self.length)
    }

    /// Inserts entries in input order, returning each previous value.
    /// Repeated keys replace earlier values in the batch and keep the
    /// existing key, exactly as separate calls to `insert` do.
    /// The descent may compare later keys before earlier entries apply.
    /// If user code panics, entries not yet inserted are dropped.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let mut map = BBTreeMap::new();
    /// assert_eq!(map.insert_many([(1, "one"), (1, "again")]), [None, Some("one")]);
    /// assert_eq!(map.get(&1), Some(&"again"));
    /// ```
    pub fn insert_many<const N: usize>(&mut self, entries: [(K, V); N]) -> [Option<V>; N]
    where
        K: Ord,
    {
        self.tree
            .insert_many(entries, &self.alloc, &mut self.length)
    }
}
