//! Merging two sets' iterators in order, element by element: the core of
//! [`Union`] and [`SymmetricDifference`], as std's `MergeIterInner` is of
//! theirs.
//!
//! [`Union`]: super::Union
//! [`SymmetricDifference`]: super::SymmetricDifference

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt;

use super::Iter;
use crate::inline_key::InlineKeyStrategy;

/// The elements of two sets in ascending order, in pairs: an element of
/// both, as both sides of a pair, or one of either set alone.
pub(super) struct AscendingPairs<'a, T, S> {
    a: Iter<'a, T, S>,
    b: Iter<'a, T, S>,
    /// The element taken from one side and found greater than the other
    /// side's, which the next pair compares again.
    peeked: Option<Peeked<'a, T>>,
}

/// Which side an element held back came from.
#[derive(Debug)]
enum Peeked<'a, T> {
    A(&'a T),
    B(&'a T),
}

impl<T> Clone for Peeked<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Peeked<'_, T> {}

impl<T, S> Clone for AscendingPairs<'_, T, S> {
    fn clone(&self) -> Self {
        Self {
            a: self.a.clone(),
            b: self.b.clone(),
            peeked: self.peeked,
        }
    }
}

/// Prints as std's `MergeIterInner`, so that `Union` and
/// `SymmetricDifference` print as std's do.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for AscendingPairs<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MergeIterInner")
            .field(&self.a)
            .field(&self.b)
            .field(&self.peeked)
            .finish()
    }
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>> AscendingPairs<'a, T, S> {
    pub(super) fn new(a: Iter<'a, T, S>, b: Iter<'a, T, S>) -> Self {
        Self { a, b, peeked: None }
    }

    /// The next pair: the smaller of the two sides' next elements alone, on
    /// its side, or, if they are equal, both. `(None, None)` once both
    /// sides are done, and from then on.
    pub(super) fn next_pair(&mut self) -> (Option<&'a T>, Option<&'a T>) {
        let (mut a_next, mut b_next) = match self.peeked.take() {
            Some(Peeked::A(a_next)) => (Some(a_next), self.b.next()),
            Some(Peeked::B(b_next)) => (self.a.next(), Some(b_next)),
            None => (self.a.next(), self.b.next()),
        };
        if let (Some(a_element), Some(b_element)) = (a_next, b_next) {
            match a_element.cmp(b_element) {
                Ordering::Less => self.peeked = b_next.take().map(Peeked::B),
                Ordering::Greater => self.peeked = a_next.take().map(Peeked::A),
                Ordering::Equal => {}
            }
        }
        (a_next, b_next)
    }

    /// How many elements each side has left, the one held back included.
    pub(super) fn lens(&self) -> (usize, usize) {
        match self.peeked {
            Some(Peeked::A(_)) => (1 + self.a.len(), self.b.len()),
            Some(Peeked::B(_)) => (self.a.len(), 1 + self.b.len()),
            None => (self.a.len(), self.b.len()),
        }
    }
}
