//! A store's records ([`Record`]) and the map's keys that point to them
//! ([`StoreKey`]).
//!
//! A record is a key and its value, in a heap page (`heap_page.rs`):
//!
//! ```text
//! | key_len: u32 | value_len: u32 | generation: u8 | key … | value … |
//! ```
//!
//! native-endian and unaligned, the generation that of the leaf whose heap
//! it was written into (`leaf_heap.rs`). The map orders its keys, and keeps
//! their inline keys, by their key bytes, as it does a byte string's.
//!
//! The map reads a key's bytes only while it holds the key's leaf latched:
//! its unlatched reads settle a key from the inline keys and hash tags they
//! stored, and never call a key's own code. So a record stays put while any
//! thread reads it, if the store moves, changes or frees it only while it
//! holds its leaf exclusively (`store.rs`).

use core::cmp::Ordering;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering as MemoryOrdering};

use bbtree::{InlineKey, InlineWord};

/// The size of a record's lengths.
const LEN_SIZE: usize = size_of::<u32>();

/// The length no record has, which [`QueryKey`] starts with.
const QUERY: u32 = u32::MAX;

/// A record in a heap page, by its address: valid while a key points to it,
/// read while its leaf is held (module documentation).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Record {
    at: NonNull<u8>,
}

impl Record {
    /// Where the key starts: after both lengths and the generation.
    const HEADER: usize = 2 * LEN_SIZE + size_of::<u8>();

    /// The size of the record of a key and a value of these lengths.
    pub(crate) const fn size_of(key_len: usize, value_len: usize) -> usize {
        Self::HEADER + key_len + value_len
    }

    /// Writes the record of `key` and `value`, written in a leaf of
    /// `generation`, at `to`.
    ///
    /// # Safety
    ///
    /// [`Self::size_of`] bytes from `to` are writable, apart from `key` and
    /// `value`, whose lengths are below `u32::MAX`.
    pub(crate) unsafe fn write(to: NonNull<u8>, key: &[u8], value: &[u8], generation: u8) -> Self {
        let [key_len, value_len] = [key, value]
            .map(|part| u32::try_from(part.len()).expect("a record's parts are under 4 GiB"));
        // SAFETY: the header, then the key and the value, lie within the
        // record's size from `to` (caller), apart from both.
        unsafe {
            to.cast::<[u8; LEN_SIZE]>()
                .write_unaligned(key_len.to_ne_bytes());
            (to.add(LEN_SIZE).cast::<[u8; LEN_SIZE]>()).write_unaligned(value_len.to_ne_bytes());
            to.add(2 * LEN_SIZE).write(generation);
            let key_at = to.add(Self::HEADER);
            key_at.copy_from_nonoverlapping(NonNull::from(key).cast(), key.len());
            (key_at.add(key.len()))
                .copy_from_nonoverlapping(NonNull::from(value).cast(), value.len());
        }
        Self { at: to }
    }

    /// The record at `at`.
    ///
    /// # Safety
    ///
    /// A record lies at `at`.
    pub(crate) unsafe fn at(at: NonNull<u8>) -> Self {
        Self { at }
    }

    pub(crate) fn addr(self) -> NonNull<u8> {
        self.at
    }

    /// The `u32` at `offset`.
    fn len_at(self, offset: usize) -> usize {
        // SAFETY: a record starts with both lengths (type invariant).
        unsafe { self.at.add(offset).cast::<u32>().read_unaligned() as usize }
    }

    fn key_len(self) -> usize {
        self.len_at(0)
    }

    fn value_len(self) -> usize {
        self.len_at(LEN_SIZE)
    }

    /// The generation of the leaf whose heap the record was written into.
    pub(crate) fn generation(self) -> u8 {
        // SAFETY: the generation follows the lengths (type invariant).
        unsafe { self.at.add(2 * LEN_SIZE).read() }
    }

    /// The record's size.
    pub(crate) fn size(self) -> usize {
        Self::size_of(self.key_len(), self.value_len())
    }

    /// The key's bytes, for as long as the caller reads the record.
    pub(crate) fn key<'a>(self) -> &'a [u8] {
        // SAFETY: the key follows the header (type invariant).
        unsafe { core::slice::from_raw_parts(self.at.add(Self::HEADER).as_ptr(), self.key_len()) }
    }

    /// The value's bytes, for as long as the caller reads the record.
    pub(crate) fn value<'a>(self) -> &'a [u8] {
        // SAFETY: the value follows the key (type invariant).
        unsafe {
            core::slice::from_raw_parts(
                self.at.add(Self::HEADER + self.key_len()).as_ptr(),
                self.value_len(),
            )
        }
    }

