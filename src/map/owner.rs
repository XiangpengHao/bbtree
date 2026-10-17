//! What the map's owner does with it, through `&mut` or by value: no other
//! thread reaches the map then, so these hand out references into it, as
//! std's map does, and take no latch (`tree/latched/owner.rs`).

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::fmt;
use core::iter::FusedIterator;

use super::BBTreeMap;
use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::node::latch::Latched;
use crate::tree::node_stack::ValMut;
use crate::tree::walk::{DyingWalk, RangeWalk};

/// An iterator over the entries of a [`BBTreeMap`], in key order, with the
/// values exclusive, from [`BBTreeMap::iter_mut`].
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct IterMut<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K> = Auto> {
    walk: RangeWalk<ValMut<'a, K, V>, K, V, Latched<S>>,
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> Iterator for IterMut<'a, K, V, S> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<(&'a K, &'a mut V)> {
        self.walk.next()
    }

    fn fold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.fold(accumulator, fold)
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> DoubleEndedIterator for IterMut<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(&'a K, &'a mut V)> {
        self.walk.next_back()
    }

    fn rfold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.walk.rfold(accumulator, fold)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for IterMut<'_, K, V, S> {}

impl<K, V, S: InlineKeyStrategy<K>> fmt::Debug for IterMut<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IterMut").finish_non_exhaustive()
    }
}

/// An iterator that moves the entries out of a [`BBTreeMap`], in key order,
/// from its [`IntoIterator`] impl. It gives each node's page back to the
/// map's allocator once past it, and drops the entries left when dropped.
pub struct IntoIter<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator = Global> {
    walk: DyingWalk<K, V, Latched<S>, A>,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Iterator for IntoIter<K, V, S, A> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.walk.next()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> FusedIterator for IntoIter<K, V, S, A> {}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> fmt::Debug for IntoIter<K, V, S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntoIter").finish_non_exhaustive()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> BBTreeMap<K, V, S, A> {
    /// The value of `key`'s entry, to change in place; `None` without one.
    /// No latch: the map's owner holds it alone.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let mut map = BBTreeMap::<u64, u64>::new();
    /// map.insert(1, 10);
    /// *map.get_mut(&1).unwrap() += 1;
    /// assert_eq!(map.get_copied(&1), Some(11));
    /// ```
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.find_mut(key)
    }

    /// An iterator over the entries, in key order and in reverse, with the
    /// values to change in place. No latch: the map's owner holds it alone.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let mut map = BBTreeMap::<u64, u64>::from([(1, 10), (2, 20)]);
    /// for (_, value) in map.iter_mut() {
    ///     *value += 1;
    /// }
    /// assert_eq!(map.get_copied(&2), Some(21));
    /// ```
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V, S>
    where
        K: Ord,
    {
        IterMut {
            walk: self.tree.walk_mut(),
        }
    }

    /// Removes every entry, giving every page back to the allocator.
    pub fn clear(&mut self) {
        self.tree.destroy(&self.alloc);
    }
}

impl<'a, K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator> IntoIterator
    for &'a mut BBTreeMap<K, V, S, A>
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, K, V, S>;

    fn into_iter(self) -> IterMut<'a, K, V, S> {
        self.iter_mut()
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> IntoIterator for BBTreeMap<K, V, S, A> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V, S, A>;

    /// Moves the entries out in key order.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::<u64, &str>::from([(2, "b"), (1, "a")]);
    /// assert_eq!(map.into_iter().collect::<Vec<_>>(), [(1, "a"), (2, "b")]);
    /// ```
    fn into_iter(mut self) -> IntoIter<K, V, S, A> {
        IntoIter {
            walk: self.tree.take_dying_walk(self.alloc.clone()),
        }
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator> Extend<(K, V)> for BBTreeMap<K, V, S, A> {
    /// Inserts each entry, as [`BBTreeMap::insert`] does.
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, entries: T) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for BBTreeMap<K, V> {
    /// A map of the entries, a later entry's value replacing an earlier
    /// one's of an equal key, as std's map does.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under [`Auto`].
    fn from_iter<T: IntoIterator<Item = (K, V)>>(entries: T) -> Self {
        let mut map = Self::new();
        map.extend(entries);
        map
    }
}

impl<K: Ord, V, const N: usize> From<[(K, V); N]> for BBTreeMap<K, V> {
    /// A map of the entries, as [`FromIterator`] makes it.
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// let map = BBTreeMap::from([(1_u64, "a"), (2, "b")]);
    /// assert_eq!(map.get_copied(&2), Some("b"));
    /// ```
    fn from(entries: [(K, V); N]) -> Self {
        entries.into_iter().collect()
    }
}
