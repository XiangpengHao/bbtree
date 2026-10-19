//! The difference of two sets: [`Difference`], and `-`, which builds a set
//! of it, as std's.
//!
//! Making the iterator picks its strategy once, as std's does: if the sets'
//! ranges don't overlap, every element of `self` but a shared end; if
//! `other` is much larger, a lookup in it for each element of `self`;
//! otherwise, a walk of both sets side by side.

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt;
use core::iter::FusedIterator;
use core::ops::Sub;

use super::lookups::LookupsAhead;
use super::merge::{MergeSides, MergeStep};
use super::{BBTreeSet, Iter};
use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};

/// A lazy iterator producing elements in the difference of `BBTreeSet`s.
///
/// This `struct` is created by the [`difference`] method on [`BBTreeSet`].
/// See its documentation for more.
///
/// [`difference`]: BBTreeSet::difference
#[must_use = "this returns the difference as an iterator, \
              without modifying either input set"]
pub struct Difference<'a, T: 'a, S: InlineKeyStrategy<T> = Auto, A: Allocator + Clone = Global> {
    strategy: DifferenceStrategy<'a, T, S, A>,
}

/// How a [`Difference`] finds the elements of `self` not in `other`.
#[expect(
    clippy::large_enum_variant,
    reason = "the iterator lives by value, as std's; boxing the walks would allocate for every set operation"
)]
enum DifferenceStrategy<'a, T, S: InlineKeyStrategy<T>, A: Allocator + Clone> {
    /// Walks all of `self` and some of `other`, side by side, a leaf's run
    /// of each at a time (`merge.rs`).
    Stitch(MergeSides<'a, T, S>),
    /// Walks `self` and looks each element up in `other`, a batch ahead.
    Search {
        self_iter: Iter<'a, T, S>,
        other_set: &'a BBTreeSet<T, S, A>,
        lookups: LookupsAhead,
    },
    /// Walks `self`, whose elements left are not in `other`.
    Iterate(Iter<'a, T, S>),
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>, A: Allocator + Clone> Iterator
    for Difference<'a, T, S, A>
{
    type Item = &'a T;

    // Inlined into the caller's loop, see `merge.rs`.
    #[inline(always)]
    fn next(&mut self) -> Option<&'a T> {
        match &mut self.strategy {
            // std's steps: `self`'s next element taken out, then `other`'s
            // peeked at, and passed over while not greater.
            DifferenceStrategy::Stitch(MergeSides { left, right }) => {
                let mut self_next = left.next()?;
                loop {
                    let order = right
                        .peek()
                        .map_or(Ordering::Less, |other_next| self_next.cmp(other_next));
                    match order {
                        Ordering::Less => return Some(self_next),
                        Ordering::Equal => {
                            self_next = left.next()?;
                            right.advance();
                        }
                        Ordering::Greater => right.advance(),
                    }
                }
            }
            DifferenceStrategy::Search {
                self_iter,
                other_set,
                lookups,
            } => loop {
                if let (self_next, false) = lookups.next(self_iter, other_set)? {
                    return Some(self_next);
                }
            },
            DifferenceStrategy::Iterate(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (self_len, other_len) = match &self.strategy {
            DifferenceStrategy::Stitch(sides) => sides.lens(),
            DifferenceStrategy::Search {
                self_iter,
                other_set,
                ..
            } => (self_iter.len(), other_set.len()),
            DifferenceStrategy::Iterate(iter) => (iter.len(), 0),
        };
        (self_len.saturating_sub(other_len), Some(self_len))
    }

    fn fold<B, F>(self, accumulator: B, mut fold: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        match self.strategy {
            DifferenceStrategy::Stitch(sides) => {
                sides.fold_steps::<B, true, false>(accumulator, |accumulator, step| match step {
                    MergeStep::Left(element) => fold(accumulator, element),
                    MergeStep::Right(_) | MergeStep::Both(_) => accumulator,
                })
            }
            DifferenceStrategy::Search {
                mut self_iter,
                other_set,
                mut lookups,
            } => {
                let mut accumulator = accumulator;
                while let Some((element, found)) = lookups.next(&mut self_iter, other_set) {
                    if !found {
                        accumulator = fold(accumulator, element);
                    }
                }
                accumulator
            }
            DifferenceStrategy::Iterate(iter) => iter.fold(accumulator, fold),
        }
    }

    fn min(mut self) -> Option<&'a T> {
        self.next()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>, A: Allocator + Clone> FusedIterator
    for Difference<'_, T, S, A>
{
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> Clone for Difference<'_, T, S, A> {
    fn clone(&self) -> Self {
        let strategy = match &self.strategy {
            DifferenceStrategy::Stitch(sides) => DifferenceStrategy::Stitch(sides.clone()),
            DifferenceStrategy::Search {
                self_iter,
                other_set,
                lookups,
            } => DifferenceStrategy::Search {
                self_iter: self_iter.clone(),
                other_set,
                lookups: *lookups,
            },
            DifferenceStrategy::Iterate(iter) => DifferenceStrategy::Iterate(iter.clone()),
        };
        Self { strategy }
    }
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>, A: Allocator + Clone> fmt::Debug
    for Difference<'_, T, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Difference").field(&self.strategy).finish()
    }
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>, A: Allocator + Clone> fmt::Debug
    for DifferenceStrategy<'_, T, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stitch(sides) => f.debug_tuple("Stitch").field(sides).finish(),
            Self::Search {
                self_iter,
                other_set,
                ..
            } => f
                .debug_struct("Search")
                .field("self_iter", self_iter)
                .field("other_set", other_set)
                .finish(),
            Self::Iterate(iter) => f.debug_tuple("Iterate").field(iter).finish(),
        }
    }
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> BBTreeSet<T, S, A> {
    /// Visits the elements representing the difference, i.e., the elements
    /// that are in `self` but not in `other`, in ascending order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut a = BBTreeSet::new();
    /// a.insert(1);
    /// a.insert(2);
    ///
    /// let mut b = BBTreeSet::new();
    /// b.insert(2);
    /// b.insert(3);
    ///
    /// let diff: Vec<_> = a.difference(&b).cloned().collect();
    /// assert_eq!(diff, [1]);
    /// ```
    pub fn difference<'a>(&'a self, other: &'a Self) -> Difference<'a, T, S, A>
    where
        T: Ord,
    {
        let strategy = if let Some(self_min) = self.first()
            && let Some(self_max) = self.last()
            && let Some(other_min) = other.first()
            && let Some(other_max) = other.last()
        {
            match (self_min.cmp(other_max), self_max.cmp(other_min)) {
                (Ordering::Greater, _) | (_, Ordering::Less) => {
                    DifferenceStrategy::Iterate(self.iter())
                }
                (Ordering::Equal, _) => {
                    let mut self_iter = self.iter();
                    self_iter.next();
                    DifferenceStrategy::Iterate(self_iter)
                }
                (_, Ordering::Equal) => {
                    let mut self_iter = self.iter();
                    self_iter.next_back();
                    DifferenceStrategy::Iterate(self_iter)
                }
                _ if other.prefers_lookups(self.len()) => DifferenceStrategy::Search {
                    self_iter: self.iter(),
                    other_set: other,
                    lookups: LookupsAhead::default(),
                },
                _ => DifferenceStrategy::Stitch(MergeSides::new(self.iter(), other.iter())),
            }
        } else {
            DifferenceStrategy::Iterate(self.iter())
        };
        Difference { strategy }
    }
}

impl<T: Ord + Clone, S: InlineKeyStrategy<T>, A: Allocator + Clone> Sub<&BBTreeSet<T, S, A>>
    for &BBTreeSet<T, S, A>
{
    type Output = BBTreeSet<T, S, A>;

    /// Returns the difference of `self` and `rhs` as a new `BBTreeSet<T>`,
    /// built into full nodes without searching it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let a = BBTreeSet::from([1, 2, 3]);
    /// let b = BBTreeSet::from([3, 4, 5]);
    ///
    /// let result = &a - &b;
    /// assert_eq!(result, BBTreeSet::from([1, 2]));
    /// ```
    fn sub(self, rhs: &BBTreeSet<T, S, A>) -> BBTreeSet<T, S, A> {
        BBTreeSet::from_ascending(self.difference(rhs).cloned(), self.allocator().clone())
    }
}
