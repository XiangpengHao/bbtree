//! Inline keys inside the nodes (D18): which keys have them, what a node
//! stores beside each key, and what a search looks for.
//!
//! A node stores each key's inline key at the node's prefix length: the 8
//! bytes of the key's encoding past the bytes that the node's fences share,
//! and so every key in it (05-fence-key.md).
//!
//! - [`KeyInliner`] resolves a map's strategy for its key type, once and for
//!   all at compile time: how its keys get inline keys, or that they have
//!   none. Page layouts follow from it (`page.rs`).
//! - [`Window`] is an inline key with the prefix length it is taken at; it
//!   moves back to a shorter prefix without reading the key.
//! - [`InlinedKey`] is a key outside the nodes with its window, for the node
//!   it goes into.
//! - [`SearchKey`] is what a search looks for: a key, and how to take its
//!   inline key at each node's prefix, if the nodes can compare it.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::marker::PhantomData;
use core::mem::size_of;

use super::page::{ByteString, InlineKeyNodeHeader, SameType};
use crate::inline_key::sealed::InlineKeySource;
use crate::inline_key::{InlineKey, InlineKeyStrategy, InlineWord};

/// The bytes an inline key holds.
const WIDTH: usize = size_of::<InlineWord>();

/// How keys `K` get their inline keys under a map's strategy: the strategy's
/// [`InlineKeySource`], resolved for `K`.
pub(super) enum KeyInliner<K> {
    /// `K` is a std byte string ([`Auto`]): its inline keys are windows of
    /// its bytes.
    ///
    /// [`Auto`]: crate::Auto
    ByteString(ByteString<K>),
    /// `K`'s [`InlineKey`] impl ([`WithInlineKey`]).
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    InlineKeyTrait(fn(&K, usize) -> InlineWord),
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
            InlineKeySource::InlineKeyTrait(inline_key_at) => {
                Some(Self::InlineKeyTrait(inline_key_at))
            }
        }
    }

    /// `key`'s inline key at `offset`: what a node whose keys share their
    /// first `offset` bytes stores beside it.
    #[inline]
    pub(super) fn inline_key_at(self, key: &K, offset: usize) -> InlineWord {
        match self {
            Self::ByteString(byte_string) => byte_string.bytes(key).inline_key_at(offset),
            Self::InlineKeyTrait(inline_key_at) => inline_key_at(key, offset),
        }
    }

    /// How many leading bytes the encodings of `a` and `b` share, both
    /// padded with zeros, compared a window at a time: a node bounded by
    /// `a` and `b` may skip that many bytes of its keys (05-fence-key.md).
    ///
    /// At most the longest prefix a node records: two different keys can
    /// have equal padded encodings (`"ab"` and `"ab\0"`, or a lossy
    /// [`InlineKey`] impl), and stopping short is always safe, as a shorter
    /// prefix is.
    pub(super) fn common_prefix_len(self, a: &K, b: &K) -> usize {
        const MAX_LEN: usize = InlineKeyNodeHeader::MAX_PREFIX_LEN;
        let mut offset = 0;
        while offset < MAX_LEN {
            let differing_bits = self.inline_key_at(a, offset) ^ self.inline_key_at(b, offset);
            if differing_bits != 0 {
                let shared_bytes = differing_bits.leading_zeros() as usize / 8;
                return (offset + shared_bytes).min(MAX_LEN);
            }
            offset += WIDTH;
        }
        MAX_LEN
    }

    /// The prefix length of a node whose fences are `low` and `high`: what
    /// they share, or 0 if either is missing, along the tree's borders.
    pub(super) fn fence_prefix_len(self, low: Option<&K>, high: Option<&K>) -> usize {
        match (low, high) {
            (Some(low), Some(high)) => self.common_prefix_len(low, high),
            _ => 0,
        }
    }

    /// How to take the inline key of `key`, a lookup key of type `Q`, at a
    /// node's prefix, if it compares with the stored keys' inline keys: for
    /// std byte-string keys, if `Q` is a std byte string too, ordered by its
    /// bytes like them; for keys with their own impl, if `Q` is `K` itself.
    /// `None` otherwise: the search then compares full keys.
    #[inline]
    fn target_source<'q, Q: ?Sized>(self, key: &'q Q) -> Option<TargetSource<'q, K>>
    where
        K: Borrow<Q>,
    {
        match self {
            Self::ByteString(_) => {
                let bytes = ByteString::<Q>::recognize()?.bytes(key);
                Some(TargetSource::Bytes(SearchBytes::new(bytes)))
            }
            Self::InlineKeyTrait(inline_key_at) => {
                let key = SameType::<K, Q>::check()?.cast(key);
                Some(TargetSource::Key(key, inline_key_at))
            }
        }
    }
}

