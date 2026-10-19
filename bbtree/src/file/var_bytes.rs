//! Keys and values of any length in a file map (`design/15-index-file.md`,
//! "Keys and values of any length"): a leaf keeps an 8-byte span for each,
//! and their bytes lie in the leaf's tail, right after the leaf.
//!
//! - [`VarBytes`] is the span: `{len, offset}`, its bytes `offset` bytes
//!   past the span itself. It compares as its bytes do, and has their
//!   inline keys, so that bbtree's search, which hands a key's impls only
//!   `&K`, reads them without knowing where the node is. [`Auto`](crate::Auto)
//!   takes it for a byte string, as it does `String`: a `&[u8]` looks it
//!   up, compared in place.
//! - [`VarBytesBuf`] holds a span and its bytes side by side: a key to look
//!   a file map up with, which compares as the file map's own keys do, for
//!   a file map searched with [`WithInlineKey`](crate::WithInlineKey).
//! - [`ArenaBytes`] is what a writer's tree holds instead while it builds:
//!   a pointer to the bytes, length first, in a [`ByteArena`]. It has the
//!   span's size, alignment and inline keys, so the tree's pages are a file
//!   map's but for those slots, which the writer rewrites into spans as it
//!   copies them.

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::cmp::Ordering;
use core::fmt;
use core::ops::Deref;
use core::ptr::NonNull;

use super::storage::Storage;
use crate::inline_key::{InlineKey, InlineWord};

/// A key or a value of any length, as a file map keeps it: a span in its
/// leaf, `{len, offset}`, to its bytes, which lie `offset` bytes past the
/// span, in the leaf's tail. Reached only by reference: from a
/// [`FileMap`](super::FileMap), whose bytes hold the span's, or from a
/// [`VarBytesBuf`], which holds them itself.
///
/// It compares and orders as its bytes do, and has their inline keys, as a
/// `[u8]` has them: a file map of `VarBytes` keys searches as a map of byte
/// strings does. With [`Auto`](crate::Auto), which takes it for a byte
/// string, it borrows as `[u8]`, and a `&[u8]` looks it up, compared in
/// place; with [`WithInlineKey`](crate::WithInlineKey), a [`VarBytesBuf`]
/// does.
#[repr(C, align(8))]
pub struct VarBytes {
    len: u32,
    offset: u32,
}

impl VarBytes {
    /// The span as a file map keeps it, 8 bytes: `len`, then `offset`,
    /// native-endian.
    pub(crate) fn encode(len: u32, offset: u32) -> [u8; 8] {
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&len.to_ne_bytes());
        bytes[4..].copy_from_slice(&offset.to_ne_bytes());
        bytes
    }

    /// The `len` and `offset` of a span kept as `encode` keeps it.
    pub(crate) fn decode(bytes: [u8; 8]) -> (u32, u32) {
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
        // SAFETY: a `VarBytes` is reached only by reference, in a file map
        // that `FileMap::open` checked, or whose caller vouched for it
        // (`open_unchecked`), or in a `VarBytesBuf`: either way, `len`
        // initialized bytes lie `offset` bytes past it, in the same
        // allocation, unchanged for as long as it is borrowed. The bytes
        // are outside the span itself, which Tree Borrows, unlike Stacked
        // Borrows, lets a reference reach.
        unsafe { core::slice::from_raw_parts(start.add(self.offset as usize), self.len as usize) }
    }

    /// The number of bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether there are no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
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

