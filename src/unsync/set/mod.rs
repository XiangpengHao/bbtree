//! [`BBTreeSet`]: an ordered set with the API and behavior of std's
//! `BTreeSet` (D1), as a [`BBTreeMap`] whose values are zero-sized, as std's
//! set is a map of zero-sized values. Each method hands its work to the map's
//! method of the same job; the iterators wrap the map's (`iter.rs`, on
//! `map/iter.rs` and `map/into_iter.rs`).
//!
//! The operations on two sets are std's algorithms on the set's iterators
//! and lookups: `difference.rs`, `intersection.rs`, `union.rs` and
//! `symmetric_difference.rs`, the last two on one merge (`merge.rs`), and
//! `subset.rs`. Their operators build the resulting set into full nodes
//! from the ascending elements, as `collect` does once it has sorted them.
//!
//! The values are `SetValZST`, std's name for its own, so that iterators
//! that print the map's entries print what std's print.

#![forbid(unsafe_code)]

mod difference;
mod intersection;
mod iter;
mod lookups;
mod merge;
mod subset;
mod symmetric_difference;
mod union;

#[cfg(test)]
mod tests;

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};

use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::unsync::map::BBTreeMap;

pub use difference::Difference;
pub use intersection::Intersection;
pub use iter::{ExtractIf, IntoIter, Iter, Range};
pub use symmetric_difference::SymmetricDifference;
pub use union::Union;

/// The value of every element of a set's map: it takes no space, and
/// compares, hashes and prints as std's does.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SetValZST;

/// An ordered set based on a B-tree with page-sized nodes: a drop-in
/// replacement for [`std::collections::BTreeSet`].
///
/// It is a [`BBTreeMap`] whose values take no space; see the map for its
/// nodes and inline keys. As with the map, the strategy `S` decides which
/// elements have inline keys, and every method behaves as std's.
///
/// It is a logic error for an item to be modified in such a way that the
/// item's ordering relative to any other item, as determined by the [`Ord`]
/// trait, changes while it is in the set. The behavior that results is
/// unspecified but memory-safe.
///
/// [`std::collections::BTreeSet`]: https://doc.rust-lang.org/std/collections/struct.BTreeSet.html
///
/// # Examples
///
/// ```
/// use bbtree::unsync::BBTreeSet;
///
/// // Type inference lets us omit an explicit type signature (which
/// // would be `BBTreeSet<&str>` in this example).
/// let mut books = BBTreeSet::new();
///
/// // Add some books.
/// books.insert("A Dance With Dragons");
/// books.insert("To Kill a Mockingbird");
/// books.insert("The Odyssey");
/// books.insert("The Great Gatsby");
///
/// // Check for a specific one.
/// if !books.contains("The Winds of Winter") {
///     println!("We have {} books, but The Winds of Winter ain't one.",
///              books.len());
/// }
///
/// // Remove a book.
/// books.remove("The Odyssey");
///
/// // Iterate over everything.
/// for book in &books {
///     println!("{book}");
/// }
/// ```
///
/// A `BBTreeSet` with a known list of items can be initialized from an
/// array:
///
/// ```
/// use bbtree::unsync::BBTreeSet;
///
/// let set = BBTreeSet::from([1, 2, 3]);
/// ```
pub struct BBTreeSet<T, S: InlineKeyStrategy<T> = Auto, A: Allocator + Clone = Global> {
    map: BBTreeMap<T, SetValZST, S, A>,
}

#[cfg(fuzzing)]
impl<T: Ord, S: InlineKeyStrategy<T>, A: Allocator + Clone> BBTreeSet<T, S, A> {
    /// Panics unless the set's tree is well formed, by the map's check
    /// ([`BBTreeMap::check_invariants`]). Only in fuzzing builds
    /// (`--cfg fuzzing`, which `cargo fuzz` sets), for the fuzz targets.
    pub fn check_invariants(&self) {
        self.map.check_invariants();
    }

    /// Checks tree shape, keys and prefixes against individual inserts.
    pub fn check_same_nodes(&self, other: &Self) {
        self.map.check_same_nodes(&other.map);
    }
}

