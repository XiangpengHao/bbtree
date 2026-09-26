//! Iterating over a map by value: [`IntoIter`], [`IntoKeys`] and
//! [`IntoValues`], which take the entries out in key order.
//!
//! For now each step removes the first or last entry, as `pop_first` and
//! `pop_last` do, rebalancing as it goes, and dropping the iterator drops the
//! entries left in ascending order, as dropping the map does. A walk that
//! takes entries out and frees each node once past it, with no rebalancing,
//! is to replace the removals.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::FusedIterator;

use super::BBTreeMap;
use crate::inline_key::{Auto, InlineKeyStrategy};

/// An owning iterator over the entries of a `BBTreeMap`, sorted by key.
///
/// This `struct` is created by the [`into_iter`] method on [`BBTreeMap`]
/// (provided by the [`IntoIterator`] trait). See its documentation for more.
///
/// [`into_iter`]: IntoIterator::into_iter
pub struct IntoIter<K, V, S: InlineKeyStrategy<K> = Auto> {
    /// The entries left.
    map: BBTreeMap<K, V, S>,
}

impl<K, V, S: InlineKeyStrategy<K>> Iterator for IntoIter<K, V, S> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.map.take_first()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.map.len(), Some(self.map.len()))
    }
}

impl<K, V, S: InlineKeyStrategy<K>> DoubleEndedIterator for IntoIter<K, V, S> {
    fn next_back(&mut self) -> Option<(K, V)> {
        self.map.take_last()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> ExactSizeIterator for IntoIter<K, V, S> {
    fn len(&self) -> usize {
        self.map.len()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for IntoIter<K, V, S> {}

/// Prints the entries left.
impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for IntoIter<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.map.iter()).finish()
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
            map: BBTreeMap {
                root: None,
                length: 0,
            },
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
        f.debug_list().entries(self.inner.map.keys()).finish()
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
        f.debug_list().entries(self.inner.map.values()).finish()
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
    fn into_iter(self) -> IntoIter<K, V, S> {
        IntoIter { map: self }
    }
}
