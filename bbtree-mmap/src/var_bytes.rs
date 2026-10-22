//! Keys and values of any length in a [`BBTreeMapFile`](crate::BBTreeMapFile)
//! (`design/15-index-file.md`): a leaf keeps an 8-byte span for each, and
//! their bytes lie in the heap pages after the leaf (`heap_pages.rs`).
//!
//! - [`VarBytes`] is the span: `{len, offset}`, its bytes `offset` bytes
//!   past the span itself. It compares as its bytes do, and has their
//!   inline keys, so that bbtree's search, which hands a key's impls only
//!   `&K`, reads them without knowing where the node is.
//! - [`VarBytesQuery`] is what a lookup compares the spans with: a span
//!   marked by a length no span has, followed by the caller's bytes'
//!   address, as a store's lookups point to their key (`bbtree-store`).
//! - [`ArenaBytes`] is what a writer's tree holds instead while it builds:
//!   a pointer to the bytes, length first, in a [`ByteArena`]. It has the
//!   span's size, alignment and inline keys, so the tree's pages are a file
//!   map's but for those slots, which the heap pages' writer rewrites into
//!   spans.

use std::cmp::Ordering;
use std::fmt;
use std::ops::Deref;
use std::ptr::NonNull;

use bbtree::file::{FileError, Storable};
use bbtree::{InlineKey, InlineWord};

/// A key or value of any length, as a `BBTreeMapFile` keeps it: a span in
/// its leaf, `{len, offset}`, to its bytes, which lie `offset` bytes past
/// the span, in the heap pages after the leaf. Reached only by reference:
/// in a file map, whose bytes hold the span's, or from a
/// [`VarBytesQuery`].
///
/// It compares and orders as its bytes do, and has their inline keys, as a
/// `[u8]` has them, under [`WithInlineKey`](bbtree::WithInlineKey): a file
/// map of `VarBytes` keys searches as a map of byte strings does.
#[repr(C, align(8))]
pub struct VarBytes {
    len: u32,
    offset: u32,
}

impl VarBytes {
    /// The length no span has: a query's ([`VarBytesQuery`]).
    const QUERY: u32 = u32::MAX;

    /// The span as a file map keeps it, 8 bytes: `len`, then `offset`,
    /// native-endian.
    pub(crate) fn encode(len: u32, offset: u32) -> [u8; 8] {
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&len.to_ne_bytes());
        bytes[4..].copy_from_slice(&offset.to_ne_bytes());
        bytes
    }

    /// The `len` and `offset` of a span kept as `encode` keeps it.
    fn decode(bytes: [u8; 8]) -> (u32, u32) {
        let [l0, l1, l2, l3, o0, o1, o2, o3] = bytes;
        (
            u32::from_ne_bytes([l0, l1, l2, l3]),
            u32::from_ne_bytes([o0, o1, o2, o3]),
        )
    }

    /// The bytes.
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        let start = (self as *const Self).cast::<u8>();
        if self.len == Self::QUERY {
            // SAFETY: a span whose length is `QUERY` is a query's, which
            // `VarBytesQuery::deref` lent from the query's whole bytes, its
            // address after the span, for as long as the query lives.
            return unsafe { (*start.cast::<VarBytesQuery<'_>>()).bytes };
        }
        // SAFETY: a `VarBytes` is reached only by reference, in a file map
        // whose slots `Storable::check` checked, or whose caller vouched for
        // it (`open_unchecked`): either way, `len` initialized bytes lie
        // `offset` bytes past it, in the same allocation, unchanged for as
        // long as it is borrowed. The bytes are outside the span itself,
        // which Tree Borrows, unlike Stacked Borrows, lets a reference reach.
        unsafe { std::slice::from_raw_parts(start.add(self.offset as usize), self.len as usize) }
    }

    /// The number of bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes().len()
    }

    /// Whether there are no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl PartialEq for VarBytes {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

impl Eq for VarBytes {}

impl PartialOrd for VarBytes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VarBytes {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes().cmp(other.bytes())
    }
}

impl fmt::Debug for VarBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.bytes(), f)
    }
}

/// Its bytes': 8-byte windows, its length as its hash tag, comparisons from
/// where keys differ and against a prefix, as `[u8]`'s.
impl InlineKey for VarBytes {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

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

// SAFETY: a writer's tree holds an `ArenaBytes` for each: a pointer, with
// explicit zeros on targets whose pointers are narrower, 8 bytes aligned to
// 8 with no padding, as a span is, with a span's inline keys; the heap
// pages' writer writes a span in its place (`heap_pages.rs`). A span reads
// its bytes alone, which `check` finds in the file map, and is no query.
unsafe impl Storable for VarBytes {
    type InWriter = ArenaBytes;
    const HAS_BYTES: bool = true;
    /// A span from the slot, `{len: u32, offset: u32}`.
    const FORMAT: u64 = 1;

    fn check(file_bytes: &[u8], slot: usize) -> Result<(), FileError> {
        let span = file_bytes
            .get(slot..slot + size_of::<Self>())
            .and_then(|span| span.try_into().ok())
            .ok_or(FileError::Corrupt("a span outside the file map"))?;
        let (len, offset) = Self::decode(span);
        let end = (slot as u64) + u64::from(offset) + u64::from(len);
        if len == Self::QUERY || end > file_bytes.len() as u64 {
            return Err(FileError::Corrupt(
                "a key or value's bytes outside the file map",
            ));
        }
        Ok(())
    }
}

/// A lookup's key: what a `BBTreeMapFile` of `VarBytes` keys compares its
/// keys with, a [`VarBytes`] by [`Deref`], with the caller's bytes. It
/// compares as a span of those bytes does, inline keys and all, and copies
/// none of them.
#[repr(C, align(8))]
pub struct VarBytesQuery<'a> {
    /// A span of length [`VarBytes::QUERY`], which tells a query apart.
    span: VarBytes,
    bytes: &'a [u8],
}

