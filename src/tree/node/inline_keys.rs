//! Inline keys inside the nodes (D18): which keys have them, what a node
//! stores beside each key, and what a search looks for.
//!
//! A node's prefix is the bytes its fences share, and so every key in it
//! (05-fence-key.md). A key's inline key at that prefix length is the 8
//! bytes of its encoding past the prefix. The node stores the first of
//! them, as many as the node kind's [`InlineWidth`]. Windows are always 8
//! bytes; only a node narrows them, when it stores them and when it compares
//! a search's with them.
//!
//! - [`KeyInliner`] resolves a map's strategy for its key type, once and for
//!   all at compile time: how its keys get inline keys, or that they have
//!   none. What each node kind does with them follows from it
//!   (`node_plan.rs`).
//! - [`InlineSlot`] is what a node stores an inline key as, at each width.
//! - [`Window`] is an inline key with the prefix length it is taken at; it
//!   moves back to a shorter prefix without reading the key.
//! - [`WindowedKey`] is a key outside the nodes with its window, for the node
//!   it goes into.
//! - [`SearchKey`] is what a search looks for: a key, and how to take its
//!   inline key at each node's prefix if the nodes can compare it. It also
//!   orders the key against what every key of the tree shares (D50).

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::marker::PhantomData;
use core::mem::{self, size_of};

use super::node_plan::NodePlan;
use super::page::{InlineKeyNodeHeader, NodeKind};
use super::type_identity::{ByteString, SameType, StdInteger};
use super::vector_count::VectorCount;
use crate::inline_key::sealed::InlineKeySource;
use crate::inline_key::{InlineKey, InlineKeyStrategy, InlineWidth, InlineWord};

/// The bytes of a window, whatever a node keeps of it.
const WINDOW_LEN: usize = size_of::<InlineWord>();

/// How many bytes a search's check against what every key shares tests for
/// equality at once ([`SearchBytes::cmp_shared`]).
const SHARED_PREFIX_CHUNK_LEN: usize = 4 * WINDOW_LEN;

/// How keys `K` get their inline keys under a map's strategy: the strategy's
/// [`InlineKeySource`], resolved for `K`.
pub(in crate::tree) enum KeyInliner<K> {
    /// `K` is a std byte string ([`Auto`]): its inline keys are windows of
    /// its bytes, and its hash tag that of its bytes.
    ///
    /// [`Auto`]: crate::Auto
    ByteString(ByteString<K>),
    /// `K`'s [`InlineKey`] impl, of whose inline keys the nodes keep
    /// `width` bytes, and its hash tag and comparison from where keys differ
    /// if the impl gives them: any key's under [`WithInlineKey`], and a std
    /// integer's, without them, under [`Auto`].
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    /// [`Auto`]: crate::Auto
    InlineKeyTrait {
        inline_key_at: fn(&K, usize) -> InlineWord,
        width: InlineWidth,
        hash_tag: Option<fn(&K) -> u8>,
        cmp_from: Option<fn(&K, &K, usize) -> Ordering>,
        cmp_prefix: fn(&K, &[u8]) -> Ordering,
    },
}

impl<K> Clone for KeyInliner<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for KeyInliner<K> {}

/// Held across a call into user code that runs while keys move: dropped
/// only by an unwind out of that call, it panics again, and a panic while
/// unwinding aborts (D48). Forgotten once the call returns.
struct AbortOnUnwind;

impl AbortOnUnwind {
    /// `user_code`'s result, or an abort if it panics.
    #[inline(always)]
    fn call<R>(user_code: impl FnOnce() -> R) -> R {
        let guard = Self;
        let result = user_code();
        mem::forget(guard);
        result
    }
}

impl Drop for AbortOnUnwind {
    fn drop(&mut self) {
        panic!("user code panicked while the map moved keys");
    }
}

impl<K> KeyInliner<K> {
    /// How keys `K` get inline keys under strategy `S`, or `None` if they
    /// have none.
    ///
    /// A pure function of `K` and `S` (the strategy's constant source and
    /// `K`'s type identity), so it folds to a constant after
    /// monomorphization. Every node plan of a map follows from it, and so
    /// does every page's layout, which every access must match
    /// (`node_plan.rs`).
    #[inline(always)]
    pub(in crate::tree) fn resolve<S: InlineKeyStrategy<K>>() -> Option<Self> {
        match S::SOURCE {
            InlineKeySource::RecognizedStdTypes => {
                ByteString::recognize().map(Self::ByteString).or_else(|| {
                    StdInteger::recognize().map(|integer| Self::InlineKeyTrait {
                        inline_key_at: integer.inline_key_at(),
                        width: integer.width(),
                        hash_tag: None,
                        cmp_from: None,
                        cmp_prefix: integer.cmp_prefix(),
                    })
                })
            }
            InlineKeySource::InlineKeyTrait {
                inline_key_at,
                width,
                hash_tag,
                cmp_from,
                cmp_prefix,
            } => Some(Self::InlineKeyTrait {
                inline_key_at,
                width,
                hash_tag,
                cmp_from,
                cmp_prefix,
            }),
        }
    }

    /// Whether `K`, under strategy `S`, is ordered by its own bytes alone,
    /// and every bit pattern of them is a `K`: the std integers and byte
    /// arrays [`Auto`] recognizes. A copy of such a key, read while a writer
    /// changes it, compares without reading anything else. A latched tree's
    /// optimistic reader relies on that (`optimistic.rs`); any other key's
    /// order may follow pointers it holds.
    ///
    /// [`Auto`]: crate::Auto
    #[inline(always)]
    pub(in crate::tree) fn orders_by_own_bytes<S: InlineKeyStrategy<K>>() -> bool {
        matches!(S::SOURCE, InlineKeySource::RecognizedStdTypes)
            && (StdInteger::<K>::recognize().is_some()
                || ByteString::<K>::recognize().is_some_and(ByteString::holds_its_bytes))
    }

