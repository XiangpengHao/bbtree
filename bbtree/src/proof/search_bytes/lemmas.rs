//! The proofs of `search_bytes.rs`.
use vstd::math::min;
use vstd::prelude::*;

use super::super::inline_key::byte_of;
use super::super::inline_key::lemmas::{
    lemma_byte_strings_order_like_bytes, lemma_bytes_of_inline_key, lemma_inline_key_of_bytes,
    lemma_inline_keys_order_encodings,
};
use super::*;

verus! {

// Byte strings.
/// Where two strings agree before `offset`, their inline keys there tell how
/// they go on: equal ones, that the strings agree for 8 more bytes; unequal
/// ones, how the strings order.
pub(super) proof fn lemma_inline_keys_order_strings(left: Seq<u8>, right: Seq<u8>, offset: int)
    requires
        0 <= offset,
        agree_before(left, right, offset),
    ensures
        inline_key_at(left, offset) == inline_key_at(right, offset) ==> agree_before(
            left,
            right,
            offset + 8,
        ),
        inline_key_at(left, offset) < inline_key_at(right, offset) ==> lexicographically_before(
            left,
            right,
        ),
        inline_key_at(left, offset) > inline_key_at(right, offset) ==> lexicographically_before(
            right,
            left,
        ),
{
    let (left_key, right_key) = (inline_key_at(left, offset), inline_key_at(right, offset));
    if left_key == right_key {
        lemma_bytes_of_inline_key(left, offset);
        lemma_bytes_of_inline_key(right, offset);
        assert forall|index: int| 0 <= index < offset + 8 implies #[trigger] padded_byte(
            left,
            index,
        ) == padded_byte(right, index) by {
            if index >= offset {
                assert(byte_of(left_key, index - offset) == byte_of(right_key, index - offset));
            }
        }
    } else if left_key < right_key {
        lemma_inline_keys_order_encodings(left, right, offset);
        lemma_byte_strings_order_like_bytes(left, right);
    } else {
        lemma_inline_keys_order_encodings(right, left, offset);
        lemma_byte_strings_order_like_bytes(right, left);
    }
}

/// Strings that agree before `len`, at least as long as the shorter one is,
/// order by their lengths.
pub(super) proof fn lemma_lengths_order_strings(left: Seq<u8>, right: Seq<u8>, len: int)
    requires
        agree_before(left, right, len),
        len >= min(left.len() as int, right.len() as int),
    ensures
        left.len() < right.len() ==> lexicographically_before(left, right),
        left.len() == right.len() ==> left == right,
        left.len() > right.len() ==> lexicographically_before(right, left),
{
    assert forall|index: int|
        0 <= index < min(left.len() as int, right.len() as int) implies left[index]
        == right[index] by {
        assert(padded_byte(left, index) == padded_byte(right, index));
    }
}

// Prefixes.
/// Where a key and `prefix` have the same chunk at `offset`, the key's
/// padded first `prefix.len()` bytes agree with `prefix` through it.
pub(super) proof fn lemma_equal_chunk_agrees(
    query: Seq<u8>,
    prefix: Seq<u8>,
    offset: int,
    chunk_len: int,
)
    requires
        0 <= offset,
        0 <= chunk_len,
        offset + chunk_len <= query.len(),
        offset + chunk_len <= prefix.len(),
        query.subrange(offset, offset + chunk_len) == prefix.subrange(offset, offset + chunk_len),
        agree_before(padded_prefix(query, prefix.len()), prefix, offset),
    ensures
        agree_before(padded_prefix(query, prefix.len()), prefix, offset + chunk_len),
{
    let padded = padded_prefix(query, prefix.len());
    assert forall|index: int| 0 <= index < offset + chunk_len implies #[trigger] padded_byte(
        padded,
        index,
    ) == padded_byte(prefix, index) by {
        if index >= offset {
            assert(query.subrange(offset, offset + chunk_len)[index - offset] == query[index]);
        }
    }
}

proof fn lemma_byte_of_kept(word: u64, kept: u64, index: u64)
    by (bit_vector)
    requires
        1 <= kept <= 8,
        index < 8,
    ensures
        (((word >> (8 * (8 - kept)) as u64) << (8 * (8 - kept)) as u64) >> (56 - 8
            * index) as u64) as u8 == if index < kept {
            (word >> (56 - 8 * index) as u64) as u8
        } else {
            0u8
        },
{
}

