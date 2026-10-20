//! The inline keys of the std key types, which their `InlineKey` impls
//! return (`src/inline_key.rs`), proven by Verus to keep `InlineKey`'s
//! contract: every key has an encoding, a byte string that orders like the
//! key, and its inline key at `offset` is bytes `offset..offset + 8` of it,
//! big-endian, zero past its end. So where the encodings of two keys agree
//! before `offset` and their inline keys there differ, the keys order like
//! those inline keys ([`lemma_inline_keys_order_keys`]).
//!
//! What this file states is what to review: `inline_key/lemmas.rs` holds
//! the proofs, which Verus checks, and `std_specs.rs` what the std functions
//! called here do, trusted.
use vstd::prelude::*;

use super::std_specs::BigEndian;

#[cfg(verus_only)]
pub(super) mod lemmas;

#[cfg(verus_only)]
use lemmas::*;

verus! {

/// Byte `index` of `encoding`, zero past its end.
pub(crate) open spec fn padded_byte(encoding: Seq<u8>, index: int) -> u8 {
    if 0 <= index < encoding.len() {
        encoding[index]
    } else {
        0
    }
}

/// Bytes `offset..offset + 8` of `encoding`, big-endian, zero past its end.
#[verusfmt::skip]
pub(crate) open spec fn inline_key_at(encoding: Seq<u8>, offset: int) -> u64 {
    (padded_byte(encoding, offset) as u64) << 56
        | (padded_byte(encoding, offset + 1) as u64) << 48
        | (padded_byte(encoding, offset + 2) as u64) << 40
        | (padded_byte(encoding, offset + 3) as u64) << 32
        | (padded_byte(encoding, offset + 4) as u64) << 24
        | (padded_byte(encoding, offset + 5) as u64) << 16
        | (padded_byte(encoding, offset + 6) as u64) << 8
        | padded_byte(encoding, offset + 7) as u64
}

/// Byte `index` of `word`, big-endian.
pub(crate) open spec fn byte_of(word: u64, index: int) -> u8 {
    (word >> (56 - 8 * index) as u64) as u8
}

/// Whether `left` and `right`, both padded with zeros, agree before
/// `offset`.
pub(crate) open spec fn agree_before(left: Seq<u8>, right: Seq<u8>, offset: int) -> bool {
    forall|index: int|
        0 <= index < offset ==> #[trigger] padded_byte(left, index) == padded_byte(right, index)
}

/// Whether `left` sorts before `right`, both padded with zeros.
pub(crate) open spec fn sorts_before(left: Seq<u8>, right: Seq<u8>) -> bool {
    exists|index: int|
        0 <= index && agree_before(left, right, index) && #[trigger] padded_byte(left, index)
            < padded_byte(right, index)
}

/// Whether `left` comes before `right` in std's order of byte strings: at
/// the first byte where they differ, or as a proper prefix of `right`.
pub(crate) open spec fn lexicographically_before(left: Seq<u8>, right: Seq<u8>) -> bool {
    (exists|index: int|
        0 <= index < left.len() && index < right.len() && (forall|before: int|
            0 <= before < index ==> left[before] == right[before]) && #[trigger] left[index]
            < right[index]) || (left.len() < right.len() && forall|index: int|
        0 <= index < left.len() ==> left[index] == right[index])
}

/// A key type whose inline keys keep `InlineKey`'s contract.
pub(crate) trait ProvenInlineKey {
    /// The key's encoding.
    spec fn encoding(&self) -> Seq<u8>;

    /// Whether `self` orders before `other` by the key's `Ord`.
    spec fn orders_before(&self, other: &Self) -> bool;

    /// The key's inline key at `offset`.
    fn inline_key_at(&self, offset: usize) -> (inline_key: u64)
        ensures
            inline_key == inline_key_at(self.encoding(), offset as int),
    ;

    /// Keys order like their encodings, both padded with zeros.
    proof fn lemma_orders_like_encoding(&self, other: &Self)
        requires
            sorts_before(self.encoding(), other.encoding()),
        ensures
            self.orders_before(other),
    ;
}

/// Where the encodings of two keys agree before `offset` and their inline
/// keys there differ, the keys order like the inline keys: what lets a node
/// search compare inline keys before keys.
proof fn lemma_inline_keys_order_keys<Key: ProvenInlineKey + ?Sized>(
    left: &Key,
    right: &Key,
    offset: int,
)
    requires
        0 <= offset,
        agree_before(left.encoding(), right.encoding(), offset),
        inline_key_at(left.encoding(), offset) < inline_key_at(right.encoding(), offset),
    ensures
        left.orders_before(right),
{
    lemma_inline_keys_order_encodings(left.encoding(), right.encoding(), offset);
    left.lemma_orders_like_encoding(right);
}

/// Its bytes are its encoding.
impl ProvenInlineKey for [u8] {
    open spec fn encoding(&self) -> Seq<u8> {
        self@
    }

    open spec fn orders_before(&self, other: &Self) -> bool {
        lexicographically_before(self@, other@)
    }

    /// With loads of a fixed size only, so no call to `memcpy`: the 8 bytes
    /// at `offset` if there are as many; otherwise the last 8, shifted so that
    /// the bytes from `offset` lead and zeros follow; or, for a string of
    /// fewer than 8 bytes, overlapping 4-, 2- or 1-byte loads, shifted the
    /// same way.
    #[inline]
    #[allow(clippy::collapsible_if)]  // Verus doesn't support let chains.
    fn inline_key_at(&self, offset: usize) -> (inline_key: u64) {
        if let Some((_, rest)) = self.split_at_checked(offset) {
            if let Some(window) = rest.first_chunk::<8>() {
                proof {
                    lemma_eight_bytes(self@, offset as int);
                }
                return BigEndian::u64_of(*window);
            }
        }
        let (word, word_start) = match self.last_chunk::<8>() {
            Some(last) => {
                proof {
                    lemma_eight_bytes(self@, self.len() - 8);
                }
                (BigEndian::u64_of(*last), self.len() - 8)
            },
            None => {
                let word = if let Some(first) = self.first_chunk::<4>() {
                    let last = self.last_chunk::<4>().expect("the first four bytes fit");
                    proof {
                        lemma_four_to_seven_bytes(self@);
                    }
                    ((BigEndian::u32_of(*first) as u64) << 32) | ((BigEndian::u32_of(*last) as u64)
                        << (8 * (8 - self.len())))
                } else if let Some(first) = self.first_chunk::<2>() {
                    let last = *self.last().expect("the first two bytes fit");
                    proof {
                        lemma_two_or_three_bytes(self@);
                    }
                    ((BigEndian::u16_of(*first) as u64) << 48) | ((last as u64) << (8 * (8
                        - self.len())))
                } else if let Some(first) = self.first() {
                    proof {
                        lemma_one_byte(self@);
                    }
                    (*first as u64) << 56
                } else {
                    proof {
                        lemma_inline_key_past_end(self@, 0);
                    }
                    0
                };
                (word, 0)
            },
        };
        // The window starts past `word_start`, as it didn't fit.
        let shift_bytes = (offset - word_start).min(8);
        proof {
            if shift_bytes < 8 {
                lemma_shifted_inline_key(self@, word_start as int, shift_bytes as u64);
            } else {
                lemma_inline_key_past_end(self@, offset as int);
            }
        }
        word.checked_shl((8 * shift_bytes) as u32).unwrap_or(0)
    }

    proof fn lemma_orders_like_encoding(&self, other: &Self) {
        lemma_byte_strings_order_like_bytes(self@, other@);
    }
}

/// Its big-endian bytes, which order like it.
impl ProvenInlineKey for u64 {
    open spec fn encoding(&self) -> Seq<u8> {
        Seq::new(8, |index: int| byte_of(*self, index))
    }

    open spec fn orders_before(&self, other: &Self) -> bool {
        *self < *other
    }

    /// The bytes before `offset` shifted out, which leaves the window on
    /// top, zeros after the last byte.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> (inline_key: u64) {
        proof {
            lemma_u64_inline_key(*self, offset as int);
        }
        if offset < 8 {
            *self << (8 * offset)
        } else {
            0
        }
    }

    proof fn lemma_orders_like_encoding(&self, other: &Self) {
        lemma_u64s_order_like_encodings(*self, *other);
    }
}