    /// `key`'s inline key at `offset`: what a node whose keys share their
    /// first `offset` bytes stores beside it. Always inlined: it is only a
    /// dispatch, and once it has enough callers LLVM outlines it, putting a
    /// call in the searches.
    #[inline(always)]
    pub(in crate::tree) fn inline_key_at(self, key: &K, offset: usize) -> InlineWord {
        match self {
            Self::ByteString(byte_string) => byte_string.bytes(key).inline_key_at(offset),
            Self::InlineKeyTrait { inline_key_at, .. } => inline_key_at(key, offset),
        }
    }

    /// `key`'s inline key at `offset`, read while keys move: by an internal
    /// node that keeps fewer bytes of it than a leaf, for a window it hands
    /// out (D43). A key's own impl runs behind an abort-on-unwind guard
    /// (D48), as unwinding would leave the move half done.
    #[inline(always)]
    pub(in crate::tree) fn inline_key_at_while_keys_move(
        self,
        key: &K,
        offset: usize,
    ) -> InlineWord {
        match self {
            Self::ByteString(_) => self.inline_key_at(key, offset),
            Self::InlineKeyTrait { inline_key_at, .. } => {
                AbortOnUnwind::call(|| inline_key_at(key, offset))
            }
        }
    }

    /// `key`'s hash tag (D36), or `None` if the keys have none: a std byte
    /// string's from its bytes, and a key's own impl's otherwise. Leaves
    /// call it as they write a key, while they move keys too, so a panic in
    /// an impl's aborts (D48): unwinding would leave the move half done.
    #[inline(always)]
    pub(in crate::tree) fn hash_tag(self, key: &K) -> Option<u8> {
        match self {
            Self::ByteString(byte_string) => Some(byte_string.bytes(key).hash_tag()),
            Self::InlineKeyTrait { hash_tag, .. } => {
                let hash_tag = hash_tag?;
                Some(AbortOnUnwind::call(|| hash_tag(key)))
            }
        }
    }

    /// How many leading bytes the zero-padded encodings of `a` and `b` share
    /// under strategy `S`, compared a window at a time; 0 for keys without
    /// inline keys. A node bounded by `a` and `b` may skip that many bytes
    /// of its keys (05-fence-key.md).
    ///
    /// At most the longest prefix a node records. Two different keys can
    /// have equal padded encodings (`"ab"` and `"ab\0"`, or a lossy
    /// [`InlineKey`] impl), and stopping short is always safe, as any
    /// shorter prefix is. Byte strings stop comparing past the longer one's
    /// end, where every window is zeros.
    pub(in crate::tree) fn common_prefix_len<S: InlineKeyStrategy<K>>(a: &K, b: &K) -> usize {
        const MAX_LEN: usize = InlineKeyNodeHeader::MAX_PREFIX_LEN;
        let difference = match Self::resolve::<S>() {
            None => return 0,
            Some(Self::ByteString(byte_string)) => {
                let (a, b) = (byte_string.bytes(a), byte_string.bytes(b));
                Self::first_difference(
                    a.len().max(b.len()),
                    |offset| a.inline_key_at(offset),
                    |offset| b.inline_key_at(offset),
                )
            }
            Some(inliner) => Self::first_difference(
                MAX_LEN,
                |offset| inliner.inline_key_at(a, offset),
                |offset| inliner.inline_key_at(b, offset),
            ),
        };
        difference.map_or(MAX_LEN, |difference| difference.min(MAX_LEN))
    }

    /// The prefix length of a node whose fences are `low` and `high`, under
    /// strategy `S`: what they share. If either is missing, along the
    /// tree's borders, it is `shared_len`, what every key of the tree shares
    /// (D50).
    pub(in crate::tree) fn fence_prefix_len<S: InlineKeyStrategy<K>>(
        low: Option<&K>,
        high: Option<&K>,
        shared_len: usize,
    ) -> usize {
        match (low, high) {
            (Some(low), Some(high)) => Self::common_prefix_len::<S>(low, high),
            _ => shared_len,
        }
    }

    /// How many leading bytes `key`'s encoding shares with `shared` under
    /// strategy `S`, at most `shared.len()`; 0 for keys without inline keys.
    /// A byte string compares by its bytes, so that a key is at least as
    /// long as what it shares. Other keys compare by zero-padded windows.
    pub(in crate::tree) fn common_prefix_len_with_bytes<S: InlineKeyStrategy<K>>(
        key: &K,
        shared: &[u8],
    ) -> usize {
        match Self::resolve::<S>() {
            None => 0,
            Some(Self::ByteString(byte_string)) => (byte_string.bytes(key).iter().zip(shared))
                .take_while(|(a, b)| a == b)
                .count(),
            Some(inliner) => Self::first_difference(
                shared.len(),
                |offset| inliner.inline_key_at(key, offset),
                |offset| shared.inline_key_at(offset),
            )
            .map_or(shared.len(), |difference| difference.min(shared.len())),
        }
    }

    /// Where two zero-padded encodings first differ, before `limit`, if they
    /// do, compared a window at a time: `a_at(offset)` and `b_at(offset)`
    /// are their windows at `offset`.
    #[inline(always)]
    fn first_difference(
        limit: usize,
        a_at: impl Fn(usize) -> InlineWord,
        b_at: impl Fn(usize) -> InlineWord,
    ) -> Option<usize> {
        let mut offset = 0;
        while offset < limit {
            let differing_bits = a_at(offset) ^ b_at(offset);
            if differing_bits != 0 {
                return Some(offset + differing_bits.leading_zeros() as usize / 8);
            }
            offset += WINDOW_LEN;
        }
        None
    }

