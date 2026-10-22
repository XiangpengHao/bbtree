//! What a [`BBTreeMapFile`](crate::BBTreeMapFile) keeps of the caller's types, and how: each type picks
//! its own storage. A plain type stays as it is, in its leaf's slot, as a
//! map's keys and values do; text and byte strings are [`VarBytes`], their
//! bytes in the heap pages after the leaf (`design/15-index-file.md`).
//!
//! - [`DiskType`] is a type a `BBTreeMapFile`'s keys or values are: what the file
//!   keeps for it, how its keys are searched, and what lookups hand back.
//! - [`DiskQuery`] is what a lookup takes: the type, or a borrowed form of
//!   it, as `str` is of `String`.
//! - [`DiskInput`] is what a `BBTreeMapFile` is written from: the type, owned or
//!   borrowed, or another form with its bytes, as `Cow<[u8]>`.
//! - [`DiskStored`] is what the file keeps: a plain type, or [`VarBytes`].
//! - [`plain_disk_type!`](crate::plain_disk_type) makes a plain type of
//!   the caller's a disk type, as the integers are.

use std::borrow::Cow;
use std::ops::Deref;

use bbtree::file::InWriter;
use bbtree::{Auto, InlineKeyStrategy, WithInlineKey};

use crate::var_bytes::{VarBytes, VarBytesQuery};

/// What a `BBTreeMapFile` keeps for a key or value: a plain type as it is,
/// in its slot, or [`VarBytes`], a span to bytes in the heap pages after its
/// leaf. Only those implement it.
pub trait DiskStored: storage::DiskStorage {}

impl<T: storage::DiskStorage> DiskStored for T {}

/// What a `BBTreeMapFile`'s writer takes for a key or value stored as `T`:
/// the value, for a plain type; its bytes, borrowed or owned (`Cow`), for
/// [`VarBytes`].
pub type Input<'a, T> = <T as storage::DiskStorage>::Input<'a>;

/// What [`DiskStored`] tells a writer, kept out of the public API.
pub(crate) mod storage {
    use bbtree::file::{PlainBytes, Storable};

    use crate::var_bytes::{ArenaBytes, ByteArena, VarBytes};

    /// How a writer takes a key or value the file keeps as `Self`, and the
    /// bytes after the leaf that its slot points to.
    pub trait DiskStorage: Storable {
        /// What a writer takes for one: the value, or its bytes.
        type Input<'a>;

        /// What the writer's tree holds for `input`, whose bytes, if it
        /// keeps them after the leaf, go into `arena`.
        fn in_writer(input: Self::Input<'_>, arena: &mut ByteArena) -> Self::InWriter;

        /// The bytes `in_writer` keeps after its leaf; none for a plain
        /// type.
        fn leaf_bytes(in_writer: &Self::InWriter) -> &[u8];
    }

    impl<T: PlainBytes> DiskStorage for T {
        type Input<'a> = T;

        fn in_writer(input: T, _arena: &mut ByteArena) -> T {
            input
        }

        fn leaf_bytes(_in_writer: &T) -> &[u8] {
            &[]
        }
    }

    impl DiskStorage for VarBytes {
        type Input<'a> = std::borrow::Cow<'a, [u8]>;

        fn in_writer(input: std::borrow::Cow<'_, [u8]>, arena: &mut ByteArena) -> ArenaBytes {
            arena.push(&input)
        }

        fn leaf_bytes(in_writer: &ArenaBytes) -> &[u8] {
            in_writer.bytes()
        }
    }
}

/// A type a [`BBTreeMapFile`](crate::BBTreeMapFile)'s keys or values are:
/// the integers, `f32` and `f64`, byte arrays, `String`, `Vec<u8>`, and
/// plain types of the caller's ([`plain_disk_type!`](crate::plain_disk_type)).
pub trait DiskType: DiskQuery {
    /// How keys of this type are searched, as a map's strategy: [`Auto`]
    /// for the integers and byte arrays; [`WithInlineKey`] for
    /// `VarBytes`, with its byte strings' inline keys, and for a caller's
    /// type with its own [`InlineKey`](bbtree::InlineKey) impl. Values
    /// don't use it.
    type Strategy: InlineKeyStrategy<Self::Stored> + InlineKeyStrategy<InWriter<Self::Stored>>;