impl<T> BBTreeSet<T> {
    /// Makes a new, empty `BBTreeSet`, with the default strategy, [`Auto`],
    /// and the global allocator. For another strategy, see
    /// [`BBTreeSet::with_strategy`]; for another allocator,
    /// [`BBTreeSet::new_in`].
    ///
    /// Does not allocate anything on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// # #![allow(unused_mut)]
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set: BBTreeSet<i32> = BBTreeSet::new();
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self::with_strategy(Auto)
    }
}

impl<T, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
    /// Makes a new, empty `BBTreeSet` with inline-key strategy `strategy`.
    /// `BBTreeSet::new()` is `BBTreeSet::with_strategy(Auto)`.
    ///
    /// Does not allocate anything on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
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
    /// let mut set: BBTreeSet<Id, WithInlineKey> = BBTreeSet::with_strategy(WithInlineKey);
    /// set.insert(Id(*b"ferris!!"));
    /// assert!(set.contains(&Id(*b"ferris!!")));
    /// ```
    #[must_use]
    pub const fn with_strategy(strategy: S) -> Self {
        Self::with_strategy_in(strategy, Global)
    }
}

impl<T, A: Allocator + Clone> BBTreeSet<T, Auto, A> {
    /// Makes a new, empty `BBTreeSet` whose nodes come from `alloc`, with
    /// the default strategy, [`Auto`].
    ///
    /// Does not allocate anything on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    /// use bbtree::Global;
    ///
    /// let mut set = BBTreeSet::new_in(Global);
    /// set.insert(1);
    /// assert!(set.contains(&1));
    /// ```
    #[must_use]
    pub const fn new_in(alloc: A) -> Self {
        Self::with_strategy_in(Auto, alloc)
    }
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> BBTreeSet<T, S, A> {
    /// Makes a new, empty `BBTreeSet` with inline-key strategy `strategy`,
    /// whose nodes come from `alloc`.
    ///
    /// Does not allocate anything on its own.
    #[must_use]
    pub const fn with_strategy_in(strategy: S, alloc: A) -> Self {
        Self {
            map: BBTreeMap::with_strategy_in(strategy, alloc),
        }
    }

    /// Clears the set, removing all elements.
    ///
    /// Drops every element in ascending order, as dropping the set does.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut v = BBTreeSet::new();
    /// v.insert(1);
    /// v.clear();
    /// assert!(v.is_empty());
    /// ```
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Tests each value in input order, as separate calls to `contains`
    /// would. Values may repeat; an empty batch returns an empty array.
    pub fn contains_many<Q, const N: usize>(&self, values: [&Q; N]) -> [bool; N]
    where
        T: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.map.get_many(values).map(|value| value.is_some())
    }

    /// Inserts each value in order, returning whether it was newly added.
    /// A repeated value returns `false` after its first insertion and keeps
    /// the existing value, as `insert` does.
    pub fn insert_many<const N: usize>(&mut self, values: [T; N]) -> [bool; N]
    where
        T: Ord,
    {
        self.map
            .insert_many(values.map(|value| (value, SetValZST)))
            .map(|old| old.is_none())
    }

