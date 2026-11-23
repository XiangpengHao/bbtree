//! What a file map asks of its keys and values ([`Storable`]), what follows
//! its leaves for those whose slots point past themselves ([`LeafBytes`]),
//! and why a reader refuses one ([`FileError`]): the format's contract with
//! the types it stores, which `crate::file` publishes.

use core::error::Error;
use core::fmt;

use super::WriterLeaf;
use crate::strategy::InlineKeyStrategy;

/// A type whose values are their bytes: what a file map's keys and values
/// must be, so that the file map holds the whole map, and a reader may take
/// whatever bytes it finds for a value.
///
/// The std integers and floats are plain, and so are `()` and arrays of
/// plain values. Of them, [`Auto`](crate::Auto) gives `u64`, `i64`, `u128`, `i128` and the
/// byte arrays `[u8; 16]`, `[u8; 20]` and `[u8; 32]` inline keys; other
/// keys compare whole, as in a map.
///
/// # Safety
///
/// Every bit pattern of `size_of::<Self>()` bytes is a value of the type,
/// and every byte of a value is initialized: it has no padding. It holds no
/// pointer, reference or handle to anything outside its bytes, and needs no
/// `Drop`, being `Copy`.
pub unsafe trait PlainBytes: Copy + 'static {}

macro_rules! plain_bytes {
    ($($plain:ty),*) => {
        // SAFETY: every bit pattern is a value, with no padding and nothing
        // to point to.
        $(unsafe impl PlainBytes for $plain {})*
    };
}

plain_bytes!(
    (),
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64
);

// SAFETY: an array is its elements side by side, with no padding between
// them, as each element's size is a multiple of its alignment.
unsafe impl<T: PlainBytes, const N: usize> PlainBytes for [T; N] {}

/// What a file map's keys and values may be: a type a reader takes from
/// the file map's bytes where they lie, in its leaf's slot. A
/// [`PlainBytes`] type is one, holding nothing beyond its slot. A type
/// whose slot points to bytes beyond it (`HAS_BYTES`) has them written
/// right after its leaf by a writer's [`LeafBytes`], and checked by
/// [`Self::check`]: bbtree-mmap's `VarBytes`, a span to bytes of any length
/// in heap pages.
///
/// # Safety
///
/// - `InWriter` has `Self`'s size and alignment, no padding, and, under the
///   map's strategy, the same inline keys: a writer builds its tree of
///   `InWriter`, copies the pages, and, where `HAS_BYTES`, its `LeafBytes`
///   writes a `Self` in place of each `InWriter`.
/// - A `Self` read from a slot whose `check` passed, or that a writer
///   wrote, reads nothing outside the file map it lies in, whatever the
///   other bytes of the file map: a reader runs its `Ord` and `InlineKey`
///   on it.
pub unsafe trait Storable: Sized {
    /// What a writer's tree holds in its place while it builds: `Self` for
    /// a plain type; for one with bytes, a pointer to them.
    type InWriter: Copy;

    /// Whether the slot points to bytes beyond it, after its leaf.
    const HAS_BYTES: bool;

    /// What the slot holds, as a number in a file map's fingerprint: 0 for a
    /// plain type; another for each kind of slot with bytes, so that a
    /// file map of one is never read as of another.
    const FORMAT: u64;

    /// Checks the slot at `slot` of `file_bytes`, a file map, which a
    /// reader is about to trust: that the bytes it points to lie in
    /// `file_bytes`, as its reads need. Nothing to check for a plain type.
    ///
    /// # Errors
    ///
    /// [`FileError::Corrupt`], if they don't.
    fn check(file_bytes: &[u8], slot: usize) -> Result<(), FileError>;
}

// SAFETY: a plain value is built and kept as itself, and is valid
// whatever its bytes.
unsafe impl<T: PlainBytes> Storable for T {
    type InWriter = T;
    const HAS_BYTES: bool = false;
    const FORMAT: u64 = 0;