/// An inline key with the prefix length it is taken at: bytes
/// `prefix_len..prefix_len + 8` of a key's encoding, as a node with that
/// prefix length stores it (05-fence-key.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Window {
    prefix_len: usize,
    inline_key: InlineWord,
}

impl Window {
    /// What a key without inline keys carries: nothing a node stores.
    pub(super) const NONE: Self = Self::new(0, 0);

    pub(super) const fn new(prefix_len: usize, inline_key: InlineWord) -> Self {
        Self {
            prefix_len,
            inline_key,
        }
    }

    pub(super) fn prefix_len(self) -> usize {
        self.prefix_len
    }

    pub(super) fn inline_key(self) -> InlineWord {
        self.inline_key
    }

    /// The same key's window at `fill`'s shorter prefix length, given
    /// `fill`, the window there of a key whose encoding shares this key's
    /// bytes up to this window's prefix length: the bytes that move in come
    /// from `fill`, so no key is read. This is how a node's prefix shrinks,
    /// and how a key moves into a node with a shorter one.
    ///
    /// A `fill` at a longer prefix length is a caller's bug, which gives
    /// wrong inline keys, and so wrong answers, but nothing worse.
    #[inline]
    pub(super) fn moved_back_to(self, fill: Self) -> Self {
        debug_assert!(
            fill.prefix_len <= self.prefix_len,
            "{fill:?} is past {self:?}"
        );
        let shift = self.prefix_len.saturating_sub(fill.prefix_len);
        let inline_key = if shift == 0 {
            self.inline_key
        } else if shift >= WIDTH {
            fill.inline_key
        } else {
            let shift_bits = 8 * shift as u32;
            let filled_bits = !(InlineWord::MAX >> shift_bits);
            (fill.inline_key & filled_bits) | (self.inline_key >> shift_bits)
        };
        Self::new(fill.prefix_len, inline_key)
    }
}

/// How a search takes its key's inline key at a node's prefix.
enum TargetSource<'q, K> {
    /// A std byte string ([`KeyInliner::ByteString`]).
    Bytes(SearchBytes<'q>),
    /// A key with its own impl ([`KeyInliner::InlineKeyTrait`]).
    Key(&'q K, fn(&K, usize) -> InlineWord),
}

impl<K> TargetSource<'_, K> {
    /// The key's inline key at a node's `prefix_len`.
    #[inline]
    fn inline_target(&self, prefix_len: usize) -> InlineTarget {
        let inline_key = match self {
            Self::Bytes(bytes) => bytes.inline_key_at(prefix_len),
            Self::Key(key, inline_key_at) => inline_key_at(key, prefix_len),
        };
        InlineTarget {
            inline_key,
            prefix_len,
        }
    }
}

/// How many leading bytes of a std byte string being searched for
/// [`SearchBytes`] copies: enough for the prefixes of the keys measured
/// (05-fence-key.md), of which paths share the most, 64 to 100 bytes in a
/// leaf. A node with a longer prefix reads the key itself.
const SEARCH_COPY_LEN: usize = 128;

/// The bytes of a std byte string being searched for, and a copy of the
/// first [`SEARCH_COPY_LEN`] of them, zero-padded past its end, made once
/// per search: the key's inline key at a node's prefix length is then one
/// load from the copy, with no length to check and nothing to pad.
struct SearchBytes<'q> {
    bytes: &'q [u8],
    padded: [u8; SEARCH_COPY_LEN],
}

