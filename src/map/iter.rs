//! Iterating over a map by shared reference: [`Iter`], [`Keys`], [`Values`]
//! and [`Range`], each a walk between two cursors (`cursor.rs`, D23). The
//! whole-map iterators know how many entries are left, and stop by that
//! count; a range stops where its ends meet. Only building a range calls
//! `Ord`, to find its ends.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::fmt;
use core::iter::FusedIterator;
use core::ops::{Bound, RangeBounds};

use super::BBTreeMap;
use super::cursor::{KeySide, LeafCursor, Root};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node::inline_keys::SearchKey;

/// The entries between two positions of a tree, taken from either end. An
/// end that has not started starts at the first or last entry of the tree
/// the first time it moves, so a walk over the whole tree costs nothing until
/// then, and one end never moves the other.
struct Walk<'a, K, V, S> {
    /// `None` for a map without a tree.
    root: Option<Root<'a, K, V, S>>,
    front: LeafCursor<'a, K, V, S>,
    back: LeafCursor<'a, K, V, S>,
}

impl<K, V, S> Clone for Walk<'_, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for Walk<'_, K, V, S> {}

impl<'a, K, V, S: InlineKeyStrategy<K>> Walk<'a, K, V, S> {
    const EMPTY: Self = Self {
        root: None,
        front: LeafCursor::UNSTARTED,
        back: LeafCursor::UNSTARTED,
    };

    /// Every entry of the tree under `root`.
    fn whole(root: Root<'a, K, V, S>) -> Self {
        Self {
            root: Some(root),
            front: LeafCursor::UNSTARTED,
            back: LeafCursor::UNSTARTED,
        }
    }

    /// The entries of the tree under `root` within the bounds, with std's
    /// panics for bounds out of order (D1).
    ///
    /// # Panics
    ///
    /// If `start > end`, if `start == end` and both are excluded, or if `Ord`
    /// is so inconsistent that the start is found after the end.
    fn between<Q>(root: Root<'a, K, V, S>, start: Bound<&Q>, end: Bound<&Q>) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match (start, end) {
            (Bound::Excluded(start), Bound::Excluded(end)) if start == end => {
                panic!("range start and end are equal and excluded in BTreeMap")
            }
            (
                Bound::Included(start) | Bound::Excluded(start),
                Bound::Included(end) | Bound::Excluded(end),
            ) if start > end => panic!("range start is greater than range end in BTreeMap"),
            _ => {}
        }
        let mut walk = Self::whole(root);
        match start {
            Bound::Included(key) => {
                walk.front
                    .move_beside_key(root, &SearchKey::new(key), KeySide::Before);
            }
            Bound::Excluded(key) => {
                walk.front
                    .move_beside_key(root, &SearchKey::new(key), KeySide::After);
            }
            Bound::Unbounded => walk.front.move_to_first(root),
        }
        match end {
            Bound::Included(key) => {
                walk.back
                    .move_beside_key(root, &SearchKey::new(key), KeySide::After);
            }
            Bound::Excluded(key) => {
                walk.back
                    .move_beside_key(root, &SearchKey::new(key), KeySide::Before);
            }
            Bound::Unbounded => walk.back.move_to_last(root),
        }
        if walk.front.cmp_position(&walk.back) == Ordering::Greater {
            panic!("Ord is ill-defined in BTreeMap range");
        }
        walk
    }

    /// The entry after the front end, which moves past it; `None` at the end
    /// of the tree. Does not check for the back end.
    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        self.front.next(self.root?)
    }

    /// The entry before the back end, which moves back past it; `None` at the
    /// start of the tree. Does not check for the front end.
    fn next_back(&mut self) -> Option<(&'a K, &'a V)> {
        self.back.next_back(self.root?)
    }

    /// Whether no entry is left between the ends, for a walk whose ends have
    /// both started, or that has no tree.
    fn ends_meet(&self) -> bool {
        self.root.is_none() || self.front.is_at(&self.back)
    }
}

/// An iterator over the entries of a `BBTreeMap`, in key order.
///
/// This `struct` is created by the [`iter`] method on [`BBTreeMap`]. See its
/// documentation for more.
///
/// [`iter`]: BBTreeMap::iter
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Iter<'a, K: 'a, V: 'a, S = Auto> {
    walk: Walk<'a, K, V, S>,
    /// The entries left between the ends.
    length: usize,
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> Iterator for Iter<'a, K, V, S> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        self.walk.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.length, Some(self.length))
    }

    fn last(mut self) -> Option<(&'a K, &'a V)> {
        self.next_back()
    }

    fn min(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next_back()
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> DoubleEndedIterator for Iter<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(&'a K, &'a V)> {
        if self.length == 0 {
            return None;
        }
        self.length -= 1;
        self.walk.next_back()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for Iter<'_, K, V, S> {
    fn len(&self) -> usize {
        self.length
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Iter<'_, K, V, S> {}

impl<K, V, S> Clone for Iter<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            walk: self.walk,
            length: self.length,
        }
    }
}

impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for Iter<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Iter<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Iter`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Iter<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            walk: Walk::EMPTY,
            length: 0,
        }
    }
}

/// An iterator over the keys of a `BBTreeMap`, in order.
///
/// This `struct` is created by the [`keys`] method on [`BBTreeMap`]. See its
/// documentation for more.
///
/// [`keys`]: BBTreeMap::keys
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Keys<'a, K, V, S = Auto> {
    inner: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for Keys<'a, K, V, S> {
    type Item = &'a K;

    fn next(&mut self) -> Option<&'a K> {
        self.inner.next().map(|(key, _)| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }

    fn last(mut self) -> Option<&'a K> {
        self.next_back()
    }

    fn min(mut self) -> Option<&'a K>
    where
        &'a K: Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<&'a K>
    where
        &'a K: Ord,
    {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for Keys<'a, K, V, S> {
    fn next_back(&mut self) -> Option<&'a K> {
        self.inner.next_back().map(|(key, _)| key)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for Keys<'_, K, V, S> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Keys<'_, K, V, S> {}

impl<K, V, S> Clone for Keys<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K: fmt::Debug, V, S: InlineKeyStrategy<K>> fmt::Debug for Keys<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Keys<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Keys`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Keys<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: Iter::default(),
        }
    }
}

/// An iterator over the values of a `BBTreeMap`, in key order.
///
/// This `struct` is created by the [`values`] method on [`BBTreeMap`]. See
/// its documentation for more.
///
/// [`values`]: BBTreeMap::values
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Values<'a, K, V, S = Auto> {
    inner: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for Values<'a, K, V, S> {
    type Item = &'a V;

    fn next(&mut self) -> Option<&'a V> {
        self.inner.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }

    fn last(mut self) -> Option<&'a V> {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for Values<'a, K, V, S> {
    fn next_back(&mut self) -> Option<&'a V> {
        self.inner.next_back().map(|(_, value)| value)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for Values<'_, K, V, S> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Values<'_, K, V, S> {}

impl<K, V, S> Clone for Values<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for Values<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Values<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Values`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Values<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: Iter::default(),
        }
    }
}

/// An iterator over a sub-range of the entries of a `BBTreeMap`, in key
/// order.
///
/// This `struct` is created by the [`range`] method on [`BBTreeMap`]. See its
/// documentation for more.
///
/// [`range`]: BBTreeMap::range
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct Range<'a, K: 'a, V: 'a, S = Auto> {
    walk: Walk<'a, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Iterator for Range<'a, K, V, S> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        if self.walk.ends_meet() {
            return None;
        }
        self.walk.next()
    }

    fn last(mut self) -> Option<(&'a K, &'a V)> {
        self.next_back()
    }

    fn min(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next_back()
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for Range<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(&'a K, &'a V)> {
        if self.walk.ends_meet() {
            return None;
        }
        self.walk.next_back()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for Range<'_, K, V, S> {}

impl<K, V, S> Clone for Range<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self { walk: self.walk }
    }
}

impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for Range<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for Range<'_, K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::Range`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::Range<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.count(), 0);
    /// ```
    fn default() -> Self {
        Self { walk: Walk::EMPTY }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Gets an iterator over the entries of the map, sorted by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(3, "c");
    /// map.insert(2, "b");
    /// map.insert(1, "a");
    ///
    /// for (key, value) in map.iter() {
    ///     println!("{key}: {value}");
    /// }
    ///
    /// let (first_key, first_value) = map.iter().next().unwrap();
    /// assert_eq!((*first_key, *first_value), (1, "a"));
    /// ```
    pub fn iter(&self) -> Iter<'_, K, V, S> {
        let walk = self
            .root
            .as_ref()
            .map_or(Walk::EMPTY, |root| Walk::whole(root.borrow()));
        Iter {
            walk,
            length: self.length,
        }
    }

    /// Gets an iterator over the keys of the map, in sorted order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(2, "b");
    /// a.insert(1, "a");
    ///
    /// let keys: Vec<_> = a.keys().cloned().collect();
    /// assert_eq!(keys, [1, 2]);
    /// ```
    pub fn keys(&self) -> Keys<'_, K, V, S> {
        Keys { inner: self.iter() }
    }

    /// Gets an iterator over the values of the map, in order by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "hello");
    /// a.insert(2, "goodbye");
    ///
    /// let values: Vec<&str> = a.values().cloned().collect();
    /// assert_eq!(values, ["hello", "goodbye"]);
    /// ```
    pub fn values(&self) -> Values<'_, K, V, S> {
        Values { inner: self.iter() }
    }

    /// Constructs a double-ended iterator over a sub-range of entries in the
    /// map. The simplest way is to use the range syntax `min..max`, thus
    /// `range(min..max)` will yield entries from min (inclusive) to max
    /// (exclusive). The range may also be entered as
    /// `(Bound<T>, Bound<T>)`, so for example
    /// `range((Excluded(4), Included(10)))` will yield a left-exclusive,
    /// right-inclusive range from 4 to 10.
    ///
    /// # Panics
    ///
    /// Panics if range `start > end`. Panics if range `start == end` and both
    /// bounds are `Excluded`. The messages are std's, word for word.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use std::ops::Bound::Included;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(3, "a");
    /// map.insert(5, "b");
    /// map.insert(8, "c");
    /// for (&key, &value) in map.range((Included(&4), Included(&8))) {
    ///     println!("{key}: {value}");
    /// }
    /// assert_eq!(Some((&5, &"b")), map.range(4..).next());
    /// ```
    pub fn range<T, R>(&self, range: R) -> Range<'_, K, V, S>
    where
        T: Ord + ?Sized,
        K: Borrow<T> + Ord,
        R: RangeBounds<T>,
    {
        let Some(root) = &self.root else {
            return Range::default();
        };
        Range {
            walk: Walk::between(root.borrow(), range.start_bound(), range.end_bound()),
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> IntoIterator for &'a BBTreeMap<K, V, S> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V, S>;

    fn into_iter(self) -> Iter<'a, K, V, S> {
        self.iter()
    }
}
