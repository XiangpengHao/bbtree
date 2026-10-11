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

use super::{BBTreeSet, Iter};
use crate::inline_key::{Auto, InlineKeyStrategy};

/// A lazy iterator producing elements in the intersection of `BBTreeSet`s.
///
/// This `struct` is created by the [`intersection`] method on
/// [`BBTreeSet`]. See its documentation for more.
///
/// [`intersection`]: BBTreeSet::intersection
#[must_use = "this returns the intersection as an iterator, \
              without modifying either input set"]
pub struct Intersection<'a, T: 'a, S: InlineKeyStrategy<T> = Auto> {
    strategy: IntersectionStrategy<'a, T, S>,
}

/// How an [`Intersection`] finds the elements of both sets.
#[expect(
    clippy::large_enum_variant,
    reason = "the iterator lives by value, as std's; boxing the walks would allocate for every set operation"
)]
enum IntersectionStrategy<'a, T, S: InlineKeyStrategy<T>> {
    /// Walks both sets, of similar sizes, side by side.
    Stitch {
        a: Iter<'a, T, S>,
        b: Iter<'a, T, S>,
    },
    /// Walks the small set and looks each element up in the large one.
    Search {
        small_iter: Iter<'a, T, S>,
        large_set: &'a BBTreeSet<T, S>,
    },
    /// The one element left, or none.
    Answer(Option<&'a T>),
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>> Iterator for Intersection<'a, T, S> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        match &mut self.strategy {
            IntersectionStrategy::Stitch { a, b } => {
                let mut a_next = a.next()?;
                let mut b_next = b.next()?;
                loop {
                    match a_next.cmp(b_next) {
                        Ordering::Less => a_next = a.next()?,
                        Ordering::Greater => b_next = b.next()?,
                        Ordering::Equal => return Some(a_next),
                    }
                }
            }
            IntersectionStrategy::Search {
                small_iter,
                large_set,
            } => small_iter.find(|&small_next| large_set.contains(small_next)),
            IntersectionStrategy::Answer(answer) => answer.take(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.strategy {
            IntersectionStrategy::Stitch { a, b } => (0, Some(a.len().min(b.len()))),
            IntersectionStrategy::Search { small_iter, .. } => (0, Some(small_iter.len())),
            IntersectionStrategy::Answer(None) => (0, Some(0)),
            IntersectionStrategy::Answer(Some(_)) => (1, Some(1)),
        }
    }

    fn min(mut self) -> Option<&'a T> {
        self.next()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>> FusedIterator for Intersection<'_, T, S> {}

impl<T, S: InlineKeyStrategy<T>> Clone for Intersection<'_, T, S> {
    fn clone(&self) -> Self {
        let strategy = match &self.strategy {
            IntersectionStrategy::Stitch { a, b } => IntersectionStrategy::Stitch {
                a: a.clone(),
                b: b.clone(),
            },
            IntersectionStrategy::Search {
                small_iter,
                large_set,
            } => IntersectionStrategy::Search {
                small_iter: small_iter.clone(),
                large_set,
            },
            IntersectionStrategy::Answer(answer) => IntersectionStrategy::Answer(*answer),
        };
        Self { strategy }
    }
}

/// Prints as std's does, the strategy's name and field names included.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for Intersection<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Intersection").field(&self.strategy).finish()
    }
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for IntersectionStrategy<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stitch { a, b } => f
                .debug_struct("Stitch")
                .field("a", a)
                .field("b", b)
                .finish(),
            Self::Search {
                small_iter,
                large_set,
            } => f
                .debug_struct("Search")
                .field("small_iter", small_iter)
                .field("large_set", large_set)
                .finish(),
            Self::Answer(answer) => f.debug_tuple("Answer").field(answer).finish(),
        }
    }
}

impl<T, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
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
    pub fn intersection<'a>(&'a self, other: &'a Self) -> Intersection<'a, T, S>
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
                },
                _ if self.prefers_lookups(other.len()) => IntersectionStrategy::Search {
                    small_iter: other.iter(),
                    large_set: self,
                },
                _ => IntersectionStrategy::Stitch {
                    a: self.iter(),
                    b: other.iter(),
                },
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

impl<T: Ord + Clone, S: InlineKeyStrategy<T>> BitAnd<&BBTreeSet<T, S>> for &BBTreeSet<T, S> {
    type Output = BBTreeSet<T, S>;

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
    fn bitand(self, rhs: &BBTreeSet<T, S>) -> BBTreeSet<T, S> {
        BBTreeSet::from_ascending(self.intersection(rhs).cloned())
    }
}
