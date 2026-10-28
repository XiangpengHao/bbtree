//! [`BBTreeMap`]: an ordered map with the API and behavior of std's
//! `BTreeMap` (D1). It is a B+-tree that stores each key once (D17), on
//! nodes sized in steps of their page size, by default 1 KB or 512 bytes,
//! without parent pointers (D11, D15, D21, D59).
//!
//! The map owns a [`Tree`], which has no root page until the first insert, so
//! that [`BBTreeMap::new`] allocates nothing. The strategy parameter `S`
//! decides which keys have inline keys (D18, `inline_key.rs`); every method
//! is the same for every `S`. The allocator parameter `A` is where the map's
//! pages come from (D8, `allocator.rs`): the map holds it, and gives it to
//! every operation on its tree that allocates or frees a page.

#![forbid(unsafe_code)]

mod append;
mod batch;
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

use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
#[cfg(any(test, feature = "inspect"))]
use crate::tree::inspect::TreeView;
use crate::tree::node_stack::KeySide;
use crate::tree::root::Tree;

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
/// relative to other keys while it is in the map, and so is a panic in user
/// code it calls ([crate docs](crate#user-code-must-not-panic)); the
/// behavior that results is unspecified but memory-safe.
///
/// Its nodes store an inline key beside each key, when the strategy `S`
/// gives the key type one: the first bytes of the key's encoding, in a
/// register, which searches compare first. By default ([`Auto`]), std byte
/// strings, 16-, 20- and 32-byte arrays, and 64- and 128-bit integers have
/// them, and other keys don't. [`WithInlineKey`] gives them to any
/// key type that implements [`InlineKey`]. Either way, every method behaves
/// as std's.
///
/// Its nodes come from the allocator `A`: by default ([`Global`]), the
/// global allocator; [`BBTreeMap::new_in`] takes another.
///
/// [`std::collections::BTreeMap`]: https://doc.rust-lang.org/std/collections/struct.BTreeMap.html
/// [`WithInlineKey`]: crate::WithInlineKey
/// [`InlineKey`]: crate::InlineKey
///
/// # Examples
///
/// ```
/// use bbtree::unsync::BBTreeMap;
///
/// let mut map = BBTreeMap::new();
/// assert_eq!(map.insert(37, "a"), None);
/// assert_eq!(map.insert(37, "b"), Some("a"));
/// assert_eq!(map.get(&37), Some(&"b"));
/// assert!(!map.contains_key(&38));
/// assert_eq!(map.len(), 1);
/// ```
pub struct BBTreeMap<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator + Clone = Global> {
    tree: Tree<K, V, S>,
    length: usize,
    /// Where the tree's pages come from and go back to.
    alloc: A,
}

impl<K, V> BBTreeMap<K, V> {
    /// Makes a new, empty `BBTreeMap`, with the default strategy, [`Auto`],
    /// and the global allocator. For another strategy, see
    /// [`BBTreeMap::with_strategy`]; for another allocator,
    /// [`BBTreeMap::new_in`].
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
    /// use bbtree::unsync::BBTreeMap;
    /// use bbtree::{InlineKey, InlineWord, WithInlineKey};
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
        Self::with_strategy_in(strategy, Global)
    }
}

impl<K, V, A: Allocator + Clone> BBTreeMap<K, V, Auto, A> {
    /// Makes a new, empty `BBTreeMap` whose nodes come from `alloc`, with
    /// the default strategy, [`Auto`].
    ///
    /// Does not allocate anything on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    /// use bbtree::Global;
    ///
    /// let mut map = BBTreeMap::new_in(Global);
    /// map.insert(1, "a");
    /// assert_eq!(map.get(&1), Some(&"a"));
    /// ```
    #[must_use]
    pub const fn new_in(alloc: A) -> Self {
        Self::with_strategy_in(Auto, alloc)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Makes a new, empty `BBTreeMap` with inline-key strategy `strategy`,
    /// whose nodes come from `alloc`.
    ///
    /// Does not allocate anything on its own.
    #[must_use]
    pub const fn with_strategy_in(strategy: S, alloc: A) -> Self {
        // Only the strategy's type matters: its value, a unit struct,
        // carries nothing.
        mem::forget(strategy);
        Self::empty(alloc)
    }

    /// A map with no tree, of any strategy, whose nodes come from `alloc`:
    /// what `new` makes.
    const fn empty(alloc: A) -> Self {
        Self {
            tree: Tree::EMPTY,
            length: 0,
            alloc,
        }
    }

    /// Returns the number of elements in the map.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length
    }

