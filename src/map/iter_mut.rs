//! Iterating over a map with mutable access to its values: [`IterMut`],
//! [`ValuesMut`] and [`RangeMut`], each a walk from both ends that hands out
//! every value at most once (`tree/walk.rs`, D23). Keys stay shared.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::fmt;
use core::iter::{self, FusedIterator};
use core::ops::RangeBounds;

use super::BBTreeMap;
use crate::allocator::Allocator;
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::node_stack::ValMut;
use crate::tree::walk::{BorrowedWalk, RangeWalk};

/// A mutable iterator over the entries of a `BBTreeMap`, in key order.
///
/// This `struct` is created by the [`iter_mut`] method on [`BBTreeMap`]. See
/// its documentation for more.
///
/// [`iter_mut`]: BBTreeMap::iter_mut
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct IterMut<'a, K: 'a, V: 'a, S = Auto> {
    walk: BorrowedWalk<ValMut<'a, K, V>, K, V, S>,
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> Iterator for IterMut<'a, K, V, S> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<(&'a K, &'a mut V)> {
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

    fn last(mut self) -> Option<(&'a K, &'a mut V)> {
        self.next_back()
    }

    fn min(mut self) -> Option<(&'a K, &'a mut V)>
    where
        (&'a K, &'a mut V): Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<(&'a K, &'a mut V)>
    where
        (&'a K, &'a mut V): Ord,
    {
        self.next_back()
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> DoubleEndedIterator for IterMut<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(&'a K, &'a mut V)> {
        self.walk.next_back()
    }

    fn rfold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.rfold(accumulator, fold)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for IterMut<'_, K, V, S> {
    fn len(&self) -> usize {
        self.walk.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for IterMut<'_, K, V, S> {}

/// Prints the entries left, without moving the iterator.
impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for IterMut<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut remaining = self.walk.reborrow();
        f.debug_list()
            .entries(iter::from_fn(|| remaining.next()))
            .finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for IterMut<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::IterMut`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::IterMut<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            walk: BorrowedWalk::EMPTY,
        }
    }
}

/// A mutable iterator over the values of a `BBTreeMap`, in key order.
///
/// This `struct` is created by the [`values_mut`] method on [`BBTreeMap`].
/// See its documentation for more.
///
/// [`values_mut`]: BBTreeMap::values_mut
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct ValuesMut<'a, K, V, S = Auto> {
    inner: IterMut<'a, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for ValuesMut<'a, K, V, S> {
    type Item = &'a mut V;

    fn next(&mut self) -> Option<&'a mut V> {
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

    fn last(mut self) -> Option<&'a mut V> {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for ValuesMut<'a, K, V, S> {
    fn next_back(&mut self) -> Option<&'a mut V> {
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

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for ValuesMut<'_, K, V, S> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for ValuesMut<'_, K, V, S> {}

/// Prints the values left, without moving the iterator.
impl<K, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for ValuesMut<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut remaining = self.inner.walk.reborrow();
        let values = iter::from_fn(|| remaining.next()).map(|(_, value)| value);
        f.debug_list().entries(values).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for ValuesMut<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::ValuesMut`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::ValuesMut<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: IterMut::default(),
        }
    }
}

/// A mutable iterator over a sub-range of the entries of a `BBTreeMap`, in
/// key order.
///
/// This `struct` is created by the [`range_mut`] method on [`BBTreeMap`].
/// See its documentation for more.
///
/// [`range_mut`]: BBTreeMap::range_mut
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct RangeMut<'a, K: 'a, V: 'a, S = Auto> {
    walk: RangeWalk<ValMut<'a, K, V>, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for RangeMut<'a, K, V, S> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<(&'a K, &'a mut V)> {
        self.walk.next()
    }

    fn fold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.fold(accumulator, fold)
    }

    fn last(mut self) -> Option<(&'a K, &'a mut V)> {
        self.next_back()
    }

    fn min(mut self) -> Option<(&'a K, &'a mut V)>
    where
        (&'a K, &'a mut V): Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<(&'a K, &'a mut V)>
    where
        (&'a K, &'a mut V): Ord,
    {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for RangeMut<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(&'a K, &'a mut V)> {
        self.walk.next_back()
    }

    fn rfold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.rfold(accumulator, fold)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for RangeMut<'_, K, V, S> {}

/// Prints the entries left, without moving the iterator.
impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for RangeMut<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut remaining = self.walk.reborrow();
        f.debug_list()
            .entries(iter::from_fn(|| remaining.next()))
            .finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for RangeMut<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::RangeMut`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::RangeMut<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.count(), 0);
    /// ```
    fn default() -> Self {
        Self {
            walk: RangeWalk::EMPTY,
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Gets a mutable iterator over the entries of the map, sorted by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert("a", 1);
    /// map.insert("b", 2);
    /// map.insert("c", 3);
    ///
    /// // add 10 to the value if the key isn't "a"
    /// for (key, value) in map.iter_mut() {
    ///     if key != &"a" {
    ///         *value += 10;
    ///     }
    /// }
    /// assert_eq!(map.get("b"), Some(&12));
    /// ```
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V, S> {
        IterMut {
            walk: BorrowedWalk::new(self.tree.root_page_mut(), self.length),
        }
    }

    /// Gets a mutable iterator over the values of the map, in order by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, String::from("hello"));
    /// a.insert(2, String::from("goodbye"));
    ///
    /// for value in a.values_mut() {
    ///     value.push_str("!");
    /// }
    ///
    /// let values: Vec<String> = a.values().cloned().collect();
    /// assert_eq!(values, [String::from("hello!"), String::from("goodbye!")]);
    /// ```
    pub fn values_mut(&mut self) -> ValuesMut<'_, K, V, S> {
        ValuesMut {
            inner: self.iter_mut(),
        }
    }

    /// Constructs a mutable double-ended iterator over a sub-range of entries
    /// in the map. The simplest way is to use the range syntax `min..max`,
    /// thus `range_mut(min..max)` will yield entries from min (inclusive) to
    /// max (exclusive). The range may also be entered as
    /// `(Bound<T>, Bound<T>)`, so for example
    /// `range_mut((Excluded(4), Included(10)))` will yield a left-exclusive,
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
    ///
    /// let mut map: BBTreeMap<&str, i32> = BBTreeMap::new();
    /// for name in ["Alice", "Bob", "Carol", "Cheryl"] {
    ///     map.insert(name, 0);
    /// }
    /// for (_, balance) in map.range_mut("B".."Cheryl") {
    ///     *balance += 100;
    /// }
    /// let balances: Vec<_> = map.values().copied().collect();
    /// assert_eq!(balances, [0, 100, 100, 0]);
    /// ```
    pub fn range_mut<T, R>(&mut self, range: R) -> RangeMut<'_, K, V, S>
    where
        T: Ord + ?Sized,
        K: Borrow<T> + Ord,
        R: RangeBounds<T>,
    {
        let Some(root_page) = self.tree.root_page_mut() else {
            return RangeMut::default();
        };
        RangeMut {
            walk: RangeWalk::new(root_page, range.start_bound(), range.end_bound()),
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> IntoIterator
    for &'a mut BBTreeMap<K, V, S, A>
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, K, V, S>;

    fn into_iter(self) -> IterMut<'a, K, V, S> {
        self.iter_mut()
    }
}
