//! Iterating over a set: [`Iter`] and [`Range`] by shared reference, and
//! [`IntoIter`] by value, each the map's iterator of the same job with the
//! values dropped from its items (`map/iter.rs`, `map/into_iter.rs`).

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::fmt;
use core::iter::FusedIterator;
use core::ops::RangeBounds;

use super::{BBTreeSet, SetValZST};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::map::{self, Keys};
use crate::node::handle::KeyRun;

/// An iterator over the items of a `BBTreeSet`.
///
/// This `struct` is created by the [`iter`] method on [`BBTreeSet`]. See its
/// documentation for more.
///
/// [`iter`]: BBTreeSet::iter
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Iter<'a, T: 'a, S = Auto> {
    iter: Keys<'a, T, SetValZST, S>,
}

impl<'a, T, S: InlineKeyStrategy<T>> Iterator for Iter<'a, T, S> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        self.iter.next()
    }

    fn fold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.iter.fold(accumulator, fold)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }

    fn last(mut self) -> Option<&'a T> {
        self.next_back()
    }

    fn min(mut self) -> Option<&'a T>
    where
        &'a T: Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<&'a T>
    where
        &'a T: Ord,
    {
        self.next_back()
    }
}

impl<'a, T, S: InlineKeyStrategy<T>> DoubleEndedIterator for Iter<'a, T, S> {
    fn next_back(&mut self) -> Option<&'a T> {
        self.iter.next_back()
    }

    fn rfold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.iter.rfold(accumulator, fold)
    }
}

impl<T, S: InlineKeyStrategy<T>> ExactSizeIterator for Iter<'_, T, S> {
    fn len(&self) -> usize {
        self.iter.len()
    }
}

impl<T, S: InlineKeyStrategy<T>> FusedIterator for Iter<'_, T, S> {}

impl<T, S> Clone for Iter<'_, T, S> {
    fn clone(&self) -> Self {
        Self {
            iter: self.iter.clone(),
        }
    }
}

/// Prints the items left, as std's does: `Iter([1, 2])`.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for Iter<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Iter").field(&self.iter).finish()
    }
}

impl<'a, T, S: InlineKeyStrategy<T>> Iter<'a, T, S> {
    /// The elements after the walk's front, a leaf's run at a time, which
    /// the walk moves past at once (`node/walk.rs`): what the operations on
    /// two sets visit.
    pub(super) fn next_key_run(&mut self) -> Option<KeyRun<'a, T>> {
        self.iter.next_key_run()
    }
}

impl<T, S: InlineKeyStrategy<T>> Default for Iter<'_, T, S> {
    /// Creates an empty `bbtree::bbtree_set::Iter`.
    ///
    /// ```
    /// # use bbtree::bbtree_set;
    /// let iter: bbtree_set::Iter<'_, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            iter: Keys::default(),
        }
    }
}

/// An iterator over a sub-range of items in a `BBTreeSet`.
///
/// This `struct` is created by the [`range`] method on [`BBTreeSet`]. See its
/// documentation for more.
///
/// [`range`]: BBTreeSet::range
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Range<'a, T: 'a, S = Auto> {
    iter: map::Range<'a, T, SetValZST, S>,
}

impl<'a, T, S: InlineKeyStrategy<T>> Iterator for Range<'a, T, S> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        self.iter.next().map(|(element, _)| element)
    }

    fn fold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.iter.fold(accumulator, |accumulator, (element, _)| {
            fold(accumulator, element)
        })
    }

    fn last(mut self) -> Option<&'a T> {
        self.next_back()
    }

    fn min(mut self) -> Option<&'a T>
    where
        &'a T: Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<&'a T>
    where
        &'a T: Ord,
    {
        self.next_back()
    }
}

impl<'a, T, S: InlineKeyStrategy<T>> DoubleEndedIterator for Range<'a, T, S> {
    fn next_back(&mut self) -> Option<&'a T> {
        self.iter.next_back().map(|(element, _)| element)
    }

    fn rfold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.iter.rfold(accumulator, |accumulator, (element, _)| {
            fold(accumulator, element)
        })
    }
}

impl<T, S: InlineKeyStrategy<T>> FusedIterator for Range<'_, T, S> {}

impl<T, S> Clone for Range<'_, T, S> {
    fn clone(&self) -> Self {
        Self {
            iter: self.iter.clone(),
        }
    }
}

/// Prints the map's range, as std's does: `Range { iter: [(1, SetValZST)] }`.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for Range<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Range").field("iter", &self.iter).finish()
    }
}

