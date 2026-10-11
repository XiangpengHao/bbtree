//! Removing the elements a predicate picks: [`ExtractIf`], on the map's walk
//! (`map/extract_if.rs`), with the predicate seeing each element alone, as
//! std's. Like the map's, it takes every element out of the set and puts the
//! kept ones back: O(n), wherever the range is.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::FusedIterator;
use core::ops::RangeBounds;

use super::{BBTreeSet, SetValZST};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::map::ExtractionWalk;

/// An iterator produced by calling `extract_if` on [`BBTreeSet`].
#[must_use = "iterators are lazy and do nothing unless consumed; \
              use `retain` or `extract_if().for_each(drop)` to remove and discard elements"]
pub struct ExtractIf<'a, T, R, F, S: InlineKeyStrategy<T> = Auto> {
    walk: ExtractionWalk<'a, T, SetValZST, R, S>,
    pred: F,
}

impl<T, R, F, S> Iterator for ExtractIf<'_, T, R, F, S>
where
    T: PartialOrd,
    R: RangeBounds<T>,
    F: FnMut(&T) -> bool,
    S: InlineKeyStrategy<T>,
{
    type Item = T;

    fn next(&mut self) -> Option<T> {
        let Self { walk, pred } = self;
        walk.next(&mut |element, _| pred(element))
            .map(|(element, _)| element)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.walk.size_hint()
    }
}

impl<T, R, F, S> FusedIterator for ExtractIf<'_, T, R, F, S>
where
    T: PartialOrd,
    R: RangeBounds<T>,
    F: FnMut(&T) -> bool,
    S: InlineKeyStrategy<T>,
{
}

/// Prints the next element to visit, as std's does.
impl<T: fmt::Debug, R, F, S: InlineKeyStrategy<T>> fmt::Debug for ExtractIf<'_, T, R, F, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtractIf")
            .field("peek", &self.walk.peek().map(|(element, _)| element))
            .finish_non_exhaustive()
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
    /// Creates an iterator that visits elements in the specified range in
    /// ascending order and uses a closure to determine if an element should
    /// be removed.
    ///
    /// If the closure returns `true`, the element is removed from the set
    /// and yielded. If the closure returns `false`, or panics, the element
    /// remains in the set and will not be yielded.
    ///
    /// If the returned `ExtractIf` is not exhausted, e.g. because it is
    /// dropped without iterating or the iteration short-circuits, then the
    /// remaining elements will be retained. Use `extract_if().for_each(drop)`
    /// if you do not need the returned iterator, or [`retain`] with a negated
    /// predicate if you also do not need to restrict the range.
    ///
    /// Unlike std's, this walk takes every element out of the set and puts
    /// the kept ones back, wherever the range is: O(n).
    ///
    /// [`retain`]: BBTreeSet::retain
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// // Splitting a set into even and odd values, reusing the original set:
    /// let mut set: BBTreeSet<i32> = (0..8).collect();
    /// let evens: BBTreeSet<_> = set.extract_if(.., |v| v % 2 == 0).collect();
    /// let odds = set;
    /// assert_eq!(evens.into_iter().collect::<Vec<_>>(), vec![0, 2, 4, 6]);
    /// assert_eq!(odds.into_iter().collect::<Vec<_>>(), vec![1, 3, 5, 7]);
    ///
    /// // Splitting a set into low and high halves, reusing the original set:
    /// let mut set: BBTreeSet<i32> = (0..8).collect();
    /// let low: BBTreeSet<_> = set.extract_if(0..4, |_v| true).collect();
    /// let high = set;
    /// assert_eq!(low.into_iter().collect::<Vec<_>>(), [0, 1, 2, 3]);
    /// assert_eq!(high.into_iter().collect::<Vec<_>>(), [4, 5, 6, 7]);
    /// ```
    pub fn extract_if<F, R>(&mut self, range: R, pred: F) -> ExtractIf<'_, T, R, F, S>
    where
        R: RangeBounds<T>,
        F: FnMut(&T) -> bool,
    {
        ExtractIf {
            walk: ExtractionWalk::new(&mut self.map, range),
            pred,
        }
    }
}