    /// Returns `true` if the set contains an element equal to the value.
    ///
    /// The value may be any borrowed form of the set's element type,
    /// but the ordering on the borrowed form *must* match the
    /// ordering on the element type.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let set = BBTreeSet::from([1, 2, 3]);
    /// assert_eq!(set.contains(&1), true);
    /// assert_eq!(set.contains(&4), false);
    /// ```
    pub fn contains<Q>(&self, value: &Q) -> bool
    where
        T: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.map.contains_key(value)
    }

    /// Returns a reference to the element in the set, if any, that is equal
    /// to the value.
    ///
    /// The value may be any borrowed form of the set's element type,
    /// but the ordering on the borrowed form *must* match the
    /// ordering on the element type.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let set = BBTreeSet::from([1, 2, 3]);
    /// assert_eq!(set.get(&2), Some(&2));
    /// assert_eq!(set.get(&4), None);
    /// ```
    pub fn get<Q>(&self, value: &Q) -> Option<&T>
    where
        T: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.map.get_key_value(value).map(|(element, _)| element)
    }

    /// Returns a reference to the first element in the set, if any.
    /// This element is always the minimum of all elements in the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::new();
    /// assert_eq!(set.first(), None);
    /// set.insert(1);
    /// assert_eq!(set.first(), Some(&1));
    /// set.insert(2);
    /// assert_eq!(set.first(), Some(&1));
    /// ```
    #[must_use]
    pub fn first(&self) -> Option<&T>
    where
        T: Ord,
    {
        self.map.first_key_value().map(|(element, _)| element)
    }

    /// Returns a reference to the last element in the set, if any.
    /// This element is always the maximum of all elements in the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::new();
    /// assert_eq!(set.last(), None);
    /// set.insert(1);
    /// assert_eq!(set.last(), Some(&1));
    /// set.insert(2);
    /// assert_eq!(set.last(), Some(&2));
    /// ```
    #[must_use]
    pub fn last(&self) -> Option<&T>
    where
        T: Ord,
    {
        self.map.last_key_value().map(|(element, _)| element)
    }

    /// Removes the first element from the set and returns it, if any.
    /// The first element is always the minimum element in the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::new();
    ///
    /// set.insert(1);
    /// while let Some(n) = set.pop_first() {
    ///     assert_eq!(n, 1);
    /// }
    /// assert!(set.is_empty());
    /// ```
    pub fn pop_first(&mut self) -> Option<T>
    where
        T: Ord,
    {
        self.map.pop_first().map(|(element, _)| element)
    }

    /// Removes the last element from the set and returns it, if any.
    /// The last element is always the maximum element in the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::new();
    ///
    /// set.insert(1);
    /// while let Some(n) = set.pop_last() {
    ///     assert_eq!(n, 1);
    /// }
    /// assert!(set.is_empty());
    /// ```
    pub fn pop_last(&mut self) -> Option<T>
    where
        T: Ord,
    {
        self.map.pop_last().map(|(element, _)| element)
    }

    /// Adds a value to the set.
    ///
    /// Returns whether the value was newly inserted. That is:
    ///
    /// - If the set did not previously contain an equal value, `true` is
    ///   returned.
    /// - If the set already contained an equal value, `false` is returned, and
    ///   the entry is not updated: the set keeps its element and drops the
    ///   given one.
    ///
    /// Panics in `T`'s `Ord` or `InlineKey` leave the set as for
    /// [`BBTreeMap::insert`].
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::new();
    ///
    /// assert_eq!(set.insert(2), true);
    /// assert_eq!(set.insert(2), false);
    /// assert_eq!(set.len(), 1);
    /// ```
    pub fn insert(&mut self, value: T) -> bool
    where
        T: Ord,
    {
        self.map.insert(value, SetValZST).is_none()
    }

    /// Adds a value to the set, replacing the existing element, if any, that
    /// is equal to the value. Returns the replaced element.
    ///
    /// It searches the set once. If `T`'s `Ord`, or its `InlineKey` with
    /// [`WithInlineKey`], panics before an element is replaced, the set is
    /// unchanged; an insert panics as [`BBTreeMap::insert`] does.
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::new();
    /// set.insert(Vec::<i32>::new());
    ///
    /// assert_eq!(set.get(&[][..]).unwrap().capacity(), 0);
    /// set.replace(Vec::with_capacity(10));
    /// assert_eq!(set.get(&[][..]).unwrap().capacity(), 10);
    /// ```
    pub fn replace(&mut self, value: T) -> Option<T>
    where
        T: Ord,
    {
        self.map.replace_key(value, SetValZST)
    }

    /// If the set contains an element equal to the value, removes it from
    /// the set and drops it. Returns whether such an element was present.
    ///
    /// The value may be any borrowed form of the set's element type,
    /// but the ordering on the borrowed form *must* match the
    /// ordering on the element type.
    ///
    /// Panics in `T`'s `Ord` or `InlineKey` leave the set as for
    /// [`BBTreeMap::remove`].
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::new();
    ///
    /// set.insert(2);
    /// assert_eq!(set.remove(&2), true);
    /// assert_eq!(set.remove(&2), false);
    /// ```
    pub fn remove<Q>(&mut self, value: &Q) -> bool
    where
        T: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.map.remove(value).is_some()
    }

    /// Removes and returns the element in the set, if any, that is equal to
    /// the value.
    ///
    /// The value may be any borrowed form of the set's element type,
    /// but the ordering on the borrowed form *must* match the
    /// ordering on the element type.
    ///
    /// Panics in `T`'s `Ord` or `InlineKey` leave the set as for
    /// [`BBTreeMap::remove`].
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::from([1, 2, 3]);
    /// assert_eq!(set.take(&2), Some(2));
    /// assert_eq!(set.take(&2), None);
    /// ```
    pub fn take<Q>(&mut self, value: &Q) -> Option<T>
    where
        T: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.map.remove_entry(value).map(|(element, _)| element)
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// In other words, remove all elements `e` for which `f(&e)` returns
    /// `false`. The elements are visited in ascending order.
    ///
    /// As the map's [`BBTreeMap::retain`], it rebuilds the tree from the
    /// elements it keeps.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut set = BBTreeSet::from([1, 2, 3, 4, 5, 6]);
    /// // Keep only the even numbers.
    /// set.retain(|&k| k % 2 == 0);
    /// assert!(set.iter().eq([2, 4, 6].iter()));
    /// ```
    pub fn retain<F>(&mut self, mut f: F)
    where
        T: Ord,
        F: FnMut(&T) -> bool,
    {
        self.map.retain(|element, _| f(element));
    }

    /// Moves all elements from `other` into `self`, leaving `other` empty.
    ///
    /// Of two equal elements, `self` keeps its own and drops `other`'s.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut a = BBTreeSet::new();
    /// a.insert(1);
    /// a.insert(2);
    /// a.insert(3);
    ///
    /// let mut b = BBTreeSet::new();
    /// b.insert(3);
    /// b.insert(4);
    /// b.insert(5);
    ///
    /// a.append(&mut b);
    ///
    /// assert_eq!(a.len(), 5);
    /// assert_eq!(b.len(), 0);
    ///
    /// assert!(a.contains(&1));
    /// assert!(a.contains(&2));
    /// assert!(a.contains(&3));
    /// assert!(a.contains(&4));
    /// assert!(a.contains(&5));
    /// ```
    pub fn append(&mut self, other: &mut Self)
    where
        T: Ord,
    {
        self.map.append(&mut other.map);
    }

    /// Splits the collection into two at the value. Returns a new collection
    /// with all elements greater than or equal to the value.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut a = BBTreeSet::new();
    /// a.insert(1);
    /// a.insert(2);
    /// a.insert(3);
    /// a.insert(17);
    /// a.insert(41);
    ///
    /// let b = a.split_off(&3);
    ///
    /// assert_eq!(a.len(), 2);
    /// assert_eq!(b.len(), 3);
    ///
    /// assert!(a.contains(&1));
    /// assert!(a.contains(&2));
    ///
    /// assert!(b.contains(&3));
    /// assert!(b.contains(&17));
    /// assert!(b.contains(&41));
    /// ```
    pub fn split_off<Q>(&mut self, value: &Q) -> Self
    where
        T: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        Self {
            map: self.map.split_off(value),
        }
    }

    /// Returns the number of elements in the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut v = BBTreeSet::new();
    /// assert_eq!(v.len(), 0);
    /// v.insert(1);
    /// assert_eq!(v.len(), 1);
    /// ```
    #[must_use]
    pub const fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns `true` if the set contains no elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let mut v = BBTreeSet::new();
    /// assert!(v.is_empty());
    /// v.insert(1);
    /// assert!(!v.is_empty());
    /// ```
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> BBTreeSet<T, S, A> {
    /// A set of `elements`, which ascend strictly, built into full nodes
    /// without comparing them (`tree/build.rs`), on pages from `alloc`.
    /// Elements out of order make a set whose searches give wrong answers,
    /// but that is memory-safe.
    fn from_ascending(elements: impl Iterator<Item = T>, alloc: A) -> Self {
        let entries = elements.map(|element| (element, SetValZST));
        Self {
            map: BBTreeMap::from_ascending_entries(entries, alloc),
        }
    }

    /// The set's allocator, for a set made from it.
    fn allocator(&self) -> &A {
        self.map.allocator()
    }

    /// Whether looking up `lookups` elements in this set one by one beats
    /// walking the set alongside them: when the set has at least 16 times
    /// as many elements, std's estimate for its sets. It must be std's: the
    /// choice decides which of two equal elements an intersection hands out
    /// (the smaller set's, when it looks them up), and what its iterator
    /// prints and hints. Dividing rather than multiplying can't overflow.
    fn prefers_lookups(&self, lookups: usize) -> bool {
        lookups <= self.len() / 16
    }
}

