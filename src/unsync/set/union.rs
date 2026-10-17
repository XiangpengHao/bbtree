//! The union of two sets: [`Union`], which merges their elements
//! (`merge.rs`), and `|`, which builds a set of it, as std's.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::FusedIterator;
use core::ops::BitOr;

use super::BBTreeSet;
use super::merge::{HeldSide, MergeSides};
use crate::allocator::Allocator;
use crate::inline_key::{Auto, InlineKeyStrategy};

/// A lazy iterator producing elements in the union of `BBTreeSet`s.
///
/// This `struct` is created by the [`union`] method on [`BBTreeSet`]. See
/// its documentation for more.
///
/// [`union`]: BBTreeSet::union
#[must_use = "this returns the union as an iterator, \
              without modifying either input set"]
pub struct Union<'a, T: 'a, S = Auto> {
    sides: MergeSides<'a, T, S>,
    /// The side std's would hold an element of, taken out of its iterator.
    held: HeldSide,
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>> Iterator for Union<'a, T, S> {
    type Item = &'a T;

    // Inlined into the caller's loop, see `merge.rs`.
    #[inline(always)]
    fn next(&mut self) -> Option<&'a T> {
        let (step, held) = self.sides.next_step()?;
        self.held = held;
        Some(step.element())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (a_len, b_len) = self.sides.lens();
        // No overflow: every element takes room in a node, even a zero-sized
        // one, so two sets can't hold `usize::MAX` elements between them.
        (a_len.max(b_len), Some(a_len + b_len))
    }

    fn fold<A, F>(self, accumulator: A, mut fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.sides
            .fold_steps::<A, true, true>(accumulator, |accumulator, step| {
                fold(accumulator, step.element())
            })
    }

    fn min(mut self) -> Option<&'a T> {
        self.next()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>> FusedIterator for Union<'_, T, S> {}

impl<T, S> Clone for Union<'_, T, S> {
    fn clone(&self) -> Self {
        Self {
            sides: self.sides.clone(),
            held: self.held,
        }
    }
}

/// Prints as std's does: `Union(MergeIterInner(..))`.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for Union<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Union")
            .field(&self.sides.printed_as_merge_iter(self.held))
            .finish()
    }
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> BBTreeSet<T, S, A> {
    /// Visits the elements representing the union, i.e., all the elements
    /// in `self` or `other`, without duplicates, in ascending order.
    ///
    /// Of equal elements, it visits `self`'s.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
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
        Union {
            sides: MergeSides::new(self.iter(), other.iter()),
            held: HeldSide::Neither,
        }
    }
}

impl<T: Ord + Clone, S: InlineKeyStrategy<T>, A: Allocator + Clone> BitOr<&BBTreeSet<T, S, A>>
    for &BBTreeSet<T, S, A>
{
    type Output = BBTreeSet<T, S, A>;

    /// Returns the union of `self` and `rhs` as a new `BBTreeSet<T>`, built
    /// into full nodes without searching it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let a = BBTreeSet::from([1, 2, 3]);
    /// let b = BBTreeSet::from([3, 4, 5]);
    ///
    /// let result = &a | &b;
    /// assert_eq!(result, BBTreeSet::from([1, 2, 3, 4, 5]));
    /// ```
    fn bitor(self, rhs: &BBTreeSet<T, S, A>) -> BBTreeSet<T, S, A> {
        BBTreeSet::from_ascending(self.union(rhs).cloned(), self.allocator().clone())
    }
}
