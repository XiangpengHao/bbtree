//! Stores (`design/16-bbtree-store.md`): ordered maps of byte strings to
//! byte strings that threads share, whose every byte lies in pages from
//! their allocator. A store is a [`BBTreeMap`](crate::BBTreeMap)'s tree
//! whose leaves keep their entries' keys and values in heap pages of their
//! own: an allocator that refuses pages past a budget bounds all the memory
//! a store takes, its keys' and values' bytes with its nodes.

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Bound, ControlFlow, RangeBounds};

use crate::allocator::{AllocError, Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::record_ptr::RecordParts;
use crate::record_ptr::RecordPtr;
use crate::tree::latched::{CopiedRange, LatchedTree, UpdateOrInsert};
use crate::tree::node::store::InStore;

/// An ordered map of byte strings to byte strings that threads share,
/// through `&self`, whose every byte lies in pages from its allocator, `A`.
///
/// Each leaf of its tree keeps its entries' keys and values, its records,
/// in a chain of heap pages of its own, sized by the node config's
/// `HEAP_PAGE_SIZE`, the leaves' page size by default; the leaf's entries
/// point to them. A key is at most a quarter of a heap page
/// ([`Self::MAX_KEY_LEN`]), and a key and its value together fit in one
/// ([`Self::MAX_RECORD_SIZE`]).
///
/// Nothing it hands out outlives a call: a lookup copies the value out
/// ([`Self::get_into`]), as entries move within their leaf's heap.
///
/// `S` is the strategy, as for a [`BBTreeMap`](crate::BBTreeMap):
/// [`Auto`] by default, or [`Configured`](crate::Configured) for other page
/// sizes.
pub struct BBTreeStore<S: InlineKeyStrategy<RecordPtr> = Auto, A: Allocator = Global> {
    tree: LatchedTree<RecordPtr, (), InStore<S>>,
    /// Where the tree's pages, nodes and heap pages, come from and go back
    /// to.
    alloc: A,
}

impl BBTreeStore {
    /// Makes a new, empty store, with the default strategy, [`Auto`], and
    /// the global allocator.
    pub fn new() -> Self {
        Self::with_strategy_in(Auto, Global)
    }
}

impl Default for BBTreeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: InlineKeyStrategy<RecordPtr>, A: Allocator> BBTreeStore<S, A> {
    /// The longest key: a quarter of a heap page.
    pub const MAX_KEY_LEN: usize = InStore::<S>::MAX_KEY_LEN;

    /// The most bytes a key and its value take together, with a length of
    /// four bytes for each: what a heap page holds after its header.
    pub const MAX_RECORD_SIZE: usize = InStore::<S>::MAX_RECORD_SIZE;

    /// Makes a new, empty store, with strategy `strategy`, whose pages come
    /// from `alloc`.
    pub fn with_strategy_in(strategy: S, alloc: A) -> Self {
        let _ = strategy;
        Self {
            tree: LatchedTree::new(),
            alloc,
        }
    }

    /// The allocator the store's pages come from: a budget's, to read how
    /// much is in use.
    pub fn allocator(&self) -> &A {
        &self.alloc
    }

    /// Appends the value of `key` to `buf`, as `Read::read_to_end` does, and
    /// returns its length; `None` without one, `buf` unchanged.
    ///
    /// ```
    /// use bbtree::store::BBTreeStore;
    /// let store = BBTreeStore::new();
    /// store.insert(b"apple", b"red")?;
    /// let mut buf = Vec::new();
    /// assert_eq!(store.get_into(b"apple", &mut buf), Some(3));
    /// assert_eq!(buf, b"red");
    /// assert_eq!(store.get_into(b"pear", &mut buf), None);
    /// # Ok::<(), bbtree::store::InsertError>(())
    /// ```
    #[inline]
    pub fn get_into(&self, key: &[u8], buf: &mut Vec<u8>) -> Option<usize> {
        self.tree.read(key, |record, ()| {
            let value = record.value();
            buf.extend_from_slice(value);
            value.len()
        })
    }

    /// A copy of the value of `key`; `None` without one.
    #[inline]
    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        self.get_into(key, &mut buf).map(|_| buf)
    }

    /// Whether the store holds `key`.
    #[inline]
    pub fn contains_key(&self, key: &[u8]) -> bool {
        self.tree.contains_key(key)
    }

    /// Inserts `key` with `value`, or gives the key it holds already `value`
    /// in place of its own; returns whether it replaced a value. Both are
    /// copied in.
    ///
    /// # Errors
    ///
    /// If the key is longer than [`Self::MAX_KEY_LEN`], or the key and
    /// value take more than [`Self::MAX_RECORD_SIZE`]; or if the allocator
    /// has no page for the insert, a node or a heap page. The store is
    /// unchanged then.
    #[inline]
    pub fn insert(&self, key: &[u8], value: &[u8]) -> Result<bool, InsertError> {
        if key.len() > Self::MAX_KEY_LEN {
            return Err(InsertError::KeyTooLong);
        }
        if RecordPtr::size_of(key.len(), value.len()) > Self::MAX_RECORD_SIZE {
            return Err(InsertError::RecordTooLarge);
        }
        // The insert's key points to `key` and `value` where they are, and
        // the leaf writes them into its heap as the entry's record.
        let parts = RecordParts::new(key, value);
        let replaced = self
            .tree
            .insert(
                parts.record_ptr(),
                (),
                UpdateOrInsert(|_: &RecordPtr, _: &mut (), (): ()| ()),
                &self.alloc,
            )
            .map_err(|_| InsertError::Alloc(AllocError))?;
        Ok(replaced.is_some())
    }

    /// Runs `f` on each entry whose key lies in `range`, in key order, with
    /// its key and value while its leaf is latched shared; returns what `f`
    /// breaks with, or `None` once past the range. The range's bounds are
    /// byte strings of any kind. As a map's scan
    /// ([`BBTreeMap::scan`](crate::BBTreeMap::scan)), it is not a snapshot.
    ///
    /// ```
    /// use core::ops::ControlFlow;
    /// use bbtree::store::BBTreeStore;
    /// let store = BBTreeStore::new();
    /// for (key, value) in [("apple", "red"), ("banana", "yellow"), ("cherry", "red")] {
    ///     store.insert(key.as_bytes(), value.as_bytes())?;
    /// }
    /// let mut red = Vec::new();
    /// store.scan(b"a".as_slice()..b"c".as_slice(), |key, value| {
    ///     if value == b"red" {
    ///         red.push(key.to_vec());
    ///     }
    ///     ControlFlow::<()>::Continue(())
    /// });
    /// assert_eq!(red, [b"apple".to_vec()]);
    /// # Ok::<(), bbtree::store::InsertError>(())
    /// ```
    ///
    /// # Panics
    ///
    /// As std's `BTreeMap::range`: if `range` starts after it ends, or
    /// starts and ends at the same key, both excluded.
    #[inline]
    pub fn scan<Q, R>(
        &self,
        range: impl RangeBounds<Q>,
        mut f: impl FnMut(&[u8], &[u8]) -> ControlFlow<R>,
    ) -> Option<R>
    where
        Q: AsRef<[u8]> + ?Sized,
    {
        let range = ByteRange::new(range);
        let (start, end) = LatchedTree::<RecordPtr, (), InStore<S>>::checked_bounds(&range);
        self.tree.scan(start, end, None, |record, (), _| {
            f(record.bytes(), record.value())
        })
    }

    /// [`Self::scan`] in reverse key order: from the end of `range` to its
    /// start.
    ///
    /// # Panics
    ///
    /// As [`Self::scan`].
    #[inline]
    pub fn scan_rev<Q, R>(
        &self,
        range: impl RangeBounds<Q>,
        mut f: impl FnMut(&[u8], &[u8]) -> ControlFlow<R>,
    ) -> Option<R>
    where
        Q: AsRef<[u8]> + ?Sized,
    {
        let range = ByteRange::new(range);
        let (start, end) = LatchedTree::<RecordPtr, (), InStore<S>>::checked_bounds(&range);
        self.tree.scan_rev(start, end, None, |record, (), _| {
            f(record.bytes(), record.value())
        })
    }

    /// An iterator over copies of the entries whose keys lie in `range`, each
    /// key and value a `Vec<u8>`: in key order, and in reverse from its back
    /// ([`DoubleEndedIterator`]). It holds no latch between calls, as a
    /// map's [`range_cloned`](crate::BBTreeMap::range_cloned), and is not a
    /// snapshot either.
    ///
    /// # Panics
    ///
    /// As [`Self::scan`].
    pub fn range_cloned<Q, R>(&self, range: R) -> RangeCloned<'_, S, Q, R>
    where
        Q: AsRef<[u8]> + ?Sized,
        R: RangeBounds<Q>,
    {
        let range = ByteRange::new(range);
        LatchedTree::<RecordPtr, (), InStore<S>>::checked_bounds(&range);
        RangeCloned {
            entries: CopiedRange::new(&self.tree, range),
        }
    }

    /// Removes `key`; returns whether the store held it. Its record's bytes
    /// count as garbage in its leaf's heap until the leaf compacts it.
    #[inline]
    pub fn remove(&self, key: &[u8]) -> bool {
        self.tree
            .remove_entry(key, |_, ()| true, &self.alloc)
            .is_some()
    }

    /// Panics unless the store's tree is well formed, with `length` entries,
    /// and each leaf's records lie in its own heap pages, counted as its
    /// heap says.
    #[cfg(test)]
    pub(crate) fn check_invariants(&mut self, length: usize) {
        self.tree.check_invariants(length);
    }
}

