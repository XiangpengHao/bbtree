//! The entry API: [`Entry`], [`OccupiedEntry`] and [`VacantEntry`], views
//! into one key's place in a map, as std's.
//!
//! An entry keeps the path its search found, from the root down to the key
//! or to the leaf edge where it goes (`node/path.rs`): every `Ord` call
//! happens in `entry`, as in std. Reading or changing an occupied entry goes
//! straight to the node at the end of the path; removing it, or inserting
//! into a vacant one, goes back up the path, comparing no keys. An insert
//! leaves the path at the new entry, wherever the splits took it, which is
//! how `insert` hands out its value.

#![forbid(unsafe_code)]

use core::fmt;

use super::BBTreeMap;
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node::handle::Subtree;
use crate::node::inline_keys::{InlinedKey, SearchKey};
use crate::node::page::{FreshPage, PageByKind};
use crate::node::path::PathMut;

/// A view into a single entry in a map, which may either be vacant or
/// occupied.
///
/// This `enum` is constructed from the [`entry`] method on [`BBTreeMap`].
///
/// [`entry`]: BBTreeMap::entry
pub enum Entry<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K> = Auto> {
    /// A vacant entry.
    Vacant(VacantEntry<'a, K, V, S>),
    /// An occupied entry.
    Occupied(OccupiedEntry<'a, K, V, S>),
}

/// A view into a vacant entry in a `BBTreeMap`. It is part of the [`Entry`]
/// enum.
pub struct VacantEntry<'a, K, V, S: InlineKeyStrategy<K> = Auto> {
    key: InlinedKey<K, S>,
    length: &'a mut usize,
    place: VacantPlace<'a, K, V, S>,
}

/// Where a vacant entry's key goes.
#[expect(
    clippy::large_enum_variant,
    reason = "an entry lives briefly and moves by value, as std's; boxing the path would allocate for every entry"
)]
enum VacantPlace<'a, K, V, S> {
    /// Into the leaf edge the path ends at.
    InTree(PathMut<'a, K, V, S>),
    /// Into a new tree, a leaf, for the map that has none.
    NoTree(&'a mut Option<Subtree<K, V, S>>),
}

/// A view into an occupied entry in a `BBTreeMap`. It is part of the
/// [`Entry`] enum.
pub struct OccupiedEntry<'a, K, V, S: InlineKeyStrategy<K> = Auto> {
    length: &'a mut usize,
    /// To the key: in a leaf, or a separator in an internal node, whose
    /// value is a leaf's head (D17).
    path: PathMut<'a, K, V, S>,
}

impl<K: fmt::Debug + Ord, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug
    for Entry<'_, K, V, S>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vacant(vacant) => f.debug_tuple("Entry").field(vacant).finish(),
            Self::Occupied(occupied) => f.debug_tuple("Entry").field(occupied).finish(),
        }
    }
}

impl<K: fmt::Debug + Ord, V, S: InlineKeyStrategy<K>> fmt::Debug for VacantEntry<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VacantEntry").field(self.key()).finish()
    }
}

impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug
    for OccupiedEntry<'_, K, V, S>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OccupiedEntry")
            .field("key", self.key())
            .field("value", self.get())
            .finish()
    }
}

