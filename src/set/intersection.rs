//! The intersection of two sets: [`Intersection`], `is_disjoint` on it, and
//! `&`, which builds a set of it, as std's.
//!
//! Making the iterator picks its strategy once, as std's does: if the sets'
//! ranges meet at most at one end, that answer; if one set is much larger,
//! a lookup in it for each element of the other; otherwise, a walk of both
//! sets side by side.

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt;
use core::iter::FusedIterator;
use core::ops::BitAnd;

use super::lookups::LookupsAhead;
use super::merge::{MergeSides, MergeStep};
use super::{BBTreeSet, Iter};
use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};

/// A lazy iterator producing elements in the intersection of `BBTreeSet`s.
///
/// This `struct` is created by the [`intersection`] method on
/// [`BBTreeSet`]. See its documentation for more.
///
/// [`intersection`]: BBTreeSet::intersection
#[must_use = "this returns the intersection as an iterator, \
              without modifying either input set"]
pub struct Intersection<'a, T: 'a, S: InlineKeyStrategy<T> = Auto, A: Allocator + Clone = Global> {
    strategy: IntersectionStrategy<'a, T, S, A>,
}

/// How an [`Intersection`] finds the elements of both sets.
#[expect(
    clippy::large_enum_variant,
    reason = "the iterator lives by value, as std's; boxing the walks would allocate for every set operation"
)]
enum IntersectionStrategy<'a, T, S: InlineKeyStrategy<T>, A: Allocator + Clone> {
    /// Walks both sets, of similar sizes, side by side, a leaf's run of
    /// each at a time (`merge.rs`).
    Stitch(MergeSides<'a, T, S>),
    /// Walks the small set and looks each element up in the large one, a
    /// batch ahead.
    Search {
        small_iter: Iter<'a, T, S>,
        large_set: &'a BBTreeSet<T, S, A>,
        lookups: LookupsAhead,
    },
    /// The one element left, or none.
    Answer(Option<&'a T>),
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>, A: Allocator + Clone> Iterator
    for Intersection<'a, T, S, A>
{
    type Item = &'a T;

    // Inlined into the caller's loop, see `merge.rs`.
    #[inline(always)]
    fn next(&mut self) -> Option<&'a T> {
        match &mut self.strategy {
            // Each side's next element is taken out before comparing, as in
            // std's: the one left over when the other side ends is gone.
            IntersectionStrategy::Stitch(MergeSides { left, right }) => {
                let mut a_next = left.next()?;
                let mut b_next = right.next()?;
                loop {
                    match a_next.cmp(b_next) {
                        Ordering::Less => a_next = left.next()?,
                        Ordering::Greater => b_next = right.next()?,
                        Ordering::Equal => return Some(a_next),
                    }
                }
            }
            IntersectionStrategy::Search {
                small_iter,
                large_set,
                lookups,
            } => loop {
                if let (small_next, true) = lookups.next(small_iter, large_set)? {
                    return Some(small_next);
                }
            },
            IntersectionStrategy::Answer(answer) => answer.take(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.strategy {
            IntersectionStrategy::Stitch(sides) => {
                let (a_len, b_len) = sides.lens();
                (0, Some(a_len.min(b_len)))
            }
            IntersectionStrategy::Search { small_iter, .. } => (0, Some(small_iter.len())),
            IntersectionStrategy::Answer(None) => (0, Some(0)),
            IntersectionStrategy::Answer(Some(_)) => (1, Some(1)),
        }
    }

    fn fold<B, F>(self, accumulator: B, mut fold: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        match self.strategy {
            IntersectionStrategy::Stitch(sides) => {
                sides.fold_steps::<B, false, false>(accumulator, |accumulator, step| match step {
                    MergeStep::Both(element) => fold(accumulator, element),
                    MergeStep::Left(_) | MergeStep::Right(_) => accumulator,
                })
            }
            IntersectionStrategy::Search {
                mut small_iter,
                large_set,
                mut lookups,
            } => {
                let mut accumulator = accumulator;
                while let Some((element, found)) = lookups.next(&mut small_iter, large_set) {
                    if found {
                        accumulator = fold(accumulator, element);
                    }
                }
                accumulator
            }
            IntersectionStrategy::Answer(answer) => answer.into_iter().fold(accumulator, fold),
        }
    }

    fn min(mut self) -> Option<&'a T> {
        self.next()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>, A: Allocator + Clone> FusedIterator
    for Intersection<'_, T, S, A>
{
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> Clone for Intersection<'_, T, S, A> {
    fn clone(&self) -> Self {
        let strategy = match &self.strategy {
            IntersectionStrategy::Stitch(sides) => IntersectionStrategy::Stitch(sides.clone()),
            IntersectionStrategy::Search {
                small_iter,
                large_set,
                lookups,
            } => IntersectionStrategy::Search {
                small_iter: small_iter.clone(),
                large_set,
                lookups: *lookups,
            },
            IntersectionStrategy::Answer(answer) => IntersectionStrategy::Answer(*answer),
        };
        Self { strategy }
    }
}

/// Prints as std's does, the strategy's name and field names included.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>, A: Allocator + Clone> fmt::Debug
    for Intersection<'_, T, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Intersection").field(&self.strategy).finish()
    }
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>, A: Allocator + Clone> fmt::Debug
    for IntersectionStrategy<'_, T, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stitch(sides) => f
                .debug_struct("Stitch")
                .field("a", &sides.left.printed_as_iter(false))
                .field("b", &sides.right.printed_as_iter(false))
                .finish(),
            Self::Search {
                small_iter,
                large_set,
                ..
            } => f
                .debug_struct("Search")
                .field("small_iter", small_iter)
                .field("large_set", large_set)
                .finish(),
            Self::Answer(answer) => f.debug_tuple("Answer").field(answer).finish(),
        }
    }
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> BBTreeSet<T, S, A> {
    /// Visits the elements representing the intersection, i.e., the
    /// elements that are both in `self` and `other`, in ascending order.
    ///
    /// Of equal elements, it visits the smaller set's when it looks them up
    /// in the larger, and `self`'s otherwise, as std's does.
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
    /// let intersection: Vec<_> = a.intersection(&b).cloned().collect();
    /// assert_eq!(intersection, [2]);
    /// ```
    pub fn intersection<'a>(&'a self, other: &'a Self) -> Intersection<'a, T, S, A>
    where
        T: Ord,
    {
        let strategy = if let Some(self_min) = self.first()
            && let Some(self_max) = self.last()
            && let Some(other_min) = other.first()
            && let Some(other_max) = other.last()
        {
            match (self_min.cmp(other_max), self_max.cmp(other_min)) {
                (Ordering::Greater, _) | (_, Ordering::Less) => IntersectionStrategy::Answer(None),
                (Ordering::Equal, _) => IntersectionStrategy::Answer(Some(self_min)),
                (_, Ordering::Equal) => IntersectionStrategy::Answer(Some(self_max)),
                _ if other.prefers_lookups(self.len()) => IntersectionStrategy::Search {
                    small_iter: self.iter(),
                    large_set: other,
                    lookups: LookupsAhead::default(),
                },
                _ if self.prefers_lookups(other.len()) => IntersectionStrategy::Search {
                    small_iter: other.iter(),
                    large_set: self,
                    lookups: LookupsAhead::default(),
                },
                _ => IntersectionStrategy::Stitch(MergeSides::new(self.iter(), other.iter())),
            }
        } else {
            IntersectionStrategy::Answer(None)
        };
        Intersection { strategy }
    }

    /// Returns `true` if `self` has no elements in common with `other`.
    /// This is equivalent to checking for an empty intersection.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let a = BBTreeSet::from([1, 2, 3]);
    /// let mut b = BBTreeSet::new();
    ///
    /// assert_eq!(a.is_disjoint(&b), true);
    /// b.insert(4);
    /// assert_eq!(a.is_disjoint(&b), true);
    /// b.insert(1);
    /// assert_eq!(a.is_disjoint(&b), false);
    /// ```
    #[must_use]
    pub fn is_disjoint(&self, other: &Self) -> bool
    where
        T: Ord,
    {
        self.intersection(other).next().is_none()
    }
}

impl<T: Ord + Clone, S: InlineKeyStrategy<T>, A: Allocator + Clone> BitAnd<&BBTreeSet<T, S, A>>
    for &BBTreeSet<T, S, A>
{
    type Output = BBTreeSet<T, S, A>;

    /// Returns the intersection of `self` and `rhs` as a new `BBTreeSet<T>`,
    /// built into full nodes without searching it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let a = BBTreeSet::from([1, 2, 3]);
    /// let b = BBTreeSet::from([2, 3, 4]);
    ///
    /// let result = &a & &b;
    /// assert_eq!(result, BBTreeSet::from([2, 3]));
    /// ```
    fn bitand(self, rhs: &BBTreeSet<T, S, A>) -> BBTreeSet<T, S, A> {
        BBTreeSet::from_ascending(self.intersection(rhs).cloned(), self.allocator().clone())
    }
}
