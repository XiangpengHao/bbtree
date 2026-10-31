//! The bytes of a std byte string being searched for ([`SearchBytes`]),
//! proven by Verus to compare like std: their inline keys are those of
//! `inline_key.rs`, and they order against a stored key, against a prefix
//! and against what every key of a tree shares as `<[u8]>::cmp` orders
//! them.
//!
//! What this file states is what to review: `search_bytes/lemmas.rs` holds
//! the proofs, which Verus checks, and `std_specs.rs` what the std functions
//! called here do, trusted.
use core::cmp::Ordering;

use vstd::prelude::*;

use super::inline_key::ProvenInlineKey;
use super::std_specs::{BigEndian, ByteArrays};

#[cfg(verus_only)]
use super::inline_key::lemmas::{
    lemma_eight_bytes, lemma_inline_key_past_end, lemma_shifted_inline_key,
};
#[cfg(verus_only)]
use super::inline_key::{agree_before, inline_key_at, lexicographically_before, padded_byte};
#[cfg(verus_only)]
use super::std_specs::axiom_byte_slice_fits_isize;

#[cfg(verus_only)]
mod lemmas;

#[cfg(verus_only)]
use lemmas::*;

verus! {

/// How many bytes a search's check against what every key shares tests for
/// equality at once ([`SearchBytes::cmp_prefix`]).
const SHARED_PREFIX_CHUNK_LEN: usize = 32;

/// Whether `ordering` is how std orders the byte string `left` against
/// `right` (`<[u8]>::cmp`).
pub(crate) open spec fn is_std_cmp(ordering: Ordering, left: Seq<u8>, right: Seq<u8>) -> bool {
    match ordering {
        Ordering::Less => lexicographically_before(left, right),
        Ordering::Equal => left == right,
        Ordering::Greater => lexicographically_before(right, left),
    }
}

/// The first `len` bytes of `encoding`, padded with zeros.
pub(crate) open spec fn padded_prefix(encoding: Seq<u8>, len: nat) -> Seq<u8> {
    Seq::new(len, |index: int| padded_byte(encoding, index))
}

/// The bytes of a std byte string being searched for. Windows read these
/// bytes directly: a search visits only a few prefix lengths, so preparing
/// every possible window would copy mostly bytes that it never reads. Std
/// byte strings' `InlineKey::cmp_from` and `InlineKey::cmp_prefix` compare
/// them too.
#[derive(Clone, Copy)]
pub(crate) struct SearchBytes<'q> {
    bytes: &'q [u8],
}

impl View for SearchBytes<'_> {
    type V = Seq<u8>;

    closed spec fn view(&self) -> Seq<u8> {
        self.bytes@
    }
}

