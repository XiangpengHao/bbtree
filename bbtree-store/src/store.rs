//! [`BBTreeStore`]: bbtree's shared map, whose keys point to their records,
//! and whose leaves keep the records in chains of heap pages of their own
//! (`leaf_heap.rs`), all from one allocator.
//!
//! - **An insert** finds the key's leaf, held exclusively: a new key's
//!   record goes into the leaf's heap, and the key goes in pointing to it;
//!   a held key's record takes the new value in place if it fits, or the
//!   key points to a new record, and the old one dies.
//! - **A removal** takes the key out, while the map holds the leaf; its
//!   record dies once the map is done with the key, after the removal.
//! - **A lookup or a scan** reads records inside the map's closures, while
//!   it holds their leaves latched shared. As only a thread holding a
//!   record's leaf exclusively moves, changes or frees it, no record goes
//!   while a thread reads it.

use core::fmt;
use core::ops::{Bound, ControlFlow, RangeBounds};
use std::vec::Vec;

use bbtree::{AllocError, Allocator, BBTreeMap, Global, NodeConfig};

use crate::heap_page::HeapPage;
use crate::leaf_heap::{RecordHeap, StoreStrategy};
use crate::range::RangeCopies;
use crate::record::{QueryKey, Record, StoreKey};

/// The node config a store takes by default: bbtree's defaults, whose heap
/// pages are its leaves' size, 1 KiB.
pub struct DefaultConfig;

impl NodeConfig for DefaultConfig {}

/// An ordered map of byte strings to byte strings that threads share,
/// through `&self`, whose every byte lies in pages from its allocator, `A`:
/// the map's nodes, and its keys and values, each leaf's in heap pages of
/// its own ([`Allocator::allocate_heap`]) of `C::HEAP_PAGE_SIZE` bytes.
///
/// Nothing it hands out outlives a call: a lookup copies the value out
/// ([`Self::get_into`]), and a scan's closure sees each entry while its
/// leaf is latched.
pub struct BBTreeStore<A: Allocator + Clone = Global, C: NodeConfig = DefaultConfig> {
    map: BBTreeMap<StoreKey, (), StoreStrategy<C>, A>,
    heap: RecordHeap<A, C>,
}

impl BBTreeStore {
    /// Makes a new, empty store on the global allocator, with bbtree's
    /// default page sizes.
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl Default for BBTreeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Allocator + Clone> BBTreeStore<A> {
    /// Makes a new, empty store whose pages come from `alloc`, with bbtree's
    /// default page sizes.
    pub fn new_in(alloc: A) -> Self {
        Self::with_config_in(DefaultConfig, alloc)
    }
}

impl<A: Allocator + Clone, C: NodeConfig> BBTreeStore<A, C> {
    /// Makes a new, empty store whose pages come from `alloc`, with the page
    /// sizes of `config`'s type.
    pub fn with_config_in(config: C, alloc: A) -> Self {
        let _ = config;
        Self {
            map: BBTreeMap::new_in(alloc.clone()),
            heap: RecordHeap::new(alloc),
        }
    }

    /// The longest key: a quarter of a heap page.
    pub fn max_key_len(&self) -> usize {
        HeapPage::<C>::SIZE / 4
    }

    /// The most bytes a key and its value take together, with nine bytes
    /// of lengths and bookkeeping: a heap page's room.
    pub fn max_record_size(&self) -> usize {
        HeapPage::<C>::ROOM
    }

    /// Appends the value of `key` to `buf`, as `Read::read_to_end` does, and
    /// returns its length; `None` without one, `buf` unchanged.
    pub fn get_into(&self, key: &[u8], buf: &mut Vec<u8>) -> Option<usize> {
        let query = QueryKey::new(key);
        self.map.read(&query.store_key(), |stored, ()| {
            let value = stored.value();
            buf.extend_from_slice(value);
            value.len()
        })
    }

    /// A copy of the value of `key`; `None` without one.
    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        self.get_into(key, &mut buf).map(|_| buf)
    }

    /// Whether the store holds `key`.
    pub fn contains_key(&self, key: &[u8]) -> bool {
        let query = QueryKey::new(key);
        self.map.contains_key(&query.store_key())
    }

    /// Inserts `key` with `value`, or gives the key it holds already `value`
    /// in place of its own; returns whether it replaced a value. Both are
    /// copied into the store's heap.
    ///
    /// # Errors
    ///
    /// If the key is longer than [`Self::max_key_len`], or the key and value
    /// take more than [`Self::max_record_size`]; or if the allocator has no
    /// page for the insert, a node or a heap page. The store holds what it
    /// held then.
    pub fn insert(&self, key: &[u8], value: &[u8]) -> Result<bool, InsertError> {
        if key.len() > self.max_key_len() {
            return Err(InsertError::KeyTooLong);
        }
        if Record::size_of(key.len(), value.len()) > self.max_record_size() {
            return Err(InsertError::RecordTooLarge);
        }
        let query = QueryKey::new(key);
        let no_page = |AllocError| HeapPage::<C>::layout();
        let inserted = self.map.update_or_insert_with_leaf(
            query.store_key(),
            (),
            |mut leaf, stored, ()| {
                (self.heap)
                    .overwrite(&mut leaf, stored, key, value)
                    .map_err(no_page)?;
                Ok(true)
            },
            |mut leaf, new, ()| {
                let record = self.heap.write(&mut leaf, key, value).map_err(no_page)?;
                // SAFETY: the record has the new key's bytes, and the key
                // goes into the leaf, which the map holds exclusively, next.
                unsafe { new.replace(record) };
                Ok(false)
            },
        );
        match inserted {
            Ok(replaced) => Ok(replaced),
            Err(_) => Err(InsertError::Alloc(AllocError)),
        }
    }