impl<'q> SearchBytes<'q> {
    /// Copies `bytes` a window at a time, each a load of a fixed size, with
    /// no call to `memcpy`.
    #[inline]
    fn new(bytes: &'q [u8]) -> Self {
        let mut padded = [0; SEARCH_COPY_LEN];
        let (words, _) = padded.as_chunks_mut::<WIDTH>();
        for (index, word) in words.iter_mut().enumerate() {
            let offset = index * WIDTH;
            if offset >= bytes.len() {
                break;
            }
            *word = bytes.inline_key_at(offset).to_be_bytes();
        }
        Self { bytes, padded }
    }

    /// The key's inline key at `offset`.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        match self
            .padded
            .get(offset..)
            .and_then(<[u8]>::first_chunk::<WIDTH>)
        {
            Some(window) => InlineWord::from_be_bytes(*window),
            None => self.bytes.inline_key_at(offset),
        }
    }

    /// Orders the key against `stored`, a std byte string, as `<[u8]>::cmp`
    /// does, given that their first `equal_len` bytes, at least 8, are equal
    /// when padded with zeros: 8 bytes at a time from there, as big-endian
    /// words, then by length. Every load is of a fixed size, so there is no
    /// call to `memcmp`, and a string that ends within the equal bytes isn't
    /// read at all.
    ///
    /// The last word ends where the shorter string does and may start before
    /// the bytes left to compare: the bytes it repeats are equal.
    #[inline]
    fn cmp_from(&self, stored: &[u8], equal_len: usize) -> Ordering {
        let search = self.bytes;
        let both_len = search.len().min(stored.len());
        let mut compared = equal_len;
        while compared < both_len {
            let word_end = (compared + WIDTH).min(both_len);
            let word = |bytes: &[u8]| {
                bytes[..word_end]
                    .last_chunk::<WIDTH>()
                    .map_or(0, |word| InlineWord::from_be_bytes(*word))
            };
            match word(search).cmp(&word(stored)) {
                Ordering::Equal => compared = word_end,
                unequal => return unequal,
            }
        }
        search.len().cmp(&stored.len())
    }
}

/// The inline key of a key being searched for, at one node's prefix: two
/// words, which stay in registers on the way down.
#[derive(Clone, Copy, Debug)]
pub(super) struct InlineTarget {
    inline_key: InlineWord,
    /// The prefix length of the node it is for.
    prefix_len: usize,
}

impl InlineTarget {
    #[inline]
    pub(super) fn inline_key(self) -> InlineWord {
        self.inline_key
    }
}

/// A key outside the nodes, with its window for the node it goes into
/// (D18), for a map with strategy `S`. In a map whose keys have no inline
/// keys, the window is [`Window::NONE`], and no node stores it.
pub(crate) struct InlinedKey<K, S> {
    key: K,
    window: Window,
    _strategy: PhantomData<fn() -> S>,
}

impl<K, S: InlineKeyStrategy<K>> InlinedKey<K, S> {
    /// `key`, with its window at `prefix_len`: for a node with that prefix
    /// length. Calls `InlineKey` with [`WithInlineKey`], so callers make it
    /// before they change anything.
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    pub(crate) fn new(key: K, prefix_len: usize) -> Self {
        let window = KeyInliner::resolve::<S>().map_or(Window::NONE, |inliner| {
            Window::new(prefix_len, inliner.inline_key_at(&key, prefix_len))
        });
        Self::from_parts(key, window)
    }
}

impl<K, S> InlinedKey<K, S> {
    /// A key and the window it had in a node.
    pub(super) fn from_parts(key: K, window: Window) -> Self {
        Self {
            key,
            window,
            _strategy: PhantomData,
        }
    }

    pub(super) fn into_parts(self) -> (K, Window) {
        (self.key, self.window)
    }

    pub(super) fn window(&self) -> Window {
        self.window
    }

    /// The key with its window moved back to `fill`'s prefix length; see
    /// [`Window::moved_back_to`].
    pub(super) fn moved_back_to(self, fill: Window) -> Self {
        Self::from_parts(self.key, self.window.moved_back_to(fill))
    }

    pub(crate) fn key(&self) -> &K {
        &self.key
    }

    pub(crate) fn into_key(self) -> K {
        self.key
    }
}