impl<'q> SearchBytes<'q> {
    #[inline]
    pub(crate) fn new(bytes: &'q [u8]) -> (search: Self)
        ensures
            search@ == bytes@,
    {
        Self { bytes }
    }

    #[inline]
    pub(crate) fn bytes(&self) -> (bytes: &'q [u8])
        ensures
            bytes@ == self@,
    {
        self.bytes
    }

    /// The query's inline key at `offset`. It checks the key's length before
    /// the window's fit: short queries need the padded word at every level,
    /// and testing for a full window first would add a branch on each node's
    /// prefix. Long queries keep their direct load. Fixed arrays use their
    /// constant-length path.
    #[inline(always)]
    pub(crate) fn inline_key_at(&self, offset: usize) -> (inline_key: u64)
        ensures
            inline_key == inline_key_at(self@, offset as int),
    {
        let last = match self.bytes.last_chunk::<8>() {
            Some(last) => last,
            None => return self.bytes.inline_key_at(offset),
        };
        let last_start = self.bytes.len() - 8;
        if offset <= last_start {
            // The whole window fits, including when it ends at the last byte.
            proof {
                lemma_eight_bytes(self@, offset as int);
            }
            return BigEndian::u64_of(
                *self.bytes[offset..].first_chunk::<8>().expect("a whole word fits"),
            );
        }
        let shift = (offset - last_start).min(8);
        proof {
            lemma_eight_bytes(self@, last_start as int);
            if shift < 8 {
                lemma_shifted_inline_key(self@, last_start as int, shift as u64);
            } else {
                lemma_inline_key_past_end(self@, offset as int);
            }
        }
        BigEndian::u64_of(*last).checked_shl((8 * shift) as u32).unwrap_or(0)
    }

    /// The chunk of `bytes` at `offset`, if they have all of it.
    #[inline(always)]
    fn chunk_at(bytes: &[u8], offset: usize) -> (chunk: Option<&[u8; SHARED_PREFIX_CHUNK_LEN]>)
        ensures
            chunk matches Some(chunk) ==> offset + SHARED_PREFIX_CHUNK_LEN <= bytes@.len() && chunk@
                == bytes@.subrange(offset as int, offset + SHARED_PREFIX_CHUNK_LEN),
    {
        match bytes.split_at_checked(offset) {
            Some((_, rest)) => rest.first_chunk(),
            None => None,
        }
    }

    /// Whether the key and `prefix` both have the chunk at `offset`, and it
    /// is the same.
    #[inline(always)]
    fn shares_chunk_at(&self, prefix: &[u8], offset: usize) -> (shares: bool)
        ensures
            shares ==> offset + SHARED_PREFIX_CHUNK_LEN <= self@.len() && offset
                + SHARED_PREFIX_CHUNK_LEN <= prefix@.len() && self@.subrange(
                offset as int,
                offset + SHARED_PREFIX_CHUNK_LEN,
            ) == prefix@.subrange(offset as int, offset + SHARED_PREFIX_CHUNK_LEN),
    {
        match (Self::chunk_at(self.bytes, offset), Self::chunk_at(prefix, offset)) {
            (Some(query_chunk), Some(prefix_chunk)) => ByteArrays::equal(query_chunk, prefix_chunk),
            _ => false,
        }
    }

    /// How the key's bytes, padded with zeros, order against `prefix` in its
    /// first `prefix.len()` bytes: `Equal` if the key has them, or ends
    /// inside them with only zeros after it there. Std byte strings'
    /// `InlineKey::cmp_prefix` (D50).
    ///
    /// The bytes are almost always equal, and may be many (file-path keys
    /// can share all 39 a map keeps). So whole 32-byte chunks first test
    /// equality without ordering them, and the rest compares a zero-padded
    /// window at a time.
    /// Windows are read from the whole keys, where most are a single load.
    /// Always inlined, with no call: a call anywhere in the search loop
    /// makes every search keep its state in stack slots (D41).
    #[inline(always)]
    pub(crate) fn cmp_prefix(&self, prefix: &[u8]) -> (ordering: Ordering)
        ensures
            is_std_cmp(ordering, padded_prefix(self@, prefix@.len()), prefix@),
    {
        let query = self.bytes;
        let end = prefix.len();
        let mut offset = 0;
        while end - offset >= SHARED_PREFIX_CHUNK_LEN && self.shares_chunk_at(prefix, offset)
            invariant
                query@ == self@,
                end == prefix@.len(),
                offset <= end,
                agree_before(padded_prefix(self@, prefix@.len()), prefix@, offset as int),
            decreases end - offset,
        {
            proof {
                lemma_equal_chunk_agrees(
                    self@,
                    prefix@,
                    offset as int,
                    SHARED_PREFIX_CHUNK_LEN as int,
                );
            }
            offset += SHARED_PREFIX_CHUNK_LEN;
        }
        proof {
            axiom_byte_slice_fits_isize(prefix);
        }
        while offset < end
            invariant
                query@ == self@,
                end == prefix@.len(),
                end <= isize::MAX,
                offset < end + 8,
                agree_before(padded_prefix(self@, prefix@.len()), prefix@, offset as int),
            decreases end + 8 - offset,
        {
            let remaining = end - offset;
            let dropped_bits = 8 * 8_usize.saturating_sub(remaining) as u32;
            let query_window = query.inline_key_at(offset) >> dropped_bits;
            let prefix_window = prefix.inline_key_at(offset) >> dropped_bits;
            proof {
                lemma_kept_windows_order(self@, prefix@, offset as int, dropped_bits);
            }
            if query_window != prefix_window {
                return query_window.cmp(&prefix_window);
            }
            offset += 8;
        }
        proof {
            lemma_agreeing_prefix(self@, prefix@, offset as int);
        }
        Ordering::Equal
    }

    /// How the key orders against `shared`, the bytes every key of the tree
    /// starts with (D50), as `<[u8]>::cmp` orders the key's first
    /// `shared.len()` bytes against them: as [`Self::cmp_prefix`] does, but
    /// a key ending inside them goes first, as every std byte string of the
    /// tree is at least as long.
    #[inline(always)]
    pub(crate) fn cmp_shared(&self, shared: &[u8]) -> (ordering: Ordering)
        ensures
            is_std_cmp(
                ordering,
                self@.subrange(0, vstd::math::min(self@.len() as int, shared@.len() as int)),
                shared@,
            ),
    {
        let ordering = self.cmp_prefix(shared);
        proof {
            lemma_truncation_orders_like_padding(self@, shared@);
        }
        match ordering {
            Ordering::Equal if self.bytes.len() < shared.len() => Ordering::Less,
            ordering => ordering,
        }
    }

    /// The 8 bytes of `bytes` that end at `end`, as a big-endian word.
    #[inline(always)]
    fn word_ending_at(bytes: &[u8], end: usize) -> (word: u64)
        requires
            8 <= end <= bytes@.len(),
        ensures
            word == inline_key_at(bytes@, end - 8),
    {
        proof {
            lemma_eight_bytes(bytes@, end - 8);
            assert(bytes@.subrange(0, end as int).subrange(end - 8, end as int) =~= bytes@.subrange(
                end - 8,
                end as int,
            ));
        }
        match bytes[0..end].last_chunk::<8>() {
            Some(word) => BigEndian::u64_of(*word),
            None => 0,
        }
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
    pub(crate) fn cmp_from(&self, stored: &[u8], equal_len: usize) -> (ordering: Ordering)
        requires
            agree_before(self@, stored@, equal_len as int),
        ensures
            is_std_cmp(ordering, self@, stored@),
    {
        let query = self.bytes;
        let both_len = query.len().min(stored.len());
        if equal_len < both_len && both_len < 8 {
            // The word loop needs 8 bytes in both; shorter strings compare
            // by their zero-padded first window, then by length, which
            // breaks ties of only zeros.
            let (query_window, stored_window) = (query.inline_key_at(0), stored.inline_key_at(0));
            proof {
                lemma_inline_keys_order_strings(self@, stored@, 0);
                if query_window == stored_window {
                    lemma_lengths_order_strings(self@, stored@, 8);
                }
            }
            return match query_window.cmp(&stored_window) {
                Ordering::Equal => query.len().cmp(&stored.len()),
                unequal => unequal,
            };
        }
        proof {
            axiom_byte_slice_fits_isize(query);
        }
        let mut compared = equal_len;
        while compared < both_len
            invariant
                query@ == self@,
                both_len == vstd::math::min(query@.len() as int, stored@.len() as int),
                query@.len() <= isize::MAX,
                compared < both_len ==> 8 <= both_len,
                agree_before(self@, stored@, compared as int),
            decreases both_len - compared,
        {
            let word_end = (compared + 8).min(both_len);
            let query_word = Self::word_ending_at(query, word_end);
            let stored_word = Self::word_ending_at(stored, word_end);
            proof {
                lemma_inline_keys_order_strings(self@, stored@, word_end - 8);
            }
            match query_word.cmp(&stored_word) {
                Ordering::Equal => compared = word_end,
                unequal => return unequal,
            }
        }
        proof {
            lemma_lengths_order_strings(self@, stored@, compared as int);
        }
        query.len().cmp(&stored.len())
    }
}

} // verus!
