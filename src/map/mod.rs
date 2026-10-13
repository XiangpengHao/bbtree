//! [`BBTreeMap`]: an ordered map with the API and behavior of std's
//! `BTreeMap` (D1), on 1 KB nodes without parent pointers (D11, D15), as a
//! B+-tree that stores each key once (D17).
//!
//! The map owns at most one tree, by its [`Root`]: none until the first insert, so
//! that [`BBTreeMap::new`] allocates nothing. Point searches and ordinary
//! fitting inserts descend from the root; increasing inserts can reuse a
//! last leaf remembered after a split. An insert that splits records a
//! path for going back up (`tree/path.rs`, `tree/insert.rs`), and so
//! does removing, for merges (`tree/remove.rs`); an entry keeps its path
//! (`entry.rs`). Iterating walks the tree from both ends at once, each end a
//! path (`iter.rs`, `iter_mut.rs`, on `tree/walk.rs`); iterating by value
//! takes the tree apart as it goes (`into_iter.rs`), as dropping the map
//! does. Bulk changes drain entries in order and build trees of full nodes
//! (`append.rs`, `extract_if.rs`, `from_entries.rs`, on `tree/build.rs`),
//! but `split_off`, which cuts the tree along a path (`tree/cut.rs`).
//!
//! The strategy parameter `S` decides which keys have inline keys (D18,
//! `inline_key.rs`); every method is the same for every `S`.

#![forbid(unsafe_code)]

mod append;
mod entry;
mod extract_if;
mod from_entries;
mod into_iter;
mod iter;
mod iter_mut;
#[cfg(test)]
mod tests;

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::mem;
use core::ops::Index;
use core::panic::{RefUnwindSafe, UnwindSafe};

use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::build::TreeBuilder;
#[cfg(any(test, feature = "inspect"))]
use crate::tree::inspect::TreeView;
use crate::tree::node::page::{FreshPage, PageByKind};
use crate::tree::path::{FirstEdge, Immut, KeySide, LastEdge, Path};
use crate::tree::root::Root;

pub use entry::{Entry, OccupiedEntry, VacantEntry};
pub use extract_if::ExtractIf;
pub(crate) use extract_if::ExtractionWalk;
pub use into_iter::{IntoIter, IntoKeys, IntoValues};
pub use iter::{Iter, Keys, Range, Values};
pub use iter_mut::{IterMut, RangeMut, ValuesMut};