    /// What lookups hand back, borrowing the file: a reference to a plain
    /// value, `&str` for `String`, `&[u8]` for `Vec<u8>`.
    type View<'a>;

    /// The view of `stored`, as the file keeps it.
    fn view(stored: &Self::Stored) -> Self::View<'_>;
}

/// What a `BBTreeMapFile`'s lookups take for a key: a [`DiskType`], or a borrowed
/// form of one, which the file keeps the same way.
///
/// The stored form orders as the key does: a plain key by its own `Ord`,
/// bytes as byte strings, as text and `[u8]` order.
pub trait DiskQuery {
    /// What the file keeps: `Self`, in its slot, for a plain type;
    /// [`VarBytes`], in the heap pages after the leaf, for text and bytes.
    type Stored: DiskStored;

    /// What a lookup compares with the file's keys, as the stored type: a
    /// reference to the key itself, for a plain type; a [`VarBytesQuery`]
    /// of its bytes, for text and bytes, which the search compares in
    /// place, inline keys and all.
    type Query<'a>: Deref<Target = Self::Stored>
    where
        Self: 'a;

    /// The key as a lookup compares it.
    fn query(&self) -> Self::Query<'_>;

    /// What the file's writer takes for this, borrowing it: the value, or
    /// its bytes.
    fn input(&self) -> Input<'_, Self::Stored>;
}

/// What a `BBTreeMapFile` is written from, for a key or value stored as `S`: a
/// [`DiskType`], owned or borrowed (`&`, `&mut`), its borrowed forms, as
/// `&str`, and bytes as `Cow`, which pass to the writer without a copy.
pub trait DiskInput<S: DiskStored> {
    /// What the file's writer takes for this: the value, or its bytes.
    fn into_input<'a>(self) -> Input<'a, S>
    where
        Self: 'a;
}

impl<T: DiskQuery + ?Sized> DiskInput<T::Stored> for &T {
    fn into_input<'a>(self) -> Input<'a, T::Stored>
    where
        Self: 'a,
    {
        self.input()
    }
}

impl<T: DiskQuery + ?Sized> DiskInput<T::Stored> for &mut T {
    fn into_input<'a>(self) -> Input<'a, T::Stored>
    where
        Self: 'a,
    {
        (*self).input()
    }
}

/// Makes `$plain`, a plain type of the caller's, a `BBTreeMapFile`'s key or value
/// type, kept as it is in its leaf's slot, as the integers are: lookups take
/// a `&$plain` and hand one back, borrowing the file.
///
/// The type is [`PlainBytes`](bbtree::file::PlainBytes), and `Ord` for a
/// key. Its keys are searched with [`Auto`](bbtree::Auto), which compares
/// them whole, or with the strategy given: [`WithInlineKey`](bbtree::WithInlineKey)
/// for a type with its own [`InlineKey`](bbtree::InlineKey) impl, which is
/// searched as the integers are.
///
/// # Examples
///
/// ```
/// use bbtree_mmap::bbtree::file::PlainBytes;
/// use bbtree_mmap::bbtree::{InlineKey, InlineWidth, InlineWord, WithInlineKey};
/// use bbtree_mmap::{BBTreeMapFile, plain_disk_type};
///
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// #[repr(transparent)]
/// struct UserId(u64);
///
/// // SAFETY: one `u64`: every bit pattern is a `UserId`, with no padding
/// // and nothing to point to.
/// unsafe impl PlainBytes for UserId {}
///
/// impl InlineKey for UserId {
///     const WIDTH: InlineWidth = u64::WIDTH;
///
///     fn inline_key_at(&self, offset: usize) -> InlineWord {
///         self.0.inline_key_at(offset)
///     }
/// }
///
/// plain_disk_type!(UserId, WithInlineKey);
///
/// let path = std::env::temp_dir().join(format!("bbtree-mmap-plain-{}", std::process::id()));
/// BBTreeMapFile::<UserId, u64>::create(&path, [(UserId(7), 70_u64), (UserId(42), 420)])?;
/// let map = BBTreeMapFile::<UserId, u64>::read(&path)?;
/// assert_eq!(map.get(&UserId(42)), Some(&420));
/// # std::fs::remove_file(&path)?;
/// # Ok::<(), std::io::Error>(())
/// ```
#[macro_export]
macro_rules! plain_disk_type {
    ($plain:ty) => {
        $crate::plain_disk_type!($plain, $crate::bbtree::Auto);
    };
    ($plain:ty, $strategy:ty) => {
        impl $crate::DiskQuery for $plain {
            type Stored = $plain;
            type Query<'a> = &'a $plain;

            fn query(&self) -> &$plain {
                self
            }

            fn input(&self) -> $plain {
                *self
            }
        }

        impl $crate::DiskInput<$plain> for $plain {
            fn into_input<'a>(self) -> $plain
            where
                Self: 'a,
            {
                self
            }
        }

        impl $crate::DiskType for $plain {
            type Strategy = $strategy;
            type View<'a> = &'a $plain;

            fn view(stored: &$plain) -> &$plain {
                stored
            }
        }
    };
}

plain_disk_type!(u8);
plain_disk_type!(u16);
plain_disk_type!(u32);
plain_disk_type!(u64);
plain_disk_type!(u128);
plain_disk_type!(i8);
plain_disk_type!(i16);
plain_disk_type!(i32);
plain_disk_type!(i64);
plain_disk_type!(i128);
plain_disk_type!(f32);
plain_disk_type!(f64);

/// Byte arrays, plain: `[u8; 16]`, `[u8; 20]` and `[u8; 32]` keys have inline
/// keys under [`Auto`]; others compare whole.
impl<const N: usize> DiskQuery for [u8; N] {
    type Stored = [u8; N];
    type Query<'a> = &'a [u8; N];

