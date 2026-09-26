//! [`BBTreeMap`]: an ordered map with the API and behavior of std's
//! `BTreeMap` (D1), on 1 KB nodes without parent pointers (D11, D15), as a
//! B+-tree that stores each key once (D17).
//!
//! The map owns at most one [`Subtree`]: none until the first insert, so
//! that [`BBTreeMap::new`] allocates nothing. Inserting is a recursive descent
//! whose call stack is the path back up for splits (`insert.rs`), and so is
//! removing, for merges (`remove.rs`); looking up is a loop down from the
//! root (`lookup.rs`).
//!
//! The strategy parameter `S` decides which keys have inline keys (D18,
//! `inline_key.rs`); every method is the same for every `S`.

#![forbid(unsafe_code)]

#[cfg(test)]
mod differential_tests;
#[cfg(test)]
mod drop_tests;
#[cfg(test)]
mod inline_key_tests;
mod insert;
#[cfg(test)]
mod invariants;
mod lookup;
mod remove;
#[cfg(test)]
mod type_tests;

use core::borrow::Borrow;
use core::mem;
use core::panic::{RefUnwindSafe, UnwindSafe};

use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node::handle::Subtree;
use crate::node::page::{FreshPage, PageByKind};

/// An ordered map based on a B-tree with page-sized nodes: a drop-in
/// replacement for [`std::collections::BTreeMap`].
///
/// As with std's map, it is a logic error for a key to change its order
/// relative to other keys while it is in the map; the behavior that results
/// is unspecified but memory-safe.
///
/// Its nodes store an inline key beside each key, a register-sized summary
/// that searches compare first, when the strategy `S` gives the key type
/// one: by default ([`Auto`]), std byte-string keys such as `String` have
/// them, and other keys don't. [`WithInlineKey`] gives them to any key type
/// that implements [`InlineKey`]. Either way, every method behaves as std's.
///
/// [`std::collections::BTreeMap`]: https://doc.rust-lang.org/std/collections/struct.BTreeMap.html
/// [`WithInlineKey`]: crate::WithInlineKey
/// [`InlineKey`]: crate::InlineKey
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
pub struct BBTreeMap<K, V, S: InlineKeyStrategy<K> = Auto> {
    root: Option<Subtree<K, V, S>>,
    length: usize,
}

