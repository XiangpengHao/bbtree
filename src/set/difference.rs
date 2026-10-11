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

use super::merge::{MergeSides, MergeStep};
use super::{BBTreeSet, Iter};
use crate::inline_key::{Auto, InlineKeyStrategy};

/// A lazy iterator producing elements in the difference of `BBTreeSet`s.
///
/// This `struct` is created by the [`difference`] method on [`BBTreeSet`].
/// See its documentation for more.
///
/// [`difference`]: BBTreeSet::difference
#[must_use = "this returns the difference as an iterator, \
              without modifying either input set"]
pub struct Difference<'a, T: 'a, S: InlineKeyStrategy<T> = Auto> {
    strategy: DifferenceStrategy<'a, T, S>,
}

/// How a [`Difference`] finds the elements of `self` not in `other`.
#[expect(
    clippy::large_enum_variant,
    reason = "the iterator lives by value, as std's; boxing the walks would allocate for every set operation"
)]
enum DifferenceStrategy<'a, T, S: InlineKeyStrategy<T>> {
    /// Walks all of `self` and some of `other`, side by side, a leaf's run
    /// of each at a time (`merge.rs`).
    Stitch {
        sides: MergeSides<'a, T, S>,
        /// Whether std's, which peeks at `other`'s next element through a
        /// `Peekable`, holds it peeked: for printing as std's does.
        other_peeked: bool,
    },
    /// Walks `self` and looks each element up in `other`.
    Search {
        self_iter: Iter<'a, T, S>,
        other_set: &'a BBTreeSet<T, S>,
    },
    /// Walks `self`, whose elements left are not in `other`.
    Iterate(Iter<'a, T, S>),
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>> Iterator for Difference<'a, T, S> {
    type Item = &'a T;

    // Inlined into the caller's loop, see `merge.rs`.
    #[inline(always)]
    fn next(&mut self) -> Option<&'a T> {
        match &mut self.strategy {
            // std's steps: `self`'s next element taken out, then `other`'s
            // peeked at, and passed over while not greater.
            DifferenceStrategy::Stitch {
                sides: MergeSides { left, right },
                other_peeked,
            } => {
                let mut self_next = left.next()?;
                loop {
                    *other_peeked = true;
                    let order = right
                        .peek()
                        .map_or(Ordering::Less, |other_next| self_next.cmp(other_next));
                    match order {
                        Ordering::Less => return Some(self_next),
                        Ordering::Equal => {
                            self_next = left.next()?;
                            right.advance();
                            *other_peeked = false;
                        }
                        Ordering::Greater => {
                            right.advance();
                            *other_peeked = false;
                        }
                    }
                }
            }
            DifferenceStrategy::Search {
                self_iter,
                other_set,
            } => self_iter.find(|&self_next| !other_set.contains(self_next)),
            DifferenceStrategy::Iterate(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (self_len, other_len) = match &self.strategy {
            DifferenceStrategy::Stitch { sides, .. } => sides.lens(),
            DifferenceStrategy::Search {
                self_iter,
                other_set,
            } => (self_iter.len(), other_set.len()),
            DifferenceStrategy::Iterate(iter) => (iter.len(), 0),
        };
        (self_len.saturating_sub(other_len), Some(self_len))
    }

    fn fold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        match self.strategy {
            DifferenceStrategy::Stitch { sides, .. } => {
                sides.fold_steps::<A, true, false>(accumulator, |accumulator, step| match step {
                    MergeStep::Left(element) => fold(accumulator, element),
                    MergeStep::Right(_) | MergeStep::Both(_) => accumulator,
                })
            }
            DifferenceStrategy::Search {
                self_iter,
                other_set,
            } => self_iter.fold(accumulator, |accumulator, element| {
                if other_set.contains(element) {
                    accumulator
                } else {
                    fold(accumulator, element)
                }
            }),
            DifferenceStrategy::Iterate(iter) => iter.fold(accumulator, fold),
        }
    }

    fn min(mut self) -> Option<&'a T> {
        self.next()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>> FusedIterator for Difference<'_, T, S> {}

impl<T, S: InlineKeyStrategy<T>> Clone for Difference<'_, T, S> {
    fn clone(&self) -> Self {
        let strategy = match &self.strategy {
            DifferenceStrategy::Stitch {
                sides,
                other_peeked,
            } => DifferenceStrategy::Stitch {
                sides: sides.clone(),
                other_peeked: *other_peeked,
            },
            DifferenceStrategy::Search {
                self_iter,
                other_set,
            } => DifferenceStrategy::Search {
                self_iter: self_iter.clone(),
                other_set,
            },
            DifferenceStrategy::Iterate(iter) => DifferenceStrategy::Iterate(iter.clone()),
        };
        Self { strategy }
    }
}

/// Prints as std's does, the strategy's name and field names included.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for Difference<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Difference").field(&self.strategy).finish()
    }
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for DifferenceStrategy<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stitch {
                sides,
                other_peeked,
            } => f
                .debug_struct("Stitch")
                .field("self_iter", &sides.left.printed_as_iter(false))
                .field(
                    "other_iter",
                    &PrintedAsPeekable {
                        sides,
                        peeked: *other_peeked,
                    },
                )
                .finish(),
            // std's names the set `other_iter` here too.
            Self::Search {
                self_iter,
                other_set,
            } => f
                .debug_struct("Search")
                .field("self_iter", self_iter)
                .field("other_iter", other_set)
                .finish(),
            Self::Iterate(iter) => f.debug_tuple("Iterate").field(iter).finish(),
        }
    }
}

/// `other`'s side of a difference, printed as std's `Peekable` over it is,
/// with its next element, or the end, peeked or not.
struct PrintedAsPeekable<'d, 'a, T, S> {
    sides: &'d MergeSides<'a, T, S>,
    peeked: bool,
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for PrintedAsPeekable<'_, '_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let other = &self.sides.right;
        let peeked = self.peeked.then(|| other.first_left());
        f.debug_struct("Peekable")
            .field("iter", &other.printed_as_iter(self.peeked))
            .field("peeked", &peeked)
            .finish()
    }
}

impl<T, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
    /// Visits the elements representing the difference, i.e., the elements
    /// that are in `self` but not in `other`, in ascending order.
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
    /// let diff: Vec<_> = a.difference(&b).cloned().collect();
    /// assert_eq!(diff, [1]);
    /// ```
    pub fn difference<'a>(&'a self, other: &'a Self) -> Difference<'a, T, S>
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
                },
                _ => DifferenceStrategy::Stitch {
                    sides: MergeSides::new(self.iter(), other.iter()),
                    other_peeked: false,
                },
            }
        } else {
            DifferenceStrategy::Iterate(self.iter())
        };
        Difference { strategy }
    }
}

impl<T: Ord + Clone, S: InlineKeyStrategy<T>> Sub<&BBTreeSet<T, S>> for &BBTreeSet<T, S> {
    type Output = BBTreeSet<T, S>;

    /// Returns the difference of `self` and `rhs` as a new `BBTreeSet<T>`,
    /// built into full nodes without searching it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let a = BBTreeSet::from([1, 2, 3]);
    /// let b = BBTreeSet::from([3, 4, 5]);
    ///
    /// let result = &a - &b;
    /// assert_eq!(result, BBTreeSet::from([1, 2]));
    /// ```
    fn sub(self, rhs: &BBTreeSet<T, S>) -> BBTreeSet<T, S> {
        BBTreeSet::from_ascending(self.difference(rhs).cloned())
    }
}
