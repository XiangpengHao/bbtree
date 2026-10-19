//! Removing the entries a predicate picks: [`ExtractIf`], and `retain` on
//! it, as std's.
//!
//! The walk is a node stack at the next entry to visit, as std's is a leaf
//! edge. It starts at the range's start, and stops at the first entry past
//! its end, comparing each entry it visits with the end, as std's does. An
//! entry the predicate passes over stays where it is; one it picks comes out
//! as a removal takes it, and the walk goes on from where the entry after it
//! went (`tree/remove.rs`). The map is whole between steps, so a panicking
//! predicate or `Ord` leaves it whole.
//!
//! The walk itself, [`ExtractionWalk`], takes the predicate at each step,
//! as std's `ExtractIfInner` does, so that the set's `ExtractIf`, whose
//! predicate sees elements alone, shares it.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::FusedIterator;
use core::ops::{Bound, RangeBounds};

use super::BBTreeMap;
use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::node_stack::NodeStackMut;

/// An iterator produced by calling `extract_if` on [`BBTreeMap`].
#[must_use = "iterators are lazy and do nothing unless consumed; \
              use `retain` to remove and drop elements"]
pub struct ExtractIf<'a, K, V, R, F, S: InlineKeyStrategy<K> = Auto, A: Allocator + Clone = Global>
{
    walk: ExtractionWalk<'a, K, V, R, S, A>,
    pred: F,
}

/// The walk of an `extract_if`, but for its predicate, which each step takes.
pub(crate) struct ExtractionWalk<'a, K, V, R, S: InlineKeyStrategy<K>, A: Allocator> {
    /// At the next entry to visit; detached once the walk is past the range.
    stack: NodeStackMut<'a, K, V, S>,
    /// The map's length, which each entry handed out leaves.
    length: &'a mut usize,
    /// The map's allocator, which the pages a removal frees go back to.
    alloc: &'a A,
    range: R,
}

impl<'a, K: Ord, V, R: RangeBounds<K>, S: InlineKeyStrategy<K>, A: Allocator + Clone>
    ExtractionWalk<'a, K, V, R, S, A>
{
    /// A walk of `map`'s entries in `range`, at the first of them.
    pub(crate) fn new(map: &'a mut BBTreeMap<K, V, S, A>, range: R) -> Self {
        let has_root = map.tree.has_root();
        let mut stack = NodeStackMut::new(&mut map.tree);
        if has_root {
            stack.go_to_first_entry_from(range.start_bound());
        }
        Self {
            stack,
            length: &mut map.length,
            alloc: &map.alloc,
            range,
        }
    }
}

impl<K, V, R, S: InlineKeyStrategy<K>, A: Allocator + Clone> ExtractionWalk<'_, K, V, R, S, A> {
    /// Hands out the next entry in the range that `pick` picks, passing over
    /// those it doesn't.
    pub(crate) fn next(&mut self, pick: &mut impl FnMut(&K, &mut V) -> bool) -> Option<(K, V)>
    where
        K: PartialOrd,
        R: RangeBounds<K>,
    {
        loop {
            let (key, value) = self.stack.key_value_mut()?;
            let in_range = match self.range.end_bound() {
                Bound::Included(end) => key <= end,
                Bound::Excluded(end) => key < end,
                Bound::Unbounded => true,
            };
            if !in_range {
                self.stack.detach();
                return None;
            }
            if pick(key, value) {
                return Some(self.stack.remove_and_go_to_next(self.alloc, self.length));
            }
            self.stack.go_past_entry();
        }
    }

    pub(crate) fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(*self.length))
    }

    /// The next entry to visit, as std's `Debug` prints it.
    pub(crate) fn peek(&self) -> Option<(&K, &V)> {
        self.stack.key_value()
    }
}

impl<K, V, R, F, S, A> Iterator for ExtractIf<'_, K, V, R, F, S, A>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
    A: Allocator + Clone,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.walk.next(&mut self.pred)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.walk.size_hint()
    }
}

impl<K, V, R, F, S, A> FusedIterator for ExtractIf<'_, K, V, R, F, S, A>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
    A: Allocator + Clone,
{
}

/// Prints the next entry to visit, as std's does.
impl<K: fmt::Debug, V: fmt::Debug, R, F, S: InlineKeyStrategy<K>, A: Allocator + Clone> fmt::Debug
    for ExtractIf<'_, K, V, R, F, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtractIf")
            .field("peek", &self.walk.peek())
            .finish_non_exhaustive()
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Creates an iterator that visits elements (key-value pairs) in the
    /// specified range in ascending key order and uses a closure to
    /// determine if an element should be removed.
    ///
    /// If the closure returns `true`, the element is removed from the map
    /// and yielded. If the closure returns `false`, or panics, the element
    /// remains in the map and will not be yielded.
    ///
    /// The iterator also lets you mutate the value of each element in the
    /// closure, regardless of whether you choose to keep or remove it.
    ///
    /// If the returned `ExtractIf` is not exhausted, e.g. because it is
    /// dropped without iterating or the iteration short-circuits, then the
    /// remaining elements will be retained. Use [`retain`] with a negated
    /// predicate if you do not need the returned iterator.
    ///
    /// [`retain`]: BBTreeMap::retain
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    ///
    /// // Splitting a map into even and odd keys, reusing the original map:
    /// let mut map: BBTreeMap<i32, i32> = (0..8).map(|x| (x, x)).collect();
    /// let evens: BBTreeMap<_, _> = map.extract_if(.., |k, _v| k % 2 == 0).collect();
    /// let odds = map;
    /// assert_eq!(evens.keys().copied().collect::<Vec<_>>(), [0, 2, 4, 6]);
    /// assert_eq!(odds.keys().copied().collect::<Vec<_>>(), [1, 3, 5, 7]);
    ///
    /// // Splitting a map into low and high halves, reusing the original map:
    /// let mut map: BBTreeMap<i32, i32> = (0..8).map(|x| (x, x)).collect();
    /// let low: BBTreeMap<_, _> = map.extract_if(0..4, |_k, _v| true).collect();
    /// let high = map;
    /// assert_eq!(low.keys().copied().collect::<Vec<_>>(), [0, 1, 2, 3]);
    /// assert_eq!(high.keys().copied().collect::<Vec<_>>(), [4, 5, 6, 7]);
    /// ```
    pub fn extract_if<F, R>(&mut self, range: R, pred: F) -> ExtractIf<'_, K, V, R, F, S, A>
    where
        R: RangeBounds<K>,
        F: FnMut(&K, &mut V) -> bool,
    {
        ExtractIf {
            walk: ExtractionWalk::new(self, range),
            pred,
        }
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// In other words, remove all pairs `(k, v)` for which `f(&k, &mut v)`
    /// returns `false`. The elements are visited in ascending key order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<i32, i32> = (0..8).map(|x| (x, x * 10)).collect();
    /// // Keep only the elements with even-numbered keys.
    /// map.retain(|&k, _| k % 2 == 0);
    /// assert!(map.into_iter().eq(vec![(0, 0), (2, 20), (4, 40), (6, 60)]));
    /// ```
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        self.extract_if(.., |key, value| !f(key, value))
            .for_each(drop);
    }
}
