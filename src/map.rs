//! [`BBTreeMap`]: an ordered map with the API and behavior of std's
//! `BTreeMap` (D1), on 1 KB nodes without parent pointers (D11, D15), as a
//! B+-tree that stores each key once (D17).
//!
//! The map owns at most one [`Subtree`]: none until the first insert, so
//! that [`BBTreeMap::new`] allocates nothing. Inserting is a recursive descent
//! whose call stack is the path back up for splits (`insert.rs`); looking up
//! is a loop down from the root (`lookup.rs`).

#![forbid(unsafe_code)]

#[cfg(test)]
mod differential_tests;
#[cfg(test)]
mod drop_tests;
mod insert;
#[cfg(test)]
mod invariants;
mod lookup;
#[cfg(test)]
mod type_tests;

use core::borrow::Borrow;
use core::panic::{RefUnwindSafe, UnwindSafe};

use crate::node::handle::Subtree;
use crate::node::page::{FreshPage, PageByKind};

/// An ordered map based on a B-tree with page-sized nodes: a drop-in
/// replacement for [`std::collections::BTreeMap`].
///
/// As with std's map, it is a logic error for a key to change its order
/// relative to other keys while it is in the map; the behavior that results
/// is unspecified but memory-safe.
///
/// [`std::collections::BTreeMap`]: https://doc.rust-lang.org/std/collections/struct.BTreeMap.html
///
/// # Examples
///
/// ```
/// use bbtree::BBTreeMap;
///
/// let mut map = BBTreeMap::new();
/// assert_eq!(map.insert(37, "a"), None);
/// assert_eq!(map.insert(37, "b"), Some("a"));
/// assert_eq!(map.get(&37), Some(&"b"));
/// assert!(!map.contains_key(&38));
/// assert_eq!(map.len(), 1);
/// ```
pub struct BBTreeMap<K, V> {
    root: Option<Subtree<K, V>>,
    length: usize,
}

impl<K, V> BBTreeMap<K, V> {
    /// Makes a new, empty `BBTreeMap`.
    ///
    /// Does not allocate anything on its own.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            root: None,
            length: 0,
        }
    }

    /// Returns the number of elements in the map.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length
    }

    /// Returns `true` if the map contains no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but the
    /// ordering on the borrowed form *must* match the ordering on the key
    /// type.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.get_key_value(key).map(|(_, value)| value)
    }

    /// Returns the key-value pair corresponding to the supplied key: the key
    /// as stored in the map, which may differ from `key` while comparing
    /// equal to it.
    ///
    /// The supplied key may be any borrowed form of the map's key type, but
    /// the ordering on the borrowed form *must* match the ordering on the key
    /// type.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.get_key_value(&1), Some((&1, &"a")));
    /// assert_eq!(map.get_key_value(&2), None);
    /// ```
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        let root = self.root.as_ref()?.borrow();
        let kv = root.search_tree(key)?;
        Some(kv.into_key_value())
    }

    /// Returns `true` if the map contains a value for the specified key.
    ///
    /// The key may be any borrowed form of the map's key type, but the
    /// ordering on the borrowed form *must* match the ordering on the key
    /// type.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.get(key).is_some()
    }

    /// Returns a mutable reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but the
    /// ordering on the borrowed form *must* match the ordering on the key
    /// type.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        let root = self.root.as_mut()?.borrow_mut();
        let kv = root.search_tree(key)?;
        Some(kv.into_value_mut())
    }

    /// Inserts a key-value pair into the map.
    ///
    /// If the map did not have this key present, `None` is returned.
    ///
    /// If the map did have this key present, the value is updated, and the
    /// old value is returned. The key is not updated, though: the map keeps
    /// its key and drops the given one, before it replaces the value.
    ///
    /// If `K`'s `Ord` panics, the map is unchanged: every comparison happens
    /// before the first change.
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Ord,
    {
        let root = self
            .root
            .get_or_insert_with(|| Subtree::new_leaf(FreshPage::allocate()).forget_type());
        let old_value = root.insert(key, value, FreshPage::allocate, FreshPage::allocate);
        if old_value.is_none() {
            self.length += 1;
        }
        old_value
    }
}

impl<K, V> Default for BBTreeMap<K, V> {
    /// Creates an empty `BBTreeMap`.
    fn default() -> Self {
        Self::new()
    }
}

/// Drops every key and value in ascending key order, each key before its
/// value, as std's map does.
///
/// Unlike std's map, whose `Drop` marks `K` and `V` `#[may_dangle]` (nightly
/// only), this one requires the keys and values to strictly outlive the map.
impl<K, V> Drop for BBTreeMap<K, V> {
    fn drop(&mut self) {
        if let Some(root) = self.root.take() {
            root.destroy(PageByKind::deallocate);
        }
    }
}

// Keys and values live in the node pages, so moving the map never moves
// them: like std's map, it is `Unpin` whatever `K` and `V` are.
impl<K, V> Unpin for BBTreeMap<K, V> {}

// std's map is `UnwindSafe` whenever its keys and values are `RefUnwindSafe`,
// an impl std keeps for compatibility; match it (D1).
impl<K: RefUnwindSafe, V: RefUnwindSafe> UnwindSafe for BBTreeMap<K, V> {}