    /// Writes the first bytes of `key`'s encoding under strategy `S` into
    /// `bytes`, at most as many as it holds, and returns how many:
    /// - of a byte string, as many as it has;
    /// - of a std integer, its size;
    /// - of a key with its own impl, all of `bytes`, zero-padded past its
    ///   encoding;
    /// - of a key without inline keys, none.
    pub(in crate::tree) fn write_leading_bytes<S: InlineKeyStrategy<K>>(
        key: &K,
        bytes: &mut [u8],
    ) -> usize {
        let Some(inliner) = Self::resolve::<S>() else {
            return 0;
        };
        let len = match inliner {
            Self::ByteString(byte_string) => byte_string.bytes(key).len(),
            Self::InlineKeyTrait { .. } if StdInteger::<K>::recognize().is_some() => size_of::<K>(),
            Self::InlineKeyTrait { .. } => bytes.len(),
        }
        .min(bytes.len());
        for (index, chunk) in bytes[..len].chunks_mut(WINDOW_LEN).enumerate() {
            let window = inliner.inline_key_at(key, index * WINDOW_LEN).to_be_bytes();
            chunk.copy_from_slice(&window[..chunk.len()]);
        }
        len
    }
}

/// What a node stores an inline key as (D18): the first bytes of its window,
/// as many as the node kind's [`InlineWidth`], in an unsigned integer of as
/// many bytes, which orders like them.
pub(in crate::tree) trait InlineSlot: Ord + VectorCount {
    /// The first bytes of `inline_key`, which a node keeps.
    fn of_inline_key(inline_key: InlineWord) -> Self;

    /// The inline key that these bytes start, zeros after them.
    fn inline_key(self) -> InlineWord;
}

/// Implements [`InlineSlot`] for an unsigned integer narrower than an inline
/// key: its top bits.
macro_rules! narrow_inline_slot {
    ($slot:ty) => {
        impl InlineSlot for $slot {
            #[inline]
            fn of_inline_key(inline_key: InlineWord) -> Self {
                (inline_key >> (InlineWord::BITS - <$slot>::BITS)) as $slot
            }

            #[inline]
            fn inline_key(self) -> InlineWord {
                InlineWord::from(self) << (InlineWord::BITS - <$slot>::BITS)
            }
        }
    };
}

narrow_inline_slot!(u16);
narrow_inline_slot!(u32);

/// The whole inline key.
impl InlineSlot for u64 {
    #[inline]
    fn of_inline_key(inline_key: InlineWord) -> Self {
        inline_key
    }

    #[inline]
    fn inline_key(self) -> InlineWord {
        self
    }
}

/// An inline key with the prefix length it is taken at: bytes
/// `prefix_len..prefix_len + 8` of a key's encoding, of which a node with
/// that prefix length stores the first (05-fence-key.md). A search's query
/// takes one at each node it visits ([`SearchKey::inline_target`]).
///
/// A window read out of a node's slots holds the bytes the node keeps,
/// zeros after them. A window that an internal node of heap byte strings
/// hands out, for a separator or a fence, is read again from its key, all
/// eight bytes (D43), so that it can fill a leaf's. Other encodings keep the
/// same width in both node kinds. Moving to a shorter prefix keeps that many
/// correct bytes (`moved_back_to`); a node whose prefix grows reads its keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tree) struct Window {
    prefix_len: usize,
    inline_key: InlineWord,
}

impl Window {
    /// What a key without inline keys carries: nothing a node stores.
    pub(super) const NONE: Self = Self::new(0, 0);

    pub(in crate::tree) const fn new(prefix_len: usize, inline_key: InlineWord) -> Self {
        Self {
            prefix_len,
            inline_key,
        }
    }

    pub(in crate::tree) fn prefix_len(self) -> usize {
        self.prefix_len
    }

    pub(in crate::tree) fn inline_key(self) -> InlineWord {
        self.inline_key
    }

    /// The same key's window at `fill`'s shorter prefix length. `fill` is
    /// the window there of a key whose encoding shares this key's bytes up
    /// to this window's prefix length. The bytes that move in come from
    /// `fill`, so no key is read. This is how a node's prefix shrinks, and
    /// how a key moves into a node with a shorter one.
    ///
    /// If only the first `n` bytes of this window and of `fill` are right,
    /// as for windows read out of slots that keep `n`, then at least the
    /// first `n` of the result are. The `shift` bytes that move in are
    /// `fill`'s first, and this window's `n` follow them; for a shift past
    /// `n`, all are `fill`'s.
    ///
    /// A `fill` at a longer prefix length is a caller's bug. It gives wrong
    /// inline keys, and so wrong answers, but nothing worse.
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
/// every possible window would copy mostly bytes that it never reads. Std
/// byte strings' `InlineKey::cmp_from` compares them too.
#[derive(Clone, Copy)]
pub(crate) struct SearchBytes<'q> {
    bytes: &'q [u8],
}

impl<'q> SearchBytes<'q> {
    #[inline]
    pub(crate) fn new(bytes: &'q [u8]) -> Self {
        Self { bytes }
    }

