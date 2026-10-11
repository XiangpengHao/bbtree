//! Inline keys (`doc/04-inline-key.md`, D18): register-sized windows of a
//! key's encoding that nodes store beside their keys and search first, so
//! that a search reads the full keys, and the heap bytes behind them, only
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
use core::mem::size_of;

/// An inline key: bytes of a key's encoding, big-endian, so that inline keys
/// compare like the bytes they come from (see [`InlineKey`]).
///
/// Always 8 bytes, of which the nodes keep the first [`InlineKey::WIDTH`].
pub type InlineWord = u64;

/// How many bytes of each inline key the nodes keep: the first 2, the first
/// 4, or all 8 of the window that [`InlineKey::inline_key_at`] gives.
///
/// Narrower inline keys pack more of a node's into each cache line, and
/// compare in more vector lanes at once, but tie more often: inline keys that
/// tie say nothing, and the search then compares the keys themselves. A key
/// stored in the node, such as an integer, makes a tie cheap, and 2 bytes
/// past the bytes a node's keys share rarely tie on keys whose bytes are
/// dense. A key behind a pointer, such as a `String`, reads the heap on a
/// tie: 8 bytes, the default, tie least.
/// With [`Auto`], heap byte strings keep eight bytes in leaves and four
/// in internal nodes; explicit [`WithInlineKey`] strategies use their
/// declared width in both kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InlineWidth {
    /// The first 2 bytes: the integers' width.
    Two,
    /// The first 4 bytes.
    Four,
    /// All 8 bytes: the default.
    Eight,
}

impl InlineWidth {
    /// How many bytes the nodes keep.
    pub(crate) const fn bytes(self) -> usize {
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
/// before `b`'s, both padded with zeros, then `a < b`; and
/// `inline_key_at(offset)` is that encoding's bytes `offset..offset + 8`,
/// big-endian, zero past its end. So where the encodings of two keys agree
/// before `offset` and their inline keys at `offset` differ, the keys order
/// like those inline keys. Equal inline keys say nothing, and the map
/// compares the full keys. Offsets past 0 let the map skip bytes that all the
/// keys of a node share.
///
/// The nodes keep the first [`Self::WIDTH`] bytes of each inline key, all 8
/// unless the impl says fewer: kept bytes that differ order the keys the
/// same way, and kept bytes that tie say nothing.
///
/// A key wrapping a byte string can window its bytes. A key ordered by
/// several fields encodes them one after the other: a `u32` and then a
/// string as the integer's big-endian bytes followed by the string's.
///
/// Implementing it is not enough for a map to use it: the map also needs the
/// [`WithInlineKey`] strategy. Std byte strings and the 64- and 128-bit
/// integers get inline keys by default ([`Auto`]).
///
/// A wrong implementation, like a wrong `Ord`, gives wrong answers but never
/// undefined behavior: whatever it returns, the map reads only its own
/// entries.
///
/// # Examples
///
/// ```
/// use bbtree::{BBTreeMap, InlineKey, InlineWord, WithInlineKey};
///
/// #[derive(PartialEq, Eq, PartialOrd, Ord)]
/// struct UserName(String);
///
/// impl InlineKey for UserName {
///     fn inline_key_at(&self, offset: usize) -> InlineWord {
///         self.0.inline_key_at(offset)
///     }
/// }
///
/// let mut names = BBTreeMap::with_strategy(WithInlineKey);
/// names.insert(UserName("ferris".to_string()), 7);
/// assert_eq!(names.get(&UserName("ferris".to_string())), Some(&7));
/// ```
pub trait InlineKey: Ord {
    /// How many bytes of each inline key the nodes keep (see
    /// [`InlineWidth`]).
    const WIDTH: InlineWidth = InlineWidth::Eight;

    /// This key's inline key at `offset`: bytes `offset..offset + 8` of its
    /// encoding, big-endian, zero past its end.
    fn inline_key_at(&self, offset: usize) -> InlineWord;
}

/// Its bytes are its encoding: byte strings order like their bytes.
impl InlineKey for [u8] {
    /// With loads of a fixed size only, so no call to `memcpy`: the 8 bytes
    /// at `offset` if there are as many; otherwise the last 8, shifted so that
    /// the bytes from `offset` lead and zeros follow; or, for a string of
    /// fewer than 8 bytes, overlapping 4-, 2- or 1-byte loads, shifted the
    /// same way.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        const WINDOW_LEN: usize = size_of::<InlineWord>();
        if let Some(window) = self
            .get(offset..)
            .and_then(<[u8]>::first_chunk::<WINDOW_LEN>)
        {
            return InlineWord::from_be_bytes(*window);
        }
        let (word, word_start) = match self.last_chunk::<WINDOW_LEN>() {
            Some(last) => (InlineWord::from_be_bytes(*last), self.len() - WINDOW_LEN),
            None => {
                let word = if let Some(first) = self.first_chunk::<4>() {
                    let last = self.last_chunk::<4>().expect("the first four bytes fit");
                    (InlineWord::from(u32::from_be_bytes(*first)) << 32)
                        | (InlineWord::from(u32::from_be_bytes(*last)) << (8 * (8 - self.len())))
                } else if let Some(first) = self.first_chunk::<2>() {
                    let last = *self.last().expect("the first two bytes fit");
                    (InlineWord::from(u16::from_be_bytes(*first)) << 48)
                        | (InlineWord::from(last) << (8 * (8 - self.len())))
                } else {
                    InlineWord::from(self.first().copied().unwrap_or(0)) << 56
                };
                (word, 0)
            }
        };
        // The window starts past `word_start`, as it didn't fit.
        let shift_bytes = (offset - word_start).min(WINDOW_LEN);
        word.checked_shl((8 * shift_bytes) as u32).unwrap_or(0)
    }
}

/// That of its UTF-8 bytes, which order like the string.
impl InlineKey for str {
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.as_bytes().inline_key_at(offset)
    }
}

