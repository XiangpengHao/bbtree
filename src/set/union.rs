//! The union of two sets: [`Union`], which merges their iterators
//! (`merge.rs`), and `|`, which builds a set of it, as std's.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::FusedIterator;
use core::ops::BitOr;

use super::BBTreeSet;
use super::merge::AscendingPairs;
use crate::inline_key::{Auto, InlineKeyStrategy};

/// A lazy iterator producing elements in the union of `BBTreeSet`s.
///
/// This `struct` is created by the [`union`] method on [`BBTreeSet`]. See
/// its documentation for more.
///
/// [`union`]: BBTreeSet::union
#[must_use = "this returns the union as an iterator, \
              without modifying either input set"]
pub struct Union<'a, T: 'a, S = Auto>(AscendingPairs<'a, T, S>);

impl<'a, T: Ord, S: InlineKeyStrategy<T>> Iterator for Union<'a, T, S> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        let (a_next, b_next) = self.0.next_pair();
        a_next.or(b_next)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (a_len, b_len) = self.0.lens();
        // No overflow: every element takes room in a node, even a zero-sized
        // one, so two sets can't hold `usize::MAX` elements between them.
        (a_len.max(b_len), Some(a_len + b_len))
    }

    fn min(mut self) -> Option<&'a T> {
        self.next()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>> FusedIterator for Union<'_, T, S> {}

impl<T, S> Clone for Union<'_, T, S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for Union<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Union").field(&self.0).finish()
    }
}

impl<T, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
    /// Visits the elements representing the union, i.e., all the elements
    /// in `self` or `other`, without duplicates, in ascending order.
    ///
    /// Of equal elements, it visits `self`'s.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let mut a = BBTreeSet::new();
    /// a.insert(1);
    ///
    /// let mut b = BBTreeSet::new();
    /// b.insert(2);
    ///
    /// let union: Vec<_> = a.union(&b).cloned().collect();
    /// assert_eq!(union, [1, 2]);
    /// ```
    pub fn union<'a>(&'a self, other: &'a Self) -> Union<'a, T, S>
    where
        T: Ord,
    {
        Union(AscendingPairs::new(self.iter(), other.iter()))
    }
}

impl<T: Ord + Clone, S: InlineKeyStrategy<T>> BitOr<&BBTreeSet<T, S>> for &BBTreeSet<T, S> {
    type Output = BBTreeSet<T, S>;

    /// Returns the union of `self` and `rhs` as a new `BBTreeSet<T>`, built
    /// into full nodes without searching it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let a = BBTreeSet::from([1, 2, 3]);
    /// let b = BBTreeSet::from([3, 4, 5]);
    ///
    /// let result = &a | &b;
    /// assert_eq!(result, BBTreeSet::from([1, 2, 3, 4, 5]));
    /// ```
    fn bitor(self, rhs: &BBTreeSet<T, S>) -> BBTreeSet<T, S> {
        BBTreeSet::from_ascending(self.union(rhs).cloned())
    }
}