/// Its bytes, which order and compare as it does.
impl Borrow<[u8]> for VarBytes {
    fn borrow(&self) -> &[u8] {
        self.bytes()
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

// SAFETY: a `VarBytes` is built from its bytes, which the writer keeps in
// its arena, its tree holding an `ArenaBytes` for each: a pointer, with
// explicit zeros on targets whose pointers are narrower, 8 bytes aligned
// to 8 with no padding, as a span is. The writer writes a span in its
// place.
unsafe impl Storage for VarBytes {
    type Input<'a> = Cow<'a, [u8]>;
    type InWriter = ArenaBytes;
    const VAR_LEN: bool = true;

    fn in_writer(input: Cow<'_, [u8]>, arena: &mut ByteArena) -> ArenaBytes {
        arena.push(&input)
    }

    fn tail_bytes(in_writer: &ArenaBytes) -> &[u8] {
        in_writer.bytes()
    }
}

/// A span with its bytes, side by side: the key to look a file map's
/// [`VarBytes`] keys up with (`map.get(&*query)`), which compares as they
/// do and has their inline keys, for a file map searched with
/// [`WithInlineKey`](crate::WithInlineKey); with [`Auto`](crate::Auto), a
/// `&[u8]` does, with no copy. Keys of up to 64 bytes are held in place,
/// longer ones in an allocation.
///
/// # Examples
///
/// ```
/// use bbtree::WithInlineKey;
/// use bbtree::file::{FileMap, FileWriter, VarBytes, VarBytesBuf};
///
/// let entries = [("apple", "red"), ("banana", "yellow")];
/// let writer = FileWriter::<VarBytes, VarBytes, WithInlineKey>::from_ascending(
///     entries.map(|(key, value)| (key.as_bytes().into(), value.as_bytes().into())),
/// )?;
/// let bytes = writer.to_buf();
/// let read = FileMap::<VarBytes, VarBytes, WithInlineKey>::open(&bytes).unwrap();
/// let value = read.get(&*VarBytesBuf::new(b"banana")).unwrap();
/// assert_eq!(value.bytes(), b"yellow");
/// # Ok::<(), bbtree::file::NotAscending>(())
/// ```
pub struct VarBytesBuf {
    /// The span, then the bytes, 8 bytes past it.
    words: QueryWords,
}

/// Where a [`VarBytesBuf`] keeps its span and bytes.
enum QueryWords {
    /// In place: the span and up to [`Self::IN_PLACE_BYTES`] bytes.
    InPlace([u64; 1 + Self::IN_PLACE_BYTES / 8]),
    /// In an allocation, for longer keys.
    Allocated(Vec<u64>),
}

impl QueryWords {
    /// The longest key held in place: one that a lookup's comparisons of
    /// the first few windows settle, as most do, needs no allocation.
    const IN_PLACE_BYTES: usize = 64;

    /// `words` words, the first for the span, the rest for the bytes,
    /// reusing an allocation if there is one.
    fn words_mut(&mut self, words: usize) -> &mut [u64] {
        if words <= 1 + Self::IN_PLACE_BYTES / 8 && !matches!(self, Self::Allocated(_)) {
            let Self::InPlace(in_place) = self else {
                unreachable!("not allocated");
            };
            return &mut in_place[..words];
        }
        if let Self::InPlace(_) = self {
            *self = Self::Allocated(Vec::new());
        }
        let Self::Allocated(allocated) = self else {
            unreachable!("allocated just above");
        };
        allocated.clear();
        allocated.resize(words, 0);
        allocated
    }

    fn as_ptr(&self) -> *const u64 {
        match self {
            Self::InPlace(in_place) => in_place.as_ptr(),
            Self::Allocated(allocated) => allocated.as_ptr(),
        }
    }
}

impl VarBytesBuf {
    /// `bytes`, with their span.
    ///
    /// A lookup right after should use [`Self::empty`] and [`Self::set`]
    /// instead, which write the key where it stays: moved after it is
    /// written, as this returns it, the key is read back by wide loads that
    /// can't take their data from the narrow stores that wrote it, which
    /// wait for those stores to retire, and so for every older cache miss.
    ///
    /// # Panics
    ///
    /// If there are more than `u32::MAX` bytes.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Self {
        let mut buf = Self::empty();
        buf.set(bytes);
        buf
    }

    /// No bytes, with room for a key of up to 64 bytes in place.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            words: QueryWords::InPlace([0; 1 + QueryWords::IN_PLACE_BYTES / 8]),
        }
    }

    /// Makes it hold `bytes` instead, reusing its room: for a loop of
    /// lookups.
    ///
    /// # Panics
    ///
    /// If there are more than `u32::MAX` bytes.
    pub fn set(&mut self, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).expect("a key of at most 4 GiB");
        let words = self.words.words_mut(1 + bytes.len().div_ceil(8));
        let offset = size_of::<VarBytes>() as u32;
        words[0] = u64::from_ne_bytes(VarBytes::encode(len, offset));
        // SAFETY: the words past the first hold at least `len` bytes, which
        // nothing else borrows.
        let key_bytes = unsafe {
            core::slice::from_raw_parts_mut(words.as_mut_ptr().add(1).cast::<u8>(), bytes.len())
        };
        key_bytes.copy_from_slice(bytes);
    }
}

/// As [`VarBytesBuf::empty`].
impl Default for VarBytesBuf {
    fn default() -> Self {
        Self::empty()
    }
}

impl Deref for VarBytesBuf {
    type Target = VarBytes;

    fn deref(&self) -> &VarBytes {
        // SAFETY: the first word holds a span, aligned as a `u64` is, to
        // the bytes in the words after it, `offset` bytes past it in the
        // same allocation or array; they stay while `self` is borrowed.
        unsafe { &*self.words.as_ptr().cast::<VarBytes>() }
    }
}

impl fmt::Debug for VarBytesBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VarBytesBuf").field(&**self).finish()
    }
}

/// What a writer's tree holds for [`VarBytes`] while it builds: a pointer
/// to the bytes in its [`ByteArena`], their length first. Public only to
/// the crate's sealed `Storage`: nothing outside the crate names it.
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
    /// The bytes it points to: what `Auto`'s inline keys and comparisons
    /// read, as a span's.
    #[inline]
    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: the record is a length and that many bytes after it, in a
        // chunk of the arena, which outlives every `ArenaBytes` it handed
        // out (`ByteArena::push`) and never changes them.
        unsafe {
            let len = self.record.cast::<u32>().read_unaligned() as usize;
            core::slice::from_raw_parts(self.record.as_ptr().add(size_of::<u32>()), len)
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
    const WIDTH: crate::InlineWidth = VarBytes::WIDTH;

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
/// Public only to the crate's sealed `Storage`.
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
    /// If there are more than `u32::MAX` bytes.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> ArenaBytes {
        let len = u32::try_from(bytes.len()).expect("a key or value of at most 4 GiB");
        let size = size_of::<u32>() + bytes.len();
        let room = self
            .chunks
            .last()
            .map_or(0, |chunk| chunk.len() - self.used);
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
