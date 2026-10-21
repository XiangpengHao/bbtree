//! A pointer to a record: a byte string's length, then its bytes, which lie
//! elsewhere. Two owners keep one per byte string:
//!
//! - a file map's writer, to the bytes in its arena while it builds
//!   (`file/var_bytes.rs`);
//! - a store, in each leaf entry, to the entry's record in the leaf's heap
//!   pages, whose key comes first, length first, and its value after it
//!   (`design/16-bbtree-store.md`, `tree/node/store.rs`). An insert's key
//!   points to the caller's key and value instead ([`RecordParts`]), until
//!   the leaf writes the record into its heap.
//!
//! It compares, and has inline keys and a hash tag, as its bytes do, those
//! of a file map's span (`VarBytes`): [`Auto`](crate::Auto) takes it for a
//! byte string (`tree/node/type_identity.rs`), so its tree's pages have the
//! layout a span's do, and a `&[u8]` looks it up.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::mem::size_of;
use core::ptr::NonNull;

use crate::inline_key::{InlineKey, InlineWidth, InlineWord};

/// A pointer to a record: a `u32` length, native-endian and unaligned, then
/// that many bytes; or, in a store, to [`RecordParts`]. Public only to the
/// crate's sealed traits: nothing outside the crate names it.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct RecordPtr {
    record: NonNull<u8>,
    /// Zeros, written, where a pointer is narrower than 8 bytes.
    zeros: [u8; 8 - size_of::<NonNull<u8>>()],
}

// SAFETY: the bytes it points to belong to its owner, which changes them
// only while it has them alone: a file map's writer never does, and a
// store changes a record only while it holds the record's leaf exclusively.
unsafe impl Send for RecordPtr {}

// SAFETY: as for `Send`: shared, the bytes are only read.
unsafe impl Sync for RecordPtr {}

impl RecordPtr {
    /// The size of a record's length.
    pub(crate) const LEN_SIZE: usize = size_of::<u32>();

    /// The length no record has, which [`RecordParts`] starts with.
    const PARTS: u32 = u32::MAX;

    /// A pointer to the record at `record`.
    ///
    /// # Safety
    ///
    /// A length other than `u32::MAX` and that many bytes lie at `record`,
    /// initialized, and stay so, unchanged, while the pointer is read: as
    /// its owner keeps them.
    pub(crate) unsafe fn new(record: NonNull<u8>) -> Self {
        Self {
            record,
            zeros: [0; 8 - size_of::<NonNull<u8>>()],
        }
    }

    /// The length at the record: its key's, or [`Self::PARTS`].
    #[inline]
    fn len_field(&self) -> u32 {
        // SAFETY: a length lies at the record, or `RecordParts` starts with
        // one (`new`, `RecordParts::record_ptr`).
        unsafe { self.record.cast::<u32>().read_unaligned() }
    }

    /// The bytes it points to: what `Auto`'s inline keys and comparisons
    /// read, as a span's.
    #[inline]
    pub(crate) fn bytes(&self) -> &[u8] {
        #[cfg(feature = "store")]
        if let Some(parts) = self.parts() {
            return parts.key;
        }
        // SAFETY: the record is a length and that many bytes after it, which
        // stay unchanged while they are read (`new`'s contract).
        unsafe {
            core::slice::from_raw_parts(
                self.record.as_ptr().add(Self::LEN_SIZE),
                self.len_field() as usize,
            )
        }
    }
}