    /// The map's allocator, for a set made from it.
    pub(crate) const fn allocator(&self) -> &A {
        &self.alloc
    }

    /// Returns `true` if the map contains no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears the map, removing all elements.
    ///
    /// Drops every key and value in ascending key order, each key before its
    /// value, as dropping the map does.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "a");
    /// a.clear();
    /// assert!(a.is_empty());
    /// ```
    pub fn clear(&mut self) {
        self.length = 0;
        self.tree.take().destroy(&self.alloc);
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
    /// use bbtree::unsync::BBTreeMap;
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
        self.tree.find(key)
    }

    /// Returns the first key-value pair in the map. The key in this pair is
    /// the minimum key in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
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
        self.tree.first_key_value()
    }

    /// Removes and returns the first element in the map. The key of this
    /// element is the minimum key that was in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
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
        let Self {
            tree,
            length,
            alloc,
        } = self;
        tree.pop_first(alloc, length)
    }

    /// Returns the last key-value pair in the map. The key in this pair is
    /// the maximum key in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
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
        self.tree.last_key_value()
    }

    /// Removes and returns the last element in the map. The key of this
    /// element is the maximum key that was in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
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
        let Self {
            tree,
            length,
            alloc,
        } = self;
        tree.pop_last(alloc, length)
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
        self.tree.find_mut(key)
    }

    /// Inserts a key-value pair into the map.
    ///
    /// If the map did not have this key present, `None` is returned.
    ///
    /// If the map did have this key present, the value is updated, and the
    /// old value is returned. The key is not updated, though: the map keeps
    /// its key and drops the given one, before it replaces the value.
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Ord,
    {
        self.tree.insert(key, value, &self.alloc, &mut self.length)
    }

    /// Removes a key from the map, returning the value at the key if the key
    /// was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but the
    /// ordering on the borrowed form *must* match the ordering on the key
    /// type.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
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
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
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
        let Self {
            tree,
            length,
            alloc,
        } = self;
        tree.remove_entry(key, alloc, length)
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Splits the collection into two at the given key. Returns everything
    /// after the given key, including the key. The returned map's nodes
    /// come from a clone of this map's allocator. Both maps keep a tree,
    /// even an empty one, as std's do.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeMap;
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
            return Self::empty(self.alloc.clone());
        }
        let cut = self.tree.split_off_at(key, KeySide::Before, &self.alloc);
        let (lower_length, upper_length) = cut.lengths(&self.tree, self.length);
        self.length = lower_length;
        let mut upper = Self {
            tree: Tree::EMPTY,
            length: upper_length,
            alloc: self.alloc.clone(),
        };
        cut.finish(&mut self.tree, &mut upper.tree);
        upper
    }
}

#[cfg(any(test, fuzzing))]
impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Panics unless the map is a well-formed B+-tree of `len()` entries
    /// (D17): in order, balanced, every node at least at its minimum, every
    /// separator's value in a leaf's head, and every node's inline keys and
    /// prefix as its keys and fences give them. `Tree::check_invariants`
    /// (`tree/invariants.rs`) lists each check. Compiled for tests and for
    /// fuzzing builds (`--cfg fuzzing`, which `cargo fuzz` sets), where the
    /// fuzz targets (`fuzz/`) call it.
    ///
    /// Calls `Ord` and the strategy's inline-key encoding: a map whose keys
    /// break their contracts fails it.
    pub fn check_invariants(&self) {
        self.tree.check_invariants(self.length);
    }

    /// Panics unless both maps have the same length, tree shape, keys and
    /// prefix lengths: for batch equivalence tests and fuzzing. It compares
    /// no values; callers compare those by iterating.
    pub fn check_same_nodes(&self, other: &Self) {
        assert_eq!(self.length, other.length);
        self.tree.check_same_nodes(&other.tree);
    }

    /// Panics unless no node's prefix length is shorter than its parent's
    /// (D44), and every node along the tree's borders has the tree's shared
    /// prefix (D50). Calls neither `Ord` nor any encoding: it holds even
    /// for keys that panic or break their contracts.
    pub fn check_prefix_lens(&self) {
        self.tree.check_prefix_lens();
    }
}

