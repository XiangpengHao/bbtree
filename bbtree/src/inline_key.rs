//! Inline keys (`design/04-inline-key.md`, D18): register-sized windows of a
//! key's encoding that nodes store beside their keys and search first. A
//! search then reads the full keys, and the heap bytes behind them, only
//! where inline keys tie.
//!
//! - [`InlineKey`] is how a key type gives its keys inline keys; std byte
//!   strings and the 64- and 128-bit integers implement it.
//! - [`InlineWidth`] is how many bytes of each inline key the nodes keep.
//! - The map's strategy parameter, [`InlineKeyStrategy`], decides which keys
//!   have them: with [`Auto`], the default, std byte strings and the 64- and
//!   128-bit integers do, recognized by their type; with [`WithInlineKey`],
//!   every key does, through its [`InlineKey`] impl.

#![forbid(unsafe_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::proof::SearchBytes;

/// An inline key: bytes of a key's encoding, big-endian, so that inline keys
/// compare like the bytes they come from (see [`InlineKey`]).
///
/// Always 8 bytes, of which the nodes keep the first 2, 4 or 8
/// ([`InlineWidth`]).
pub type InlineWord = u64;

/// How many bytes of each inline key the nodes keep: the first 2, the first
/// 4, or all 8 of the window that [`InlineKey::inline_key_at`] gives.
///
/// Narrower inline keys pack more of a node's inline keys into each cache
/// line, and compare in more vector lanes at once, but tie more often:
/// inline keys that tie say nothing, and the search then compares the keys
/// themselves. A key stored in the node, such as an integer, makes a tie
/// cheap, and 2 bytes past the bytes a node's keys share rarely tie on keys
/// whose bytes are dense. A key behind a pointer, such as a `String`, reads
/// the heap on a tie: 8 bytes, the default, tie least.
///
/// With [`Auto`], heap byte strings keep 8 bytes in leaves and 4 in internal
/// nodes; [`WithInlineKey`] keeps the impl's declared width in both kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InlineWidth {
    /// The first 2 bytes: the std integers' width, and under [`Auto`], fixed
    /// byte arrays'.
    Two,
    /// The first 4 bytes.
    Four,
    /// All 8 bytes: the default.
    Eight,
}

impl InlineWidth {
    /// How many bytes the nodes keep.
    pub(crate) const fn byte_len(self) -> usize {
        match self {
            Self::Two => 2,
            Self::Four => 4,
            Self::Eight => 8,
        }
    }
}

/// A key type whose keys have inline keys: windows of the key's encoding, a
/// byte string that orders like the key.
///
/// The contract: every key has an encoding such that if `a`'s encoding sorts
/// before `b`'s, both padded with zeros, then `a < b`. And
/// `inline_key_at(offset)` is that encoding's bytes `offset..offset + 8`,
/// big-endian, zero past its end. So where the encodings of two keys agree
/// before `offset` and their inline keys at `offset` differ, the keys order
/// like those inline keys. Equal inline keys say nothing, and the map
/// compares the full keys. Offsets past 0 let the map skip bytes that all the
/// keys of a node share.
///
/// The impls for std byte strings and the 64- and 128-bit integers are
/// proven, by Verus, to keep this contract (`src/proof/inline_key.rs`).
///
/// Where the map uses this impl (every key under [`WithInlineKey`], and the
/// std integers under [`Auto`]), the nodes keep the first [`Self::WIDTH`]
/// bytes of each inline key, all 8 unless the impl says fewer. Kept bytes
/// that differ order the keys the same way, and kept bytes that tie say
/// nothing.
///
/// A key wrapping a byte string can window its bytes, and take its hash
/// tag and its comparisons from where keys differ and against a prefix. A key ordered by several fields encodes them one after the other: a
/// `u32` and then a string as the integer's big-endian bytes followed by the
/// string's.
///
/// Implementing it is not enough for a map to use it: the map also needs the
/// [`WithInlineKey`] strategy. Std byte strings and the 64- and 128-bit
/// integers get inline keys by default ([`Auto`]). A [`BBTreeMap`], which
/// threads share, needs inline keys; the [`unsync`](crate::unsync) map and
/// set work without, comparing full keys.
///
/// [`BBTreeMap`]: crate::BBTreeMap
///
/// A wrong implementation, like a wrong `Ord`, gives wrong answers but never
/// undefined behavior: whatever it returns, the map reads only its own
/// entries.
///
/// # Examples
///
/// ```
/// use std::cmp::Ordering;
///
/// use bbtree::{BBTreeMap, InlineKey, InlineWord, WithInlineKey};
///
/// #[derive(PartialEq, Eq, PartialOrd, Ord)]
/// struct UserName(String);
///
/// impl InlineKey for UserName {
///     const HAS_HASH_TAG: bool = true;
///     const HAS_CMP_FROM: bool = true;
///
///     fn inline_key_at(&self, offset: usize) -> InlineWord {
///         self.0.inline_key_at(offset)
///     }
///
///     fn hash_tag(&self) -> u8 {
///         self.0.hash_tag()
///     }
///
///     fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
///         self.0.cmp_from(&other.0, equal_len)
///     }
///
///     fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
///         self.0.cmp_prefix(prefix)
///     }
/// }
///
/// let names = BBTreeMap::with_strategy(WithInlineKey);
/// names.insert(UserName("ferris".to_string()), 7)?;
/// assert_eq!(names.get_copied(&UserName("ferris".to_string())), Some(7));
/// # Ok::<(), bbtree::AllocError>(())
/// ```
pub trait InlineKey: Ord {
    /// How many bytes of each inline key the nodes keep, where the map uses
    /// this impl (see [`InlineWidth`]).
    const WIDTH: InlineWidth = InlineWidth::Eight;

