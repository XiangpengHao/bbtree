//! Inline keys (`doc/04-inline-key.md`, D18): a register-sized summary of a
//! key that nodes store beside their keys and search first, so that a search
//! reads the full keys, and the heap bytes behind them, only where inline
//! keys tie.
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

/// An inline key: the first bytes of a byte string, big-endian, so that
/// inline keys compare like the bytes they come from.
///
/// 8 bytes for now. Its width is this one type: `u128` makes it 16.
pub type InlineWord = u64;

/// A key type whose keys have inline keys: a key summarized in an
/// [`InlineWord`] that orders like the key wherever two summaries differ.
///
/// The contract: if `a.inline_key() != b.inline_key()`, then `a` and `b`
/// order like their inline keys, that is,
/// `a.cmp(&b) == a.inline_key().cmp(&b.inline_key())`. Equal inline keys say
/// nothing, and the map compares the full keys.
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
///     fn inline_key(&self) -> InlineWord {
///         self.0.inline_key()
///     }
/// }
///
/// let mut names = BBTreeMap::with_strategy(WithInlineKey);
/// names.insert(UserName("ferris".to_string()), 7);
/// assert_eq!(names.get(&UserName("ferris".to_string())), Some(&7));
/// ```
pub trait InlineKey: Ord {
    /// This key's inline key.
    fn inline_key(&self) -> InlineWord;
}

/// The first `size_of::<InlineWord>()` bytes, big-endian, zero-padded: for
/// two byte strings whose inline keys differ, they differ within those
/// bytes, and the first difference orders both alike.
impl InlineKey for [u8] {
    #[inline]
    fn inline_key(&self) -> InlineWord {
        const WIDTH: usize = size_of::<InlineWord>();
        let leading_bytes = match self.first_chunk::<WIDTH>() {
            Some(first) => *first,
            None => {
                let mut padded = [0; WIDTH];
                padded[..self.len()].copy_from_slice(self);
                padded
            }
        };
        InlineWord::from_be_bytes(leading_bytes)
    }
}

/// That of its UTF-8 bytes, which order like the string.
impl InlineKey for str {
    #[inline]
    fn inline_key(&self) -> InlineWord {
        self.as_bytes().inline_key()
    }
}

impl InlineKey for String {
    #[inline]
    fn inline_key(&self) -> InlineWord {
        self.as_bytes().inline_key()
    }
}

impl InlineKey for Vec<u8> {
    #[inline]
    fn inline_key(&self) -> InlineWord {
        self.as_slice().inline_key()
    }
}

impl<const N: usize> InlineKey for [u8; N] {
    #[inline]
    fn inline_key(&self) -> InlineWord {
        self.as_slice().inline_key()
    }
}

/// The referenced key's: covers `&str` and `&[u8]`.
impl<T: InlineKey + ?Sized> InlineKey for &T {
    #[inline]
    fn inline_key(&self) -> InlineWord {
        (**self).inline_key()
    }
}

/// The boxed key's: covers `Box<str>` and `Box<[u8]>`.
impl<T: InlineKey + ?Sized> InlineKey for Box<T> {
    #[inline]
    fn inline_key(&self) -> InlineWord {
        (**self).inline_key()
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
        /// Std byte strings, recognized by their type, have their leading
        /// bytes as inline keys; other keys have none ([`Auto`]).
        StdByteStrings,
        /// Every key has the inline key its [`InlineKey`] impl gives
        /// ([`WithInlineKey`]).
        InlineKeyTrait(fn(&K) -> InlineWord),
    }

    pub trait Sealed<K> {
        const SOURCE: InlineKeySource<K>;
    }

    impl<K> Sealed<K> for Auto {
        const SOURCE: InlineKeySource<K> = InlineKeySource::StdByteStrings;
    }

    impl<K: InlineKey> Sealed<K> for WithInlineKey {
        const SOURCE: InlineKeySource<K> = InlineKeySource::InlineKeyTrait(K::inline_key);
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
    fn leading_bytes_big_endian_and_zero_padded() {
        assert_eq!("".inline_key(), 0);
        assert_eq!("a".inline_key(), 0x6100_0000_0000_0000);
        assert_eq!("abc".inline_key(), 0x6162_6300_0000_0000);
        assert_eq!(b"abcdefgh".inline_key(), 0x6162_6364_6566_6768);
        assert_eq!(b"abcdefghij".inline_key(), b"abcdefgh".inline_key());
        assert_eq!([0xff_u8; 3].inline_key(), 0xffff_ff00_0000_0000);
    }

    #[test]
    fn std_byte_strings_agree() {
        let text = "inline keys";
        let expected = text.as_bytes().inline_key();
        assert_eq!(text.inline_key(), expected);
        assert_eq!(String::from(text).inline_key(), expected);
        assert_eq!(Vec::from(text).inline_key(), expected);
        assert_eq!(Box::<str>::from(text).inline_key(), expected);
        assert_eq!(Box::<[u8]>::from(text.as_bytes()).inline_key(), expected);
        assert_eq!((&text).inline_key(), expected);
        assert_eq!((&text.as_bytes()).inline_key(), expected);
        let array: [u8; 11] = text.as_bytes().try_into().unwrap();
        assert_eq!(array.inline_key(), expected);
    }

    /// The contract, checked on random short byte strings over a small
    /// alphabet with zero bytes, where prefixes, padding and ties are common.
    #[test]
    fn differing_inline_keys_order_like_the_bytes() {
        let rng = TestRng::new(41);
        let random_bytes = || -> Vec<u8> {
            let len = rng.below(12);
            (0..len).map(|_| [0, 1, b'a', 0xff][rng.below(4)]).collect()
        };
        let rounds = if cfg!(miri) { 300 } else { 30_000 };
        let (mut ties, mut decided) = (0, 0);
        for _ in 0..rounds {
            let (a, b) = (random_bytes(), random_bytes());
            let (inline_a, inline_b): (InlineWord, InlineWord) = (a.inline_key(), b.inline_key());
            if inline_a == inline_b {
                ties += 1;
            } else {
                decided += 1;
                assert_eq!(inline_a.cmp(&inline_b), a.cmp(&b), "{a:?} vs {b:?}");
            }
        }
        assert!(ties > 0 && decided > 0);
    }
}