    /// Writes a copy of the record at `to`, with `generation`.
    ///
    /// # Safety
    ///
    /// As for [`Self::write`]: [`Self::size`] bytes from `to` are writable,
    /// apart from the record.
    pub(crate) unsafe fn copy_to(self, to: NonNull<u8>, generation: u8) -> Self {
        // SAFETY: as the caller promises.
        unsafe { Self::write(to, self.key(), self.value(), generation) }
    }

    /// Gives the record `value` in place of its own, which is no shorter:
    /// the bytes past the new value are garbage.
    ///
    /// # Safety
    ///
    /// The caller holds the record's leaf exclusively: no thread reads it.
    pub(crate) unsafe fn overwrite_value(self, value: &[u8]) {
        assert!(
            value.len() <= self.value_len(),
            "a value overwrites one no shorter"
        );
        let value_len = value.len() as u32;
        // SAFETY: the new value fits where the old one was, in the record,
        // which no thread reads meanwhile (caller).
        unsafe {
            (self.at.add(LEN_SIZE).cast::<[u8; LEN_SIZE]>())
                .write_unaligned(value_len.to_ne_bytes());
            (self.at.add(Self::HEADER + self.key_len()))
                .copy_from_nonoverlapping(NonNull::from(value).cast(), value.len());
        }
    }
}

/// A store's key: a pointer to its record, which the store swaps for
/// another of the same key, in place, while it holds the key's leaf
/// exclusively, as the value changes or the record moves.
pub(crate) struct StoreKey {
    record: AtomicPtr<u8>,
}

impl StoreKey {
    /// The record the key points to.
    pub(crate) fn record(&self) -> Record {
        let at =
            NonNull::new(self.record.load(MemoryOrdering::Relaxed)).expect("a key has a record");
        Record { at }
    }

    /// Points the key to `record`, of the same key, and returns the one it
    /// pointed to.
    ///
    /// # Safety
    ///
    /// The record stays where it is, unchanged, while the key points to it
    /// and a thread may read it (module documentation); the caller holds the
    /// key's leaf exclusively, so that no thread reads the key meanwhile.
    pub(crate) unsafe fn replace(&self, record: Record) -> Record {
        let old = self
            .record
            .swap(record.addr().as_ptr(), MemoryOrdering::Relaxed);
        Record {
            at: NonNull::new(old).expect("a key has a record"),
        }
    }

    /// The key's bytes.
    pub(crate) fn key(&self) -> &[u8] {
        let record = self.record();
        if record.len_at(0) == QUERY as usize {
            // SAFETY: a pointer whose first length is `QUERY` points to a
            // `QueryKey`, which outlives it (`QueryKey::store_key`).
            return unsafe { record.at.cast::<QueryKey<'_>>().as_ref() }.key;
        }
        record.key()
    }

    /// The value's bytes.
    ///
    /// # Panics
    ///
    /// If the key is a lookup's ([`QueryKey`]), which has no value.
    pub(crate) fn value(&self) -> &[u8] {
        let record = self.record();
        assert_ne!(
            record.len_at(0),
            QUERY as usize,
            "a lookup's key has no value"
        );
        record.value()
    }
}

impl PartialEq for StoreKey {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for StoreKey {}

impl PartialOrd for StoreKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StoreKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(other.key())
    }
}

/// Its key bytes', as a byte slice's: 8-byte windows, its length as its
/// hash tag, comparisons from where keys differ and against a prefix.
impl InlineKey for StoreKey {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.key().inline_key_at(offset)
    }

    fn hash_tag(&self) -> u8 {
        self.key().hash_tag()
    }

    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        self.key().cmp_from(other.key(), equal_len)
    }

    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        self.key().cmp_prefix(prefix)
    }
}

/// A lookup's key, or a new key's before its record is written, where the
/// caller has it: the map looks a [`StoreKey`] up with inline keys only
/// when the query is a `StoreKey` too, which [`Self::store_key`] makes. It
/// starts with a length no record has, which tells it from a record.
#[repr(C)]
pub(crate) struct QueryKey<'a> {
    /// [`QUERY`].
    marker: u32,
    key: &'a [u8],
}

impl<'a> QueryKey<'a> {
    pub(crate) fn new(key: &'a [u8]) -> Self {
        Self { marker: QUERY, key }
    }

    /// A store key that points to `self`, valid while `self` is borrowed:
    /// the caller drops it first, or points it to a record.
    pub(crate) fn store_key(&self) -> StoreKey {
        StoreKey {
            record: AtomicPtr::new(NonNull::from(self).cast::<u8>().as_ptr()),
        }
    }
}