    /// Whether the leaves keep each key's [`Self::hash_tag`], where the map
    /// uses this impl and a leaf's layout has room for a byte per key:
    /// `false` unless the impl says so. The std byte strings' impls do.
    const HAS_HASH_TAG: bool = false;

    /// Whether the map compares keys whose inline keys tie with
    /// [`Self::cmp_from`], past the bytes the inline keys showed equal:
    /// `false` unless the impl says so. The std byte strings' impls do.
    ///
    /// Internal nodes then keep 4 bytes of each 8-byte inline key, and
    /// leaves all 8, as for std byte strings under [`Auto`]. More of the
    /// separators' inline keys tie in 4 bytes, which costs little only
    /// because the comparison skips the bytes known equal.
    const HAS_CMP_FROM: bool = false;

    /// This key's inline key at `offset`: bytes `offset..offset + 8` of its
    /// encoding, big-endian, zero past its end.
    fn inline_key_at(&self, offset: usize) -> InlineWord;

    /// This key's hash tag: a byte that equal keys share, so that keys whose
    /// tags differ are different. Where an exact-key lookup's inline key
    /// ties with several keys of a leaf, their tags rule out the keys
    /// without its tag before any comparison, so that `Ord` reads one or two
    /// keys instead of several. Called only if [`Self::HAS_HASH_TAG`].
    ///
    /// A tag that differs between equal keys, like a wrong `Ord`, gives
    /// wrong answers but never undefined behavior. The map also calls it
    /// while it moves keys between nodes, where a panic would leave the move
    /// half done: a panic there aborts the process.
    fn hash_tag(&self) -> u8 {
        0
    }

    /// How this key orders against `other`, as [`Ord::cmp`] does, given
    /// that the first `equal_len` bytes of their encodings, zero-padded,
    /// are equal. The map calls it where their inline keys tie, with what
    /// the node's keys share and the bytes of the inline keys it keeps,
    /// so that a key whose bytes lie elsewhere is read from where keys can
    /// differ, not from its start. Called only if [`Self::HAS_CMP_FROM`].
    ///
    /// One that orders keys otherwise than `Ord`, like a wrong `Ord`,
    /// gives wrong answers but never undefined behavior. With it, the map
    /// also calls [`Self::inline_key_at`] while it moves keys between
    /// nodes, to read a window's other 4 bytes, where a panic would leave
    /// the move half done: a panic there aborts the process.
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        let _ = equal_len;
        self.cmp(other)
    }

    /// How this key's encoding, padded with zeros, orders against `prefix`
    /// in its first `prefix.len()` bytes. The map checks every search's key
    /// against what all of its keys share, at most 64 bytes, once, before
    /// the root: a key without them is outside every node.
    ///
    /// The default compares a window at a time. The std byte strings' impls
    /// compare 32 bytes at a time, as [`Auto`]'s nodes do: a key whose
    /// bytes lie elsewhere is then read in two loads, where eight windows,
    /// in ascending order, draw the hardware to prefetch lines past it.
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        let (words, tail) = prefix.as_chunks::<8>();
        for (index, word) in words.iter().enumerate() {
            let window = self.inline_key_at(8 * index);
            let prefix_word = InlineWord::from_be_bytes(*word);
            if window != prefix_word {
                return window.cmp(&prefix_word);
            }
        }
        if tail.is_empty() {
            return Ordering::Equal;
        }
        let dropped_bits = 8 * (8 - tail.len()) as u32;
        let window = self.inline_key_at(8 * words.len()) >> dropped_bits;
        window.cmp(&(tail.inline_key_at(0) >> dropped_bits))
    }
}