impl<K, V> BBTreeMap<K, V> {
    /// Makes a new, empty `BBTreeMap`, with the default strategy, [`Auto`].
    /// For another, see [`BBTreeMap::with_strategy`].
    ///
    /// Does not allocate anything on its own.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_strategy(Auto)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Makes a new, empty `BBTreeMap` with inline-key strategy `strategy`.
    /// `BBTreeMap::new()` is `BBTreeMap::with_strategy(Auto)`.
    ///
    /// Does not allocate anything on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::{BBTreeMap, InlineKey, InlineWord, WithInlineKey};
    ///
    /// #[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
    /// struct Id([u8; 8]);
    ///
    /// impl InlineKey for Id {
    ///     fn inline_key(&self) -> InlineWord {
    ///         self.0.inline_key()
    ///     }
    /// }
    ///
    /// let mut map: BBTreeMap<Id, &str, WithInlineKey> = BBTreeMap::with_strategy(WithInlineKey);
    /// map.insert(Id(*b"ferris!!"), "crab");
    /// assert_eq!(map.get(&Id(*b"ferris!!")), Some(&"crab"));
    /// ```
    #[must_use]
    pub const fn with_strategy(strategy: S) -> Self {
        // Only the strategy's type matters: its value, a unit struct,
        // carries nothing.
        mem::forget(strategy);
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

    /// Clears the map, removing all elements.
    ///
    /// Drops every key and value in ascending key order, each key before its
    /// value, as dropping the map does. If one of their `Drop`s panics, the
    /// rest are still dropped, and the map is left empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "a");
    /// a.clear();
    /// assert!(a.is_empty());
    /// ```
    pub fn clear(&mut self) {
        self.length = 0;
        if let Some(root) = self.root.take() {
            root.destroy(PageByKind::deallocate);
        }
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

    /// Returns the first key-value pair in the map. The key in this pair is
    /// the minimum key in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// assert_eq!(map.first_key_value(), None);
    /// map.insert(1, "b");
    /// map.insert(2, "a");
    /// assert_eq!(map.first_key_value(), Some((&1, &"b")));
    /// ```
    pub fn first_key_value(&self) -> Option<(&K, &V)>
    where
        K: Ord,
    {
        let kv = self.root.as_ref()?.borrow().first_leaf_kv()?;
        Some(kv.into_key_value())
    }

    /// Removes and returns the first element in the map. The key of this
    /// element is the minimum key that was in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "a");
    /// map.insert(2, "b");
    /// assert_eq!(map.pop_first(), Some((1, "a")));
    /// assert_eq!(map.pop_first(), Some((2, "b")));
    /// assert_eq!(map.pop_first(), None);
    /// assert!(map.is_empty());
    /// ```
    pub fn pop_first(&mut self) -> Option<(K, V)>
    where
        K: Ord,
    {
        let entry = self.root.as_mut()?.pop_first(PageByKind::deallocate)?;
        self.length -= 1;
        Some(entry)
    }

    /// Returns the last key-value pair in the map. The key in this pair is
    /// the maximum key in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "b");
    /// map.insert(2, "a");
    /// assert_eq!(map.last_key_value(), Some((&2, &"a")));
    /// ```
    pub fn last_key_value(&self) -> Option<(&K, &V)>
    where
        K: Ord,
    {
        let kv = self.root.as_ref()?.borrow().last_leaf_kv()?;
        Some(kv.into_key_value())
    }

    /// Removes and returns the last element in the map. The key of this
    /// element is the maximum key that was in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "a");
    /// map.insert(2, "b");
    /// assert_eq!(map.pop_last(), Some((2, "b")));
    /// assert_eq!(map.pop_last(), Some((1, "a")));
    /// assert_eq!(map.pop_last(), None);
    /// assert!(map.is_empty());
    /// ```
    pub fn pop_last(&mut self) -> Option<(K, V)>
    where
        K: Ord,
    {
        let entry = self.root.as_mut()?.pop_last(PageByKind::deallocate)?;
        self.length -= 1;
        Some(entry)
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
    /// If `K`'s `Ord`, or its `InlineKey` with [`WithInlineKey`], panics, the
    /// map is unchanged: every call to them happens before the first change.
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
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

    /// Removes a key from the map, returning the value at the key if the key
    /// was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but the
    /// ordering on the borrowed form *must* match the ordering on the key
    /// type.
    ///
    /// If `K`'s `Ord`, or its `InlineKey` with [`WithInlineKey`], panics, the
    /// map is unchanged: every call to them happens before the first change.
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.remove(&1), Some("a"));
    /// assert_eq!(map.remove(&1), None);
    /// ```
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.remove_entry(key).map(|(_, value)| value)
    }

    /// Removes a key from the map, returning the stored key and value if the
    /// key was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but the
    /// ordering on the borrowed form *must* match the ordering on the key
    /// type.
    ///
    /// Panics in `K`'s `Ord` or `InlineKey` leave the map unchanged, as for
    /// [`BBTreeMap::remove`].
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.remove_entry(&1), Some((1, "a")));
    /// assert_eq!(map.remove_entry(&1), None);
    /// ```
    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        let entry = self
            .root
            .as_mut()?
            .remove_entry(key, PageByKind::deallocate)?;
        self.length -= 1;
        Some(entry)
    }
}

impl<K, V> Default for BBTreeMap<K, V> {
    /// Creates an empty `BBTreeMap`, with the default strategy, as
    /// [`BBTreeMap::new`] does.
    fn default() -> Self {
        Self::new()
    }
}

/// Drops every key and value in ascending key order, each key before its
/// value, as std's map does.
///
/// Unlike std's map, whose `Drop` marks `K` and `V` `#[may_dangle]` (nightly
/// only), this one requires the keys and values to strictly outlive the map.
impl<K, V, S: InlineKeyStrategy<K>> Drop for BBTreeMap<K, V, S> {
    fn drop(&mut self) {
        self.clear();
    }
}

// Keys and values live in the node pages, so moving the map never moves
// them: like std's map, it is `Unpin` whatever `K` and `V` are.
impl<K, V, S: InlineKeyStrategy<K>> Unpin for BBTreeMap<K, V, S> {}

// std's map is `UnwindSafe` whenever its keys and values are `RefUnwindSafe`,
// an impl std keeps for compatibility; match it (D1).
impl<K: RefUnwindSafe, V: RefUnwindSafe, S: InlineKeyStrategy<K>> UnwindSafe
    for BBTreeMap<K, V, S>
{
}