impl<'a> VarBytesQuery<'a> {
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            span: VarBytes {
                len: VarBytes::QUERY,
                offset: 0,
            },
            bytes,
        }
    }
}

impl Deref for VarBytesQuery<'_> {
    type Target = VarBytes;

    fn deref(&self) -> &VarBytes {
        // SAFETY: the span starts the query, which `repr(C)` lays out; the
        // reference comes from the whole query's, so that the span reaches
        // the address after it (`VarBytes::bytes`).
        unsafe { &*(self as *const Self).cast::<VarBytes>() }
    }
}

impl fmt::Debug for VarBytesQuery<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VarBytesQuery").field(&self.bytes).finish()
    }
}

/// What a writer's tree holds for [`VarBytes`] while it builds: a pointer
/// to the bytes in its [`ByteArena`], their length first.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct ArenaBytes {
    record: NonNull<u8>,
    /// Zeros, written, where a pointer is narrower than a span.
    zeros: [u8; 8 - size_of::<NonNull<u8>>()],
}

// SAFETY: the bytes it points to are the arena's, which a writer holds and
// never changes once written.
unsafe impl Send for ArenaBytes {}

// SAFETY: as for `Send`: shared, the bytes are only read.
unsafe impl Sync for ArenaBytes {}

impl ArenaBytes {
    /// The bytes it points to: what its inline keys and comparisons read,
    /// as a span's.
    #[inline]
    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: the record is a length and that many bytes after it, in a
        // chunk of the arena, which outlives every `ArenaBytes` it handed
        // out (`ByteArena::push`) and never changes them.
        unsafe {
            let len = self.record.cast::<u32>().read_unaligned() as usize;
            std::slice::from_raw_parts(self.record.as_ptr().add(size_of::<u32>()), len)
        }
    }
}

impl PartialEq for ArenaBytes {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

impl Eq for ArenaBytes {}

impl PartialOrd for ArenaBytes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ArenaBytes {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes().cmp(other.bytes())
    }
}

/// [`VarBytes`]'s, so that the tree a writer builds has a file map's inline
/// keys, hash tags and layout.
impl InlineKey for ArenaBytes {
    const HAS_HASH_TAG: bool = VarBytes::HAS_HASH_TAG;
    const HAS_CMP_FROM: bool = VarBytes::HAS_CMP_FROM;
    const WIDTH: bbtree::InlineWidth = VarBytes::WIDTH;

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

/// Where a writer keeps the bytes of its keys and values while it builds:
/// chunks that never move, each byte string its length, then its bytes.
///
/// Each chunk is held by the pointer `Box::leak` gave, which every
/// record's pointer derives from: nothing reborrows a chunk while records
/// point into it.
#[derive(Default)]
pub struct ByteArena {
    chunks: Vec<NonNull<[u8]>>,
    /// How much of the last chunk is taken.
    used: usize,
}

// SAFETY: the arena owns its chunks, as a `Vec<Box<[u8]>>` would.
unsafe impl Send for ByteArena {}

// SAFETY: as for `Send`; shared, the arena only lends its bytes to read.
unsafe impl Sync for ByteArena {}

impl ByteArena {
    /// A chunk's size, unless a byte string needs more.
    const CHUNK_SIZE: usize = 1 << 20;

    /// `bytes`, kept until the arena is dropped.
    ///
    /// # Panics
    ///
    /// If there are `u32::MAX` bytes or more.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> ArenaBytes {
        let len = (u32::try_from(bytes.len()).ok())
            .filter(|&len| len < VarBytes::QUERY)
            .expect("a key or value under 4 GiB");
        let size = size_of::<u32>() + bytes.len();
        let room = (self.chunks.last()).map_or(0, |chunk| chunk.len() - self.used);
        if size > room {
            let chunk = vec![0; size.max(Self::CHUNK_SIZE)].into_boxed_slice();
            self.chunks.push(NonNull::from(Box::leak(chunk)));
            self.used = 0;
        }
        let chunk = *self.chunks.last().expect("a chunk with room");
        // SAFETY: `size` bytes from `used` lie in the chunk, which the arena
        // owns, unborrowed: no record overlaps them yet.
        let record = unsafe {
            let record = chunk.cast::<u8>().add(self.used);
            record.cast::<[u8; 4]>().write_unaligned(len.to_ne_bytes());
            record
                .add(size_of::<u32>())
                .copy_from_nonoverlapping(NonNull::from(bytes).cast(), bytes.len());
            record
        };
        self.used += size;
        ArenaBytes {
            record,
            zeros: [0; 8 - size_of::<NonNull<u8>>()],
        }
    }
}

impl Drop for ByteArena {
    fn drop(&mut self) {
        for chunk in self.chunks.drain(..) {
            // SAFETY: each chunk came from `Box::leak`, and is given back
            // once; no record of it is read after the arena is dropped.
            drop(unsafe { Box::from_raw(chunk.as_ptr()) });
        }
    }
}

#[cfg(test)]
mod tests;