/// Its bytes are its encoding: byte strings order like their bytes. Its hash
/// tag is its length, saturated at 255: keys rarely reach it, and below it
/// equal tags are equal lengths. It compares from where keys differ as
/// [`Auto`]'s nodes do, 8 bytes at a time with no call to `memcmp`, and
/// against a prefix 32 bytes at a time.
impl InlineKey for [u8] {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        crate::proof::ProvenInlineKey::inline_key_at(self, offset)
    }

    #[inline]
    fn hash_tag(&self) -> u8 {
        u8::try_from(self.len()).unwrap_or(u8::MAX)
    }

    #[inline]
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        SearchBytes::new(self).cmp_from(other, equal_len)
    }

    #[inline]
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        SearchBytes::new(self).cmp_prefix(prefix)
    }
}

/// That of its UTF-8 bytes, which order like the string.
impl InlineKey for str {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.as_bytes().inline_key_at(offset)
    }

    #[inline]
    fn hash_tag(&self) -> u8 {
        self.as_bytes().hash_tag()
    }

    #[inline]
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        self.as_bytes().cmp_from(other.as_bytes(), equal_len)
    }

    #[inline]
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        self.as_bytes().cmp_prefix(prefix)
    }
}

impl InlineKey for String {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.as_bytes().inline_key_at(offset)
    }

    #[inline]
    fn hash_tag(&self) -> u8 {
        self.as_bytes().hash_tag()
    }

    #[inline]
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        self.as_bytes().cmp_from(other.as_bytes(), equal_len)
    }

    #[inline]
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        self.as_bytes().cmp_prefix(prefix)
    }
}

impl InlineKey for Vec<u8> {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.as_slice().inline_key_at(offset)
    }

    #[inline]
    fn hash_tag(&self) -> u8 {
        self.as_slice().hash_tag()
    }

    #[inline]
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        self.as_slice().cmp_from(other.as_slice(), equal_len)
    }

    #[inline]
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        self.as_slice().cmp_prefix(prefix)
    }
}

impl<const N: usize> InlineKey for [u8; N] {
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.as_slice().inline_key_at(offset)
    }
}

/// Its big-endian bytes, which order like it. The nodes keep 2 of them, past
/// the bytes that a node's keys share.
impl InlineKey for u64 {
    const WIDTH: InlineWidth = InlineWidth::Two;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        crate::proof::ProvenInlineKey::inline_key_at(self, offset)
    }
}

/// Its big-endian bytes, which order like it. The nodes keep 2 of them, past
/// the bytes that a node's keys share.
impl InlineKey for u128 {
    const WIDTH: InlineWidth = InlineWidth::Two;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        crate::proof::ProvenInlineKey::inline_key_at(self, offset)
    }
}

/// That of its bits with the sign bit flipped, as a `u64`: big-endian bytes
/// that order like it, negatives first.
impl InlineKey for i64 {
    const WIDTH: InlineWidth = InlineWidth::Two;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        crate::proof::ProvenInlineKey::inline_key_at(self, offset)
    }
}

/// That of its bits with the sign bit flipped, as a `u128`: big-endian bytes
/// that order like it, negatives first.
impl InlineKey for i128 {
    const WIDTH: InlineWidth = InlineWidth::Two;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        crate::proof::ProvenInlineKey::inline_key_at(self, offset)
    }
}

