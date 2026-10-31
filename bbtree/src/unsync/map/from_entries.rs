//! Building a map from entries, as std's map does: [`FromIterator`], `From`
//! an array, and [`Extend`].
//!
//! `extend` inserts in input order, batching descents on large trees.
//! `collect` and `from` sort entries by key, stably, and build the map with
//! `from_sorted_in`, which takes entries sorted already, of any strategy and
//! allocator. It keeps only the last key and value of each run of equal
//! keys, dropping the others in order, and builds the tree into full nodes,
//! comparing each key with the next alone (`tree/build.rs`).
//!
//! Like `new` and `Default`, `collect` and `from` make maps of the default
//! strategy only, as std's make maps of the global allocator only: with a
//! strategy parameter, `BBTreeMap::from([(1, 2)])` could not infer it.

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::cmp::Ordering;
use core::iter::Peekable;
use core::mem;

use super::BBTreeMap;
use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::build::TreeBuilder;
use crate::tree::root::Tree;

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// A map of `entries`, sorted by key, with inline-key strategy
    /// `strategy`, whose nodes come from `alloc`: built into full nodes, as
    /// `collect` builds its sorted entries, each key compared only with the
    /// next. Of each run of equal keys, only the last is kept, as `collect`
    /// keeps it; the others are dropped in order. If `alloc` refuses a page,
    /// calls [`handle_alloc_error`](alloc::alloc::handle_alloc_error), as
    /// `insert` does.
    ///
    /// # Panics
    ///
    /// If a key is less than the one before it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    /// use bbtree::{Auto, Global};
    ///
    /// let map = BBTreeMap::from_sorted_in([(1, "a"), (2, "b"), (2, "c")], Auto, Global);
    /// assert_eq!(map.len(), 2);
    /// assert_eq!(map.get(&2), Some(&"c"));
    /// ```
    pub fn from_sorted_in(
        entries: impl IntoIterator<Item = (K, V)>,
        strategy: S,
        alloc: A,
    ) -> Self {
        // Only the strategy's type matters, as in `with_strategy_in`.
        mem::forget(strategy);
        let entries = LastOfEqualKeys {
            entries: entries.into_iter().peekable(),
        };
        Self::from_ascending_entries(entries, alloc)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Inserts `entries` 16 at a time, and the last, partial batch one by
    /// one. Out of line, to keep the batch and lane state out of `extend`'s
    /// callers. It stops at the iterator's first `None`, fused or not.
    #[inline(never)]
    fn extend_batches(&mut self, entries: impl Iterator<Item = (K, V)>)
    where
        K: Ord,
    {
        let mut entries = entries.fuse();
        loop {
            let batch: [_; 16] = core::array::from_fn(|_| entries.next());
            if batch[15].is_none() {
                for (key, value) in batch.into_iter().flatten() {
                    self.tree.insert(key, value, &self.alloc, &mut self.length);
                }
                return;
            }
            self.insert_many(batch.map(|entry| entry.expect("a full batch")));
        }
    }

    /// A map of `entries`, whose keys ascend strictly, built into full nodes
    /// comparing no keys (`tree/build.rs`), on pages from `alloc`. Keys out
    /// of order make a map whose searches give wrong answers, but that is
    /// memory-safe.
    pub(crate) fn from_ascending_entries(entries: impl Iterator<Item = (K, V)>, alloc: A) -> Self {
        let mut builder = TreeBuilder::new(&alloc);
        builder.extend(entries);
        let (tree, length) = builder.finish();
        Self {
            tree,
            length,
            alloc,
        }
    }
}

/// The entries of a sequence sorted by key, but for those followed by one
/// with an equal key: the last of each run of equal keys, which std's
/// `collect` keeps. The others drop whole, key before value. A key less
/// than the one before it panics: the sequence isn't sorted.
struct LastOfEqualKeys<I: Iterator> {
    entries: Peekable<I>,
}

impl<K: Ord, V, I: Iterator<Item = (K, V)>> Iterator for LastOfEqualKeys<I> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        loop {
            let entry = self.entries.next()?;
            match self.entries.peek().map(|next| next.0.cmp(&entry.0)) {
                Some(Ordering::Less) => panic!("entries not sorted by key"),
                Some(Ordering::Equal) => {}
                Some(Ordering::Greater) | None => return Some(entry),
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
        Self::from_sorted_in(entries, Auto, Global)
    }
}

impl<K: Ord, V, const N: usize> From<[(K, V); N]> for BBTreeMap<K, V> {
    /// Converts a `[(K, V); N]` into a `BBTreeMap<K, V>`.
    ///
    /// If any entries in the array have equal keys, all but one of the
    /// corresponding values will be dropped.
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    ///
    /// let map1 = BBTreeMap::from([(1, 2), (3, 4)]);
    /// let map2: BBTreeMap<_, _> = [(1, 2), (3, 4)].into();
    /// assert_eq!(map1, map2);
    /// ```
    fn from(mut entries: [(K, V); N]) -> Self {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self::from_sorted_in(entries, Auto, Global)
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> Extend<(K, V)>
    for BBTreeMap<K, V, S, A>
{
    /// Inserts each entry in turn, as `insert` does. Large trees may pull
    /// sixteen entries ahead of the insert.
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, iter: T) {
        let mut entries = iter.into_iter();
        // Automatic batching must not put large inline values in several
        // temporary arrays. Keep its input buffer within one 4 KiB page.
        if core::mem::size_of::<[Option<(K, V)>; 16]>() > 4096
            || entries.size_hint().1.is_some_and(|remaining| {
                !Tree::<K, V, S>::batches(self.length.saturating_add(remaining))
            })
        {
            for (key, value) in entries {
                self.tree.insert(key, value, &self.alloc, &mut self.length);
            }
            return;
        }
        while !Tree::<K, V, S>::batches(self.length) {
            let Some((key, value)) = entries.next() else {
                return;
            };
            self.tree.insert(key, value, &self.alloc, &mut self.length);
        }
        self.extend_batches(entries);
    }
}

impl<'a, K: Ord + Copy, V: Copy, S: InlineKeyStrategy<K>, A: Allocator + Clone>
    Extend<(&'a K, &'a V)> for BBTreeMap<K, V, S, A>
{
    /// Inserts a copy of each entry in turn, as `insert` does.
    fn extend<T: IntoIterator<Item = (&'a K, &'a V)>>(&mut self, iter: T) {
        self.extend(iter.into_iter().map(|(&key, &value)| (key, value)));
    }
}