    /// The query's inline key at `offset`. It checks the key's length before
    /// the window's fit: short queries need the padded word at every level,
    /// and testing for a full window first would add a branch on each node's
    /// prefix. Long queries keep their direct load. Fixed arrays use their
    /// constant-length path.
    #[inline(always)]
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        let Some(last) = self.bytes.last_chunk::<WINDOW_LEN>() else {
            return self.bytes.inline_key_at(offset);
        };
        let last_start = self.bytes.len() - WINDOW_LEN;
        if offset <= last_start {
            // The whole window fits, including when it ends at the last byte.
            return InlineWord::from_be_bytes(
                *self.bytes[offset..]
                    .first_chunk::<WINDOW_LEN>()
                    .expect("a whole word fits"),
            );
        }
        let shift = (offset - last_start).min(WINDOW_LEN);
        InlineWord::from_be_bytes(*last)
            .checked_shl((8 * shift) as u32)
            .unwrap_or(0)
    }

    /// The chunk of `bytes` at `offset`, if they have all of it.
    #[inline(always)]
    fn chunk_at(bytes: &[u8], offset: usize) -> Option<&[u8; SHARED_PREFIX_CHUNK_LEN]> {
        bytes.get(offset..)?.first_chunk()
    }

    /// How the key's bytes, padded with zeros, order against `prefix` in its
    /// first `prefix.len()` bytes: `Equal` if the key has them, or ends
    /// inside them with only zeros after it there. Std byte strings'
    /// `InlineKey::cmp_prefix` (D50).
    ///
    /// The bytes are almost always equal, and may be many (file-path keys
    /// can share 64). So whole 32-byte chunks first test equality without
    /// ordering them, and the rest compares a zero-padded window at a time.
    /// Windows are read from the whole keys, where most are a single load.
    /// Always inlined, with no call: a call anywhere in the search loop
    /// makes every search keep its state in stack slots (D41).
    #[inline(always)]
    pub(crate) fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        let query = self.bytes;
        let end = prefix.len();
        let mut offset = 0;
        while end - offset >= SHARED_PREFIX_CHUNK_LEN
            && let Some(query_chunk) = Self::chunk_at(query, offset)
            && Self::chunk_at(prefix, offset) == Some(query_chunk)
        {
            offset += SHARED_PREFIX_CHUNK_LEN;
        }
        while offset < end {
            let remaining = end - offset;
            let dropped_bits = 8 * WINDOW_LEN.saturating_sub(remaining) as u32;
            let query_window = query.inline_key_at(offset) >> dropped_bits;
            let prefix_window = prefix.inline_key_at(offset) >> dropped_bits;
            if query_window != prefix_window {
                return query_window.cmp(&prefix_window);
            }
            offset += WINDOW_LEN;
        }
        Ordering::Equal
    }

    /// How the key orders against `shared`, the bytes every key of the tree
    /// starts with (D50), as `<[u8]>::cmp` orders the key's first
    /// `shared.len()` bytes against them: as [`Self::cmp_prefix`] does, but
    /// a key ending inside them goes first, as every std byte string of the
    /// tree is at least as long.
    #[inline(always)]
    fn cmp_shared(&self, shared: &[u8]) -> Ordering {
        self.cmp_prefix(shared).then_with(|| {
            if self.bytes.len() < shared.len() {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        })
    }

    /// Orders the key against `stored`, a std byte string, as `<[u8]>::cmp`
    /// does, given that their first `equal_len` bytes are equal when padded
    /// with zeros. It compares 8 bytes at a time from there, as big-endian
    /// words, then by length. Every load is of a fixed size, so there is no
    /// call to `memcmp`, and a string that ends within the equal bytes isn't
    /// read at all.
    ///
    /// The last word ends where the shorter string does and may start before
    /// the bytes left to compare: the bytes it repeats are equal.
    #[inline]
    pub(crate) fn cmp_from(&self, stored: &[u8], equal_len: usize) -> Ordering {
        let query = self.bytes;
        let both_len = query.len().min(stored.len());
        if equal_len < both_len && both_len < WINDOW_LEN {
            // The word loop needs 8 bytes in both; shorter strings compare
            // by their zero-padded first window, then by length, which
            // breaks ties of only zeros.
            return query
                .inline_key_at(0)
                .cmp(&stored.inline_key_at(0))
                .then_with(|| query.len().cmp(&stored.len()));
        }
        let mut compared = equal_len;
        while compared < both_len {
            let word_end = (compared + WINDOW_LEN).min(both_len);
            let word = |bytes: &[u8]| {
                bytes[..word_end]
                    .last_chunk::<WINDOW_LEN>()
                    .map_or(0, |word| InlineWord::from_be_bytes(*word))
            };
            match word(query).cmp(&word(stored)) {
                Ordering::Equal => compared = word_end,
                unequal => return unequal,
            }
        }
        query.len().cmp(&stored.len())
    }
}

/// A key outside the nodes, with its window for the node it goes into
/// (D18), for a map with strategy `S`. In a map whose keys have no inline
/// keys, the window is [`Window::NONE`], and no node stores it.
pub(in crate::tree) struct WindowedKey<K, S> {
    key: K,
    window: Window,
    _strategy: PhantomData<fn() -> S>,
}

impl<K, S: InlineKeyStrategy<K>> WindowedKey<K, S> {
    /// `key`, with its window at `prefix_len`: for a node with that prefix
    /// length. Calls `InlineKey` with [`WithInlineKey`], so callers make it
    /// before they change anything.
    ///
    /// [`WithInlineKey`]: crate::WithInlineKey
    pub(in crate::tree) fn new(key: K, prefix_len: usize) -> Self {
        let window = KeyInliner::resolve::<S>().map_or(Window::NONE, |inliner| {
            Window::new(prefix_len, inliner.inline_key_at(&key, prefix_len))
        });
        Self::from_parts(key, window)
    }
}