/// The referenced key's: covers `&str` and `&[u8]`.
impl<T: InlineKey + ?Sized> InlineKey for &T {
    const WIDTH: InlineWidth = T::WIDTH;
    const HAS_HASH_TAG: bool = T::HAS_HASH_TAG;
    const HAS_CMP_FROM: bool = T::HAS_CMP_FROM;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (**self).inline_key_at(offset)
    }

    #[inline]
    fn hash_tag(&self) -> u8 {
        (**self).hash_tag()
    }

    #[inline]
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        (**self).cmp_from(&**other, equal_len)
    }

    #[inline]
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        (**self).cmp_prefix(prefix)
    }
}

/// The boxed key's: covers `Box<str>` and `Box<[u8]>`.
impl<T: InlineKey + ?Sized> InlineKey for Box<T> {
    const WIDTH: InlineWidth = T::WIDTH;
    const HAS_HASH_TAG: bool = T::HAS_HASH_TAG;
    const HAS_CMP_FROM: bool = T::HAS_CMP_FROM;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (**self).inline_key_at(offset)
    }

    #[inline]
    fn hash_tag(&self) -> u8 {
        (**self).hash_tag()
    }

    #[inline]
    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        (**self).cmp_from(&**other, equal_len)
    }

    #[inline]
    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        (**self).cmp_prefix(prefix)
    }
}

/// The default strategy: std byte-string keys and 64- and 128-bit integer
/// keys have inline keys, other keys have none, at no cost.
///
/// The key types recognized are the byte strings `String`, `str`,
/// `Vec<u8>`, `[u8]`, `Box<str>`, `Box<[u8]>`, `&str`, `&[u8]`, `[u8; 16]`,
/// `[u8; 20]` and `[u8; 32]`, with the `file` feature a file map's
/// `VarBytes`, and the integers `u64`, `i64`, `u128` and `i128`. Their
/// inline keys are those their [`InlineKey`] impls give. They
/// are recognized by their type alone: a newtype around one of them is
/// another type, and needs [`WithInlineKey`]. A lookup uses the inline keys
/// when the type it looks up with is recognized too, as `&str` is for
/// `String` keys; otherwise it compares full keys, as a map without inline
/// keys does.
///
/// Fixed byte arrays keep 2 bytes per inline key. Heap-backed byte strings
/// keep 8 in leaves, to read the heap less on prefix ties, and 4 in internal
/// nodes, to keep those compact.
#[derive(Clone, Copy, Debug, Default)]
pub struct Auto;

/// The opt-in strategy: every key has the inline key its [`InlineKey`] impl
/// gives, of which the nodes keep [`InlineKey::WIDTH`] bytes, and the hash
/// tag it gives if [`InlineKey::HAS_HASH_TAG`].
///
/// A lookup uses the inline keys when it looks up with the key type itself.
/// A lookup through another borrowed form of the key (a `Q` other than `K`
/// in [`BBTreeMap::read`]) has no inline key to compare, and compares full
/// keys instead. Inline keys only speed up the search, so the answer is the
/// same.
///
/// [`BBTreeMap::read`]: crate::BBTreeMap::read
#[derive(Clone, Copy, Debug, Default)]
pub struct WithInlineKey;

/// Which keys of a map with keys `K` have inline keys: [`Auto`] or
/// [`WithInlineKey`], with the default [`NodeConfig`], or either of them
/// with another, [`Configured`]. These are the only implementations.
///
/// [`NodeConfig`]: crate::NodeConfig
/// [`Configured`]: crate::Configured
pub trait InlineKeyStrategy<K>: sealed::Sealed<K> {}

impl<K> InlineKeyStrategy<K> for Auto {}

impl<K: InlineKey> InlineKeyStrategy<K> for WithInlineKey {}

/// What a strategy tells the nodes, kept out of the public API.
pub(crate) mod sealed {
    use core::cmp::Ordering;

    use super::{Auto, InlineKey, InlineWidth, InlineWord, WithInlineKey};
    use crate::node_config::{DefaultNodeConfig, NodeConfig};