impl InlineKey for String {
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.as_bytes().inline_key_at(offset)
    }
}

impl InlineKey for Vec<u8> {
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.as_slice().inline_key_at(offset)
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

    /// The bytes before `offset` shifted out, which leaves the window on
    /// top, zeros after the last byte.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        let shift_bits = u32::try_from(offset).map_or(u32::MAX, |bytes| bytes.saturating_mul(8));
        self.checked_shl(shift_bits).unwrap_or(0)
    }
}

/// Its big-endian bytes, which order like it. The nodes keep 2 of them, past
/// the bytes that a node's keys share.
impl InlineKey for u128 {
    const WIDTH: InlineWidth = InlineWidth::Two;

    /// The bytes before `offset` shifted out, which leaves the window in the
    /// top 8 bytes, zeros after the last byte.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        let shift_bits = u32::try_from(offset).map_or(u32::MAX, |bytes| bytes.saturating_mul(8));
        let shifted = self.checked_shl(shift_bits).unwrap_or(0);
        (shifted >> InlineWord::BITS) as InlineWord
    }
}

/// That of its bits with the sign bit flipped, as a `u64`: big-endian bytes
/// that order like it, negatives first.
impl InlineKey for i64 {
    const WIDTH: InlineWidth = InlineWidth::Two;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (self.cast_unsigned() ^ (1 << 63)).inline_key_at(offset)
    }
}

/// That of its bits with the sign bit flipped, as a `u128`: big-endian bytes
/// that order like it, negatives first.
impl InlineKey for i128 {
    const WIDTH: InlineWidth = InlineWidth::Two;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (self.cast_unsigned() ^ (1 << 127)).inline_key_at(offset)
    }
}

/// The referenced key's: covers `&str` and `&[u8]`.
impl<T: InlineKey + ?Sized> InlineKey for &T {
    const WIDTH: InlineWidth = T::WIDTH;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (**self).inline_key_at(offset)
    }
}