/// What a search looks for: `key`, of a type `Q` that keys `K` borrow as,
/// and how to take its inline key at each node's prefix, if the nodes of a
/// map with strategy `S` can compare it (D18, 05-fence-key.md).
pub(crate) struct SearchKey<'q, K, S, Q: ?Sized> {
    key: &'q Q,
    source: Option<TargetSource<'q, K>>,
    _map: PhantomData<fn() -> (K, S)>,
}

impl<'q, K, S, Q> SearchKey<'q, K, S, Q>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    Q: ?Sized,
{
    /// Looks for `key`.
    #[inline]
    pub(crate) fn new(key: &'q Q) -> Self {
        Self {
            key,
            source: KeyInliner::resolve::<S>().and_then(|inliner| inliner.target_source(key)),
            _map: PhantomData,
        }
    }
}

impl<K, S: InlineKeyStrategy<K>, Q: ?Sized> SearchKey<'_, K, S, Q> {
    pub(super) fn key(&self) -> &Q {
        self.key
    }

    /// The key's inline key at a node's `prefix_len`, if the nodes can
    /// compare it with theirs. Calls `InlineKey` with
    /// [`WithInlineKey`](crate::WithInlineKey), as searches do.
    #[inline]
    pub(super) fn inline_target(&self, prefix_len: usize) -> Option<InlineTarget> {
        self.source
            .as_ref()
            .map(|source| source.inline_target(prefix_len))
    }

    /// How this key orders against `stored`, a key of the node `target` is
    /// for whose inline key equals this key's. For std byte strings, from
    /// past the bytes known equal, the node's prefix and the inline key
    /// ([`SearchBytes::cmp_from`]): the search key shares the prefix,
    /// lying between the node's fences, as every key in it does. Otherwise
    /// by `Q`'s `Ord`.
    #[inline]
    pub(super) fn cmp_tied(&self, target: InlineTarget, stored: &K) -> Ordering
    where
        K: Borrow<Q>,
        Q: Ord,
    {
        if let Some(TargetSource::Bytes(search)) = &self.source
            && let Some(KeyInliner::ByteString(stored_bytes)) = KeyInliner::<K>::resolve::<S>()
        {
            return search.cmp_from(stored_bytes.bytes(stored), target.prefix_len + WIDTH);
        }
        self.key.cmp(stored.borrow())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::ToOwned;
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    use core::cmp::Ordering;

    use super::{
        InlineKeyNodeHeader, InlinedKey, KeyInliner, SearchBytes, SearchKey, WIDTH, Window,
    };
    use crate::inline_key::{Auto, InlineKey, InlineWord, WithInlineKey};
    use crate::node::test_support::TestRng;

    /// A newtype with its own inline key, which looks up by `str` too.
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct UserName(String);

    impl InlineKey for UserName {
        fn inline_key_at(&self, offset: usize) -> InlineWord {
            self.0.inline_key_at(offset)
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

    /// The inline key a search for `key` among keys `K` compares in a node
    /// with prefix 0, if any.
    fn target<K: core::borrow::Borrow<Q>, S: crate::InlineKeyStrategy<K>, Q: ?Sized>(
        key: &Q,
    ) -> Option<InlineWord> {
        SearchKey::<K, S, Q>::new(key)
            .inline_target(0)
            .map(|target| target.inline_key())
    }

    #[test]
    fn lookups_by_any_recognized_form_use_inline_keys() {
        let expected = Some("ferris".inline_key_at(0));
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
            Some(array.inline_key_at(0))
        );
        // Keys without inline keys have no target.
        assert_eq!(target::<u64, Auto, u64>(&7), None);
    }

    #[test]
    fn with_inline_key_lookups_by_another_form_compare_full_keys() {
        let name = UserName("ferris".to_owned());
        assert_eq!(
            target::<UserName, WithInlineKey, UserName>(&name),
            Some(name.inline_key_at(0))
        );
        assert_eq!(target::<UserName, WithInlineKey, str>("ferris"), None);
    }

    /// Random byte strings sharing a random stem, from a small alphabet
    /// with zeros, so that they share prefixes, tie, and end in zeros often.
    fn stemmed_pair(rng: &TestRng) -> (Vec<u8>, Vec<u8>) {
        let random_bytes = |max_len: usize| -> Vec<u8> {
            (0..rng.below(max_len))
                .map(|_| [0, 1, b'a', 0xff][rng.below(4)])
                .collect()
        };
        let stem = random_bytes(24);
        (
            [stem.as_slice(), &random_bytes(20)].concat(),
            [stem.as_slice(), &random_bytes(20)].concat(),
        )
    }

    /// A search's inline key at any offset is the key's, from its padded
    /// copy or, past it, from the key.
    #[test]
    fn search_bytes_take_the_keys_windows() {
        for len in [0, 1, 7, 8, 9, 15, 16, 127, 128, 129, 200] {
            let key: Vec<u8> = (0..len).map(|index| (index * 7 + 1) as u8).collect();
            let search = SearchBytes::new(&key);
            for offset in 0..len + 20 {
                assert_eq!(
                    search.inline_key_at(offset),
                    key.inline_key_at(offset),
                    "{len} bytes at {offset}"
                );
            }
        }
    }

    /// Byte strings whose first `equal_len` bytes are equal, padded, order
    /// like slices when compared from there, whatever their lengths.
    #[test]
    fn search_bytes_compare_like_slices() {
        let rng = TestRng::new(47);
        for _ in 0..if cfg!(miri) { 300 } else { 30_000 } {
            let (search, stored) = stemmed_pair(&rng);
            let max_len = search.len().max(stored.len());
            let padded = |bytes: &[u8], index: usize| bytes.get(index).copied().unwrap_or(0);
            let padded_equal = (0..max_len)
                .take_while(|&index| padded(&search, index) == padded(&stored, index))
                .count();
            // Any length up to where they differ; past the longer, any.
            let max_equal_len = if padded_equal == max_len {
                max_len + WIDTH
            } else {
                padded_equal
            };
            for equal_len in WIDTH..=max_equal_len {
                assert_eq!(
                    SearchBytes::new(&search).cmp_from(&stored, equal_len),
                    search.cmp(&stored),
                    "{search:?} vs {stored:?} from {equal_len}"
                );
            }
        }
    }

    /// Tied std byte strings compare like their `Ord`, at every prefix
    /// length the two share, whether looked up as themselves or borrowed.
    #[test]
    fn tied_byte_strings_compare_like_their_ord() {
        let rng = TestRng::new(48);
        for _ in 0..if cfg!(miri) { 300 } else { 30_000 } {
            let (search, stored) = stemmed_pair(&rng);
            let shared = search
                .iter()
                .zip(&stored)
                .take_while(|(left, right)| left == right)
                .count();
            for prefix_len in 0..=shared {
                if search.inline_key_at(prefix_len) != stored.inline_key_at(prefix_len) {
                    continue;
                }
                let key = SearchKey::<Vec<u8>, Auto, [u8]>::new(&search);
                let target = key.inline_target(prefix_len).unwrap();
                assert_eq!(key.cmp_tied(target, &stored), search.cmp(&stored));
            }
        }
        let name = UserName("ferris".to_owned());
        let key = SearchKey::<UserName, WithInlineKey, UserName>::new(&name);
        let target = key.inline_target(0).unwrap();
        assert_eq!(
            key.cmp_tied(target, &UserName("ferrit".to_owned())),
            Ordering::Less
        );
    }

    #[test]
    fn inlined_keys_carry_their_window() {
        let key: InlinedKey<String, Auto> = InlinedKey::new("ferris the crab".to_owned(), 7);
        assert_eq!(
            key.window(),
            Window::new(7, "ferris the crab".inline_key_at(7))
        );
        let target = SearchKey::<String, Auto, String>::new(key.key())
            .inline_target(7)
            .map(|target| target.inline_key());
        assert_eq!(target, Some(key.window().inline_key()));

        let plain: InlinedKey<u64, Auto> = InlinedKey::new(7, 3);
        assert_eq!(plain.into_parts(), (7, Window::NONE));
    }

    /// A window moved back to a shorter prefix length, filled from another
    /// key's window there, is the key's own window there, as long as the two
    /// keys share the bytes up to the longer prefix length; across every
    /// pair of lengths up to two windows apart.
    #[test]
    fn windows_move_back_like_the_keys() {
        let rng = TestRng::new(46);
        let random_bytes = |len: usize| -> Vec<u8> {
            (0..len).map(|_| [0, 1, b'a', 0xff][rng.below(4)]).collect()
        };
        for _ in 0..if cfg!(miri) { 30 } else { 1000 } {
            let shared = random_bytes(rng.below(20));
            let key = [shared.as_slice(), &random_bytes(rng.below(12))].concat();
            let other = [shared.as_slice(), &random_bytes(rng.below(12))].concat();
            for from in 0..=shared.len() {
                for to in from.saturating_sub(17)..=from {
                    let window = Window::new(from, key.inline_key_at(from));
                    let fill = Window::new(to, other.inline_key_at(to));
                    let expected = Window::new(to, key.inline_key_at(to));
                    assert_eq!(
                        window.moved_back_to(fill),
                        expected,
                        "{key:?} from {from} to {to}"
                    );
                }
            }
        }
    }

    /// Where the zero-padded `a` and `b` first differ, byte by byte; `None`
    /// if they never do.
    fn first_padded_difference(a: &[u8], b: &[u8]) -> Option<usize> {
        let padded = |bytes: &[u8], index: usize| bytes.get(index).copied().unwrap_or(0);
        (0..a.len().max(b.len())).find(|&index| padded(a, index) != padded(b, index))
    }

    /// A window at a time, the common prefix length is where the
    /// zero-padded bytes first differ, for both strategies and across
    /// window boundaries; and at that length the inline keys differ and
    /// order the keys.
    #[test]
    fn common_prefix_len_is_the_first_difference() {
        let rng = TestRng::new(45);
        let random_bytes = |max_len: usize| -> Vec<u8> {
            (0..rng.below(max_len))
                .map(|_| [0, 1, b'a', 0x7f][rng.below(4)])
                .collect()
        };
        let with_stem = |stem: &[u8]| -> Vec<u8> { [stem, &random_bytes(6)].concat() };
        let by_bytes = KeyInliner::<String>::resolve::<Auto>().unwrap();
        let by_trait = KeyInliner::<UserName>::resolve::<WithInlineKey>().unwrap();
        for _ in 0..if cfg!(miri) { 100 } else { 10_000 } {
            let stem = random_bytes(20);
            let (a, b) = (with_stem(&stem), with_stem(&stem));
            let Some(shared) = first_padded_difference(&a, &b) else {
                continue;
            };
            let (a, b) = (String::from_utf8(a).unwrap(), String::from_utf8(b).unwrap());
            assert_eq!(by_bytes.common_prefix_len(&a, &b), shared, "{a:?} vs {b:?}");
            let (a, b) = (UserName(a), UserName(b));
            assert_eq!(by_trait.common_prefix_len(&a, &b), shared);
            let windows = (
                by_trait.inline_key_at(&a, shared),
                by_trait.inline_key_at(&b, shared),
            );
            assert_eq!(windows.0.cmp(&windows.1), a.cmp(&b));
        }
    }

    /// Different keys whose zero-padded bytes are equal, and equal keys,
    /// share the longest prefix a node records.
    #[test]
    fn common_prefix_len_stops_at_the_longest_prefix() {
        let inliner = KeyInliner::<String>::resolve::<Auto>().unwrap();
        let max_len = InlineKeyNodeHeader::MAX_PREFIX_LEN;
        let (short, padded) = ("ab".to_owned(), "ab\0".to_owned());
        assert_eq!(inliner.common_prefix_len(&short, &padded), max_len);
        assert_eq!(inliner.common_prefix_len(&short, &short), max_len);
    }

    /// Tied byte-array keys compare word by word, and agree with their
    /// `Ord`.
    #[test]
    fn tied_byte_arrays_compare_like_their_ord() {
        let rng = TestRng::new(44);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let mut search = [7_u8; 20];
            let mut stored = search;
            search[WIDTH + rng.below(20 - WIDTH)] = rng.below(256) as u8;
            stored[WIDTH + rng.below(20 - WIDTH)] = rng.below(256) as u8;
            let key: SearchKey<'_, [u8; 20], Auto, [u8; 20]> = SearchKey::new(&search);
            let target = key.inline_target(0).unwrap();
            assert_eq!(key.cmp_tied(target, &stored), search.cmp(&stored));
        }
    }
}