    /// Where a strategy's inline keys come from. Plain data: the node code
    /// resolves it for a key type (`tree/node/inline_keys.rs`), and page
    /// layouts depend on the answer.
    pub enum InlineKeySource<K> {
        /// Std byte strings and integers, and a file map's `VarBytes`,
        /// recognized by their type, have inline keys: windows of a byte
        /// string's bytes, and an integer's own [`InlineKey`] impl. Other
        /// keys have none ([`Auto`]).
        RecognizedStdTypes,
        /// Every key has the inline keys its [`InlineKey`] impl gives, of
        /// which the nodes keep `width` bytes, and its hash tag and
        /// comparison from where keys differ if the impl gives them
        /// ([`WithInlineKey`]).
        InlineKeyTrait {
            inline_key_at: fn(&K, usize) -> InlineWord,
            width: InlineWidth,
            hash_tag: Option<fn(&K) -> u8>,
            cmp_from: Option<fn(&K, &K, usize) -> Ordering>,
            cmp_prefix: fn(&K, &[u8]) -> Ordering,
        },
    }

    pub trait Sealed<K> {
        const SOURCE: InlineKeySource<K>;

        /// How the nodes are built (`node_config.rs`): every default, or a
        /// `Configured` strategy's config. A strategy that wraps another
        /// forwards the other's.
        type Config: NodeConfig;

        /// Whether the nodes keep a latch word in their header, for a tree
        /// that threads share (`tree/node/latch.rs`): only the strategy a
        /// shared `BBTreeMap` wraps its own in, never a public one.
        const LATCHED: bool = false;

        /// Whether the tree lies in a file map, whose edges are offsets from
        /// their node's page rather than pointers (`tree/file.rs`): only
        /// the strategy a `FileMap` wraps its own in, never a public one.
        const IN_FILE: bool = false;

        /// Whether the tree is a store's, whose leaves keep their entries'
        /// records in heap pages of their own (`tree/store`): only the
        /// strategy a store wraps its own in, never a public one. A latched
        /// tree's strategy forwards it.
        const IN_STORE: bool = false;
    }

    impl<K> Sealed<K> for Auto {
        const SOURCE: InlineKeySource<K> = InlineKeySource::RecognizedStdTypes;
        type Config = DefaultNodeConfig;
    }

    impl<K: InlineKey> Sealed<K> for WithInlineKey {
        const SOURCE: InlineKeySource<K> = InlineKeySource::InlineKeyTrait {
            inline_key_at: K::inline_key_at,
            width: K::WIDTH,
            hash_tag: if K::HAS_HASH_TAG {
                Some(K::hash_tag)
            } else {
                None
            },
            cmp_from: if K::HAS_CMP_FROM {
                Some(K::cmp_from)
            } else {
                None
            },
            cmp_prefix: K::cmp_prefix,
        };
        type Config = DefaultNodeConfig;
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    use super::{InlineKey, InlineWidth, InlineWord};
    use crate::tree::test_support::TestRng;

    #[test]
    fn windows_big_endian_and_zero_padded() {
        assert_eq!("".inline_key_at(0), 0);
        assert_eq!("a".inline_key_at(0), 0x6100_0000_0000_0000);
        assert_eq!("abc".inline_key_at(0), 0x6162_6300_0000_0000);
        assert_eq!(b"abcdefgh".inline_key_at(0), 0x6162_6364_6566_6768);
        assert_eq!(b"abcdefghij".inline_key_at(0), b"abcdefgh".inline_key_at(0));
        assert_eq!([0xff_u8; 3].inline_key_at(0), 0xffff_ff00_0000_0000);

        assert_eq!(b"abcdefghij".inline_key_at(2), b"cdefghij".inline_key_at(0));
        assert_eq!("abc".inline_key_at(1), 0x6263_0000_0000_0000);
        assert_eq!("abc".inline_key_at(3), 0);
        assert_eq!("abc".inline_key_at(usize::MAX), 0);
    }

    /// Every window of every short string is its bytes from the offset,
    /// zero-padded, wherever the window falls: inside, across the end, past
    /// it.
    #[test]
    fn windows_match_their_bytes() {
        let bytes: Vec<u8> = (1..=24).collect();
        for len in 0..=bytes.len() {
            let key = &bytes[..len];
            for offset in 0..len + 12 {
                let expected =
                    core::array::from_fn(|index| key.get(offset + index).copied().unwrap_or(0));
                assert_eq!(
                    key.inline_key_at(offset),
                    InlineWord::from_be_bytes(expected),
                    "{len} bytes at {offset}"
                );
            }
        }
    }