/// The inline key at `offset` of the first `len` bytes of `encoding`,
/// padded, is `encoding`'s with the bytes past `len` shifted out, and zeros
/// shifted in for them.
proof fn lemma_window_of_padded_prefix(encoding: Seq<u8>, len: int, offset: int)
    requires
        0 <= offset < len,
    ensures
        ({
            let dropped_bits = (8 * (8 - min(len - offset, 8))) as u64;
            (inline_key_at(encoding, offset) >> dropped_bits) << dropped_bits == inline_key_at(
                padded_prefix(encoding, len as nat),
                offset,
            )
        }),
{
    let kept = min(len - offset, 8);
    let dropped_bits = (8 * (8 - kept)) as u64;
    let word = inline_key_at(encoding, offset);
    let padded = padded_prefix(encoding, len as nat);
    lemma_bytes_of_inline_key(encoding, offset);
    assert forall|index: int| 0 <= index < 8 implies #[trigger] byte_of(
        (word >> dropped_bits) << dropped_bits,
        index,
    ) == padded_byte(padded, offset + index) by {
        lemma_byte_of_kept(word, kept as u64, index as u64);
        assert(byte_of(word, index) == padded_byte(encoding, offset + index));
    }
    lemma_inline_key_of_bytes((word >> dropped_bits) << dropped_bits, padded, offset);
}

proof fn lemma_shifted_out_words_order(left: u64, right: u64, shift: u64)
    by (bit_vector)
    requires
        shift < 64,
    ensures
        (left >> shift == right >> shift) == ((left >> shift) << shift == (right >> shift)
            << shift),
        (left >> shift < right >> shift) == ((left >> shift) << shift < (right >> shift) << shift),
{
}

/// A window of `cmp_prefix`: the key's inline key at `offset` and
/// `prefix`'s, both with the bytes past `prefix` shifted out, order like
/// the key's padded first `prefix.len()` bytes against `prefix`, which agree
/// before `offset`, or show them agreeing for 8 more bytes.
pub(super) proof fn lemma_kept_windows_order(
    query: Seq<u8>,
    prefix: Seq<u8>,
    offset: int,
    dropped_bits: u32,
)
    requires
        0 <= offset < prefix.len(),
        dropped_bits == 8 * (8 - min(prefix.len() - offset, 8)),
        agree_before(padded_prefix(query, prefix.len()), prefix, offset),
    ensures
        ({
            let padded = padded_prefix(query, prefix.len());
            let query_window = inline_key_at(query, offset) >> dropped_bits;
            let prefix_window = inline_key_at(prefix, offset) >> dropped_bits;
            &&& query_window == prefix_window ==> agree_before(padded, prefix, offset + 8)
            &&& query_window < prefix_window ==> lexicographically_before(padded, prefix)
            &&& query_window > prefix_window ==> lexicographically_before(prefix, padded)
        }),
{
    let padded = padded_prefix(query, prefix.len());
    lemma_window_of_padded_prefix(query, prefix.len() as int, offset);
    lemma_window_of_padded_prefix(prefix, prefix.len() as int, offset);
    lemma_shifted_out_words_order(
        inline_key_at(query, offset),
        inline_key_at(prefix, offset),
        dropped_bits as u64,
    );
    lemma_inline_keys_order_strings(padded, prefix, offset);
}

/// A key whose padded first `prefix.len()` bytes agree with `prefix` before
/// its end has them.
pub(super) proof fn lemma_agreeing_prefix(query: Seq<u8>, prefix: Seq<u8>, offset: int)
    requires
        offset >= prefix.len(),
        agree_before(padded_prefix(query, prefix.len()), prefix, offset),
    ensures
        padded_prefix(query, prefix.len()) == prefix,
{
    let padded = padded_prefix(query, prefix.len());
    assert forall|index: int| 0 <= index < prefix.len() implies padded[index] == prefix[index] by {
        assert(padded_byte(padded, index) == padded_byte(prefix, index));
    }
}

/// How a key's first `prefix.len()` bytes order against `prefix`: as its
/// padded first `prefix.len()` bytes do, except that a key that ends inside
/// `prefix`, with only zeros after it there, goes first.
pub(super) proof fn lemma_truncation_orders_like_padding(query: Seq<u8>, prefix: Seq<u8>)
    ensures
        ({
            let padded = padded_prefix(query, prefix.len());
            let truncated = query.subrange(0, min(query.len() as int, prefix.len() as int));
            &&& lexicographically_before(padded, prefix) ==> lexicographically_before(
                truncated,
                prefix,
            )
            &&& lexicographically_before(prefix, padded) ==> lexicographically_before(
                prefix,
                truncated,
            )
            &&& padded == prefix && query.len() < prefix.len() ==> lexicographically_before(
                truncated,
                prefix,
            )
            &&& padded == prefix && query.len() >= prefix.len() ==> truncated == prefix
        }),
{
    let padded = padded_prefix(query, prefix.len());
    let truncated = query.subrange(0, min(query.len() as int, prefix.len() as int));
    if lexicographically_before(padded, prefix) {
        let index = choose|index: int|
            0 <= index < padded.len() && index < prefix.len() && (forall|before: int|
                0 <= before < index ==> padded[before] == prefix[before])
                && #[trigger] padded[index] < prefix[index];
        if index < truncated.len() {
            assert(truncated[index] < prefix[index]);
        }
    }
}

} // verus!
