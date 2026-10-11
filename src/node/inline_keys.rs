//! Inline keys inside the nodes (D18): which keys have them, what a node
//! stores beside each key, and what a search looks for.
//!
//! A node stores each key's inline key at the node's prefix length: the
//! first bytes of the 8 of the key's encoding past the bytes that the node's
//! fences share, and so every key in it (05-fence-key.md), as many as the
//! node kind's [`InlineWidth`]. Windows are always 8 bytes; only a node narrows
//! them, as it stores them, and as it compares a search's with them.
//!
//! - [`KeyInliner`] resolves a map's strategy for its key type, once and for
//!   all at compile time: how its keys get inline keys, or that they have
//!   none. What each node kind does with them follows from it
//!   (`node_plan.rs`).
//! - [`InlineSlot`] is what a node stores an inline key as, at each width.
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

use super::node_plan::NodePlan;
use super::page::{ByteString, InlineKeyNodeHeader, PageKind, SameType, StdInteger};
use super::vector_count::VectorCount;
use crate::inline_key::sealed::InlineKeySource;
use crate::inline_key::{InlineKey, InlineKeyStrategy, InlineWidth, InlineWord};

/// The bytes of a window, whatever a node keeps of it.
const WINDOW_LEN: usize = size_of::<InlineWord>();

/// How keys `K` get their inline keys under a map's strategy: the strategy's
/// [`InlineKeySource`], resolved for `K`.
pub(super) enum KeyInliner<K> {
    /// `K` is a std byte string ([`Auto`]): its inline keys are windows of
    /// its bytes.
    ///
    /// [`Auto`]: crate::Auto
    ByteString(ByteString<K>),
    /// `K`'s [`InlineKey`] impl, of whose inline keys the nodes keep
    /// `width` bytes: any key's under [`WithInlineKey`], and a std
    /// integer's under [`Auto`].
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    /// [`Auto`]: crate::Auto
    InlineKeyTrait {
        inline_key_at: fn(&K, usize) -> InlineWord,
        width: InlineWidth,
    },
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
    /// monomorphization: every node plan of a map follows from it, and so
    /// does every page's layout, which must (`node_plan.rs`).
    #[inline(always)]
    pub(super) fn resolve<S: InlineKeyStrategy<K>>() -> Option<Self> {
        match S::SOURCE {
            InlineKeySource::RecognizedStdTypes => {
                ByteString::recognize().map(Self::ByteString).or_else(|| {
                    StdInteger::recognize().map(|integer| Self::InlineKeyTrait {
                        inline_key_at: integer.inline_key_at(),
                        width: integer.width(),
                    })
                })
            }
            InlineKeySource::InlineKeyTrait {
                inline_key_at,
                width,
            } => Some(Self::InlineKeyTrait {
                inline_key_at,
                width,
            }),
        }
    }

    /// `key`'s inline key at `offset`: what a node whose keys share their
    /// first `offset` bytes stores beside it.
    #[inline]
    pub(super) fn inline_key_at(self, key: &K, offset: usize) -> InlineWord {
        match self {
            Self::ByteString(byte_string) => byte_string.bytes(key).inline_key_at(offset),
            Self::InlineKeyTrait { inline_key_at, .. } => inline_key_at(key, offset),
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
            offset += WINDOW_LEN;
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
}

/// What a node stores an inline key as (D18): the first bytes of its window,
/// as many as the node kind's [`InlineWidth`], in an unsigned integer of as many
/// bytes, which orders like them.
pub(super) trait InlineSlot: Copy + Ord + VectorCount {
    /// Whether one pass that counts every slot up to the capacity compiles
    /// to vector compares on the target, which makes it faster than a binary
    /// search (`handle.rs`): 16- and 32-bit lanes on x86-64 and AArch64, but
    /// 64-bit ones only with AVX-512 (04, Q6).
    const COUNTS_IN_VECTORS: bool;

    /// The first bytes of `window`, which a node keeps.
    fn of_window(window: InlineWord) -> Self;

    /// The window that these bytes start, zeros after them.
    fn window(self) -> InlineWord;
}

/// Implements [`InlineSlot`] for an unsigned integer narrower than a window:
/// its top bits.
macro_rules! narrow_inline_slot {
    ($slot:ty) => {
        impl InlineSlot for $slot {
            const COUNTS_IN_VECTORS: bool =
                cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));

            #[inline]
            fn of_window(window: InlineWord) -> Self {
                (window >> (InlineWord::BITS - <$slot>::BITS)) as $slot
            }

            #[inline]
            fn window(self) -> InlineWord {
                InlineWord::from(self) << (InlineWord::BITS - <$slot>::BITS)
            }
        }
    };
}