/// An ordered map based on a B-tree with page-sized nodes: a drop-in
/// replacement for [`std::collections::BTreeMap`].
///
/// As with std's map, it is a logic error for a key to change its order
/// relative to other keys while it is in the map; the behavior that results
/// is unspecified but memory-safe.
///
/// Its nodes store an inline key beside each key, a register-sized summary
/// that searches compare first, when the strategy `S` gives the key type
/// one: by default ([`Auto`]), std byte-string and 64- and 128-bit integer
/// keys have them, and other keys don't. [`WithInlineKey`] gives them to any
/// key type that implements [`InlineKey`]. Either way, every method behaves
/// as std's.
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
    root: Root<K, V, S>,
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
    ///     fn inline_key_at(&self, offset: usize) -> InlineWord {
    ///         self.0.inline_key_at(offset)
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
            root: Root::EMPTY,
            length: 0,
        }
    }

    /// A map with no tree, of any strategy: what `new` makes.
    const fn empty() -> Self {
        Self {
            root: Root::EMPTY,
            length: 0,
        }
    }

    /// The map of what `builder` built.
    fn from_builder(builder: TreeBuilder<K, V, S>) -> Self {
        let (root, length) = builder.finish();
        Self { root, length }
    }

    /// Gives the map a tree, an empty leaf, if it has none: as std's map
    /// keeps an empty root after losing its entries, which decides whether
    /// `range` checks its bounds.
    fn keep_a_tree(&mut self) {
        self.root.page_or_new();
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
        self.root.take().destroy(PageByKind::deallocate);
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
        self.root.find(key)
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
        let root = self.root.page()?;
        let mut path = Path::<Immut<'_>, K, V, S>::detached();
        path.attach(root.top(), root.height());
        if path.descend_to_first_key() {
            Some(path.key_value())
        } else {
            None
        }
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
        self.take_first()
    }

    /// [`Self::pop_first`] without its `Ord` bound, which finding the first
    /// entry doesn't need: `IntoIter` has none.
    fn take_first(&mut self) -> Option<(K, V)> {
        let Self { root, length } = self;
        root.pop_first(PageByKind::deallocate, length)
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
        let root = self.root.page()?;
        let mut path = Path::<Immut<'_>, K, V, S>::detached();
        path.attach(root.top(), root.height());
        if path.descend_to_last_key() {
            Some(path.key_value())
        } else {
            None
        }
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
        self.take_last()
    }

    /// [`Self::pop_last`] without its `Ord` bound, as [`Self::take_first`].
    fn take_last(&mut self) -> Option<(K, V)> {
        let Self { root, length } = self;
        root.pop_last(PageByKind::deallocate, length)
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
        self.root.find_mut(key)
    }

    /// Inserts a key-value pair into the map.
    ///
    /// If the map did not have this key present, `None` is returned.
    ///
    /// If the map did have this key present, the value is updated, and the
    /// old value is returned. The key is not updated, though: the map keeps
    /// its key and drops the given one, before it replaces the value.
    ///
    /// If `K`'s `Ord` panics, the map is unchanged: every call to it happens
    /// before the first change. So does every call to its `InlineKey` with
    /// [`WithInlineKey`], but for those an insert makes last, once the entry
    /// is in and counted, if it split nodes, to fit their inline keys to
    /// their new fences: if one of those panics, the map has the new entry.
    /// Either way, it stays valid.
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Ord,
    {
        self.root.insert(
            key,
            value,
            FreshPage::allocate,
            FreshPage::allocate,
            &mut self.length,
        )
    }

    /// Removes a key from the map, returning the value at the key if the key
    /// was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but the
    /// ordering on the borrowed form *must* match the ordering on the key
    /// type.
    ///
    /// If `K`'s `Ord` panics, the map is unchanged: every call to it happens
    /// before the first change. So does every call to its `InlineKey` with
    /// [`WithInlineKey`], but for those a removal makes last, once the entry
    /// is out and counted, if it merged nodes or moved entries between them
    /// or removed a key from an internal node, to fit their inline keys to
    /// their new fences: if one of those panics, the entry is gone. Either
    /// way, the map stays valid.
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
    /// Panics in `K`'s `Ord` or `InlineKey` leave the map as for
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
        let Self { root, length } = self;
        root.remove_entry(key, PageByKind::deallocate, length)
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Splits the collection into two at the given key. Returns everything
    /// after the given key, including the key.
    ///
    /// Every `Ord` call happens first, before any entry moves. `InlineKey`
    /// is called last, once both maps are counted: if it panics, both stay
    /// valid. Both maps keep a tree, even an empty one, as std's do.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "a");
    /// a.insert(2, "b");
    /// a.insert(3, "c");
    /// a.insert(17, "d");
    /// a.insert(41, "e");
    ///
    /// let b = a.split_off(&3);
    ///
    /// assert_eq!(a.len(), 2);
    /// assert_eq!(b.len(), 3);
    ///
    /// assert_eq!(a[&1], "a");
    /// assert_eq!(a[&2], "b");
    ///
    /// assert_eq!(b[&3], "c");
    /// assert_eq!(b[&17], "d");
    /// assert_eq!(b[&41], "e");
    /// ```
    pub fn split_off<Q>(&mut self, key: &Q) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.is_empty() {
            return Self::empty();
        }
        let length = self.length;
        let lower = &mut self.root;
        let upper = lower.split_off_at(key, KeySide::Before);
        let (lower_length, upper_length) = if lower.height() < upper.height() {
            let lower_length = lower.top().count_entries();
            (lower_length, length - lower_length)
        } else {
            let upper_length = upper.top().count_entries();
            (length - upper_length, upper_length)
        };
        self.length = lower_length;
        let mut upper = Self {
            root: upper,
            length: upper_length,
        };
        self.root.fit_border_siblings::<LastEdge>();
        upper.root.fit_border_siblings::<FirstEdge>();
        upper
    }
}

