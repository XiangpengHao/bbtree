//! Building a map from entries, as std's map does: [`FromIterator`], `From`
//! an array, and [`Extend`].
//!
//! `extend` inserts each entry in turn. `collect` and `from` sort the
//! entries by key, stably, and keep only the last of each run of equal keys,
//! key and value, dropping the others in order; then they build the tree
//! from the sorted entries, into full nodes, comparing no more keys
//! (`tree/build.rs`).
//!
//! Like `new` and `Default`, `collect` and `from` make maps of the default
//! strategy only, as std's make maps of the global allocator only: with a
//! strategy parameter, `BBTreeMap::from([(1, 2)])` could not infer it.

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::iter::Peekable;

use super::BBTreeMap;
use crate::inline_key::InlineKeyStrategy;
use crate::tree::build::TreeBuilder;

impl<K: Ord, V> BBTreeMap<K, V> {
    /// A map of `entries`, sorted by key, of which only the last of each run
    /// of equal keys is kept; the others are dropped in order.
    fn from_sorted_keeping_last(entries: impl Iterator<Item = (K, V)>) -> Self {
        Self::from_ascending_entries(LastOfEqualKeys {
            entries: entries.peekable(),
        })
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// A map of `entries`, whose keys ascend strictly, built into full nodes
    /// comparing no keys (`tree/build.rs`). Keys out of order make a map
    /// whose searches give wrong answers, but that is memory-safe.
    pub(crate) fn from_ascending_entries(entries: impl Iterator<Item = (K, V)>) -> Self {
        let mut builder = TreeBuilder::new();
        builder.extend(entries);
        Self::from_builder(builder)
    }
}

/// The entries of a sequence sorted by key, but for those followed by one
/// with an equal key: the last of each run of equal keys, which std's
/// `collect` keeps. The others drop whole, key before value.
struct LastOfEqualKeys<I: Iterator> {
    entries: Peekable<I>,
}

impl<K: PartialEq, V, I: Iterator<Item = (K, V)>> Iterator for LastOfEqualKeys<I> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        loop {
            let entry = self.entries.next()?;
            if self.entries.peek().is_none_or(|next| next.0 != entry.0) {
                return Some(entry);
            }
        }
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for BBTreeMap<K, V> {
    /// Collects the entries, sorts them by key, and keeps only the last of
    /// entries with equal keys, as std's map does.
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut entries: Vec<(K, V)> = iter.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self::from_sorted_keeping_last(entries.into_iter())
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
        Self::from_sorted_keeping_last(entries.into_iter())
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
