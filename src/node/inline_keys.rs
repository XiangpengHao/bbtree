//! Inline keys inside the nodes (D18): which keys have them, what a node
//! stores beside each key, and what a search looks for.
//!
//! - [`KeyInliner`] resolves a map's strategy for its key type, once and for
//!   all at compile time: how its keys get inline keys, or that they have
//!   none. Page layouts follow from it (`page.rs`).
//! - [`InlinedKey`] is a key outside the nodes with the inline key it goes
//!   into a node with.
//! - [`SearchKey`] is what a search looks for: a key, and its inline key if
//!   the nodes can compare it, computed once for a whole descent.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::marker::PhantomData;
use core::mem::size_of;

use super::page::{ByteString, SameType};
use crate::inline_key::sealed::InlineKeySource;
use crate::inline_key::{InlineKey, InlineKeyStrategy, InlineWord};

/// How keys `K` get their inline keys under a map's strategy: the strategy's
/// [`InlineKeySource`], resolved for `K`.
pub(super) enum KeyInliner<K> {
    /// `K` is a std byte string ([`Auto`]): its inline key is its leading
    /// bytes.
    ///
    /// [`Auto`]: crate::Auto
    ByteString(ByteString<K>),
    /// `K`'s [`InlineKey`] impl ([`WithInlineKey`]).
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    InlineKeyTrait(fn(&K) -> InlineWord),
}

impl<K> Clone for KeyInliner<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for KeyInliner<K> {}

impl<K> KeyInliner<K> {
    /// How keys `K` get inline keys under strategy `S`, or `None` if they
    /// have none.
    ///
    /// A pure function of `K` and `S`, the strategy's constant source and
    /// `K`'s type identity, which folds to a constant after
    /// monomorphization: every page of a map gets the same layout from it,
    /// and must (`page.rs`).
    #[inline]
    pub(super) fn resolve<S: InlineKeyStrategy<K>>() -> Option<Self> {
        match S::SOURCE {
            InlineKeySource::StdByteStrings => ByteString::recognize().map(Self::ByteString),
            InlineKeySource::InlineKeyTrait(inline_key) => Some(Self::InlineKeyTrait(inline_key)),
        }
    }

    /// `key`'s inline key: what a node stores beside it.
    #[inline]
    pub(super) fn inline_key(self, key: &K) -> InlineWord {
        match self {
            Self::ByteString(byte_string) => byte_string.bytes(key).inline_key(),
            Self::InlineKeyTrait(inline_key) => inline_key(key),
        }
    }

    /// The inline key of `key`, a lookup key of type `Q`, if it compares
    /// with the stored keys' inline keys: for std byte-string keys, if `Q`
    /// is a std byte string too, ordered by its bytes like them; for keys
    /// with their own impl, if `Q` is `K` itself. `None` otherwise: the
    /// search then compares full keys.
    #[inline]
    fn target<Q: ?Sized>(self, key: &Q) -> Option<InlineTarget>
    where
        K: Borrow<Q>,
    {
        match self {
            Self::ByteString(_) => {
                let bytes = ByteString::<Q>::recognize()?.bytes(key);
                Some(InlineTarget {
                    inline_key: bytes.inline_key(),
                    whole_len: InlineTarget::whole_len(bytes),
                })
            }
            Self::InlineKeyTrait(inline_key) => {
                let key = SameType::<K, Q>::check()?.cast(key);
                Some(InlineTarget {
                    inline_key: inline_key(key),
                    whole_len: None,
                })
            }
        }
    }

    /// A stored key's length, if its inline key holds all of its bytes:
    /// see [`InlineTarget::whole_len`]. Only std byte strings have one, and
    /// it comes from the key itself, in the node, not from the bytes behind
    /// it.
    #[inline]
    fn whole_len(self, key: &K) -> Option<usize> {
        match self {
            Self::ByteString(byte_string) => InlineTarget::whole_len(byte_string.bytes(key)),
            Self::InlineKeyTrait(_) => None,
        }
    }
}

/// The inline key of a key being searched for.
#[derive(Clone, Copy, Debug)]
pub(super) struct InlineTarget {
    inline_key: InlineWord,
    /// The key's length if its inline key holds all of its bytes: a std
    /// byte string of at most `size_of::<InlineWord>()` bytes. A stored std
    /// byte string with the same inline key and the same length is then
    /// equal to it, which the search tells without reading either's bytes.
    whole_len: Option<usize>,
}