impl<'a, K: Ord, V, S: InlineKeyStrategy<K>> Entry<'a, K, V, S> {
    /// Ensures a value is in the entry by inserting the default if empty,
    /// and returns a mutable reference to the value in the entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    ///
    /// assert_eq!(map["poneyland"], 12);
    /// ```
    pub fn or_insert(self, default: V) -> &'a mut V {
        match self {
            Self::Occupied(entry) => entry.into_mut(),
            Self::Vacant(entry) => entry.insert(default),
        }
    }

    /// Ensures a value is in the entry by inserting the result of the
    /// default function if empty, and returns a mutable reference to the
    /// value in the entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, String> = BBTreeMap::new();
    /// let s = "hoho".to_string();
    ///
    /// map.entry("poneyland").or_insert_with(|| s);
    ///
    /// assert_eq!(map["poneyland"], "hoho".to_string());
    /// ```
    pub fn or_insert_with<F: FnOnce() -> V>(self, default: F) -> &'a mut V {
        match self {
            Self::Occupied(entry) => entry.into_mut(),
            Self::Vacant(entry) => entry.insert(default()),
        }
    }

    /// Ensures a value is in the entry by inserting, if empty, the result of
    /// the default function. This method allows for generating key-derived
    /// values for insertion by providing the default function a reference to
    /// the key that was moved during the `.entry(key)` method call.
    ///
    /// The reference to the moved key is provided so that cloning or copying
    /// the key is unnecessary, unlike with `.or_insert_with(|| ... )`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    ///
    /// map.entry("poneyland").or_insert_with_key(|key| key.chars().count());
    ///
    /// assert_eq!(map["poneyland"], 9);
    /// ```
    pub fn or_insert_with_key<F: FnOnce(&K) -> V>(self, default: F) -> &'a mut V {
        match self {
            Self::Occupied(entry) => entry.into_mut(),
            Self::Vacant(entry) => {
                let value = default(entry.key());
                entry.insert(value)
            }
        }
    }

    /// Returns a reference to this entry's key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// assert_eq!(map.entry("poneyland").key(), &"poneyland");
    /// ```
    pub fn key(&self) -> &K {
        match self {
            Self::Occupied(entry) => entry.key(),
            Self::Vacant(entry) => entry.key(),
        }
    }

    /// Provides in-place mutable access to an occupied entry before any
    /// potential inserts into the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    ///
    /// map.entry("poneyland")
    ///    .and_modify(|e| { *e += 1 })
    ///    .or_insert(42);
    /// assert_eq!(map["poneyland"], 42);
    ///
    /// map.entry("poneyland")
    ///    .and_modify(|e| { *e += 1 })
    ///    .or_insert(42);
    /// assert_eq!(map["poneyland"], 43);
    /// ```
    pub fn and_modify<F: FnOnce(&mut V)>(self, f: F) -> Self {
        match self {
            Self::Occupied(mut entry) => {
                f(entry.get_mut());
                Self::Occupied(entry)
            }
            Self::Vacant(entry) => Self::Vacant(entry),
        }
    }

    /// Sets the value of the entry, and returns an `OccupiedEntry`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, String> = BBTreeMap::new();
    /// let entry = map.entry("poneyland").insert_entry("hoho".to_string());
    ///
    /// assert_eq!(entry.key(), &"poneyland");
    /// ```
    pub fn insert_entry(self, value: V) -> OccupiedEntry<'a, K, V, S> {
        match self {
            Self::Occupied(mut entry) => {
                entry.insert(value);
                entry
            }
            Self::Vacant(entry) => entry.insert_entry(value),
        }
    }
}

impl<'a, K: Ord, V: Default, S: InlineKeyStrategy<K>> Entry<'a, K, V, S> {
    /// Ensures a value is in the entry by inserting the default value if
    /// empty, and returns a mutable reference to the value in the entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, Option<usize>> = BBTreeMap::new();
    /// map.entry("poneyland").or_default();
    ///
    /// assert_eq!(map["poneyland"], None);
    /// ```
    pub fn or_default(self) -> &'a mut V {
        match self {
            Self::Occupied(entry) => entry.into_mut(),
            Self::Vacant(entry) => entry.insert(V::default()),
        }
    }
}

impl<'a, K: Ord, V, S: InlineKeyStrategy<K>> VacantEntry<'a, K, V, S> {
    /// Gets a reference to the key that would be used when inserting a value
    /// through the `VacantEntry`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// assert_eq!(map.entry("poneyland").key(), &"poneyland");
    /// ```
    pub fn key(&self) -> &K {
        self.key.key()
    }

    /// Take ownership of the key.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    ///
    /// if let Entry::Vacant(v) = map.entry("poneyland") {
    ///     v.into_key();
    /// }
    /// ```
    pub fn into_key(self) -> K {
        self.key.into_key()
    }