impl<T, S: InlineKeyStrategy<T>> Default for Range<'_, T, S> {
    /// Creates an empty `bbtree::bbtree_set::Range`.
    ///
    /// ```
    /// # use bbtree::bbtree_set;
    /// let iter: bbtree_set::Range<'_, u8> = Default::default();
    /// assert_eq!(iter.count(), 0);
    /// ```
    fn default() -> Self {
        Self {
            iter: map::Range::default(),
        }
    }
}

/// An owning iterator over the items of a `BBTreeSet` in ascending order.
///
/// This `struct` is created by the [`into_iter`] method on [`BBTreeSet`]
/// (provided by the [`IntoIterator`] trait). See its documentation for more.
///
/// [`into_iter`]: IntoIterator::into_iter
pub struct IntoIter<T, S: InlineKeyStrategy<T> = Auto> {
    iter: map::IntoIter<T, SetValZST, S>,
}

impl<T, S: InlineKeyStrategy<T>> Iterator for IntoIter<T, S> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.iter.next().map(|(element, _)| element)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<T, S: InlineKeyStrategy<T>> DoubleEndedIterator for IntoIter<T, S> {
    fn next_back(&mut self) -> Option<T> {
        self.iter.next_back().map(|(element, _)| element)
    }
}

impl<T, S: InlineKeyStrategy<T>> ExactSizeIterator for IntoIter<T, S> {
    fn len(&self) -> usize {
        self.iter.len()
    }
}

impl<T, S: InlineKeyStrategy<T>> FusedIterator for IntoIter<T, S> {}

/// Prints the map's iterator, as std's does:
/// `IntoIter { iter: [(1, SetValZST)] }`.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for IntoIter<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntoIter")
            .field("iter", &self.iter)
            .finish()
    }
}

impl<T, S: InlineKeyStrategy<T>> Default for IntoIter<T, S> {
    /// Creates an empty `bbtree::bbtree_set::IntoIter`.
    ///
    /// ```
    /// # use bbtree::bbtree_set;
    /// let iter: bbtree_set::IntoIter<u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            iter: map::IntoIter::default(),
        }
    }
}

impl<T, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
    /// Gets an iterator that visits the elements in the `BBTreeSet` in
    /// ascending order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let set = BBTreeSet::from([3, 1, 2]);
    /// let mut set_iter = set.iter();
    /// assert_eq!(set_iter.next(), Some(&1));
    /// assert_eq!(set_iter.next(), Some(&2));
    /// assert_eq!(set_iter.next(), Some(&3));
    /// assert_eq!(set_iter.next(), None);
    /// ```
    pub fn iter(&self) -> Iter<'_, T, S> {
        Iter {
            iter: self.map.keys(),
        }
    }

    /// Constructs a double-ended iterator over a sub-range of elements in
    /// the set. The simplest way is to use the range syntax `min..max`, thus
    /// `range(min..max)` will yield elements from min (inclusive) to max
    /// (exclusive). The range may also be entered as `(Bound<T>, Bound<T>)`,
    /// so for example `range((Excluded(4), Included(10)))` will yield a
    /// left-exclusive, right-inclusive range from 4 to 10.
    ///
    /// # Panics
    ///
    /// Panics if range `start > end`. Panics if range `start == end` and both
    /// bounds are `Excluded`. The messages are std's, word for word.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    /// use std::ops::Bound::Included;
    ///
    /// let mut set = BBTreeSet::new();
    /// set.insert(3);
    /// set.insert(5);
    /// set.insert(8);
    /// for &elem in set.range((Included(&4), Included(&8))) {
    ///     println!("{elem}");
    /// }
    /// assert_eq!(Some(&5), set.range(4..).next());
    /// ```
    pub fn range<Q, R>(&self, range: R) -> Range<'_, T, S>
    where
        Q: Ord + ?Sized,
        T: Borrow<Q> + Ord,
        R: RangeBounds<Q>,
    {
        Range {
            iter: self.map.range(range),
        }
    }
}

impl<'a, T, S: InlineKeyStrategy<T>> IntoIterator for &'a BBTreeSet<T, S> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T, S>;

    fn into_iter(self) -> Iter<'a, T, S> {
        self.iter()
    }
}

impl<T, S: InlineKeyStrategy<T>> IntoIterator for BBTreeSet<T, S> {
    type Item = T;
    type IntoIter = IntoIter<T, S>;

    /// Gets an iterator for moving out the `BBTreeSet`'s contents in
    /// ascending order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let set = BBTreeSet::from([1, 2, 3, 4]);
    ///
    /// let v: Vec<_> = set.into_iter().collect();
    /// assert_eq!(v, [1, 2, 3, 4]);
    /// ```
    fn into_iter(self) -> IntoIter<T, S> {
        IntoIter {
            iter: self.map.into_iter(),
        }
    }
}
