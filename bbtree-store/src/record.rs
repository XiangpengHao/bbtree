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
//! it was written into (`leaf_heap.rs`). A value too long for its record to
//! fit in a heap page's room goes into overflow pages (`overflow.rs`), the
//! record keeping a link to them after the key, then the value's start:
//!
//! ```text
//! | key_len: u32 | value_len: u32 | generation: u8 | key … | link | start … |
//! ```
//!
//! The pages are all full, the start what is left over, as SQLite does,
//! if it fits; else the record keeps none of the value, and the last page
//! holds what is left. Which of them a record is, and how much it keeps,
//! its lengths tell ([`Record::kept_len`]). The map orders its keys, and
//! keeps their inline keys, by their key bytes, as it does a byte string's.
//!
//! The map reads a key's bytes only while it holds the key's leaf latched:
//! its unlatched reads settle a key from the inline keys and hash tags they
//! stored, and never call a key's own code. So a record, and its overflow
//! pages, stay put while any thread reads them, if the store moves, changes
//! or frees them only while it holds their leaf exclusively (`store.rs`).

use core::cmp::Ordering;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering as MemoryOrdering};
use std::vec::Vec;

use bbtree::{InlineKey, InlineWord, NodeConfig};

use crate::heap_page::HeapPage;
use crate::overflow::Overflow;

/// The size of a record's lengths.
const LEN_SIZE: usize = size_of::<u32>();

/// The length no record has, which [`QueryKey`] starts with.
const QUERY: u32 = u32::MAX;

/// A record in a heap page of node config `C`, by its address: valid while
/// a key points to it, read while its leaf is held (module documentation).
pub(crate) struct Record<C> {
    at: NonNull<u8>,
    _config: PhantomData<fn() -> C>,
}

// By hand: derives would bound `C`, which the marker doesn't hold.

impl<C> Clone for Record<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for Record<C> {}

impl<C> Record<C> {
    /// Where the key starts: after both lengths and the generation.
    const HEADER: usize = 2 * LEN_SIZE + size_of::<u8>();

    /// The size of the link to a value's overflow pages.
    const LINK: usize = size_of::<*mut u8>();

    /// The record at `at`.
    ///
    /// # Safety
    ///
    /// A record lies at `at`.
    pub(crate) unsafe fn at(at: NonNull<u8>) -> Self {
        Self {
            at,
            _config: PhantomData,
        }
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

    /// The value's length, overflow pages and all.
    fn value_len(self) -> usize {
        self.len_at(LEN_SIZE)
    }

    /// The generation of the leaf whose heap the record was written into.
    pub(crate) fn generation(self) -> u8 {
        // SAFETY: the generation follows the lengths (type invariant).
        unsafe { self.at.add(2 * LEN_SIZE).read() }
    }

    /// The key's bytes, for as long as the caller reads the record.
    pub(crate) fn key<'a>(self) -> &'a [u8] {
        // SAFETY: the key follows the header (type invariant).
        unsafe { core::slice::from_raw_parts(self.at.add(Self::HEADER).as_ptr(), self.key_len()) }
    }
}

impl<C: NodeConfig> Record<C> {
    /// How many of the bytes of a value of `value_len` its record keeps,
    /// with a key of `key_len`: all of them if the record fits in a heap
    /// page's room; else, past the link to its overflow pages, what is left
    /// over once they are full, if it fits; else none.
    pub(crate) const fn kept_len(key_len: usize, value_len: usize) -> usize {
        let room = HeapPage::<C>::ROOM;
        if Self::HEADER + key_len + value_len <= room {
            return value_len;
        }
        let left_over = value_len % Overflow::<C>::ROOM;
        if Self::HEADER + key_len + Self::LINK + left_over <= room {
            left_over
        } else {
            0
        }
    }

    /// The size of the record of a key and a value of these lengths: at
    /// most a heap page's room, for a key no longer than a quarter page.
    pub(crate) const fn size_of(key_len: usize, value_len: usize) -> usize {
        let kept = Self::kept_len(key_len, value_len);
        let link = if kept < value_len { Self::LINK } else { 0 };
        Self::HEADER + key_len + link + kept
    }