impl<K, S> WindowedKey<K, S> {
    /// A key and the window it had in a node.
    pub(in crate::tree) fn from_parts(key: K, window: Window) -> Self {
        Self {
            key,
            window,
            _strategy: PhantomData,
        }
    }

    pub(super) fn into_parts(self) -> (K, Window) {
        (self.key, self.window)
    }

    /// The key with its window moved back to `fill`'s prefix length; see
    /// [`Window::moved_back_to`].
    pub(super) fn moved_back_to(self, fill: Window) -> Self {
        Self::from_parts(self.key, self.window.moved_back_to(fill))
    }

    pub(in crate::tree) fn into_key(self) -> K {
        self.key
    }
}

/// A separator outside its internal node (D17): on its way up from a split,
/// between internal nodes, or into a merged leaf. Without inline keys,
/// internal nodes keep their separator keys, and a separator is its key.
/// With inline keys (D49), it is the key's window alone, at the prefix
/// length of the node it is for; the key stays a leaf head and moves only
/// with leaf entries.
pub(in crate::tree) struct Separator<K, S> {
    key: Option<K>,
    window: Window,
    _strategy: PhantomData<fn() -> S>,
}

impl<K, S> Separator<K, S> {
    /// A separator that is `key`: in a map without inline keys.
    pub(in crate::tree) fn with_key(key: WindowedKey<K, S>) -> Self {
        let (key, window) = key.into_parts();
        Self {
            key: Some(key),
            window,
            _strategy: PhantomData,
        }
    }

    /// A separator whose key is a leaf head below (D49): `window` alone.
    pub(in crate::tree) fn without_key(window: Window) -> Self {
        Self {
            key: None,
            window,
            _strategy: PhantomData,
        }
    }

    pub(in crate::tree) fn window(&self) -> Window {
        self.window
    }

    /// The separator with its window moved back to `fill`'s prefix length;
    /// see [`Window::moved_back_to`].
    pub(super) fn moved_back_to(self, fill: Window) -> Self {
        Self {
            window: self.window.moved_back_to(fill),
            ..self
        }
    }

    /// The key, with its window, if the separator is one: in a map without
    /// inline keys.
    pub(in crate::tree) fn into_windowed_key(self) -> Option<WindowedKey<K, S>> {
        let window = self.window;
        self.key.map(|key| WindowedKey::from_parts(key, window))
    }

    /// The key, if the separator is one.
    #[cfg(test)]
    pub(in crate::tree) fn key(&self) -> Option<&K> {
        self.key.as_ref()
    }
}