impl InlineTarget {
    #[inline]
    pub(super) fn inline_key(self) -> InlineWord {
        self.inline_key
    }

    /// `bytes.len()`, if the inline key of `bytes` holds all of them.
    #[inline]
    fn whole_len(bytes: &[u8]) -> Option<usize> {
        (bytes.len() <= size_of::<InlineWord>()).then_some(bytes.len())
    }
}

/// A key outside the nodes, with the inline key it goes into a node with
/// (D18), for a map with strategy `S`.
///
/// A new key gets its inline key computed once, before the insert changes
/// anything, so a panicking `InlineKey` leaves the map unchanged. A key moving
/// between nodes, such as a split's separator, keeps the one it had. In a map
/// whose keys have no inline keys, the inline key is 0, and no node stores
/// it.
pub(crate) struct InlinedKey<K, S> {
    key: K,
    inline_key: InlineWord,
    _strategy: PhantomData<fn() -> S>,
}

impl<K, S: InlineKeyStrategy<K>> InlinedKey<K, S> {
    pub(crate) fn new(key: K) -> Self {
        let inline_key = KeyInliner::resolve::<S>().map_or(0, |inliner| inliner.inline_key(&key));
        Self::from_parts(key, inline_key)
    }
}

impl<K, S> InlinedKey<K, S> {
    /// A key and the inline key it had in a node.
    pub(super) fn from_parts(key: K, inline_key: InlineWord) -> Self {
        Self {
            key,
            inline_key,
            _strategy: PhantomData,
        }
    }

    pub(super) fn into_parts(self) -> (K, InlineWord) {
        (self.key, self.inline_key)
    }

    pub(crate) fn key(&self) -> &K {
        &self.key
    }

    pub(crate) fn into_key(self) -> K {
        self.key
    }
}

/// What a search looks for: `key`, of a type `Q` that keys `K` borrow as,
/// and its inline key if the nodes of a map with strategy `S` can compare it
/// (D18).
///
/// Made once per descent, so that the inline key is computed once, not once
/// per node.
pub(crate) struct SearchKey<'q, K, S, Q: ?Sized> {
    key: &'q Q,
    inline: Option<InlineTarget>,
    _map: PhantomData<fn() -> (K, S)>,
}

impl<'q, K, S, Q> SearchKey<'q, K, S, Q>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    Q: ?Sized,
{
    /// Looks for `key`, computing its inline key.
    #[inline]
    pub(crate) fn new(key: &'q Q) -> Self {
        Self {
            key,
            inline: KeyInliner::resolve::<S>().and_then(|inliner| inliner.target(key)),
            _map: PhantomData,
        }
    }
}

impl<'q, K, S: InlineKeyStrategy<K>> SearchKey<'q, K, S, K> {
    /// Looks for a key being inserted, with the inline key it already has.
    #[inline]
    pub(crate) fn for_inlined(key: &'q InlinedKey<K, S>) -> Self {
        Self {
            key: &key.key,
            inline: KeyInliner::resolve::<S>().map(|inliner| InlineTarget {
                inline_key: key.inline_key,
                whole_len: inliner.whole_len(&key.key),
            }),
            _map: PhantomData,
        }
    }
}

impl<K, S: InlineKeyStrategy<K>, Q: ?Sized> SearchKey<'_, K, S, Q> {
    pub(super) fn key(&self) -> &Q {
        self.key
    }

    /// The key's inline key, if the nodes can compare it with theirs.
    pub(super) fn inline_target(&self) -> Option<InlineTarget> {
        self.inline
    }

    /// Whether `stored`, a key whose inline key equals this key's, is equal
    /// to this key, told from their lengths alone: so it is if both are std
    /// byte strings of the same length that their inline keys hold whole.
    /// `false` means "compare them".
    #[inline]
    pub(super) fn is_equal_by_length(&self, stored: &K) -> bool {
        let Some(whole_len) = self.inline.and_then(|target| target.whole_len) else {
            return false;
        };
        KeyInliner::resolve::<S>()
            .is_some_and(|inliner| inliner.whole_len(stored) == Some(whole_len))
    }

    /// How this key orders against `stored`, a key whose inline key equals
    /// this key's: equal if their lengths tell ([`Self::is_equal_by_length`]);
    /// for byte arrays, word by word ([`EqualLengthBytes`]), with no call to
    /// `memcmp`; otherwise by `Q`'s `Ord`. Byte arrays order like their
    /// bytes, as every std byte string does.
    #[inline]
    pub(super) fn cmp_tied(&self, stored: &K) -> Ordering
    where
        K: Borrow<Q>,
        Q: Ord,
    {
        if self.is_equal_by_length(stored) {
            return Ordering::Equal;
        }
        if let Some(KeyInliner::ByteString(stored_bytes)) = KeyInliner::<K>::resolve::<S>()
            && let Some(search_bytes) = ByteString::<Q>::recognize()
            && stored_bytes.is_array()
            && search_bytes.is_array()
            && let Some(bytes) =
                EqualLengthBytes::new(search_bytes.bytes(self.key), stored_bytes.bytes(stored))
        {
            return bytes.cmp();
        }
        self.key.cmp(stored.borrow())
    }
}