    /// Writes the record of `key` and `value`, written in a leaf of
    /// `generation`, at `to`, `overflow` holding the bytes of the value it
    /// doesn't keep ([`Self::kept_len`]), if there are any.
    ///
    /// # Safety
    ///
    /// [`Self::size_of`] bytes from `to` are writable, apart from `key` and
    /// `value`.
    pub(crate) unsafe fn write(
        to: NonNull<u8>,
        key: &[u8],
        value: &[u8],
        overflow: Option<Overflow<C>>,
        generation: u8,
    ) -> Self {
        let kept = Self::kept_len(key.len(), value.len());
        assert_eq!(
            overflow.is_some(),
            kept < value.len(),
            "overflow pages hold what a record doesn't keep of its value"
        );
        let [key_len, value_len] = [key, value]
            .map(|part| u32::try_from(part.len()).expect("a record's parts are under 4 GiB"));
        // SAFETY: the header, then the key, the link and the bytes of the
        // value the record keeps, lie within the record's size from `to`
        // (caller), apart from the key and the value.
        unsafe {
            to.cast::<[u8; LEN_SIZE]>()
                .write_unaligned(key_len.to_ne_bytes());
            (to.add(LEN_SIZE).cast::<[u8; LEN_SIZE]>()).write_unaligned(value_len.to_ne_bytes());
            to.add(2 * LEN_SIZE).write(generation);
            let mut at = to.add(Self::HEADER);
            at.copy_from_nonoverlapping(NonNull::from(key).cast(), key.len());
            at = at.add(key.len());
            if let Some(overflow) = overflow {
                at.cast::<*mut u8>()
                    .write_unaligned(overflow.link().as_ptr());
                at = at.add(Self::LINK);
            }
            at.copy_from_nonoverlapping(NonNull::from(value).cast(), kept);
        }
        // SAFETY: the record was just written there.
        unsafe { Self::at(to) }
    }

    /// The record's size, in its page.
    pub(crate) fn size(self) -> usize {
        Self::size_of(self.key_len(), self.value_len())
    }

    /// The value, for as long as the caller reads the record.
    pub(crate) fn value<'a>(self) -> RecordValue<'a, C> {
        let (key_len, value_len) = (self.key_len(), self.value_len());
        let kept = Self::kept_len(key_len, value_len);
        // SAFETY: the key follows the header, then the value, or the link
        // and the value's start (type invariant).
        unsafe {
            let after_key = self.at.add(Self::HEADER + key_len);
            if kept == value_len {
                let start = core::slice::from_raw_parts(after_key.as_ptr(), value_len);
                return RecordValue { start, rest: None };
            }
            let link = after_key.cast::<*mut u8>().read_unaligned();
            let start = core::slice::from_raw_parts(after_key.add(Self::LINK).as_ptr(), kept);
            let overflow = Overflow::at(NonNull::new(link).expect("a record links its pages"));
            RecordValue {
                start,
                rest: Some((overflow, value_len - kept)),
            }
        }
    }

    /// The value's overflow pages; `None` if the record keeps it whole.
    pub(crate) fn overflow(self) -> Option<Overflow<C>> {
        self.value().rest.map(|(overflow, _)| overflow)
    }

    /// Writes a copy of the record at `to`, with `generation`: the copy
    /// links the record's overflow pages, which are its then.
    ///
    /// # Safety
    ///
    /// [`Self::size`] bytes from `to` are writable, apart from the record.
    pub(crate) unsafe fn copy_to(self, to: NonNull<u8>, generation: u8) -> Self {
        // SAFETY: as the caller promises; the generation lies in the copy.
        unsafe {
            to.copy_from_nonoverlapping(self.at, self.size());
            to.add(2 * LEN_SIZE).write(generation);
            Self::at(to)
        }
    }