/// What a search looks for: `key`, of a type `Q` that keys `K` borrow as, in
/// a map with strategy `S`. Where they can, the nodes compare its window at
/// each node's prefix with their inline keys (D18, 05-fence-key.md):
/// - for std byte-string keys, if `Q` is a std byte string too, ordered by
///   its bytes like them;
/// - for keys with their own impl, if `Q` is `K` itself.
///
/// Otherwise the search compares full keys. Every choice between the two
/// goes by the strategy, resolved for `K` where it is made, a constant: the
/// nodes of a map run only its own way.
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
    #[inline]
    pub(crate) fn new(key: &'q Q) -> Self {
        let bytes = Self::query_byte_string()
            .map(|byte_string| byte_string.bytes(key))
            .unwrap_or(&[]);
        Self {
            key,
            bytes: SearchBytes::new(bytes),
            _map: PhantomData,
        }
    }

    /// The key's window at a node's `prefix_len`, if the nodes can compare
    /// it with theirs. It holds all 8 bytes; the node narrows them to the
    /// ones it keeps as it compares. A window is two words, which stay in
    /// registers on the way down. Calls `InlineKey` with
    /// [`WithInlineKey`](crate::WithInlineKey), as searches do.
    ///
    /// Always inlined: whether there is one depends on the types alone, and
    /// a search must see that to drop its search over full keys. Outlined,
    /// it leaves that search, `memcmp` calls and all, in a `String` lookup's
    /// loop, and spills the loop's state to the stack.
    #[inline(always)]
    pub(super) fn inline_target(&self, prefix_len: usize) -> Option<Window> {
        let inline_key = match KeyInliner::<K>::resolve::<S>()? {
            KeyInliner::ByteString(_) => {
                let query = self.recognized_bytes()?;
                if Self::query_byte_string()?.holds_its_bytes() {
                    query.bytes.inline_key_at(prefix_len)
                } else {
                    query.inline_key_at(prefix_len)
                }
            }
            inliner @ KeyInliner::InlineKeyTrait { .. } => {
                let key = SameType::<K, Q>::recognize()?.cast(self.key);
                inliner.inline_key_at(key, prefix_len)
            }
        };
        Some(Window::new(prefix_len, inline_key))
    }

    /// The proof that `Q` is a std byte string, where `K` is one the
    /// strategy encodes by its bytes ([`Auto`](crate::Auto)): only then do
    /// the nodes compare a query by its bytes. Keys with their own
    /// `InlineKey` impl keep their `Ord`, whatever `Q` is.
    #[inline]
    fn query_byte_string() -> Option<ByteString<Q>> {
        let KeyInliner::ByteString(_) = KeyInliner::<K>::resolve::<S>()? else {
            return None;
        };
        ByteString::<Q>::recognize()
    }

    /// The query's bytes, if the nodes compare it by them. A query that
    /// holds its bytes is read again from the key, so that a fixed array's
    /// length stays a constant even in an outlined search. Heap strings use
    /// the cached slice, avoiding another metadata load.
    #[inline]
    fn recognized_bytes(&self) -> Option<SearchBytes<'q>> {
        let byte_string = Self::query_byte_string()?;
        Some(if byte_string.holds_its_bytes() {
            SearchBytes::new(byte_string.bytes(self.key))
        } else {
            self.bytes
        })
    }

    pub(in crate::tree) fn key(&self) -> &Q {
        self.key
    }

    /// Orders complete keys, without assuming any equal prefix. Only
    /// recognized built-in byte orderings use fixed-word comparison;
    /// explicit strategies and unrecognized queries keep their `Ord`.
    #[inline]
    pub(in crate::tree) fn cmp_full(&self, stored: &K) -> Ordering
    where
        Q: Ord,
    {
        if let Some(KeyInliner::ByteString(byte_string)) = KeyInliner::<K>::resolve::<S>()
            && let Some(query) = self.recognized_bytes()
        {
            return query.cmp_from(byte_string.bytes(stored), 0);
        }
        self.key.cmp(stored.borrow())
    }

    /// Whether the key equals `stored`, comparing their bytes all at once,
    /// for std byte string keys and a recognized byte query. `None` for any
    /// other, as keys with their own `InlineKey` impl keep their `Ord`. An
    /// exact-key lookup compares its one candidate this way, a fixed byte
    /// array's in a length the compiler knows (D38).
    #[inline(always)]
    pub(super) fn equals_whole_bytes(&self, stored: &K) -> Option<bool> {
        let KeyInliner::ByteString(byte_string) = KeyInliner::<K>::resolve::<S>()? else {
            return None;
        };
        Some(self.recognized_bytes()?.bytes == byte_string.bytes(stored))
    }

    /// The first line read to form the next search window. Short tails use
    /// the last whole word, as `inline_key_at` does; an empty tail reads none.
    #[inline(always)]
    pub(in crate::tree) fn window_line(&self, prefix_len: usize) -> Option<*const u8> {
        let bytes = self.recognized_bytes()?.bytes;
        if prefix_len >= bytes.len() {
            return None;
        }
        let offset = prefix_len.min(bytes.len().saturating_sub(WINDOW_LEN));
        bytes.get(offset).map(core::ptr::from_ref)
    }

    /// Whether the key, a recognized byte string, ends within the inline key
    /// that a node of kind `Kind`, of std byte strings, keeps at
    /// `prefix_len`; `false` for any other key. If it does, a stored key that
    /// shares those first `prefix_len` bytes and has an equal inline key is
    /// this key if their lengths are equal.
    #[inline(always)]
    pub(super) fn ends_within_window<Kind: NodeKind>(&self, prefix_len: usize) -> bool {
        let plan = NodePlan::<K>::of::<S, Kind>();
        let (Some(KeyInliner::ByteString(_)), Some(width), Some(query)) =
            (plan.inliner(), plan.inline_width(), self.recognized_bytes())
        else {
            return false;
        };
        query.bytes.len() <= prefix_len + width.byte_len()
    }

    /// The query's hash tag (D36), if the nodes can compare it with theirs:
    /// a recognized byte string's from its bytes, and with a key's own impl,
    /// that impl's if `Q` is `K`. Calls `InlineKey` with
    /// [`WithInlineKey`](crate::WithInlineKey), as searches do.
    #[inline]
    pub(super) fn hash_tag(&self) -> Option<u8> {
        match KeyInliner::<K>::resolve::<S>()? {
            KeyInliner::ByteString(_) => Some(self.recognized_bytes()?.bytes.hash_tag()),
            KeyInliner::InlineKeyTrait { hash_tag, .. } => {
                Some(hash_tag?(SameType::<K, Q>::recognize()?.cast(self.key)))
            }
        }
    }

    /// The first stored byte read by `cmp_tied`, after its entry metadata
    /// is ready. A short key ending inside the known prefix reads none.
    #[inline(always)]
    pub(super) fn tied_bytes_line<Kind: NodeKind>(
        &self,
        prefix_len: usize,
        stored: &K,
    ) -> Option<*const u8> {
        let plan = NodePlan::<K>::of::<S, Kind>();
        let KeyInliner::ByteString(byte_string) = plan.inliner()? else {
            return None;
        };
        let query = self.recognized_bytes()?;
        let equal_len = prefix_len + plan.inline_width()?.byte_len();
        if query.bytes.len() <= equal_len {
            return None;
        }
        let stored = byte_string.bytes(stored);
        let both_len = query.bytes.len().min(stored.len());
        if equal_len >= both_len {
            return None;
        }
        // cmp_from's last word ends at the shorter string's end; short
        // strings start at zero. This selects that same first load.
        let offset = (equal_len + WINDOW_LEN)
            .min(both_len)
            .saturating_sub(WINDOW_LEN);
        stored.get(offset).map(core::ptr::from_ref)
    }

    /// How this key orders against every key of a tree whose encodings all
    /// start with `shared` (D50). It is before or after them all if it
    /// differs in those bytes, or, as a byte string, ends inside them;
    /// `Equal` if it has them. `None` if the key has no encoding the nodes
    /// compare, as for a key with its own impl looked up by another type: its
    /// searches compare full keys and need no check.
    #[inline]
    pub(in crate::tree) fn cmp_shared_prefix(&self, shared: &[u8]) -> Option<Ordering> {
        match KeyInliner::<K>::resolve::<S>()? {
            KeyInliner::ByteString(_) => Some(self.recognized_bytes()?.cmp_shared(shared)),
            KeyInliner::InlineKeyTrait { cmp_prefix, .. } => Some(cmp_prefix(
                SameType::<K, Q>::recognize()?.cast(self.key),
                shared,
            )),
        }
    }

    /// How this key orders against `stored`, a key of a node of kind `Kind`
    /// that `target` is for, whose inline key equals this key's in the bytes
    /// the node keeps. Std byte strings compare from past the bytes known
    /// equal ([`SearchBytes::cmp_from`]): the node's prefix, and the bytes of
    /// each inline key its plan stores. So do keys whose impl has
    /// `InlineKey::cmp_from`, looked up by their own type. The search key
    /// shares the prefix because it lies between the node's fences, as every
    /// key in the node does. Other keys compare by `Q`'s `Ord`.
    #[inline]
    pub(super) fn cmp_tied<Kind: NodeKind>(&self, target: Window, stored: &K) -> Ordering
    where
        Q: Ord,
    {
        let plan = NodePlan::<K>::of::<S, Kind>();
        if let Some(width) = plan.inline_width() {
            let equal_len = target.prefix_len + width.byte_len();
            if let Some(KeyInliner::ByteString(byte_string)) = plan.inliner()
                && let Some(query) = self.recognized_bytes()
            {
                return query.cmp_from(byte_string.bytes(stored), equal_len);
            }
            if let Some(KeyInliner::InlineKeyTrait {
                cmp_from: Some(cmp_from),
                ..
            }) = plan.inliner()
                && let Some(same_type) = SameType::<K, Q>::recognize()
            {
                return cmp_from(same_type.cast(self.key), stored, equal_len);
            }
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
        InlineKeyNodeHeader, InlineSlot, KeyInliner, NodePlan, SearchBytes, SearchKey, WINDOW_LEN,
        Window, WindowedKey,
    };
    use crate::inline_key::{Auto, InlineKey, InlineWidth, InlineWord, WithInlineKey};
    use crate::tree::node::internal::Internal;
    use crate::tree::node::leaf::Leaf;
    use crate::tree::test_support::TestRng;

    #[test]
    fn query_windows_match_zero_padded_bytes_at_every_offset() {
        let max_len = if cfg!(miri) { 16 } else { 64 };
        let rng = TestRng::new(42);
        for len in 0..=max_len {
            for bytes in [
                (0..len).map(|_| rng.below(256) as u8).collect::<Vec<_>>(),
                std::vec![0; len],
                std::vec![u8::MAX; len],
            ] {
                let query = SearchBytes::new(&bytes);
                for offset in (0..=len + WINDOW_LEN).chain([usize::MAX]) {
                    let mut expected = [0; WINDOW_LEN];
                    for (slot, byte) in expected.iter_mut().zip(bytes.get(offset..).unwrap_or(&[]))
                    {
                        *slot = *byte;
                    }
                    assert_eq!(
                        query.inline_key_at(offset),
                        InlineWord::from_be_bytes(expected)
                    );
                }
            }
        }
    }

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
        assert!(SearchKey::<String, WithInlineKey, str>::query_byte_string().is_none());

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
            NodePlan::<K>::of::<S, Leaf<K, (), S>>().inline_width()
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

    /// A key that ends inside the shared prefix sorts first; bytes past it
    /// don't matter (D50).
    #[test]
    fn shared_prefix_checks_put_short_keys_first() {
        let stored = (0..193)
            .map(|i| [0, 0x80, 0xff, 1][i % 4])
            .collect::<Vec<_>>();
        for len in [1, 2, 7, 8, 9, 31, 32, 33, 63, 64, 65, 96] {
            let shared = &stored[..len];
            for query_len in 0..=stored.len() {
                let query = &stored[..query_len];
                let expected = if query_len < len {
                    Ordering::Less
                } else {
                    Ordering::Equal
                };
                assert_eq!(
                    SearchKey::<Vec<u8>, Auto, [u8]>::new(query).cmp_shared_prefix(shared),
                    Some(expected),
                    "shared length {len}, query length {query_len}"
                );
            }
            let mut query = stored.clone();
            query[len..].fill(0x77);
            assert_eq!(
                SearchKey::<Vec<u8>, Auto, [u8]>::new(&query).cmp_shared_prefix(shared),
                Some(Ordering::Equal),
            );
        }
    }

    /// A shared-prefix check orders a key by the first of the shared bytes
    /// in which it differs, and ignores bytes past them: a heap byte
    /// string's, across chunk boundaries, and a fixed-width key's, an
    /// integer's big-endian or an array's.
    #[test]
    fn shared_prefix_checks_order_by_the_first_difference() {
        const BYTES: [u8; 5] = [0, 1, 0x7f, 0x80, 0xff];
        let stored: Vec<u8> = (0..80).map(|index| BYTES[index % 5]).collect();
        for len in [1, 2, 8, 9, 32, 33, stored.len()] {
            for differing in 0..stored.len() {
                for byte in BYTES {
                    let mut query = stored.clone();
                    query[differing] = byte;
                    let expected = if differing < len {
                        byte.cmp(&stored[differing])
                    } else {
                        Ordering::Equal
                    };
                    assert_eq!(
                        SearchKey::<Vec<u8>, Auto, [u8]>::new(&query)
                            .cmp_shared_prefix(&stored[..len]),
                        Some(expected),
                        "byte {differing} of {len} shared set to {byte:#x}"
                    );
                }
            }
        }
        let rng = TestRng::new(49);
        let random_u128 = || u128::from_be_bytes(core::array::from_fn(|_| rng.below(256) as u8));
        for _ in 0..if cfg!(miri) { 300 } else { 30_000 } {
            let stored = random_u128();
            let differing = rng.below(16);
            let mut bytes = stored.to_be_bytes();
            bytes[differing] = BYTES[rng.below(BYTES.len())];
            let query = u128::from_be_bytes(bytes);
            let len = 1 + rng.below(16);
            let shared = &stored.to_be_bytes()[..len];
            let expected = if differing < len {
                bytes[differing].cmp(&stored.to_be_bytes()[differing])
            } else {
                Ordering::Equal
            };
            assert_eq!(
                SearchKey::<u128, Auto, u128>::new(&query).cmp_shared_prefix(shared),
                Some(expected),
                "{query:#x} vs {stored:#x}, {len} shared"
            );
            assert_eq!(
                SearchKey::<[u8; 16], Auto, [u8; 16]>::new(&query.to_be_bytes())
                    .cmp_shared_prefix(shared),
                Some(expected),
            );
            if len <= 8 {
                let query = (query >> 64) as u64;
                assert_eq!(
                    SearchKey::<u64, Auto, u64>::new(&query).cmp_shared_prefix(shared),
                    Some(expected),
                );
                // An `i64`'s encoding is its bits with the sign bit flipped.
                let signed = (query ^ 1 << 63).cast_signed();
                assert_eq!(
                    SearchKey::<i64, Auto, i64>::new(&signed).cmp_shared_prefix(shared),
                    Some(expected),
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
                    let shift = 8 * (InlineWidth::Eight.byte_len() - width.byte_len());
                    if search.inline_key_at(prefix_len) >> shift
                        != stored.inline_key_at(prefix_len) >> shift
                    {
                        continue;
                    }
                    let key = SearchKey::<Vec<u8>, Auto, [u8]>::new(&search);
                    let target = key.inline_target(prefix_len).unwrap();
                    let compared = match width {
                        InlineWidth::Four => {
                            key.cmp_tied::<Internal<Vec<u8>, (), Auto>>(target, &stored)
                        }
                        InlineWidth::Eight => {
                            key.cmp_tied::<Leaf<Vec<u8>, (), Auto>>(target, &stored)
                        }
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
            key.cmp_tied::<Internal<UserName, (), WithInlineKey>>(
                target,
                &UserName("ferrit".to_owned())
            ),
            Ordering::Less
        );
    }

    #[test]
    fn inlined_keys_carry_their_window() {
        let (key, window) =
            WindowedKey::<String, Auto>::new("ferris the crab".to_owned(), 7).into_parts();
        assert_eq!(window, Window::new(7, "ferris the crab".inline_key_at(7)));
        let target = SearchKey::<String, Auto, String>::new(&key)
            .inline_target(7)
            .map(|target| target.inline_key());
        assert_eq!(target, Some(window.inline_key()));

        let integer: WindowedKey<u64, Auto> = WindowedKey::new(7, 3);
        assert_eq!(
            integer.into_parts(),
            (7, Window::new(3, 7_u64.inline_key_at(3)))
        );

        let plain: WindowedKey<u32, Auto> = WindowedKey::new(7, 3);
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

    /// What a node of slot type `W` keeps of `inline_key`, zeros after it.
    fn kept<W: InlineSlot>(inline_key: InlineWord) -> InlineWord {
        W::of_inline_key(inline_key).inline_key()
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
        assert_eq!(u16::of_inline_key(window), 0x0102);
        assert_eq!(u32::of_inline_key(window), 0x0102_0304);
        assert_eq!(u64::of_inline_key(window), window);
        assert_eq!(0x0102_u16.inline_key(), 0x0102_0000_0000_0000);
        assert_eq!(0x0102_0304_u32.inline_key(), 0x0102_0304_0000_0000);
        assert!(u16::of_inline_key(window) < u16::of_inline_key(0x0103_0000_0000_0000));
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
        let by_trait = KeyInliner::<UserName>::resolve::<WithInlineKey>().unwrap();
        for _ in 0..if cfg!(miri) { 100 } else { 10_000 } {
            let stem = random_bytes(20);
            let (a, b) = (with_stem(&stem), with_stem(&stem));
            let Some(shared) = first_padded_difference(&a, &b) else {
                continue;
            };
            let (a, b) = (String::from_utf8(a).unwrap(), String::from_utf8(b).unwrap());
            assert_eq!(
                KeyInliner::common_prefix_len::<Auto>(&a, &b),
                shared,
                "{a:?} vs {b:?}"
            );
            let (a, b) = (UserName(a), UserName(b));
            assert_eq!(
                KeyInliner::common_prefix_len::<WithInlineKey>(&a, &b),
                shared
            );
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
        let max_len = InlineKeyNodeHeader::MAX_PREFIX_LEN;
        let (short, padded) = ("ab".to_owned(), "ab\0".to_owned());
        let common_prefix_len = KeyInliner::common_prefix_len::<Auto>;
        assert_eq!(common_prefix_len(&short, &padded), max_len);
        assert_eq!(common_prefix_len(&short, &short), max_len);
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
            assert_eq!(
                key.cmp_tied::<Leaf<[u8; 20], (), Auto>>(target, &stored),
                search.cmp(&stored)
            );
        }
    }

    /// An array's narrow prefix also accepts borrowed slices shorter than
    /// one word, including a proper prefix ending in zeros.
    #[test]
    fn arrays_compare_short_borrowed_slices() {
        let key = SearchKey::<[u8; 20], Auto, [u8]>::new(&[0, 0, 1]);
        assert_eq!(
            key.cmp_tied::<Leaf<[u8; 20], (), Auto>>(key.inline_target(0).unwrap(), &[0; 20]),
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
                        key.cmp_tied::<Internal<[u8; 20], (), Auto>>(target, &stored),
                        search.cmp(&stored),
                        "{len} bytes, byte {changed} = {byte}"
                    );
                }
            }
        }
    }
}