/// Two byte strings of the same length, compared a word at a time: their
/// whole 8-byte words as big-endian `u64`s, then the bytes left over,
/// zero-padded into one more. For byte arrays, whose length is a constant,
/// the compiler unrolls the loop into one compare per word, with no call to
/// `memcmp`, which std's comparison of byte slices makes.
struct EqualLengthBytes<'a> {
    left: &'a [u8],
    right: &'a [u8],
}

impl<'a> EqualLengthBytes<'a> {
    /// The pair, if `left` and `right` have the same length.
    #[inline]
    fn new(left: &'a [u8], right: &'a [u8]) -> Option<Self> {
        (left.len() == right.len()).then_some(Self { left, right })
    }

    /// Orders `left` against `right` like `<[u8]>::cmp`. The bytes after
    /// the whole words, fewer than 8 and as many on both sides, order like
    /// their inline keys, which are those bytes zero-padded.
    #[inline]
    fn cmp(self) -> Ordering {
        let (left_words, left_tail) = self.left.as_chunks::<8>();
        let (right_words, right_tail) = self.right.as_chunks::<8>();
        for (left_word, right_word) in left_words.iter().zip(right_words) {
            let order = u64::from_be_bytes(*left_word).cmp(&u64::from_be_bytes(*right_word));
            if order.is_ne() {
                return order;
            }
        }
        left_tail.inline_key().cmp(&right_tail.inline_key())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::ToOwned;
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    use super::{EqualLengthBytes, InlinedKey, KeyInliner, SearchKey};
    use crate::inline_key::{Auto, InlineKey, InlineWord, WithInlineKey};
    use crate::node::test_support::TestRng;

    /// A newtype with its own inline key, which looks up by `str` too.
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct UserName(String);

    impl InlineKey for UserName {
        fn inline_key(&self) -> InlineWord {
            self.0.inline_key()
        }
    }

    impl core::borrow::Borrow<str> for UserName {
        fn borrow(&self) -> &str {
            &self.0
        }
    }

    fn has_inline_keys<K, S: crate::InlineKeyStrategy<K>>() -> bool {
        KeyInliner::<K>::resolve::<S>().is_some()
    }

    #[test]
    fn auto_gives_std_byte_strings_inline_keys() {
        assert!(has_inline_keys::<String, Auto>());
        assert!(has_inline_keys::<Vec<u8>, Auto>());
        assert!(has_inline_keys::<Box<str>, Auto>());
        assert!(has_inline_keys::<Box<[u8]>, Auto>());
        assert!(has_inline_keys::<&str, Auto>());
        assert!(has_inline_keys::<&'static [u8], Auto>());
        assert!(has_inline_keys::<[u8; 16], Auto>());
        assert!(has_inline_keys::<[u8; 20], Auto>());
        assert!(has_inline_keys::<[u8; 32], Auto>());

        assert!(!has_inline_keys::<u64, Auto>());
        assert!(!has_inline_keys::<[u8; 8], Auto>());
        assert!(!has_inline_keys::<Vec<u16>, Auto>());
        assert!(!has_inline_keys::<&String, Auto>());
        assert!(!has_inline_keys::<UserName, Auto>());
        assert!(!has_inline_keys::<(String, u8), Auto>());
    }

    #[test]
    fn with_inline_key_gives_every_key_inline_keys() {
        assert!(has_inline_keys::<UserName, WithInlineKey>());
        assert!(has_inline_keys::<String, WithInlineKey>());
    }

    /// The inline key a search for `key` among keys `K` compares, if any.
    fn target<K: core::borrow::Borrow<Q>, S: crate::InlineKeyStrategy<K>, Q: ?Sized>(
        key: &Q,
    ) -> Option<InlineWord> {
        SearchKey::<K, S, Q>::new(key)
            .inline_target()
            .map(|target| target.inline_key())
    }

    #[test]
    fn lookups_by_any_recognized_form_use_inline_keys() {
        let expected = Some("ferris".inline_key());
        assert_eq!(
            target::<String, Auto, String>(&"ferris".to_owned()),
            expected
        );
        assert_eq!(target::<String, Auto, str>("ferris"), expected);
        assert_eq!(target::<Box<str>, Auto, str>("ferris"), expected);
        assert_eq!(target::<&str, Auto, &str>(&"ferris"), expected);
        assert_eq!(target::<&str, Auto, str>("ferris"), expected);
        assert_eq!(target::<Vec<u8>, Auto, [u8]>(b"ferris"), expected);
        let array = [7_u8; 16];
        assert_eq!(
            target::<[u8; 16], Auto, [u8]>(&array),
            Some(array.inline_key())
        );
        // Keys without inline keys have no target.
        assert_eq!(target::<u64, Auto, u64>(&7), None);
    }

    #[test]
    fn with_inline_key_lookups_by_another_form_compare_full_keys() {
        let name = UserName("ferris".to_owned());
        assert_eq!(
            target::<UserName, WithInlineKey, UserName>(&name),
            Some(name.inline_key())
        );
        assert_eq!(target::<UserName, WithInlineKey, str>("ferris"), None);
    }

    #[test]
    fn short_byte_strings_are_equal_by_length() {
        let short: SearchKey<'_, String, Auto, str> = SearchKey::new("ferris");
        assert!(short.is_equal_by_length(&"ferris".to_owned()));
        assert!(!short.is_equal_by_length(&"ferri".to_owned()));
        // Same inline key, other length: compared.
        assert!(!short.is_equal_by_length(&"ferris\0".to_owned()));

        let long: SearchKey<'_, String, Auto, str> = SearchKey::new("ferris the crab");
        assert!(!long.is_equal_by_length(&"ferris the crab".to_owned()));

        let name = UserName("ferris".to_owned());
        let opted_in: SearchKey<'_, UserName, WithInlineKey, UserName> = SearchKey::new(&name);
        assert!(!opted_in.is_equal_by_length(&UserName("ferris".to_owned())));
    }

    #[test]
    fn inlined_keys_carry_their_inline_key() {
        let key: InlinedKey<String, Auto> = InlinedKey::new("ferris".to_owned());
        let search = SearchKey::for_inlined(&key);
        let target = search.inline_target().map(|target| target.inline_key());
        assert_eq!(target, Some("ferris".inline_key()));
        assert!(search.is_equal_by_length(&"ferris".to_owned()));
        assert_eq!(key.into_parts().1, "ferris".inline_key());

        let plain: InlinedKey<u64, Auto> = InlinedKey::new(7);
        assert_eq!(plain.into_parts(), (7, 0));
    }

    /// Word by word, byte strings of the same length order like their
    /// bytes, whichever word or tail byte they first differ in, and equal
    /// ones are equal; strings of different lengths aren't paired.
    #[test]
    fn equal_length_bytes_order_like_slices() {
        let rng = TestRng::new(43);
        for len in [0, 3, 8, 16, 20, 32] {
            for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
                let left: Vec<u8> = (0..len).map(|_| [0, 1, 0xff][rng.below(3)]).collect();
                let mut right = left.clone();
                if len > 0 && rng.below(2) == 0 {
                    right[rng.below(len)] = [0, 1, 0xff][rng.below(3)];
                }
                let bytes = EqualLengthBytes::new(&left, &right).unwrap();
                assert_eq!(bytes.cmp(), left.cmp(&right), "{left:?} vs {right:?}");
            }
        }
        assert!(EqualLengthBytes::new(&[1, 2], &[1, 2, 3]).is_none());
    }

    /// Byte-array keys compare word by word, and agree with their `Ord`.
    #[test]
    fn tied_byte_arrays_compare_like_their_ord() {
        let rng = TestRng::new(44);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let mut search = [7_u8; 20];
            let mut stored = search;
            search[rng.below(20)] = rng.below(256) as u8;
            stored[rng.below(20)] = rng.below(256) as u8;
            let key: SearchKey<'_, [u8; 20], Auto, [u8; 20]> = SearchKey::new(&search);
            assert_eq!(key.cmp_tied(&stored), search.cmp(&stored));
        }
    }
}
