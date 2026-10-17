//! Iterating over a map by value: [`IntoIter`], [`IntoKeys`] and
//! [`IntoValues`], which take the entries out in key order.
//!
//! The iterator takes the tree apart as it goes (`tree/walk.rs`, D23): each
//! step moves an entry out of its slot, with nothing shifting and nothing
//! rebalanced, and each node is freed once both ends are done with it.
//! Dropping the iterator drops the entries left in ascending order, as
//! dropping the map does, which is the same walk.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::{self, FusedIterator};
use core::mem;
use core::panic::{RefUnwindSafe, UnwindSafe};

use super::BBTreeMap;
use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::walk::DyingWalk;

/// An owning iterator over the entries of a `BBTreeMap`, sorted by key.
///
/// This `struct` is created by the [`into_iter`] method on [`BBTreeMap`]
/// (provided by the [`IntoIterator`] trait). See its documentation for more.
///
/// [`into_iter`]: IntoIterator::into_iter
pub struct IntoIter<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator + Clone = Global> {
    /// The walk taking the map's tree apart, its pages going back to the
    /// map's allocator.
    walk: DyingWalk<K, V, S, A>,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> IntoIter<K, V, S, A> {
    /// The entries left, by reference, in ascending order.
    fn remaining(&self) -> impl Iterator<Item = (&K, &V)> {
        let mut walk = self.walk.remaining();
        iter::from_fn(move || walk.next())
    }

    /// The next entry, by reference, without moving past it.
    pub(super) fn peek(&self) -> Option<(&K, &V)> {
        self.remaining().next()
    }
}

// Entries are moved out of the tree's pages, never pinned: like the map, the
// iterator is `Unpin` whatever `K` and `V` are, if its allocator is, as
// std's is.
impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone + Unpin> Unpin for IntoIter<K, V, S, A> {}

// std's iterator is `UnwindSafe` whenever its keys and values are
// `RefUnwindSafe` and its allocator `UnwindSafe`, as the map is; match it
// (D1).
impl<K, V, S, A> UnwindSafe for IntoIter<K, V, S, A>
where
    K: RefUnwindSafe,
    V: RefUnwindSafe,
    S: InlineKeyStrategy<K>,
    A: Allocator + Clone + UnwindSafe,
{
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> Iterator for IntoIter<K, V, S, A> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.walk.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.walk.len(), Some(self.walk.len()))
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> DoubleEndedIterator
    for IntoIter<K, V, S, A>
{
    fn next_back(&mut self) -> Option<(K, V)> {
        self.walk.next_back()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> ExactSizeIterator
    for IntoIter<K, V, S, A>
{
    fn len(&self) -> usize {
        self.walk.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> FusedIterator for IntoIter<K, V, S, A> {}

/// Prints the entries left.
impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>, A: Allocator + Clone> fmt::Debug
    for IntoIter<K, V, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.remaining()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone + Default> Default
    for IntoIter<K, V, S, A>
{
    /// Creates an empty `bbtree::unsync::bbtree_map::IntoIter`.
    ///
    /// ```
    /// # use bbtree::unsync::bbtree_map;
    /// let iter: bbtree_map::IntoIter<u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            walk: DyingWalk::new(None, 0, A::default()),
        }
    }
}

/// An owning iterator over the keys of a `BBTreeMap`, in order.
///
/// This `struct` is created by the [`into_keys`] method on [`BBTreeMap`]. See
/// its documentation for more.
///
/// [`into_keys`]: BBTreeMap::into_keys
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct IntoKeys<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator + Clone = Global> {
    inner: IntoIter<K, V, S, A>,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> Iterator for IntoKeys<K, V, S, A> {
    type Item = K;

    fn next(&mut self) -> Option<K> {
        self.inner.next().map(|(key, _)| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }

    fn last(mut self) -> Option<K> {
        self.next_back()
    }

    fn min(mut self) -> Option<K>
    where
        K: Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<K>
    where
        K: Ord,
    {
        self.next_back()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> DoubleEndedIterator
    for IntoKeys<K, V, S, A>
{
    fn next_back(&mut self) -> Option<K> {
        self.inner.next_back().map(|(key, _)| key)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> ExactSizeIterator
    for IntoKeys<K, V, S, A>
{
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> FusedIterator for IntoKeys<K, V, S, A> {}

/// Prints the keys left.
impl<K: fmt::Debug, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> fmt::Debug
    for IntoKeys<K, V, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.inner.remaining().map(|(key, _)| key))
            .finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone + Default> Default
    for IntoKeys<K, V, S, A>
{
    /// Creates an empty `bbtree::unsync::bbtree_map::IntoKeys`.
    ///
    /// ```
    /// # use bbtree::unsync::bbtree_map;
    /// let iter: bbtree_map::IntoKeys<u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: IntoIter::default(),
        }
    }
}

/// An owning iterator over the values of a `BBTreeMap`, in order by key.
///
/// This `struct` is created by the [`into_values`] method on [`BBTreeMap`].
/// See its documentation for more.
///
/// [`into_values`]: BBTreeMap::into_values
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct IntoValues<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator + Clone = Global> {
    inner: IntoIter<K, V, S, A>,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> Iterator for IntoValues<K, V, S, A> {
    type Item = V;

    fn next(&mut self) -> Option<V> {
        self.inner.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }

    fn last(mut self) -> Option<V> {
        self.next_back()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> DoubleEndedIterator
    for IntoValues<K, V, S, A>
{
    fn next_back(&mut self) -> Option<V> {
        self.inner.next_back().map(|(_, value)| value)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> ExactSizeIterator
    for IntoValues<K, V, S, A>
{
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> FusedIterator for IntoValues<K, V, S, A> {}

/// Prints the values left.
impl<K, V: fmt::Debug, S: InlineKeyStrategy<K>, A: Allocator + Clone> fmt::Debug
    for IntoValues<K, V, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.inner.remaining().map(|(_, value)| value))
            .finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone + Default> Default
    for IntoValues<K, V, S, A>
{
    /// Creates an empty `bbtree::unsync::bbtree_map::IntoValues`.
    ///
    /// ```
    /// # use bbtree::unsync::bbtree_map;
    /// let iter: bbtree_map::IntoValues<u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: IntoIter::default(),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Creates a consuming iterator visiting all the keys, in sorted order.
    /// The map cannot be used after calling this. The iterator element type
    /// is `K`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(2, "b");
    /// a.insert(1, "a");
    ///
    /// let keys: Vec<i32> = a.into_keys().collect();
    /// assert_eq!(keys, [1, 2]);
    /// ```
    pub fn into_keys(self) -> IntoKeys<K, V, S, A> {
        IntoKeys {
            inner: self.into_iter(),
        }
    }

    /// Creates a consuming iterator visiting all the values, in order by key.
    /// The map cannot be used after calling this. The iterator element type
    /// is `V`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "hello");
    /// a.insert(2, "goodbye");
    ///
    /// let values: Vec<&str> = a.into_values().collect();
    /// assert_eq!(values, ["hello", "goodbye"]);
    /// ```
    pub fn into_values(self) -> IntoValues<K, V, S, A> {
        IntoValues {
            inner: self.into_iter(),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> IntoIterator for BBTreeMap<K, V, S, A> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V, S, A>;

    /// Gets an owning iterator over the entries of the map, sorted by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(3, "c");
    /// map.insert(2, "b");
    /// map.insert(1, "a");
    ///
    /// for (key, value) in map.into_iter() {
    ///     println!("{key}: {value}");
    /// }
    /// ```
    fn into_iter(mut self) -> IntoIter<K, V, S, A> {
        let tree = self.tree.take();
        let length = mem::take(&mut self.length);
        IntoIter {
            walk: DyingWalk::new(tree.root_page(), length, self.alloc.clone()),
        }
    }
}
