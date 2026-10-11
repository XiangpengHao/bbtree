//! Iterating over a map by shared reference: [`Iter`], [`Keys`], [`Values`]
//! and [`Range`], each a walk from both ends (`walk.rs`, D23). The whole-map
//! iterators know how many entries are left; a range stops where its ends
//! meet. Only building a range calls `Ord`, to find its ends.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::fmt;
use core::iter::FusedIterator;
use core::ops::RangeBounds;

use super::BBTreeMap;
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node::handle::Immut;
use crate::node::walk::{BorrowedWalk, RangeWalk};

#[cfg(test)]
#[path = "iterator_tests.rs"]
mod tests;

/// An iterator over the entries of a `BBTreeMap`, in key order.
///
/// This `struct` is created by the [`iter`] method on [`BBTreeMap`]. See its
/// documentation for more.
///
/// [`iter`]: BBTreeMap::iter
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Iter<'a, K: 'a, V: 'a, S = Auto> {
    walk: BorrowedWalk<Immut<'a>, K, V, S>,
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> Iterator for Iter<'a, K, V, S> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        self.walk.next()
    }

    fn fold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.fold(accumulator, fold)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.walk.len(), Some(self.walk.len()))
    }

    fn last(mut self) -> Option<(&'a K, &'a V)> {
        self.next_back()
    }

    fn min(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next_back()
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> DoubleEndedIterator for Iter<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(&'a K, &'a V)> {
        self.walk.next_back()
    }

    fn rfold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.rfold(accumulator, fold)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for Iter<'_, K, V, S> {
    fn len(&self) -> usize {
        self.walk.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Iter<'_, K, V, S> {}

impl<K, V, S> Clone for Iter<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            walk: self.walk.clone(),
        }
    }
}

impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for Iter<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Iter<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Iter`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Iter<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            walk: BorrowedWalk::EMPTY,
        }
    }
}

/// An iterator over the keys of a `BBTreeMap`, in order.
///
/// This `struct` is created by the [`keys`] method on [`BBTreeMap`]. See its
/// documentation for more.
///
/// [`keys`]: BBTreeMap::keys
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Keys<'a, K, V, S = Auto> {
    inner: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for Keys<'a, K, V, S> {
    type Item = &'a K;

    fn next(&mut self) -> Option<&'a K> {
        self.inner.next().map(|(key, _)| key)
    }

    fn fold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.inner
            .fold(accumulator, |accumulator, (key, _)| fold(accumulator, key))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }

    fn last(mut self) -> Option<&'a K> {
        self.next_back()
    }

    fn min(mut self) -> Option<&'a K>
    where
        &'a K: Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<&'a K>
    where
        &'a K: Ord,
    {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for Keys<'a, K, V, S> {
    fn next_back(&mut self) -> Option<&'a K> {
        self.inner.next_back().map(|(key, _)| key)
    }

    fn rfold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.inner
            .rfold(accumulator, |accumulator, (key, _)| fold(accumulator, key))
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for Keys<'_, K, V, S> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Keys<'_, K, V, S> {}

impl<K, V, S> Clone for Keys<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K: fmt::Debug, V, S: InlineKeyStrategy<K>> fmt::Debug for Keys<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Keys<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Keys`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Keys<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: Iter::default(),
        }
    }
}