    fn query(&self) -> &[u8; N] {
        self
    }

    fn input(&self) -> [u8; N] {
        *self
    }
}

impl<const N: usize> DiskInput<[u8; N]> for [u8; N] {
    fn into_input<'a>(self) -> [u8; N]
    where
        Self: 'a,
    {
        self
    }
}

impl<const N: usize> DiskType for [u8; N] {
    type Strategy = Auto;
    type View<'a> = &'a [u8; N];

    fn view(stored: &[u8; N]) -> &[u8; N] {
        stored
    }
}

/// The bytes of byte strings and text, in the heap pages after the leaf:
/// `$owned` is the disk type, `$borrowed` its borrowed form, `$bytes` how
/// either gives its bytes.
macro_rules! byte_string_disk_type {
    ($owned:ty, $borrowed:ty, $bytes:ident) => {
        impl DiskQuery for $borrowed {
            type Stored = VarBytes;
            type Query<'a> = VarBytesQuery<'a>;

            fn query(&self) -> VarBytesQuery<'_> {
                VarBytesQuery::new(self.$bytes())
            }

            fn input(&self) -> Cow<'_, [u8]> {
                Cow::Borrowed(self.$bytes())
            }
        }

        impl DiskQuery for $owned {
            type Stored = VarBytes;
            type Query<'a> = VarBytesQuery<'a>;

            fn query(&self) -> VarBytesQuery<'_> {
                VarBytesQuery::new(self.$bytes())
            }

            fn input(&self) -> Cow<'_, [u8]> {
                Cow::Borrowed(self.$bytes())
            }
        }

        impl DiskInput<VarBytes> for $owned {
            fn into_input<'a>(self) -> Cow<'a, [u8]>
            where
                Self: 'a,
            {
                Cow::Owned(self.into())
            }
        }

        impl DiskInput<VarBytes> for Cow<'_, $borrowed> {
            fn into_input<'a>(self) -> Cow<'a, [u8]>
            where
                Self: 'a,
            {
                match self {
                    Cow::Borrowed(borrowed) => Cow::Borrowed(borrowed.$bytes()),
                    Cow::Owned(owned) => Cow::Owned(owned.into()),
                }
            }
        }
    };
}

byte_string_disk_type!(String, str, as_bytes);
byte_string_disk_type!(Vec<u8>, [u8], as_ref);

/// Text, whose bytes are its UTF-8.
impl DiskType for String {
    type Strategy = WithInlineKey;
    type View<'a> = &'a str;

    /// # Panics
    ///
    /// If the bytes are not UTF-8, which only a damaged file holds.
    fn view(stored: &VarBytes) -> &str {
        std::str::from_utf8(stored.bytes()).expect("text a BBTreeMapFile keeps is UTF-8")
    }
}

impl DiskType for Vec<u8> {
    type Strategy = WithInlineKey;
    type View<'a> = &'a [u8];

    fn view(stored: &VarBytes) -> &[u8] {
        stored.bytes()
    }
}