    /// Gives the record `value` in place of its own, which it keeps whole,
    /// and which is no shorter: the bytes past the new value are garbage.
    ///
    /// # Safety
    ///
    /// The caller holds the record's leaf exclusively: no thread reads it.
    pub(crate) unsafe fn overwrite_value(self, value: &[u8]) {
        let old = self.value();
        assert!(
            old.rest.is_none() && value.len() <= old.start.len(),
            "a value overwrites one kept whole, no shorter"
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

/// A record's value: the bytes its record keeps, then its overflow pages'.
pub(crate) struct RecordValue<'a, C> {
    /// The value's start, in the record: all of it, without overflow
    /// pages.
    start: &'a [u8],
    /// The overflow pages, and how many of the value's bytes they hold.
    rest: Option<(Overflow<C>, usize)>,
}

impl<'a, C: NodeConfig> RecordValue<'a, C> {
    pub(crate) fn len(&self) -> usize {
        self.start.len() + self.rest.map_or(0, |(_, len)| len)
    }

    /// Appends the value to `buf`.
    pub(crate) fn append_to(&self, buf: &mut Vec<u8>) {
        let Some((overflow, len)) = self.rest else {
            buf.extend_from_slice(self.start);
            return;
        };
        buf.reserve(self.start.len() + len);
        buf.extend_from_slice(self.start);
        overflow.append_to(len, buf);
    }

    /// The value's bytes: the record's, or, with overflow pages, those of
    /// `gathered`, cleared and filled with them.
    pub(crate) fn gather<'b>(&self, gathered: &'b mut Vec<u8>) -> &'b [u8]
    where
        'a: 'b,
    {
        if self.rest.is_none() {
            return self.start;
        }
        gathered.clear();
        self.append_to(gathered);
        gathered
    }
}

/// A store's key: a pointer to its record, which the store swaps for
/// another of the same key, in place, while it holds the key's leaf
/// exclusively, as the value changes or the record moves.
///
/// Not generic over the store's node config: only reading its record's
/// value needs it ([`Self::value`]). A generic key made a store's get 45 ns
/// slower at 10M keys, for eight instructions more (2026-10-21).
pub(crate) struct StoreKey {
    record: AtomicPtr<u8>,
}

impl StoreKey {
    /// The record the key points to, in a store of node config `C`.
    pub(crate) fn record<C>(&self) -> Record<C> {
        let at =
            NonNull::new(self.record.load(MemoryOrdering::Relaxed)).expect("a key has a record");
        // SAFETY: a key points to its record, or to a query, whose callers
        // read no more than the first length (`Self::key`).
        unsafe { Record::at(at) }
    }

    /// Points the key to `record`, of the same key, and returns the one it
    /// pointed to.
    ///
    /// # Safety
    ///
    /// The record stays where it is, unchanged, while the key points to it
    /// and a thread may read it (module documentation); the caller holds the
    /// key's leaf exclusively, so that no thread reads the key meanwhile.
    pub(crate) unsafe fn replace<C>(&self, record: Record<C>) -> Record<C> {
        let old = self
            .record
            .swap(record.addr().as_ptr(), MemoryOrdering::Relaxed);
        // SAFETY: the key pointed to its record.
        unsafe { Record::at(NonNull::new(old).expect("a key has a record")) }
    }

    /// The key's bytes.
    pub(crate) fn key(&self) -> &[u8] {
        // A record's key reads the same in every node config.
        let record = self.record::<()>();
        if record.len_at(0) == QUERY as usize {
            // SAFETY: a pointer whose first length is `QUERY` points to a
            // `QueryKey`, which outlives it (`QueryKey::store_key`).
            return unsafe { record.at.cast::<QueryKey<'_>>().as_ref() }.key;
        }
        record.key()
    }

    /// The value, in a store of node config `C`.
    ///
    /// # Panics
    ///
    /// If the key is a lookup's ([`QueryKey`]), which has no value.
    pub(crate) fn value<C: NodeConfig>(&self) -> RecordValue<'_, C> {
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