#[cfg(any(test, feature = "inspect"))]
impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// The map's tree, to read node by node: its keys, inline keys, prefixes
    /// and page layouts, as the map stores them. For the visualizer (`vis/`);
    /// the `inspect` feature promises no stability.
    pub fn inspect(&self) -> TreeView<'_, K, V, S> {
        TreeView::new(&self.tree, self.length)
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
/// `clone` on every key and value, in key order, and no `Ord`. The copy's
/// nodes come from a clone of the allocator.
impl<K: Clone, V: Clone, S: InlineKeyStrategy<K>, A: Allocator + Clone> Clone
    for BBTreeMap<K, V, S, A>
{
    fn clone(&self) -> Self {
        let alloc = self.alloc.clone();
        Self {
            tree: self.tree.clone_in(&alloc),
            length: self.length,
            alloc,
        }
    }
}

impl<K, Q, V, S, A> Index<&Q> for BBTreeMap<K, V, S, A>
where
    K: Borrow<Q> + Ord,
    Q: Ord + ?Sized,
    S: InlineKeyStrategy<K>,
    A: Allocator + Clone,
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

impl<K: fmt::Debug, V: fmt::Debug, S: InlineKeyStrategy<K>, A: Allocator + Clone> fmt::Debug
    for BBTreeMap<K, V, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: PartialEq, V: PartialEq, S: InlineKeyStrategy<K>, A: Allocator + Clone> PartialEq
    for BBTreeMap<K, V, S, A>
{
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a == b)
    }
}

impl<K: Eq, V: Eq, S: InlineKeyStrategy<K>, A: Allocator + Clone> Eq for BBTreeMap<K, V, S, A> {}

/// Compares the entries in key order, lexicographically, as std's map does.
impl<K: PartialOrd, V: PartialOrd, S: InlineKeyStrategy<K>, A: Allocator + Clone> PartialOrd
    for BBTreeMap<K, V, S, A>
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.iter().partial_cmp(other.iter())
    }
}

/// Compares the entries in key order, lexicographically, as std's map does.
impl<K: Ord, V: Ord, S: InlineKeyStrategy<K>, A: Allocator + Clone> Ord for BBTreeMap<K, V, S, A> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.iter().cmp(other.iter())
    }
}

/// Hashes the length, then every entry in key order. std's map writes the
/// length with `Hasher::write_length_prefix`, unstable, whose default is
/// `write_usize`: the same bytes for every hasher on stable.
impl<K: Hash, V: Hash, S: InlineKeyStrategy<K>, A: Allocator + Clone> Hash
    for BBTreeMap<K, V, S, A>
{
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
impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> Drop for BBTreeMap<K, V, S, A> {
    fn drop(&mut self) {
        self.clear();
    }
}

// Keys and values live in the node pages, so moving the map never moves
// them: like std's map, it is `Unpin` whatever `K` and `V` are, if its
// allocator is.
impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone + Unpin> Unpin for BBTreeMap<K, V, S, A> {}

// std's map is `UnwindSafe` whenever its keys and values are `RefUnwindSafe`
// and its allocator `UnwindSafe`, an impl std keeps for compatibility; match
// it (D1).
impl<K: RefUnwindSafe, V: RefUnwindSafe, S: InlineKeyStrategy<K>, A: Allocator + Clone + UnwindSafe>
    UnwindSafe for BBTreeMap<K, V, S, A>
{
}