/// The boxed key's: covers `Box<str>` and `Box<[u8]>`.
impl<T: InlineKey + ?Sized> InlineKey for Box<T> {
    const WIDTH: InlineWidth = T::WIDTH;

    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (**self).inline_key_at(offset)
    }
}

/// The default strategy: std byte-string and integer keys have inline keys,
/// other keys have none, at no cost.
///
/// The key types recognized are the byte strings `String`, `str`,
/// `Vec<u8>`, `[u8]`, `Box<str>`, `Box<[u8]>`, `&str`, `&[u8]`, `[u8; 16]`,
/// `[u8; 20]` and `[u8; 32]`, and the integers `u64`, `i64`, `u128` and
/// `i128`, through their [`InlineKey`] impls, by their type alone: a newtype
/// around one of them is another type, and needs [`WithInlineKey`]. A lookup
/// uses the inline keys when the type it looks up with is recognized too, as
/// `&str` is for `String` keys; otherwise it compares full keys, as a map
/// without inline keys does.
/// Fixed byte arrays keep two bytes per inline key. Heap-backed byte strings
/// keep eight in leaves to limit allocation reads on prefix ties, and four
/// in internal nodes to keep routing metadata compact.
#[derive(Clone, Copy, Debug, Default)]
pub struct Auto;

/// The opt-in strategy: every key has the inline key its [`InlineKey`] impl
/// gives, of which the nodes keep [`InlineKey::WIDTH`] bytes.
///
/// A lookup uses the inline keys when it looks up with the key type itself.
/// A lookup through another borrowed form of the key (a `Q` other than `K`
/// in [`BBTreeMap::get`]) has no inline key to compare, and compares full
/// keys instead: inline keys only speed up the search, so the answer is the
/// same.
///
/// [`BBTreeMap::get`]: crate::BBTreeMap::get
#[derive(Clone, Copy, Debug, Default)]
pub struct WithInlineKey;

/// Which keys of a map with keys `K` have inline keys: [`Auto`] or
/// [`WithInlineKey`], the only implementations.
pub trait InlineKeyStrategy<K>: sealed::Sealed<K> {}

impl<K> InlineKeyStrategy<K> for Auto {}

impl<K: InlineKey> InlineKeyStrategy<K> for WithInlineKey {}

/// What a strategy tells the nodes, kept out of the public API.
pub(crate) mod sealed {
    use super::{Auto, InlineKey, InlineWidth, InlineWord, WithInlineKey};

    /// Where a strategy's inline keys come from. Plain data: the node code
    /// resolves it for a key type (`node/inline_keys.rs`), and page layouts
    /// depend on the answer.
    pub enum InlineKeySource<K> {
        /// Std byte strings and integers, recognized by their type, have
        /// inline keys: windows of a byte string's bytes, and an integer's
        /// own [`InlineKey`] impl. Other keys have none ([`Auto`]).
        RecognizedStdTypes,
        /// Every key has the inline keys its [`InlineKey`] impl gives, of
        /// which the nodes keep `width` bytes ([`WithInlineKey`]).
        InlineKeyTrait {
            inline_key_at: fn(&K, usize) -> InlineWord,
            width: InlineWidth,
        },
    }

    pub trait Sealed<K> {
        const SOURCE: InlineKeySource<K>;
    }

    impl<K> Sealed<K> for Auto {
        const SOURCE: InlineKeySource<K> = InlineKeySource::RecognizedStdTypes;
    }

    impl<K: InlineKey> Sealed<K> for WithInlineKey {
        const SOURCE: InlineKeySource<K> = InlineKeySource::InlineKeyTrait {
            inline_key_at: K::inline_key_at,
            width: K::WIDTH,
        };
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    use super::{InlineKey, InlineWidth, InlineWord};
    use crate::node::test_support::TestRng;

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