    /// Sets the value of the entry with the `VacantEntry`'s key, and returns
    /// a mutable reference to it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, u32> = BBTreeMap::new();
    ///
    /// if let Entry::Vacant(o) = map.entry("poneyland") {
    ///     o.insert(37);
    /// }
    /// assert_eq!(map["poneyland"], 37);
    /// ```
    pub fn insert(self, value: V) -> &'a mut V {
        let (path, length) = self.insert_and_count(value);
        *length += 1;
        path.into_value_mut()
    }

    /// Sets the value of the entry with the `VacantEntry`'s key, and returns
    /// an `OccupiedEntry`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, u32> = BBTreeMap::new();
    ///
    /// if let Entry::Vacant(o) = map.entry("poneyland") {
    ///     let entry = o.insert_entry(37);
    ///     assert_eq!(entry.get(), &37);
    /// }
    /// assert_eq!(map["poneyland"], 37);
    /// ```
    pub fn insert_entry(self, value: V) -> OccupiedEntry<'a, K, V, S> {
        let (path, length) = self.insert_and_count(value);
        *length += 1;
        OccupiedEntry { length, path }
    }

    /// Inserts the key with `value`, and returns the path to the new entry,
    /// and the map's length for the caller to count it.
    fn insert_and_count(self, value: V) -> (PathMut<'a, K, V, S>, &'a mut usize) {
        let Self { key, length, place } = self;
        let mut path = match place {
            VacantPlace::InTree(path) => path,
            VacantPlace::NoTree(root) => {
                let mut path = PathMut::new(
                    root.insert(Subtree::new_leaf(FreshPage::allocate()).forget_type()),
                );
                path.start_at_root();
                path
            }
        };
        path.insert(key, value, FreshPage::allocate, FreshPage::allocate);
        (path, length)
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> OccupiedEntry<'a, K, V, S> {
    /// Gets a reference to the key in the entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    /// assert_eq!(map.entry("poneyland").key(), &"poneyland");
    /// ```
    pub fn key(&self) -> &K {
        self.path.key()
    }

    /// Take ownership of the key and value from the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    ///
    /// if let Entry::Occupied(o) = map.entry("poneyland") {
    ///     // We delete the entry from the map.
    ///     assert_eq!(o.remove_entry(), ("poneyland", 12));
    /// }
    ///
    /// // If now try to get the value, it will panic:
    /// // println!("{}", map["poneyland"]);
    /// assert!(map.is_empty());
    /// ```
    pub fn remove_entry(mut self) -> (K, V) {
        let entry = self.path.remove(PageByKind::deallocate);
        *self.length -= 1;
        entry
    }

    /// Gets a reference to the value in the entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    ///
    /// if let Entry::Occupied(o) = map.entry("poneyland") {
    ///     assert_eq!(o.get(), &12);
    /// }
    /// ```
    pub fn get(&self) -> &V {
        self.path.value()
    }

    /// Gets a mutable reference to the value in the entry.
    ///
    /// If you need a reference to the `OccupiedEntry` that may outlive the
    /// destruction of the `Entry` value, see [`into_mut`].
    ///
    /// [`into_mut`]: OccupiedEntry::into_mut
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    ///
    /// assert_eq!(map["poneyland"], 12);
    /// if let Entry::Occupied(mut o) = map.entry("poneyland") {
    ///     *o.get_mut() += 10;
    ///     assert_eq!(*o.get(), 22);
    ///
    ///     // We can use the same Entry multiple times.
    ///     *o.get_mut() += 2;
    /// }
    /// assert_eq!(map["poneyland"], 24);
    /// ```
    pub fn get_mut(&mut self) -> &mut V {
        self.path.value_mut()
    }

    /// Converts the entry into a mutable reference to its value.
    ///
    /// If you need multiple references to the `OccupiedEntry`, see
    /// [`get_mut`].
    ///
    /// [`get_mut`]: OccupiedEntry::get_mut
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    ///
    /// assert_eq!(map["poneyland"], 12);
    /// if let Entry::Occupied(o) = map.entry("poneyland") {
    ///     *o.into_mut() += 10;
    /// }
    /// assert_eq!(map["poneyland"], 22);
    /// ```
    pub fn into_mut(self) -> &'a mut V {
        self.path.into_value_mut()
    }

    /// Sets the value of the entry with the `OccupiedEntry`'s key, and
    /// returns the entry's old value.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    ///
    /// if let Entry::Occupied(mut o) = map.entry("poneyland") {
    ///     assert_eq!(o.insert(15), 12);
    /// }
    /// assert_eq!(map["poneyland"], 15);
    /// ```
    pub fn insert(&mut self, value: V) -> V {
        core::mem::replace(self.get_mut(), value)
    }

    /// Takes the value of the entry out of the map, and returns it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    /// use bbtree::bbtree_map::Entry;
    ///
    /// let mut map: BBTreeMap<&str, usize> = BBTreeMap::new();
    /// map.entry("poneyland").or_insert(12);
    ///
    /// if let Entry::Occupied(o) = map.entry("poneyland") {
    ///     assert_eq!(o.remove(), 12);
    /// }
    /// // If we try to get "poneyland"'s value, it'll panic:
    /// // println!("{}", map["poneyland"]);
    /// assert!(map.is_empty());
    /// ```
    pub fn remove(self) -> V {
        self.remove_entry().1
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Gets the given key's corresponding entry in the map for in-place
    /// manipulation.
    ///
    /// Every `Ord` call the entry makes happens here; the entry's methods
    /// compare no keys. If `K`'s `Ord`, or its `InlineKey` with
    /// [`WithInlineKey`], panics, the map is unchanged.
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut count: BBTreeMap<&str, usize> = BBTreeMap::new();
    ///
    /// // count the number of occurrences of letters in the vec
    /// for x in ["a", "b", "a", "c", "a", "b"] {
    ///     count.entry(x).and_modify(|curr| *curr += 1).or_insert(1);
    /// }
    ///
    /// assert_eq!(count["a"], 3);
    /// assert_eq!(count["b"], 2);
    /// assert_eq!(count["c"], 1);
    /// ```
    pub fn entry(&mut self, key: K) -> Entry<'_, K, V, S>
    where
        K: Ord,
    {
        let key = InlinedKey::new(key);
        let Self { root, length } = self;
        if root.is_none() {
            return Entry::Vacant(VacantEntry {
                key,
                length,
                place: VacantPlace::NoTree(root),
            });
        }
        let mut path = PathMut::new(root.as_mut().expect("checked just above"));
        if path.search_expecting_append(&SearchKey::for_inlined(&key)) {
            Entry::Occupied(OccupiedEntry { length, path })
        } else {
            Entry::Vacant(VacantEntry {
                key,
                length,
                place: VacantPlace::InTree(path),
            })
        }
    }

    /// Returns the first entry in the map for in-place manipulation. The key
    /// of this entry is the minimum key in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "a");
    /// map.insert(2, "b");
    /// if let Some(mut entry) = map.first_entry() {
    ///     if *entry.key() > 0 {
    ///         entry.insert("first");
    ///     }
    /// }
    /// assert_eq!(*map.get(&1).unwrap(), "first");
    /// assert_eq!(*map.get(&2).unwrap(), "b");
    /// ```
    pub fn first_entry(&mut self) -> Option<OccupiedEntry<'_, K, V, S>>
    where
        K: Ord,
    {
        let Self { root, length } = self;
        let mut path = PathMut::new(root.as_mut()?);
        if path.descend_to_first_key() {
            Some(OccupiedEntry { length, path })
        } else {
            None
        }
    }

    /// Returns the last entry in the map for in-place manipulation. The key
    /// of this entry is the maximum key in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map = BBTreeMap::new();
    /// map.insert(1, "a");
    /// map.insert(2, "b");
    /// if let Some(mut entry) = map.last_entry() {
    ///     if *entry.key() > 0 {
    ///         entry.insert("last");
    ///     }
    /// }
    /// assert_eq!(*map.get(&1).unwrap(), "a");
    /// assert_eq!(*map.get(&2).unwrap(), "last");
    /// ```
    pub fn last_entry(&mut self) -> Option<OccupiedEntry<'_, K, V, S>>
    where
        K: Ord,
    {
        let Self { root, length } = self;
        let mut path = PathMut::new(root.as_mut()?);
        if path.descend_to_last_key() {
            Some(OccupiedEntry { length, path })
        } else {
            None
        }
    }
}
