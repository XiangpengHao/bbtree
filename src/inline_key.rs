//! Inline keys (`doc/04-inline-key.md`, D18): register-sized windows of a
//! key's encoding that nodes store beside their keys and search first, so
//! that a search reads the full keys, and the heap bytes behind them, only
//! where inline keys tie.
//!
//! - [`InlineKey`] is how a key type gives its keys inline keys; std byte
//!   strings implement it.
//! - The map's strategy parameter, [`InlineKeyStrategy`], decides which keys
//!   have them: with [`Auto`], the default, std byte strings do, recognized
//!   by their type; with [`WithInlineKey`], every key does, through its
//!   [`InlineKey`] impl.

#![forbid(unsafe_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;

/// An inline key: bytes of a key's encoding, big-endian, so that inline keys
/// compare like the bytes they come from (see [`InlineKey`]).
///
/// 8 bytes for now. Its width is this one type: `u128` makes it 16.
pub type InlineWord = u64;

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
/// A key wrapping a byte string can window its bytes. A key ordered by
/// several fields encodes them one after the other: a `u32` and then a
/// string as the integer's big-endian bytes followed by the string's.
///
/// Implementing it is not enough for a map to use it: the map also needs the
/// [`WithInlineKey`] strategy. Std byte strings get inline keys by default
/// ([`Auto`]).
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
    /// This key's inline key at `offset`: bytes `offset..offset + 8` of its
    /// encoding, big-endian, zero past its end.
    fn inline_key_at(&self, offset: usize) -> InlineWord;
}

/// Its bytes are its encoding: byte strings order like their bytes.
impl InlineKey for [u8] {
    /// With loads of a fixed size only, so no call to `memcpy`: the 8 bytes
    /// at `offset` if there are as many; otherwise the last 8, shifted so that
    /// the bytes from `offset` lead and zeros follow; or, for a string of
    /// fewer than 8 bytes, its bytes one by one, shifted the same way.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        const WIDTH: usize = size_of::<InlineWord>();
        if let Some(window) = self.get(offset..).and_then(<[u8]>::first_chunk::<WIDTH>) {
            return InlineWord::from_be_bytes(*window);
        }
        let (word, word_start) = match self.last_chunk::<WIDTH>() {
            Some(last) => (InlineWord::from_be_bytes(*last), self.len() - WIDTH),
            None => {
                let padded = core::array::from_fn(|index| self.get(index).copied().unwrap_or(0));
                (InlineWord::from_be_bytes(padded), 0)
            }
        };
        // The window starts past `word_start`, as it didn't fit.
        let shift_bytes = (offset - word_start).min(WIDTH);
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

/// The referenced key's: covers `&str` and `&[u8]`.
impl<T: InlineKey + ?Sized> InlineKey for &T {
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (**self).inline_key_at(offset)
    }
}

/// The boxed key's: covers `Box<str>` and `Box<[u8]>`.
impl<T: InlineKey + ?Sized> InlineKey for Box<T> {
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        (**self).inline_key_at(offset)
    }
}

/// The default strategy: std byte-string keys have inline keys, other keys
/// have none, at no cost.
///
/// The key types recognized are `String`, `str`, `Vec<u8>`, `[u8]`,
/// `Box<str>`, `Box<[u8]>`, `&str`, `&[u8]`, `[u8; 16]`, `[u8; 20]` and
/// `[u8; 32]`, by their type alone: a newtype around one of them is another
/// type, and needs [`WithInlineKey`]. A lookup uses the inline keys when the
/// type it looks up with is recognized too, as `&str` is for `String` keys;
/// otherwise it compares full keys, as a map without inline keys does.
#[derive(Clone, Copy, Debug, Default)]
pub struct Auto;

/// The opt-in strategy: every key has the inline key its [`InlineKey`] impl
/// gives.
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
    use super::{Auto, InlineKey, InlineWord, WithInlineKey};

    /// Where a strategy's inline keys come from. Plain data: the node code
    /// resolves it for a key type (`node/inline_keys.rs`), and page layouts
    /// depend on the answer.
    pub enum InlineKeySource<K> {
        /// Std byte strings, recognized by their type, have windows of their
        /// bytes as inline keys; other keys have none ([`Auto`]).
        StdByteStrings,
        /// Every key has the inline keys its [`InlineKey`] impl gives
        /// ([`WithInlineKey`]).
        InlineKeyTrait(fn(&K, usize) -> InlineWord),
    }

    pub trait Sealed<K> {
        const SOURCE: InlineKeySource<K>;
    }

    impl<K> Sealed<K> for Auto {
        const SOURCE: InlineKeySource<K> = InlineKeySource::StdByteStrings;
    }

    impl<K: InlineKey> Sealed<K> for WithInlineKey {
        const SOURCE: InlineKeySource<K> = InlineKeySource::InlineKeyTrait(K::inline_key_at);
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    use super::{InlineKey, InlineWord};
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
}