    fn check(_file_bytes: &[u8], _slot: usize) -> Result<(), FileError> {
        Ok(())
    }
}

/// What a [`FileWriter`](crate::file::FileWriter)'s tree holds for a key or value of type `T` while
/// it builds: `T` itself, for a [`PlainBytes`] type; a pointer to its
/// bytes, with the same inline keys and order, for one with bytes.
pub type InWriter<T> = <T as Storable>::InWriter;

/// What a file map keeps right after each leaf: the bytes of the leaf's
/// keys and values whose slots point beyond themselves ([`Storable`]),
/// which the code that keeps them lays out: bbtree-mmap's heap pages.
/// Nothing for plain keys and values ([`NoLeafBytes`]).
///
/// # Safety
///
/// `write` writes into every slot of the leaf's page whose type
/// `HAS_BYTES` a value that points into the bytes it wrote, so that
/// [`Storable::check`] passes; and `len` is how many it writes.
pub unsafe trait LeafBytes<K: Storable, V: Storable> {
    /// How many bytes follow `leaf` in the file map.
    fn len<S: InlineKeyStrategy<K::InWriter>>(leaf: &WriterLeaf<'_, K, V, S>) -> usize;

    /// Writes into `bytes`, [`Self::len`] of them, zeroed, what follows
    /// `leaf`, right after its page; and into `page`, the leaf's page as the
    /// file map keeps it, a `K` or `V` in each slot of an entry whose type
    /// `HAS_BYTES` ([`WriterEntry`](super::WriterEntry)). The slots past the entries are zeros.
    fn write<S: InlineKeyStrategy<K::InWriter>>(
        leaf: &WriterLeaf<'_, K, V, S>,
        page: &mut [u8],
        bytes: &mut [u8],
    );
}

/// Nothing after a leaf: what a file map of plain keys and values keeps.
pub struct NoLeafBytes;

// SAFETY: no slot of a plain type has bytes, and nothing follows a leaf.
unsafe impl<K: PlainBytes, V: PlainBytes> LeafBytes<K, V> for NoLeafBytes {
    fn len<S: InlineKeyStrategy<K>>(_leaf: &WriterLeaf<'_, K, V, S>) -> usize {
        0
    }

    fn write<S: InlineKeyStrategy<K>>(
        _leaf: &WriterLeaf<'_, K, V, S>,
        _page: &mut [u8],
        _bytes: &mut [u8],
    ) {
    }
}

/// Why [`FileMap::open`](crate::file::FileMap::open) refused a file map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileError {
    /// The bytes don't start with a file map's header.
    NotAFileMap,
    /// A file map of another format version.
    UnsupportedVersion(u32),
    /// A file map of another layout: other key or value types, another
    /// strategy, another bbtree layout, pointer width or byte order.
    LayoutMismatch,
    /// The bytes don't start at an address aligned to `alignment`
    /// ([`FileMap::alignment`](crate::file::FileMap::alignment)).
    Misaligned {
        /// What the file map's first byte must be aligned to.
        alignment: usize,
    },
    /// Fewer bytes than the file map's header says it holds.
    Truncated {
        /// The file map's size, by its header.
        byte_len: usize,
        /// The bytes given.
        available: usize,
    },
    /// A page or edge that would lead a reader out of bounds: the file map is
    /// damaged.
    Corrupt(&'static str),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAFileMap => f.write_str("not a bbtree file map"),
            Self::UnsupportedVersion(version) => {
                write!(
                    f,
                    "a bbtree file map of unsupported format version {version}"
                )
            }
            Self::LayoutMismatch => f.write_str("a bbtree file map of another layout"),
            Self::Misaligned { alignment } => {
                write!(f, "a bbtree file map not aligned to {alignment} bytes")
            }
            Self::Truncated {
                byte_len,
                available,
            } => write!(
                f,
                "a bbtree file map of {byte_len} bytes, of which {available} are given"
            ),
            Self::Corrupt(what) => write!(f, "a corrupt bbtree file map: {what}"),
        }
    }
}

impl Error for FileError {}