impl<S: InlineKeyStrategy<RecordPtr>, A: Allocator> Drop for BBTreeStore<S, A> {
    fn drop(&mut self) {
        self.tree.destroy(&self.alloc);
    }
}

impl<S: InlineKeyStrategy<RecordPtr>, A: Allocator> fmt::Debug for BBTreeStore<S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BBTreeStore").finish_non_exhaustive()
    }
}

/// A range of byte strings of any kind, `Q`, as a range of `[u8]`: what a
/// store's scans and copies take.
struct ByteRange<Q: ?Sized, R> {
    range: R,
    _query: PhantomData<fn(&Q)>,
}

impl<Q: AsRef<[u8]> + ?Sized, R: RangeBounds<Q>> ByteRange<Q, R> {
    fn new(range: R) -> Self {
        Self {
            range,
            _query: PhantomData,
        }
    }
}

impl<Q: AsRef<[u8]> + ?Sized, R: RangeBounds<Q>> RangeBounds<[u8]> for ByteRange<Q, R> {
    fn start_bound(&self) -> Bound<&[u8]> {
        self.range.start_bound().map(AsRef::as_ref)
    }

    fn end_bound(&self) -> Bound<&[u8]> {
        self.range.end_bound().map(AsRef::as_ref)
    }
}

/// The copies a store's [`RangeCloned`] hands out: each entry's key and
/// value bytes, from a range of byte strings `Q`.
type RecordCopies<'s, S, Q, R> =
    CopiedRange<'s, RecordPtr, (), InStore<S>, Vec<u8>, Vec<u8>, [u8], ByteRange<Q, R>>;

