//! Building a map from entries, as std's map does: [`FromIterator`], `From`
//! an array, and [`Extend`].
//!
//! `extend` inserts each entry in turn. `collect` and `from` sort the
//! entries by key, stably, and keep only the last of each run of equal keys,
//! key and value, dropping the others in order; then they insert the sorted
//! entries, each one after the last, which the inserts' search for appends
//! finds at once. A bulk build into full nodes is to replace those inserts
//! (decisions, "Fill factor").
//!
//! Like `new` and `Default`, `collect` and `from` make maps of the default
//! strategy only, as std's make maps of the global allocator only: with a
//! strategy parameter, `BBTreeMap::from([(1, 2)])` could not infer it.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

use super::BBTreeMap;
use crate::inline_key::InlineKeyStrategy;

impl<K: Ord, V> BBTreeMap<K, V> {
    /// A map of `entries`, sorted by key, of which only the last of each run
    /// of equal keys is kept; the others are dropped in order.
    fn from_sorted_entries(entries: impl Iterator<Item = (K, V)>) -> Self {
        let mut map = Self::new();
        let mut entries = entries.peekable();
        while let Some(entry) = entries.next() {
            // An entry left out drops whole, its key before its value.
            if entries.peek().is_some_and(|next| next.0 == entry.0) {
                continue;
            }
            let (key, value) = entry;
            map.insert(key, value);
        }
        map
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for BBTreeMap<K, V> {
    /// Collects the entries, sorts them by key, and keeps only the last of
    /// entries with equal keys, as std's map does.
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut entries: Vec<(K, V)> = iter.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self::from_sorted_entries(entries.into_iter())
    }
}

impl<K: Ord, V, const N: usize> From<[(K, V); N]> for BBTreeMap<K, V> {
    /// Converts a `[(K, V); N]` into a `BBTreeMap<K, V>`.
    ///
    /// If any entries in the array have equal keys, all but one of the
    /// corresponding values will be dropped.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let map1 = BBTreeMap::from([(1, 2), (3, 4)]);
    /// let map2: BBTreeMap<_, _> = [(1, 2), (3, 4)].into();
    /// assert_eq!(map1, map2);
    /// ```
    fn from(mut entries: [(K, V); N]) -> Self {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self::from_sorted_entries(entries.into_iter())
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> Extend<(K, V)> for BBTreeMap<K, V, S> {
    /// Inserts each entry in turn, as `insert` does.
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<'a, K: Ord + Copy, V: Copy, S: InlineKeyStrategy<K>> Extend<(&'a K, &'a V)>
    for BBTreeMap<K, V, S>
{
    /// Inserts a copy of each entry in turn, as `insert` does.
    fn extend<T: IntoIterator<Item = (&'a K, &'a V)>>(&mut self, iter: T) {
        self.extend(iter.into_iter().map(|(&key, &value)| (key, value)));
    }
}