narrow_inline_slot!(u16);
narrow_inline_slot!(u32);

/// The whole window.
impl InlineSlot for u64 {
    const COUNTS_IN_VECTORS: bool = cfg!(target_feature = "avx512f");

    #[inline]
    fn of_window(window: InlineWord) -> Self {
        window
    }

    #[inline]
    fn window(self) -> InlineWord {
        self
    }
}

/// An inline key with the prefix length it is taken at: bytes
/// `prefix_len..prefix_len + 8` of a key's encoding, of which a node with
/// that prefix length stores the first (05-fence-key.md).
///
/// Raw inline slots yield their kept bytes, zeros after them. A window
/// exported with a heap-byte key or bound is rehydrated to all eight bytes,
/// even from a four-byte internal node, so it can fill a leaf window. Other
/// encodings keep the same width in both node kinds. Moving to a shorter
/// prefix preserves that many correct bytes (`moved_back_to`); a node whose
/// prefix grows reads its keys.
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
    /// If only the first `n` bytes of this window and of `fill` are right,
    /// as for raw windows read out of slots that keep `n`, at least the first
    /// `n` of the result are: the `shift` bytes that move in are `fill`'s
    /// first, and this window's `n` follow them, or, for a shift past `n`,
    /// all are `fill`'s.
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
        } else if shift >= WINDOW_LEN {
            fill.inline_key
        } else {
            let shift_bits = 8 * shift as u32;
            let filled_bits = !(InlineWord::MAX >> shift_bits);
            (fill.inline_key & filled_bits) | (self.inline_key >> shift_bits)
        };
        Self::new(fill.prefix_len, inline_key)
    }
}

/// The bytes of a std byte string being searched for. Windows read these
/// bytes directly: a search visits only a few prefix lengths, so preparing
/// every possible window would copy mostly bytes that it never reads.
struct SearchBytes<'q> {
    bytes: &'q [u8],
}

/// A fixed-size part of a local prefix guard. Guards usually match, so
/// test all four words together before ordering a differing word.
#[derive(Clone, Copy)]
struct PrefixChunk<'a>(&'a [u8; 4 * WINDOW_LEN]);

impl<'a> PrefixChunk<'a> {
    const LEN: usize = 4 * WINDOW_LEN;

    #[inline]
    fn new(bytes: &'a [u8; 4 * WINDOW_LEN]) -> Self {
        Self(bytes)
    }

    #[inline]
    fn word(&self, index: usize) -> u64 {
        let start = index * WINDOW_LEN;
        u64::from_ne_bytes(self.0[start..start + WINDOW_LEN].try_into().unwrap())
    }

    #[inline]
    fn matches(self, other: Self) -> bool {
        let different = (self.word(0) ^ other.word(0))
            | (self.word(1) ^ other.word(1))
            | (self.word(2) ^ other.word(2))
            | (self.word(3) ^ other.word(3));
        different == 0
    }

    /// Keeping the rare ordering decision separate lets equality discard
    /// individual words instead of keeping eight of them live in registers.
    #[cold]
    #[inline(never)]
    fn cmp_different(self, other: Self) -> Ordering {
        SearchBytes::new(self.0).cmp_from(other.0, 0)
    }
}

/// A byte query that a point lookup compares with a std byte string key's
/// bytes all at once, for equality alone: once the inline keys leave one
/// candidate, a leaf needs no order. Where the plan says so (D38), a fixed
/// byte array's, whose length the compiler knows.
pub(super) struct WholeBytesQuery<'q, K> {
    query: &'q [u8],
    stored: ByteString<K>,
}

impl<K> WholeBytesQuery<'_, K> {
    #[inline]
    pub(super) fn matches(&self, key: &K) -> bool {
        self.query == self.stored.bytes(key)
    }
}

impl<'q> SearchBytes<'q> {
    #[inline]
    fn new(bytes: &'q [u8]) -> Self {
        Self { bytes }
    }