    #[test]
    fn std_byte_strings_agree() {
        let text = "inline keys";
        for offset in [0, 3, 11, 20] {
            let expected = text.as_bytes().inline_key_at(offset);
            assert_eq!(text.inline_key_at(offset), expected);
            assert_eq!(String::from(text).inline_key_at(offset), expected);
            assert_eq!(Vec::from(text).inline_key_at(offset), expected);
            assert_eq!(Box::<str>::from(text).inline_key_at(offset), expected);
            let boxed_bytes = Box::<[u8]>::from(text.as_bytes());
            assert_eq!(boxed_bytes.inline_key_at(offset), expected);
            assert_eq!((&text).inline_key_at(offset), expected);
            assert_eq!((&text.as_bytes()).inline_key_at(offset), expected);
            let array: [u8; 11] = text.as_bytes().try_into().unwrap();
            assert_eq!(array.inline_key_at(offset), expected);
        }
    }

    /// The contract, checked on random short byte strings over a small
    /// alphabet with zero bytes, where prefixes, padding and ties are common:
    /// at every offset up to where the zero-padded strings first differ,
    /// differing windows order like the strings.
    #[test]
    fn differing_windows_order_like_the_bytes() {
        let rng = TestRng::new(41);
        let random_bytes = || -> Vec<u8> {
            let len = rng.below(12);
            (0..len).map(|_| [0, 1, b'a', 0xff][rng.below(4)]).collect()
        };
        let padded = |bytes: &[u8], index: usize| bytes.get(index).copied().unwrap_or(0);
        let rounds = if cfg!(miri) { 300 } else { 30_000 };
        let (mut ties, mut decided) = (0, 0);
        for _ in 0..rounds {
            let (a, b) = (random_bytes(), random_bytes());
            let shared = (0..a.len().max(b.len()))
                .take_while(|&index| padded(&a, index) == padded(&b, index))
                .count();
            for offset in 0..=shared {
                let (window_a, window_b) = (a.inline_key_at(offset), b.inline_key_at(offset));
                if window_a == window_b {
                    ties += 1;
                } else {
                    decided += 1;
                    assert_eq!(
                        window_a.cmp(&window_b),
                        a.cmp(&b),
                        "{a:?} vs {b:?} at {offset}"
                    );
                }
            }
        }
        assert!(ties > 0 && decided > 0);
    }

    /// A key with an impl of its windows alone, whose `cmp_prefix` is the
    /// trait's default.
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct WindowsOnly(Vec<u8>);

    impl InlineKey for WindowsOnly {
        fn inline_key_at(&self, offset: usize) -> InlineWord {
            self.0.inline_key_at(offset)
        }
    }

    /// Byte strings compare against a prefix 32 bytes at a time as the
    /// default does a window at a time, and as their bytes padded with
    /// zeros order against it: on random strings and prefixes of up to 80
    /// bytes over a small alphabet with zeros, many of them a string's own
    /// leading bytes, so that whole chunks tie and strings end inside.
    #[test]
    fn byte_strings_compare_against_prefixes_as_the_default_does() {
        let rng = TestRng::new(44);
        let random_bytes = |max_len: usize| -> Vec<u8> {
            let len = rng.below(max_len + 1);
            (0..len).map(|_| [0, 1, b'a', 0xff][rng.below(4)]).collect()
        };
        for _ in 0..if cfg!(miri) { 200 } else { 20_000 } {
            let key = random_bytes(80);
            let prefix = if rng.below(2) == 0 {
                let mut prefix = key[..rng.below(key.len() + 1)].to_vec();
                prefix.resize(prefix.len() + rng.below(3), 0);
                prefix
            } else {
                random_bytes(80)
            };
            let padded: Vec<u8> = (0..prefix.len())
                .map(|index| key.get(index).copied().unwrap_or(0))
                .collect();
            let expected = padded.cmp(&prefix);
            assert_eq!(
                key.cmp_prefix(&prefix),
                expected,
                "{key:?} against {prefix:?}"
            );
            assert_eq!(WindowsOnly(key.clone()).cmp_prefix(&prefix), expected);
        }
    }