impl<T> Default for BBTreeSet<T> {
    /// Creates an empty `BBTreeSet`, with the default strategy, as
    /// [`BBTreeSet::new`] does.
    fn default() -> Self {
        Self::new()
    }
}

/// Copies the tree as the map's `clone` does: calls `clone` on every
/// element, in order, and no `Ord`.
impl<T: Clone, S: InlineKeyStrategy<T>, A: Allocator + Clone> Clone for BBTreeSet<T, S, A> {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
        }
    }
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>, A: Allocator + Clone> fmt::Debug
    for BBTreeSet<T, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<T: PartialEq, S: InlineKeyStrategy<T>, A: Allocator + Clone> PartialEq for BBTreeSet<T, S, A> {
    fn eq(&self, other: &Self) -> bool {
        self.map == other.map
    }
}

impl<T: Eq, S: InlineKeyStrategy<T>, A: Allocator + Clone> Eq for BBTreeSet<T, S, A> {}

/// Compares the elements in order, lexicographically, as std's set does.
impl<T: PartialOrd, S: InlineKeyStrategy<T>, A: Allocator + Clone> PartialOrd
    for BBTreeSet<T, S, A>
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.map.partial_cmp(&other.map)
    }
}

/// Compares the elements in order, lexicographically, as std's set does.
impl<T: Ord, S: InlineKeyStrategy<T>, A: Allocator + Clone> Ord for BBTreeSet<T, S, A> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.map.cmp(&other.map)
    }
}