/// Its big-endian bytes, which order like it.
impl ProvenInlineKey for u128 {
    open spec fn encoding(&self) -> Seq<u8> {
        Seq::new(16, |index: int| (*self >> (120 - 8 * index) as u64) as u8)
    }

    open spec fn orders_before(&self, other: &Self) -> bool {
        *self < *other
    }

    /// The bytes before `offset` shifted out, which leaves the window in the
    /// top 8 bytes, zeros after the last byte.
    #[inline]
    fn inline_key_at(&self, offset: usize) -> (inline_key: u64) {
        proof {
            lemma_u128_inline_key(*self, offset as int);
        }
        if offset < 16 {
            ((*self << (8 * offset)) >> 64) as u64
        } else {
            0
        }
    }

    proof fn lemma_orders_like_encoding(&self, other: &Self) {
        lemma_u128s_order_like_encodings(*self, *other);
    }
}

/// That of its bits with the sign bit flipped, as a `u64`: big-endian bytes
/// that order like it, negatives first.
impl ProvenInlineKey for i64 {
    open spec fn encoding(&self) -> Seq<u8> {
        ((*self as u64) ^ (1u64 << 63)).encoding()
    }

    open spec fn orders_before(&self, other: &Self) -> bool {
        *self < *other
    }

    #[inline]
    fn inline_key_at(&self, offset: usize) -> (inline_key: u64) {
        (*self as u64 ^ (1 << 63)).inline_key_at(offset)
    }

    proof fn lemma_orders_like_encoding(&self, other: &Self) {
        lemma_u64s_order_like_encodings(
            (*self as u64) ^ (1u64 << 63),
            (*other as u64) ^ (1u64 << 63),
        );
        lemma_flipped_sign_orders_i64(*self, *other);
    }
}

/// That of its bits with the sign bit flipped, as a `u128`: big-endian bytes
/// that order like it, negatives first.
impl ProvenInlineKey for i128 {
    open spec fn encoding(&self) -> Seq<u8> {
        ((*self as u128) ^ (1u128 << 127)).encoding()
    }

    open spec fn orders_before(&self, other: &Self) -> bool {
        *self < *other
    }

    #[inline]
    fn inline_key_at(&self, offset: usize) -> (inline_key: u64) {
        (*self as u128 ^ (1 << 127)).inline_key_at(offset)
    }

    proof fn lemma_orders_like_encoding(&self, other: &Self) {
        lemma_u128s_order_like_encodings(
            (*self as u128) ^ (1u128 << 127),
            (*other as u128) ^ (1u128 << 127),
        );
        lemma_flipped_sign_orders_i128(*self, *other);
    }
}

} // verus!