    /// The key's inline key at `offset`.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.bytes.inline_key_at(offset)
    }

    /// Local prefix guards normally agree over their full length. Group
    /// complete chunks without reading past either slice; a differing
    /// chunk still decides by its first unequal byte. Terminal key ties
    /// keep `cmp_from`, whose early mismatches favor individual words.
    #[inline]
    fn cmp_guard(&self, stored: &[u8]) -> Ordering {
        let mut equal_len = 0;
        let (search_chunks, _) = self.bytes.as_chunks::<{ PrefixChunk::LEN }>();
        let (stored_chunks, _) = stored.as_chunks::<{ PrefixChunk::LEN }>();
        for (search, stored) in search_chunks.iter().zip(stored_chunks) {
            let search = PrefixChunk::new(search);
            let stored = PrefixChunk::new(stored);
            if !search.matches(stored) {
                return search.cmp_different(stored);
            }
            equal_len += PrefixChunk::LEN;
        }
        self.cmp_from(stored, equal_len)
    }

    /// Orders the key against `stored`, a std byte string, as `<[u8]>::cmp`
    /// does, given that their first `equal_len` bytes are equal
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
        if equal_len < both_len && both_len < WINDOW_LEN {
            // A fixed array's two-byte inline key can tie with a borrowed
            // slice shorter than a word. Zero-padded windows decide every
            // differing byte; length breaks ties consisting only of zeros.
            return search
                .inline_key_at(0)
                .cmp(&stored.inline_key_at(0))
                .then_with(|| search.len().cmp(&stored.len()));
        }
        let mut compared = equal_len;
        while compared < both_len {
            let word_end = (compared + WINDOW_LEN).min(both_len);
            let word = |bytes: &[u8]| {
                bytes[..word_end]
                    .last_chunk::<WINDOW_LEN>()
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

/// The inline key of a key being searched for, at one node's prefix, all 8
/// bytes of it, which the node narrows to the ones it keeps as it compares:
/// two words, which stay in registers on the way down.
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
/// whose inline key at each node's prefix the nodes of a map with strategy
/// `S` compare with theirs, if they can (D18, 05-fence-key.md): for std
/// byte-string keys, if `Q` is a std byte string too, ordered by its bytes
/// like them; for keys with their own impl, if `Q` is `K` itself. Otherwise
/// the search compares full keys.
///
/// Every choice between the two goes by the strategy, resolved for `K` where
/// it is made, a constant: the nodes of a map run only its own way.
pub(crate) struct SearchKey<'q, K, S, Q: ?Sized> {
    key: &'q Q,
    /// The query's bytes when both types use the built-in byte encoding;
    /// an unused empty slice otherwise. Availability is a type property,
    /// checked by `recognized_bytes`, not a runtime tag carried into each
    /// outlined node search.
    bytes: SearchBytes<'q>,
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
        let bytes = Self::byte_string_query()
            .map(|byte_string| byte_string.bytes(key))
            .unwrap_or(&[]);
        Self {
            key,
            bytes: SearchBytes::new(bytes),
            _map: PhantomData,
        }
    }

    /// The key's inline key at a node's `prefix_len`, if the nodes can
    /// compare it with theirs. Calls `InlineKey` with
    /// [`WithInlineKey`](crate::WithInlineKey), as searches do.
    #[inline]
    pub(super) fn inline_target(&self, prefix_len: usize) -> Option<InlineTarget> {
        let inline_key = match KeyInliner::<K>::resolve::<S>()? {
            KeyInliner::ByteString(_) => self.recognized_bytes()?.inline_key_at(prefix_len),
            inliner @ KeyInliner::InlineKeyTrait { .. } => {
                let key = SameType::<K, Q>::check()?.cast(self.key);
                inliner.inline_key_at(key, prefix_len)
            }
        };
        Some(InlineTarget {
            inline_key,
            prefix_len,
        })
    }
}