    /// Removes `key`; returns whether the store held it.
    pub fn remove(&self, key: &[u8]) -> bool {
        let query = QueryKey::new(key);
        let removed = self
            .map
            .remove_with_leaf(&query.store_key(), |mut leaf, stored, ()| {
                self.heap.before_removal(&mut leaf, stored);
            });
        let Some((stored, ())) = removed else {
            return false;
        };
        // The map reads the key until the removal returns: its record dies
        // only now.
        self.heap.kill_removed(stored.record());
        true
    }

    /// Runs `f` on each entry whose key lies in `range`, in key order, with
    /// its key and value while its leaf is latched shared; returns what `f`
    /// breaks with, or `None` once past the range. As a map's
    /// [`scan`](BBTreeMap::scan), it is not a snapshot.
    ///
    /// # Panics
    ///
    /// As std's `BTreeMap::range`: if `range` starts after it ends, or
    /// starts and ends at the same key, both excluded.
    pub fn scan<Q, R>(
        &self,
        range: impl RangeBounds<Q>,
        mut f: impl FnMut(&[u8], &[u8]) -> ControlFlow<R>,
    ) -> Option<R>
    where
        Q: AsRef<[u8]> + ?Sized,
    {
        let (start, end) = QueryRange::new(&range);
        let (start, end) = (start.keys(), end.keys());
        self.map.scan((start.as_ref(), end.as_ref()), |stored, ()| {
            f(stored.key(), stored.value())
        })
    }

    /// [`Self::scan`] in reverse key order: from the end of `range` to its
    /// start.
    ///
    /// # Panics
    ///
    /// As [`Self::scan`].
    pub fn scan_rev<Q, R>(
        &self,
        range: impl RangeBounds<Q>,
        mut f: impl FnMut(&[u8], &[u8]) -> ControlFlow<R>,
    ) -> Option<R>
    where
        Q: AsRef<[u8]> + ?Sized,
    {
        let (start, end) = QueryRange::new(&range);
        let (start, end) = (start.keys(), end.keys());
        self.map
            .scan_rev((start.as_ref(), end.as_ref()), |stored, ()| {
                f(stored.key(), stored.value())
            })
    }

    /// Copies of the entries whose keys lie in `range`, key and value, in
    /// key order from the front, in reverse from the back, a few at a time
    /// ([`RangeCopies`]). As [`Self::scan`], not a snapshot.
    ///
    /// # Panics
    ///
    /// As [`Self::scan`].
    pub fn range<Q, R>(&self, range: R) -> RangeCopies<'_, A, C>
    where
        Q: AsRef<[u8]> + ?Sized,
        R: RangeBounds<Q>,
    {
        // Checks the range as a scan does, copying nothing.
        let bounds = (range.start_bound(), range.end_bound());
        self.scan::<Q, ()>(bounds, |_, _| ControlFlow::Break(()));
        let owned = |bound: Bound<&Q>| bound.map(|key| key.as_ref().to_vec());
        RangeCopies::new(self, owned(range.start_bound()), owned(range.end_bound()))
    }
}

impl<A: Allocator + Clone, C: NodeConfig> Drop for BBTreeStore<A, C> {
    fn drop(&mut self) {
        // The chains go with the map's leaves. Orphans go as their records
        // die, which only killing them all does.
        if self.heap.has_orphans() {
            self.map.scan::<StoreKey, ()>(.., |stored, ()| {
                self.heap.kill_at_drop(stored.record());
                ControlFlow::Continue(())
            });
        }
    }
}

impl<A: Allocator + Clone, C: NodeConfig> fmt::Debug for BBTreeStore<A, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BBTreeStore").finish_non_exhaustive()
    }
}

/// A range's bound as a lookup key, for the map's scans.
struct QueryRange<'a> {
    bound: Bound<QueryKey<'a>>,
}

impl<'a> QueryRange<'a> {
    /// The start and end bounds of `range`.
    fn new<Q: AsRef<[u8]> + ?Sized + 'a>(range: &'a impl RangeBounds<Q>) -> (Self, Self) {
        let bound = |bound: Bound<&'a Q>| Self {
            bound: bound.map(|key| QueryKey::new(key.as_ref())),
        };
        (bound(range.start_bound()), bound(range.end_bound()))
    }

    /// The bound as a store key, valid while `self` is borrowed.
    fn keys(&self) -> Bound<StoreKey> {
        self.bound.as_ref().map(QueryKey::store_key)
    }
}

/// Why an insert into a [`BBTreeStore`] failed; the store holds what it
/// held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertError {
    /// The key is longer than [`BBTreeStore::max_key_len`].
    KeyTooLong,
    /// The key and value take more than [`BBTreeStore::max_record_size`].
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
