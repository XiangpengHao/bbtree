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
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::node::page::PageByKind;
use crate::tree::root::Root;
use crate::tree::walk::DyingWalk;

/// An owning iterator over the entries of a `BBTreeMap`, sorted by key.
///
/// This `struct` is created by the [`into_iter`] method on [`BBTreeMap`]
/// (provided by the [`IntoIterator`] trait). See its documentation for more.
///
/// [`into_iter`]: IntoIterator::into_iter
pub struct IntoIter<K, V, S: InlineKeyStrategy<K> = Auto> {
    /// The walk taking the map's tree apart.
    walk: DyingWalk<K, V, S, Deallocate<K, V, S>>,
}

/// Where an owning walk's freed pages go: back to the global allocator.
pub(super) type Deallocate<K, V, S> = fn(PageByKind<K, V, S>);

impl<K, V, S: InlineKeyStrategy<K>> IntoIter<K, V, S> {
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
// iterator is `Unpin` whatever `K` and `V` are, as std's is.
impl<K, V, S: InlineKeyStrategy<K>> Unpin for IntoIter<K, V, S> {}

// std's iterator is `UnwindSafe` whenever its keys and values are
// `RefUnwindSafe`, as the map is; match it (D1).
impl<K: RefUnwindSafe, V: RefUnwindSafe, S: InlineKeyStrategy<K>> UnwindSafe for IntoIter<K, V, S> {}

impl<K, V, S: InlineKeyStrategy<K>> Iterator for IntoIter<K, V, S> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.walk.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.walk.len(), Some(self.walk.len()))
    }
}

impl<K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for IntoIter<K, V, S> {
    fn next_back(&mut self) -> Option<(K, V)> {
        self.walk.next_back()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for IntoIter<K, V, S> {
    fn len(&self) -> usize {
        self.walk.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for IntoIter<K, V, S> {}

/// Prints the entries left.
impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for IntoIter<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.remaining()).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for IntoIter<K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::IntoIter`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::IntoIter<u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            walk: DyingWalk::new(None, 0, PageByKind::deallocate),
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
pub struct IntoKeys<K, V, S: InlineKeyStrategy<K> = Auto> {
    inner: IntoIter<K, V, S>,
}

impl<K, V, S: InlineKeyStrategy<K>> Iterator for IntoKeys<K, V, S> {
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

impl<K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for IntoKeys<K, V, S> {
    fn next_back(&mut self) -> Option<K> {
        self.inner.next_back().map(|(key, _)| key)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for IntoKeys<K, V, S> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for IntoKeys<K, V, S> {}

/// Prints the keys left.
impl<K: fmt::Debug, V, S: InlineKeyStrategy<K>> fmt::Debug for IntoKeys<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.inner.remaining().map(|(key, _)| key))
            .finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for IntoKeys<K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::IntoKeys`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
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
pub struct IntoValues<K, V, S: InlineKeyStrategy<K> = Auto> {
    inner: IntoIter<K, V, S>,
}

impl<K, V, S: InlineKeyStrategy<K>> Iterator for IntoValues<K, V, S> {
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

impl<K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for IntoValues<K, V, S> {
    fn next_back(&mut self) -> Option<V> {
        self.inner.next_back().map(|(_, value)| value)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for IntoValues<K, V, S> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for IntoValues<K, V, S> {}

/// Prints the values left.
impl<K, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for IntoValues<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.inner.remaining().map(|(_, value)| value))
            .finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Default for IntoValues<K, V, S> {
    /// Creates an empty `bbtree::bbtree_map::IntoValues`.
    ///
    /// ```
    /// # use bbtree::bbtree_map;
    /// let iter: bbtree_map::IntoValues<u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Self {
            inner: IntoIter::default(),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Creates a consuming iterator visiting all the keys, in sorted order.
    /// The map cannot be used after calling this. The iterator element type
    /// is `K`.
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
    /// let keys: Vec<i32> = a.into_keys().collect();
    /// assert_eq!(keys, [1, 2]);
    /// ```
    pub fn into_keys(self) -> IntoKeys<K, V, S> {
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
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "hello");
    /// a.insert(2, "goodbye");
    ///
    /// let values: Vec<&str> = a.into_values().collect();
    /// assert_eq!(values, ["hello", "goodbye"]);
    /// ```
    pub fn into_values(self) -> IntoValues<K, V, S> {
        IntoValues {
            inner: self.into_iter(),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> IntoIterator for BBTreeMap<K, V, S> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V, S>;

    /// Gets an owning iterator over the entries of the map, sorted by key.
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
    /// for (key, value) in map.into_iter() {
    ///     println!("{key}: {value}");
    /// }
    /// ```
    fn into_iter(mut self) -> IntoIter<K, V, S> {
        let tree = self.root.take();
        let length = mem::take(&mut self.length);
        IntoIter {
            walk: DyingWalk::new(tree.map(Root::into_top), length, PageByKind::deallocate),
        }
    }
}