    /// Integers' windows are those of their big-endian bytes, the signed
    /// ones with the sign bit flipped, at every offset, inside and past
    /// them.
    #[test]
    fn integer_windows_are_their_big_endian_bytes() {
        let rng = TestRng::new(42);
        for _ in 0..if cfg!(miri) { 20 } else { 1000 } {
            let high = (rng.below(1 << 30) as u64) << 34 | rng.below(1 << 30) as u64;
            let low = (rng.below(1 << 30) as u64) << 34 | rng.below(1 << 30) as u64;
            let wide = u128::from(high) << 64 | u128::from(low);
            for offset in [0, 1, 3, 7, 8, 9, 15, 16, 17, usize::MAX] {
                assert_eq!(
                    high.inline_key_at(offset),
                    high.to_be_bytes().inline_key_at(offset)
                );
                assert_eq!(
                    wide.inline_key_at(offset),
                    wide.to_be_bytes().inline_key_at(offset)
                );
                let signed = high.cast_signed();
                let flipped = (signed.cast_unsigned() ^ (1 << 63)).to_be_bytes();
                assert_eq!(signed.inline_key_at(offset), flipped.inline_key_at(offset));
                let signed = wide.cast_signed();
                let flipped = (signed.cast_unsigned() ^ (1 << 127)).to_be_bytes();
                assert_eq!(signed.inline_key_at(offset), flipped.inline_key_at(offset));
            }
        }
        assert_eq!(
            0x0102_0304_0506_0708_u64.inline_key_at(2),
            0x0304_0506_0708_0000
        );
        assert_eq!((-1_i64).inline_key_at(0), 0x7fff_ffff_ffff_ffff);
        assert_eq!(0_i64.inline_key_at(0), 0x8000_0000_0000_0000);
        assert_eq!(i128::MIN.inline_key_at(0), 0);
        assert_eq!(u128::MAX.inline_key_at(9), 0xffff_ffff_ffff_ff00);
    }

    /// Integers keep 2 bytes of their inline keys; other keys all 8.
    #[test]
    fn integers_keep_two_bytes() {
        assert_eq!(<u64 as InlineKey>::WIDTH, InlineWidth::Two);
        assert_eq!(<i64 as InlineKey>::WIDTH, InlineWidth::Two);
        assert_eq!(<u128 as InlineKey>::WIDTH, InlineWidth::Two);
        assert_eq!(<i128 as InlineKey>::WIDTH, InlineWidth::Two);
        assert_eq!(<&i64 as InlineKey>::WIDTH, InlineWidth::Two);
        assert_eq!(<Box<u128> as InlineKey>::WIDTH, InlineWidth::Two);
        assert_eq!(<String as InlineKey>::WIDTH, InlineWidth::Eight);
        assert_eq!(<[u8; 16] as InlineKey>::WIDTH, InlineWidth::Eight);
    }

    /// The contract, on signed integers that share their leading bytes
    /// often and cross zero: at every offset up to where the encodings first
    /// differ, the first 2 bytes of the windows, where they differ, order
    /// like the integers.
    #[test]
    fn differing_integer_windows_order_like_the_integers() {
        let rng = TestRng::new(43);
        let random = || (rng.below(1 << 12) as i64 - (1 << 11)) << (8 * rng.below(7));
        let (mut ties, mut decided) = (0, 0);
        for _ in 0..if cfg!(miri) { 300 } else { 30_000 } {
            let (a, b) = (random(), random());
            let shared = (a.inline_key_at(0) ^ b.inline_key_at(0)).leading_zeros() as usize / 8;
            for offset in 0..=shared.min(7) {
                let kept = |window: InlineWord| window >> 48;
                let (window_a, window_b) =
                    (kept(a.inline_key_at(offset)), kept(b.inline_key_at(offset)));
                if window_a == window_b {
                    ties += 1;
                } else {
                    decided += 1;
                    assert_eq!(window_a.cmp(&window_b), a.cmp(&b), "{a} vs {b} at {offset}");
                }
            }
        }
        assert!(ties > 0 && decided > 0);
    }
}