/// A store's records: its key, length first, then its value, length first
/// (`tree/node/store.rs`); or the caller's key and value, which an insert
/// carries until the leaf writes the record ([`RecordParts`]).
#[cfg(feature = "store")]
impl RecordPtr {
    /// The caller's key and value, if it points to them rather than to a
    /// record.
    #[inline]
    fn parts(&self) -> Option<&RecordParts<'_>> {
        // SAFETY: a pointer whose length is `PARTS` points to a
        // `RecordParts`, which outlives it (`RecordParts::record_ptr`).
        (self.len_field() == Self::PARTS).then(|| unsafe { self.record.cast().as_ref() })
    }

    /// Where the record starts, in its heap page.
    ///
    /// # Panics
    ///
    /// If it points to the caller's key and value rather than a record.
    pub(crate) fn record(self) -> NonNull<u8> {
        assert!(self.parts().is_none(), "a record in a heap page");
        self.record
    }

    /// The value's bytes, after the key's.
    pub(crate) fn value(&self) -> &[u8] {
        if let Some(parts) = self.parts() {
            return parts.value;
        }
        // SAFETY: a store's record holds the value's length after the key,
        // then that many bytes, unchanged while they are read (`new`).
        unsafe {
            let at = self.record.add(Self::LEN_SIZE + self.bytes().len());
            let len = at.cast::<u32>().read_unaligned() as usize;
            core::slice::from_raw_parts(at.as_ptr().add(Self::LEN_SIZE), len)
        }
    }

    /// The size of a record of a key and a value of these lengths: both
    /// lengths, the key and the value.
    pub(crate) const fn size_of(key_len: usize, value_len: usize) -> usize {
        2 * Self::LEN_SIZE + key_len + value_len
    }

    /// The size of its record.
    pub(crate) fn size(&self) -> usize {
        Self::size_of(self.bytes().len(), self.value().len())
    }

    /// Writes `value`, no longer than its value, in place of its value: the
    /// record keeps its place, and the bytes past the new value are garbage.
    ///
    /// # Safety
    ///
    /// It points to a record whose owner has it alone, apart from `value`.
    pub(crate) unsafe fn write_value(&self, value: &[u8]) {
        debug_assert!(value.len() <= self.value().len());
        let len = u32::try_from(value.len()).expect("a record's parts fit in a heap page");
        // SAFETY: the value's length and bytes lie past the key in the
        // record, with room for `value`'s (caller).
        unsafe {
            let at = self.record().add(Self::LEN_SIZE + self.bytes().len());
            at.cast::<[u8; 4]>().write_unaligned(len.to_ne_bytes());
            at.add(Self::LEN_SIZE)
                .copy_from_nonoverlapping(NonNull::from(value).cast(), value.len());
        }
    }

    /// Writes its record at `to`: the key, length first, then the value,
    /// length first.
    ///
    /// # Safety
    ///
    /// [`Self::size`] bytes from `to` are writable, and apart from the
    /// record's own.
    pub(crate) unsafe fn write_record(&self, to: NonNull<u8>) {
        let mut at = to;
        for part in [self.bytes(), self.value()] {
            let len = u32::try_from(part.len()).expect("a record's parts fit in a heap page");
            // SAFETY: the length and the part lie within the record's size
            // from `to` (caller), apart from the part's own bytes.
            unsafe {
                at.cast::<[u8; 4]>().write_unaligned(len.to_ne_bytes());
                at = at.add(Self::LEN_SIZE);
                at.copy_from_nonoverlapping(NonNull::from(part).cast(), part.len());
                at = at.add(part.len());
            }
        }
    }
}

/// An insert's key and value, where the caller has them, for a store's tree
/// to search with and the leaf to write as a record
/// ([`RecordPtr::write_record`]). It starts with a length no record has, so
/// that a record pointer to it tells it from a record.
#[cfg(feature = "store")]
#[repr(C)]
pub(crate) struct RecordParts<'a> {
    /// [`RecordPtr::PARTS`].
    marker: u32,
    key: &'a [u8],
    value: &'a [u8],
}

#[cfg(feature = "store")]
impl<'a> RecordParts<'a> {
    pub(crate) fn new(key: &'a [u8], value: &'a [u8]) -> Self {
        Self {
            marker: RecordPtr::PARTS,
            key,
            value,
        }
    }

    /// A record pointer to the key and value, for as long as they are
    /// borrowed: the borrow of `self` bounds it, which the caller keeps for
    /// as long as the pointer is used.
    pub(crate) fn record_ptr(&self) -> RecordPtr {
        // SAFETY: `RecordParts` starts with `PARTS`, which `parts` tells
        // apart from a record's length, and stays while borrowed.
        unsafe { RecordPtr::new(NonNull::from(self).cast()) }
    }
}

impl PartialEq for RecordPtr {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

impl Eq for RecordPtr {}

impl PartialOrd for RecordPtr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RecordPtr {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes().cmp(other.bytes())
    }
}

/// Its bytes, which order and compare as it does: what a store is looked
/// up with.
impl Borrow<[u8]> for RecordPtr {
    fn borrow(&self) -> &[u8] {
        self.bytes()
    }
}

/// Its bytes', as a byte slice's: 8-byte windows, its length as its hash
/// tag, comparisons from where keys differ and against a prefix.
impl InlineKey for RecordPtr {
    const HAS_HASH_TAG: bool = <[u8]>::HAS_HASH_TAG;
    const HAS_CMP_FROM: bool = <[u8]>::HAS_CMP_FROM;
    const WIDTH: InlineWidth = <[u8]>::WIDTH;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.bytes().inline_key_at(offset)
    }

    #[inline]
    fn hash_tag(&self) -> u8 {
        self.bytes().hash_tag()
    }

    #[inline]
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        self.bytes().cmp_from(other.bytes(), equal_len)
    }

    #[inline]
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        self.bytes().cmp_prefix(prefix)
    }
}