/// An iterator over the values of a `BBTreeMap`, in key order.
///
/// This `struct` is created by the [`values`] method on [`BBTreeMap`]. See
/// its documentation for more.
///
/// [`values`]: BBTreeMap::values
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Values<'a, K, V, S = Auto> {
    inner: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for Values<'a, K, V, S> {
    type Item = &'a V;

    fn next(&mut self) -> Option<&'a V> {
        self.inner.next().map(|(_, value)| value)
    }

    fn fold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.inner.fold(accumulator, |accumulator, (_, value)| {
            fold(accumulator, value)
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }

    fn last(mut self) -> Option<&'a V> {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for Values<'a, K, V, S> {
    fn next_back(&mut self) -> Option<&'a V> {
        self.inner.next_back().map(|(_, value)| value)
    }

    fn rfold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.inner.rfold(accumulator, |accumulator, (_, value)| {
            fold(accumulator, value)
        })
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for Values<'_, K, V, S> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Values<'_, K, V, S> {}

impl<K, V, S> Clone for Values<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for Values<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Values<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Values`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Values<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: Iter::default(),
        }
    }
}

/// An iterator over a sub-range of the entries of a `BBTreeMap`, in key
/// order.
///
/// This `struct` is created by the [`range`] method on [`BBTreeMap`]. See its
/// documentation for more.
///
/// [`range`]: BBTreeMap::range
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Range<'a, K: 'a, V: 'a, S = Auto> {
    walk: RangeWalk<Immut<'a>, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for Range<'a, K, V, S> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        self.walk.next()
    }

    fn fold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.fold(accumulator, fold)
    }

    fn last(mut self) -> Option<(&'a K, &'a V)> {
        self.next_back()
    }

    fn min(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for Range<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(&'a K, &'a V)> {
        self.walk.next_back()
    }

    fn rfold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.rfold(accumulator, fold)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Range<'_, K, V, S> {}

impl<K, V, S> Clone for Range<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            walk: self.walk.clone(),
        }
    }
}

impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for Range<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Range<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Range`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Range<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.count(), 0);
    /// ```
    fn default() -> Self {
        Self {
            walk: RangeWalk::EMPTY,
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Gets an iterator over the entries of the map, sorted by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(3, "c");
    /// map.insert(2, "b");
    /// map.insert(1, "a");
    ///
    /// for (key, value) in map.iter() {
    ///     println!("{key}: {value}");
    /// }
    ///
    /// let (first_key, first_value) = map.iter().next().unwrap();
    /// assert_eq!((*first_key, *first_value), (1, "a"));
    /// ```
    pub fn iter(&self) -> Iter<'_, K, V, S> {
        let root = self.root.as_ref().map(|root| root.borrow());
        Iter {
            walk: BorrowedWalk::new(root, self.length),
        }
    }

    /// Gets an iterator over the keys of the map, in sorted order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(2, "b");
    /// a.insert(1, "a");
    ///
    /// let keys: Vec<_> = a.keys().cloned().collect();
    /// assert_eq!(keys, [1, 2]);
    /// ```
    pub fn keys(&self) -> Keys<'_, K, V, S> {
        Keys { inner: self.iter() }
    }

    /// Gets an iterator over the values of the map, in order by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "hello");
    /// a.insert(2, "goodbye");
    ///
    /// let values: Vec<&str> = a.values().cloned().collect();
    /// assert_eq!(values, ["hello", "goodbye"]);
    /// ```
    pub fn values(&self) -> Values<'_, K, V, S> {
        Values { inner: self.iter() }
    }

    /// Constructs a double-ended iterator over a sub-range of entries in the
    /// map. The simplest way is to use the range syntax `min..max`, thus
    /// `range(min..max)` will yield entries from min (inclusive) to max
    /// (exclusive). The range may also be entered as
    /// `(Bound<T>, Bound<T>)`, so for example
    /// `range((Excluded(4), Included(10)))` will yield a left-exclusive,
    /// right-inclusive range from 4 to 10.
    ///
    /// # Panics
    ///
    /// Panics if range `start > end`. Panics if range `start == end` and both
    /// bounds are `Excluded`. The messages are std's, word for word.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use std::ops::Bound::Included;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(3, "a");
    /// map.insert(5, "b");
    /// map.insert(8, "c");
    /// for (&key, &value) in map.range((Included(&4), Included(&8))) {
    ///     println!("{key}: {value}");
    /// }
    /// assert_eq!(Some((&5, &"b")), map.range(4..).next());
    /// ```
    pub fn range<T, R>(&self, range: R) -> Range<'_, K, V, S>
    where
        T: Ord + ?Sized,
        K: Borrow<T> + Ord,
        R: RangeBounds<T>,
    {
        let Some(root) = &self.root else {
            return Range::default();
        };
        Range {
            walk: RangeWalk::new(root.borrow(), range.start_bound(), range.end_bound()),
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> IntoIterator for &'a BBTreeMap<K, V, S> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V, S>;

    fn into_iter(self) -> Iter<'a, K, V, S> {
        self.iter()
    }
}