/// Hashes as the map does, the length and then every element in order: the
/// values hash nothing, so the bytes are std's set's.
impl<T: Hash, S: InlineKeyStrategy<T>, A: Allocator + Clone> Hash for BBTreeSet<T, S, A> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.map.hash(state);
    }
}

/// Collects the elements, sorts them, and keeps only the last of equal
/// elements, as std's set does. Like `new` and `Default`, it makes sets of
/// the default strategy only.
impl<T: Ord> FromIterator<T> for BBTreeSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self {
            map: iter
                .into_iter()
                .map(|element| (element, SetValZST))
                .collect(),
        }
    }
}

impl<T: Ord, const N: usize> From<[T; N]> for BBTreeSet<T> {
    /// Converts a `[T; N]` into a `BBTreeSet<T>`.
    ///
    /// If the array contains any equal values, all but the last will be
    /// dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::unsync::BBTreeSet;
    ///
    /// let set1 = BBTreeSet::from([1, 2, 3, 4]);
    /// let set2: BBTreeSet<_> = [1, 2, 3, 4].into();
    /// assert_eq!(set1, set2);
    /// ```
    fn from(elements: [T; N]) -> Self {
        Self {
            map: BBTreeMap::from(elements.map(|element| (element, SetValZST))),
        }
    }
}

impl<T: Ord, S: InlineKeyStrategy<T>, A: Allocator + Clone> Extend<T> for BBTreeSet<T, S, A> {
    /// Inserts each element in turn, as `insert` does: of equal elements,
    /// the set keeps the first.
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.map
            .extend(iter.into_iter().map(|element| (element, SetValZST)));
    }
}

impl<'a, T: Ord + Copy + 'a, S: InlineKeyStrategy<T>, A: Allocator + Clone> Extend<&'a T>
    for BBTreeSet<T, S, A>
{
    /// Inserts a copy of each element in turn, as `insert` does.
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, iter: I) {
        self.extend(iter.into_iter().copied());
    }
}