#[cfg(any(test, fuzzing))]
impl<K: Ord, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Panics unless the map is a well-formed B+-tree of `len()` entries
    /// (D17): in order, balanced, every node at least at its minimum, every
    /// separator's value in a leaf's head, and every node's inline keys and
    /// prefix as its keys and fences give them; `Root::check_invariants`
    /// (`tree/invariants.rs`) lists each check. Compiled for tests and for
    /// fuzzing builds (`--cfg fuzzing`, which `cargo fuzz` sets), where the
    /// fuzz targets (`fuzz/`) call it.
    ///
    /// Calls `Ord` and the strategy's inline-key encoding: a map whose keys
    /// break their contracts fails it.
    pub fn check_invariants(&self) {
        self.root.check_invariants(self.length);
    }

    /// Panics unless no node's prefix length is shorter than its parent's
    /// (D44), and every node along the tree's borders has the tree's shared
    /// prefix (D50). Calls neither `Ord` nor any encoding: it holds even
    /// for keys that panic or break their contracts.
    pub fn check_prefix_lens(&self) {
        self.root.check_prefix_lens();
    }
}

#[cfg(any(test, feature = "inspect"))]
impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// The map's tree, to read node by node: its keys, inline keys, prefixes
    /// and page layouts, as the map stores them. For the visualizer (`vis/`);
    /// the `inspect` feature promises no stability.
    pub fn inspect(&self) -> TreeView<'_, K, V, S> {
        TreeView::new(&self.root)
    }
}

impl<K, V> Default for BBTreeMap<K, V> {
    /// Creates an empty `BBTreeMap`, with the default strategy, as
    /// [`BBTreeMap::new`] does.
    fn default() -> Self {
        Self::new()
    }
}

/// Copies the tree node by node, with its shape, as std's map does: calls
/// `clone` on every key and value, in key order, and no `Ord`.
impl<K: Clone, V: Clone, S: InlineKeyStrategy<K>> Clone for BBTreeMap<K, V, S> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone_tree(),
            length: self.length,
        }
    }
}

impl<K, Q, V, S> Index<&Q> for BBTreeMap<K, V, S>
where
    K: Borrow<Q> + Ord,
    Q: Ord + ?Sized,
    S: InlineKeyStrategy<K>,
{
    type Output = V;

    /// Returns a reference to the value corresponding to the supplied key.
    ///
    /// # Panics
    ///
    /// Panics if the key is not present in the `BBTreeMap`.
    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("no entry found for key")
    }
}

impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>> fmt::Debug for BBTreeMap<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: PartialEq, V: PartialEq, S: InlineKeyStrategy<K>> PartialEq for BBTreeMap<K, V, S> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a == b)
    }
}

impl<K: Eq, V: Eq, S: InlineKeyStrategy<K>> Eq for BBTreeMap<K, V, S> {}

/// Compares the entries in key order, lexicographically, as std's map does.
impl<K: PartialOrd, V: PartialOrd, S: InlineKeyStrategy<K>> PartialOrd for BBTreeMap<K, V, S> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.iter().partial_cmp(other.iter())
    }
}

/// Compares the entries in key order, lexicographically, as std's map does.
impl<K: Ord, V: Ord, S: InlineKeyStrategy<K>> Ord for BBTreeMap<K, V, S> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.iter().cmp(other.iter())
    }
}

/// Hashes the length, then every entry in key order. std's map writes the
/// length with `Hasher::write_length_prefix`, unstable, whose default is
/// `write_usize`: the same bytes for every hasher on stable.
impl<K: Hash, V: Hash, S: InlineKeyStrategy<K>> Hash for BBTreeMap<K, V, S> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.len());
        for entry in self {
            entry.hash(state);
        }
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
