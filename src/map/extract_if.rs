//! Removing the entries a predicate picks: [`ExtractIf`], and `retain` on
//! it, as std's.
//!
//! For now the walk takes every entry out of the map, from the front, and
//! builds the map again from those it keeps (`node/build.rs`): O(n)
//! wherever the range is, where std's walk passes over the entries outside
//! it in place. As std's, it starts at the range's start, having passed the
//! entries before it when made, and stops at the first entry after the
//! range's end, from which on it peeks at nothing. The predicate sees the
//! entries within the range, in ascending order; each one it picks is handed
//! out as it is taken.
//!
//! The map stays empty while the walk lasts. When the walk is dropped,
//! finished or not, or unwinds from a panicking predicate, the entries not
//! handed out go back into it untested, the one being tested included, as
//! std's walk leaves them in place.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::{self, FusedIterator};
use core::mem;
use core::ops::{Bound, RangeBounds, RangeFull};

use super::BBTreeMap;
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node::build::TreeBuilder;

/// An iterator produced by calling `extract_if` on [`BBTreeMap`].
#[must_use = "iterators are lazy and do nothing unless consumed; \
              use `retain` to remove and drop elements"]
pub struct ExtractIf<'a, K, V, R, F, S: InlineKeyStrategy<K> = Auto> {
    /// Empty until the walk is dropped, when it gets back the entries not
    /// handed out.
    map: &'a mut BBTreeMap<K, V, S>,
    /// Whether the map had a tree, which it keeps even if no entry stays.
    had_tree: bool,
    /// The entries not visited yet.
    rest: BBTreeMap<K, V, S>,
    /// The entry the predicate is testing: kept if it panics.
    testing: Option<(K, V)>,
    /// The entries visited and kept, in order, those before the range
    /// included.
    kept: TreeBuilder<K, V, S>,
    /// Whether the walk went past the range's end, or the map's: it visits
    /// nothing more, and peeks at nothing.
    finished: bool,
    range: R,
    pred: F,
}

impl<K: PartialOrd, V, R: RangeBounds<K>, F, S: InlineKeyStrategy<K>> ExtractIf<'_, K, V, R, F, S> {
    fn is_before_range(&self, key: &K) -> bool {
        match self.range.start_bound() {
            Bound::Included(start) => key < start,
            Bound::Excluded(start) => key <= start,
            Bound::Unbounded => false,
        }
    }

    fn is_after_range(&self, key: &K) -> bool {
        match self.range.end_bound() {
            Bound::Included(end) => key > end,
            Bound::Excluded(end) => key >= end,
            Bound::Unbounded => false,
        }
    }
}

impl<K, V, R, F, S> Iterator for ExtractIf<'_, K, V, R, F, S>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        while !self.finished {
            let next_is_after = match self.rest.iter().next() {
                Some((key, _)) => self.is_after_range(key),
                None => true,
            };
            if next_is_after {
                self.finished = true;
                return None;
            }
            let entry = self.rest.take_first()?;
            let (key, value) = self.testing.insert(entry);
            if (self.pred)(key, value) {
                return self.testing.take();
            }
            if let Some((key, value)) = self.testing.take() {
                self.kept.push(key, value);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.kept.len() + self.rest.len()))
    }
}

impl<K, V, R, F, S> FusedIterator for ExtractIf<'_, K, V, R, F, S>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
{
}

/// Prints the next entry to visit, as std's does.
impl<K: fmt::Debug, V: fmt::Debug, R, F, S: InlineKeyStrategy<K>> fmt::Debug
    for ExtractIf<'_, K, V, R, F, S>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let peek = if self.finished {
            None
        } else {
            self.rest.iter().next()
        };
        f.debug_struct("ExtractIf")
            .field("peek", &peek)
            .finish_non_exhaustive()
    }
}

// The entry being tested is held by value, but never pinned: like the map,
// the walk is `Unpin` whatever `K` and `V` are, and as std's, if its range
// and predicate are.
impl<K, V, R: Unpin, F: Unpin, S: InlineKeyStrategy<K>> Unpin for ExtractIf<'_, K, V, R, F, S> {}

impl<K, V, R, F, S: InlineKeyStrategy<K>> Drop for ExtractIf<'_, K, V, R, F, S> {
    /// Gives the map back the entries not handed out, in order.
    fn drop(&mut self) {
        if let Some((key, value)) = self.testing.take() {
            self.kept.push(key, value);
        }
        let rest = &mut self.rest;
        self.kept.extend(iter::from_fn(|| rest.take_first()));
        let kept = mem::replace(&mut self.kept, TreeBuilder::new());
        *self.map = BBTreeMap::from_builder(kept);
        if self.had_tree {
            self.map.keep_a_tree();
        }
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
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
    /// Unlike std's, this walk takes every entry out of the map and puts the
    /// kept ones back, wherever the range is: O(n).
    ///
    /// [`retain`]: BBTreeMap::retain
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
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
    pub fn extract_if<F, R>(&mut self, range: R, pred: F) -> ExtractIf<'_, K, V, R, F, S>
    where
        R: RangeBounds<K>,
        F: FnMut(&K, &mut V) -> bool,
    {
        let had_tree = self.root.is_some();
        let rest = mem::replace(self, Self::empty());
        let mut walk = ExtractIf {
            map: self,
            had_tree,
            rest,
            testing: None,
            kept: TreeBuilder::new(),
            finished: false,
            range,
            pred,
        };
        while let Some((key, _)) = walk.rest.iter().next()
            && walk.is_before_range(key)
        {
            let (key, value) = walk.rest.take_first().expect("the entry just seen");
            walk.kept.push(key, value);
        }
        walk
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// In other words, remove all pairs `(k, v)` for which `f(&k, &mut v)`
    /// returns `false`. The elements are visited in ascending key order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
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
        self.extract_if::<_, RangeFull>(.., |key, value| !f(key, value))
            .for_each(drop);
    }
}
