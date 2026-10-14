//! The symmetric difference of two sets: [`SymmetricDifference`], which
//! merges their elements (`merge.rs`) and skips those of both, and
//! `^`, which builds a set of it, as std's.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::FusedIterator;
use core::ops::BitXor;

use super::BBTreeSet;
use super::merge::{HeldSide, MergeSides, MergeStep};
use crate::inline_key::{Auto, InlineKeyStrategy};

/// A lazy iterator producing elements in the symmetric difference of
/// `BBTreeSet`s.
///
/// This `struct` is created by the [`symmetric_difference`] method on
/// [`BBTreeSet`]. See its documentation for more.
///
/// [`symmetric_difference`]: BBTreeSet::symmetric_difference
#[must_use = "this returns the difference as an iterator, \
              without modifying either input set"]
pub struct SymmetricDifference<'a, T: 'a, S = Auto> {
    sides: MergeSides<'a, T, S>,
    /// The side std's would hold an element of, taken out of its iterator.
    held: HeldSide,
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>> Iterator for SymmetricDifference<'a, T, S> {
    type Item = &'a T;

    // Inlined into the caller's loop, see `merge.rs`.
    #[inline(always)]
    fn next(&mut self) -> Option<&'a T> {
        loop {
            let (step, held) = self.sides.next_step()?;
            self.held = held;
            match step {
                MergeStep::Left(element) | MergeStep::Right(element) => return Some(element),
                MergeStep::Both(_) => {}
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (a_len, b_len) = self.sides.lens();
        // No overflow, as for `Union`.
        (0, Some(a_len + b_len))
    }

    fn fold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.sides
            .fold_steps::<A, true, true>(accumulator, |accumulator, step| match step {
                MergeStep::Left(element) | MergeStep::Right(element) => fold(accumulator, element),
                MergeStep::Both(_) => accumulator,
            })
    }

    fn min(mut self) -> Option<&'a T> {
        self.next()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>> FusedIterator for SymmetricDifference<'_, T, S> {}

impl<T, S> Clone for SymmetricDifference<'_, T, S> {
    fn clone(&self) -> Self {
        Self {
            sides: self.sides.clone(),
            held: self.held,
        }
    }
}

/// Prints as std's does: `SymmetricDifference(MergeIterInner(..))`.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for SymmetricDifference<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SymmetricDifference")
            .field(&self.sides.printed_as_merge_iter(self.held))
            .finish()
    }
}

impl<T, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
    /// Visits the elements representing the symmetric difference, i.e., the
    /// elements that are in `self` or in `other` but not in both, in
    /// ascending order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let mut a = BBTreeSet::new();
    /// a.insert(1);
    /// a.insert(2);
    ///
    /// let mut b = BBTreeSet::new();
    /// b.insert(2);
    /// b.insert(3);
    ///
    /// let sym_diff: Vec<_> = a.symmetric_difference(&b).cloned().collect();
    /// assert_eq!(sym_diff, [1, 3]);
    /// ```
    pub fn symmetric_difference<'a>(&'a self, other: &'a Self) -> SymmetricDifference<'a, T, S>
    where
        T: Ord,
    {
        SymmetricDifference {
            sides: MergeSides::new(self.iter(), other.iter()),
            held: HeldSide::Neither,
        }
    }
}

impl<T: Ord + Clone, S: InlineKeyStrategy<T>> BitXor<&BBTreeSet<T, S>> for &BBTreeSet<T, S> {
    type Output = BBTreeSet<T, S>;

    /// Returns the symmetric difference of `self` and `rhs` as a new
    /// `BBTreeSet<T>`, built into full nodes without searching it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let a = BBTreeSet::from([1, 2, 3]);
    /// let b = BBTreeSet::from([2, 3, 4]);
    ///
    /// let result = &a ^ &b;
    /// assert_eq!(result, BBTreeSet::from([1, 4]));
    /// ```
    fn bitxor(self, rhs: &BBTreeSet<T, S>) -> BBTreeSet<T, S> {
        BBTreeSet::from_ascending(self.symmetric_difference(rhs).cloned())
    }
}