/// An iterator over copies of the entries of a range of a [`BBTreeStore`]:
/// in key order from the front, in reverse from the back. See
/// [`BBTreeStore::range_cloned`].
pub struct RangeCloned<'s, S: InlineKeyStrategy<RecordPtr>, Q: ?Sized, R> {
    entries: RecordCopies<'s, S, Q, R>,
}

impl<S: InlineKeyStrategy<RecordPtr>, Q: ?Sized, R> RangeCloned<'_, S, Q, R> {
    /// A store's entry, copied out of its leaf as its key and value bytes.
    fn copy_record(record: &RecordPtr, (): &()) -> (Vec<u8>, Vec<u8>) {
        (record.bytes().to_vec(), record.value().to_vec())
    }
}

impl<S, Q, R> Iterator for RangeCloned<'_, S, Q, R>
where
    S: InlineKeyStrategy<RecordPtr>,
    Q: AsRef<[u8]> + ?Sized,
    R: RangeBounds<Q>,
{
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.entries.advance(true, Self::copy_record)
    }
}

impl<S, Q, R> DoubleEndedIterator for RangeCloned<'_, S, Q, R>
where
    S: InlineKeyStrategy<RecordPtr>,
    Q: AsRef<[u8]> + ?Sized,
    R: RangeBounds<Q>,
{
    fn next_back(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.entries.advance(false, Self::copy_record)
    }
}

impl<S: InlineKeyStrategy<RecordPtr>, Q: ?Sized, R> fmt::Debug for RangeCloned<'_, S, Q, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RangeCloned").finish_non_exhaustive()
    }
}

/// Why an insert into a [`BBTreeStore`] failed; the store is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertError {
    /// The key is longer than [`BBTreeStore::MAX_KEY_LEN`].
    KeyTooLong,
    /// The key and value take more than [`BBTreeStore::MAX_RECORD_SIZE`].
    RecordTooLarge,
    /// The allocator had no page to give.
    Alloc(AllocError),
}

impl fmt::Display for InsertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyTooLong => f.write_str("the key is longer than a store's keys may be"),
            Self::RecordTooLarge => f.write_str("the key and value don't fit in a heap page"),
            Self::Alloc(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl core::error::Error for InsertError {}

#[cfg(all(test, shuttle))]
mod shuttle_tests;
#[cfg(test)]
mod tests;