impl<K, S: InlineKeyStrategy<K>, Q: ?Sized> SearchKey<'_, K, S, Q> {
    /// The existing proofs establish byte ordering for both stored keys
    /// and queries. A custom strategy must keep its `Ord` comparisons even
    /// when the query itself is a recognized string type.
    #[inline]
    fn byte_string_query() -> Option<ByteString<Q>> {
        let KeyInliner::ByteString(_) = KeyInliner::<K>::resolve::<S>()? else {
            return None;
        };
        ByteString::<Q>::recognize()
    }

    #[inline]
    fn recognized_bytes(&self) -> Option<&SearchBytes<'_>> {
        Self::byte_string_query()?;
        Some(&self.bytes)
    }

    pub(super) fn key(&self) -> &Q {
        self.key
    }

    /// Orders complete keys, without assuming any equal prefix or summary.
    /// Only recognized built-in byte orderings use fixed-word comparison;
    /// explicit strategies and unrecognized queries retain their `Ord`.
    #[inline]
    pub(super) fn cmp_full(&self, stored: &K) -> Ordering
    where
        K: Borrow<Q>,
        Q: Ord,
    {
        if let Some(KeyInliner::ByteString(stored_bytes)) = KeyInliner::<K>::resolve::<S>()
            && let Some(search) = self.recognized_bytes()
        {
            return search.cmp_from(stored_bytes.bytes(stored), 0);
        }
        self.key.cmp(stored.borrow())
    }

    /// The query, for comparing with a key's bytes all at once: for std byte
    /// string keys and a recognized byte query only. Keys with their own
    /// `InlineKey` impl keep their `Ord`.
    #[inline]
    pub(super) fn whole_bytes_query(&self) -> Option<WholeBytesQuery<'_, K>> {
        let KeyInliner::ByteString(stored) = KeyInliner::<K>::resolve::<S>()? else {
            return None;
        };
        Some(WholeBytesQuery {
            query: self.recognized_bytes()?.bytes,
            stored,
        })
    }

    /// Equality may reject a different byte length without reading the
    /// query's contents. Truncation only creates harmless collisions.
    #[inline]
    pub(super) fn length_tag(&self) -> Option<u16> {
        self.recognized_bytes()
            .map(|bytes| bytes.bytes.len() as u16)
    }

    /// Validates bytes beyond a node's conservative fence prefix before
    /// its local windows may order the query. A short query sorts before
    /// the stored prefix. Whole chunks test equality together; differing
    /// chunks and short tails use the fixed-word comparator.
    #[inline]
    pub(super) fn cmp_prefix(&self, stored: &K, start: usize, end: usize) -> Option<Ordering> {
        let KeyInliner::ByteString(stored_bytes) = KeyInliner::<K>::resolve::<S>()? else {
            return None;
        };
        let query = self.recognized_bytes()?.bytes;
        let stored = stored_bytes.bytes(stored).get(start..end)?;
        let query = query.get(start..query.len().min(end)).unwrap_or_default();
        Some(SearchBytes::new(query).cmp_guard(stored))
    }

    /// How this key orders against `stored`, a key of a node of kind `Kind`
    /// that `target` is for, whose inline key equals this key's in the bytes
    /// the node keeps. For std byte strings, from past the bytes known
    /// equal, the node's prefix and the bytes of each inline key its plan
    /// stores ([`SearchBytes::cmp_from`]): the search key shares the prefix,
    /// lying between the node's fences, as every key in it does. Otherwise
    /// by `Q`'s `Ord`.
    #[inline]
    pub(super) fn cmp_tied<Kind: PageKind>(&self, target: InlineTarget, stored: &K) -> Ordering
    where
        K: Borrow<Q>,
        Q: Ord,
    {
        let plan = NodePlan::<K>::of::<S, Kind>();
        if let Some(KeyInliner::ByteString(stored_bytes)) = plan.inliner()
            && let Some(width) = plan.inline_width()
            && let Some(search) = self.recognized_bytes()
        {
            let equal_len = target.prefix_len + width.bytes();
            return search.cmp_from(stored_bytes.bytes(stored), equal_len);
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
        InlineKeyNodeHeader, InlineSlot, InlinedKey, KeyInliner, NodePlan, SearchBytes, SearchKey,
        WINDOW_LEN, Window,
    };
    use crate::inline_key::{Auto, InlineKey, InlineWidth, InlineWord, WithInlineKey};
    use crate::node::handle::{Internal, Leaf};
    use crate::node::test_support::TestRng;

    #[test]
    fn complete_comparisons_respect_short_zero_bytes_and_strategy_proofs() {
        let texts = [
            "",
            "\0",
            "a",
            "a\0",
            "a\0\0",
            "ab",
            "abcdefgh",
            "abcdefgh\0",
        ];
        for query in texts {
            for stored in texts.map(String::from) {
                let expected = query.cmp(stored.as_str());
                assert_eq!(
                    SearchKey::<String, Auto, str>::new(query).cmp_full(&stored),
                    expected
                );
                assert_eq!(
                    SearchKey::<String, WithInlineKey, str>::new(query).cmp_full(&stored),
                    expected
                );
            }
        }
        assert!(SearchKey::<String, WithInlineKey, str>::byte_string_query().is_none());

        // A query whose ordering is unknown to Auto must still supply the
        // whole comparison, even when the stored key is a recognized String.
        trait ReverseText {
            fn text(&self) -> &str;
        }
        impl ReverseText for String {
            fn text(&self) -> &str {
                self
            }
        }
        impl PartialEq for dyn ReverseText {
            fn eq(&self, other: &Self) -> bool {
                self.text() == other.text()
            }
        }
        impl Eq for dyn ReverseText {}
        impl PartialOrd for dyn ReverseText {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for dyn ReverseText {
            fn cmp(&self, other: &Self) -> Ordering {
                other.text().cmp(self.text())
            }
        }
        impl core::borrow::Borrow<dyn ReverseText> for String {
            fn borrow(&self) -> &(dyn ReverseText + 'static) {
                self
            }
        }
        let query = String::from("a");
        let stored = String::from("b");
        let borrowed: &dyn ReverseText = &query;
        assert_eq!(
            SearchKey::<String, Auto, dyn ReverseText>::new(borrowed).cmp_full(&stored),
            Ordering::Greater
        );
        assert_eq!(
            SearchKey::<String, WithInlineKey, dyn ReverseText>::new(borrowed).cmp_full(&stored),
            Ordering::Greater
        );
    }

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
    fn auto_gives_std_byte_strings_and_integers_inline_keys() {
        assert!(has_inline_keys::<String, Auto>());
        assert!(has_inline_keys::<Vec<u8>, Auto>());
        assert!(has_inline_keys::<Box<str>, Auto>());
        assert!(has_inline_keys::<Box<[u8]>, Auto>());
        assert!(has_inline_keys::<&str, Auto>());
        assert!(has_inline_keys::<&'static [u8], Auto>());
        assert!(has_inline_keys::<[u8; 16], Auto>());
        assert!(has_inline_keys::<[u8; 20], Auto>());
        assert!(has_inline_keys::<[u8; 32], Auto>());
        assert!(has_inline_keys::<u64, Auto>());
        assert!(has_inline_keys::<i64, Auto>());
        assert!(has_inline_keys::<u128, Auto>());
        assert!(has_inline_keys::<i128, Auto>());

        assert!(!has_inline_keys::<u32, Auto>());
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
        assert!(has_inline_keys::<u64, WithInlineKey>());
        assert!(has_inline_keys::<i128, WithInlineKey>());
    }

    /// Heap-backed byte strings keep all 8 bytes in leaves, arrays keep 2;
    /// integers and keys with their own impl keep what it says.
    #[test]
    fn strategies_keep_their_keys_widths() {
        fn width<K, S: crate::InlineKeyStrategy<K>>() -> Option<InlineWidth> {
            NodePlan::<K>::of::<S, Leaf>().inline_width()
        }
        assert_eq!(width::<String, Auto>(), Some(InlineWidth::Eight));
        assert_eq!(width::<[u8; 16], Auto>(), Some(InlineWidth::Two));
        assert_eq!(width::<[u8; 20], Auto>(), Some(InlineWidth::Two));
        assert_eq!(width::<[u8; 32], Auto>(), Some(InlineWidth::Two));
        assert_eq!(width::<u64, Auto>(), Some(InlineWidth::Two));
        assert_eq!(width::<i128, Auto>(), Some(InlineWidth::Two));
        assert_eq!(width::<u32, Auto>(), None);
        assert_eq!(width::<u64, WithInlineKey>(), Some(InlineWidth::Two));
        assert_eq!(width::<i64, WithInlineKey>(), Some(InlineWidth::Two));
        assert_eq!(width::<UserName, WithInlineKey>(), Some(InlineWidth::Eight));
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
        assert_eq!(target::<u64, Auto, u64>(&7), Some(7_u64.inline_key_at(0)));
        // Keys without inline keys have no target.
        assert_eq!(target::<u32, Auto, u32>(&7), None);
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

    /// A search's inline key at any offset is the key's.
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
                max_len + WINDOW_LEN
            } else {
                padded_equal
            };
            for equal_len in 0..=max_equal_len {
                assert_eq!(
                    SearchBytes::new(&search).cmp_from(&stored, equal_len),
                    search.cmp(&stored),
                    "{search:?} vs {stored:?} from {equal_len}"
                );
            }
        }
    }

    /// Every byte can decide a guard's order, including chunk boundaries,
    /// zero bytes and bytes whose signed order differs. Later differences
    /// must never override the first one.
    #[test]
    fn prefix_guards_compare_like_slices() {
        for len in [0, 1, 7, 8, 9, 31, 32, 33, 63, 64, 65, 95, 96, 97] {
            let stored: Vec<u8> = (0..len)
                .map(|index| [0, 1, 0x7f, 0x80, 0xff][index % 5])
                .collect();
            let check = |query: &[u8]| {
                assert_eq!(
                    SearchBytes::new(query).cmp_guard(&stored),
                    query.cmp(&stored),
                    "{query:?} vs {stored:?}"
                );
                assert_eq!(
                    SearchBytes::new(&stored).cmp_guard(query),
                    stored.as_slice().cmp(query),
                );
            };
            for short in 0..=len {
                check(&stored[..short]);
            }
            let mut query = stored.clone();
            query.extend_from_slice(&[0, 0]);
            check(&query);
            for differing in 0..len {
                for byte in [0, 1, 0x7f, 0x80, 0xff] {
                    let mut query = stored.clone();
                    query[differing..].fill(byte);
                    check(&query);
                }
            }
        }
    }

    /// Prefix guards compare only the requested interval. A query ending
    /// before or inside it sorts first; bytes beyond it are irrelevant.
    #[test]
    fn local_prefix_guards_respect_bounds_and_short_queries() {
        let stored = (0..193)
            .map(|i| [0, 0x80, 0xff, 1][i % 4])
            .collect::<Vec<_>>();
        for start in [0, 1, 7, 31, 32, 63] {
            for len in [8, 31, 32, 33, 63, 64, 65, 96] {
                let end = start + len;
                for query_len in 0..=stored.len() {
                    let query = &stored[..query_len];
                    let expected = if query_len < end {
                        Ordering::Less
                    } else {
                        Ordering::Equal
                    };
                    assert_eq!(
                        SearchKey::<Vec<u8>, Auto, [u8]>::new(query)
                            .cmp_prefix(&stored, start, end),
                        Some(expected),
                        "guard {start}..{end}, query length {query_len}"
                    );
                }
                let mut query = stored.clone();
                query[end..].fill(0x77);
                assert_eq!(
                    SearchKey::<Vec<u8>, Auto, [u8]>::new(&query).cmp_prefix(&stored, start, end),
                    Some(Ordering::Equal),
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
                for width in [InlineWidth::Four, InlineWidth::Eight] {
                    let shift = 8 * (InlineWidth::Eight.bytes() - width.bytes());
                    if search.inline_key_at(prefix_len) >> shift
                        != stored.inline_key_at(prefix_len) >> shift
                    {
                        continue;
                    }
                    let key = SearchKey::<Vec<u8>, Auto, [u8]>::new(&search);
                    let target = key.inline_target(prefix_len).unwrap();
                    let compared = match width {
                        InlineWidth::Four => key.cmp_tied::<Internal>(target, &stored),
                        InlineWidth::Eight => key.cmp_tied::<Leaf>(target, &stored),
                        InlineWidth::Two => unreachable!(),
                    };
                    assert_eq!(compared, search.cmp(&stored));
                }
            }
        }
        let name = UserName("ferris".to_owned());
        let key = SearchKey::<UserName, WithInlineKey, UserName>::new(&name);
        let target = key.inline_target(0).unwrap();
        assert_eq!(
            key.cmp_tied::<Internal>(target, &UserName("ferrit".to_owned())),
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

        let integer: InlinedKey<u64, Auto> = InlinedKey::new(7, 3);
        assert_eq!(integer.window(), Window::new(3, 7_u64.inline_key_at(3)));

        let plain: InlinedKey<u32, Auto> = InlinedKey::new(7, 3);
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

    /// What a node of slot type `W` keeps of `window`, as a window.
    fn kept<W: InlineSlot>(window: InlineWord) -> InlineWord {
        W::of_window(window).window()
    }

    /// A window read out of a node holds only the bytes the node keeps;
    /// moved back to a shorter prefix length, filled from another such
    /// window there, it still holds what a node keeps of the key's own
    /// window there, across every pair of lengths up to two windows apart.
    fn assert_kept_bytes_move_back_like_the_keys<W: InlineSlot>(seed: u64) {
        let rng = TestRng::new(seed);
        let random_bytes = |len: usize| -> Vec<u8> {
            (0..len).map(|_| [0, 1, b'a', 0xff][rng.below(4)]).collect()
        };
        for _ in 0..if cfg!(miri) { 30 } else { 1000 } {
            let shared = random_bytes(rng.below(20));
            let key = [shared.as_slice(), &random_bytes(rng.below(12))].concat();
            let other = [shared.as_slice(), &random_bytes(rng.below(12))].concat();
            for from in 0..=shared.len() {
                for to in from.saturating_sub(17)..=from {
                    let window = Window::new(from, kept::<W>(key.inline_key_at(from)));
                    let fill = Window::new(to, kept::<W>(other.inline_key_at(to)));
                    let moved = window.moved_back_to(fill);
                    assert_eq!(moved.prefix_len(), to);
                    assert_eq!(
                        kept::<W>(moved.inline_key()),
                        kept::<W>(key.inline_key_at(to)),
                        "{key:?} from {from} to {to}"
                    );
                }
            }
        }
    }

    #[test]
    fn kept_bytes_move_back_like_the_keys() {
        assert_kept_bytes_move_back_like_the_keys::<u16>(49);
        assert_kept_bytes_move_back_like_the_keys::<u32>(50);
        assert_kept_bytes_move_back_like_the_keys::<u64>(51);
    }

    /// Slots keep the first bytes of a window, and order like them.
    #[test]
    fn slots_keep_the_first_bytes() {
        let window = 0x0102_0304_0506_0708;
        assert_eq!(u16::of_window(window), 0x0102);
        assert_eq!(u32::of_window(window), 0x0102_0304);
        assert_eq!(u64::of_window(window), window);
        assert_eq!(0x0102_u16.window(), 0x0102_0000_0000_0000);
        assert_eq!(0x0102_0304_u32.window(), 0x0102_0304_0000_0000);
        assert!(u16::of_window(window) < u16::of_window(0x0103_0000_0000_0000));
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
            search[2 + rng.below(18)] = rng.below(256) as u8;
            stored[2 + rng.below(18)] = rng.below(256) as u8;
            let key: SearchKey<'_, [u8; 20], Auto, [u8; 20]> = SearchKey::new(&search);
            let target = key.inline_target(0).unwrap();
            assert_eq!(key.cmp_tied::<Leaf>(target, &stored), search.cmp(&stored));
        }
    }

    /// An array's narrow prefix also accepts borrowed slices shorter than
    /// one word, including a proper prefix ending in zeros.
    #[test]
    fn arrays_compare_short_borrowed_slices() {
        let key = SearchKey::<[u8; 20], Auto, [u8]>::new(&[0, 0, 1]);
        assert_eq!(
            key.cmp_tied::<Leaf>(key.inline_target(0).unwrap(), &[0; 20]),
            Ordering::Greater,
            "a shorter slice can sort after the array despite a tied prefix"
        );
        for len in 0..WINDOW_LEN {
            for changed in 2..WINDOW_LEN {
                for byte in [0, 1, 0x80, 0xff] {
                    let mut stored = [0_u8; 20];
                    stored[changed] = byte;
                    let search = &stored[..len];
                    let key = SearchKey::<[u8; 20], Auto, [u8]>::new(search);
                    let target = key.inline_target(0).unwrap();
                    assert_eq!(
                        key.cmp_tied::<Internal>(target, &stored),
                        search.cmp(&stored),
                        "{len} bytes, byte {changed} = {byte}"
                    );
                }
            }
        }
    }
}
