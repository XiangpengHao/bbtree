//! The proofs of `inline_key.rs`.
use vstd::prelude::*;

use super::*;

verus! {

// Words and their bytes.
/// A word is its bytes, big-endian.
#[verusfmt::skip]
proof fn lemma_word_of_its_bytes(word: u64)
    ensures
        word == (byte_of(word, 0) as u64) << 56
            | (byte_of(word, 1) as u64) << 48
            | (byte_of(word, 2) as u64) << 40
            | (byte_of(word, 3) as u64) << 32
            | (byte_of(word, 4) as u64) << 24
            | (byte_of(word, 5) as u64) << 16
            | (byte_of(word, 6) as u64) << 8
            | byte_of(word, 7) as u64,
{
    assert(word == ((word >> 56u64) as u8 as u64) << 56
        | ((word >> 48u64) as u8 as u64) << 48
        | ((word >> 40u64) as u8 as u64) << 40
        | ((word >> 32u64) as u8 as u64) << 32
        | ((word >> 24u64) as u8 as u64) << 24
        | ((word >> 16u64) as u8 as u64) << 16
        | ((word >> 8u64) as u8 as u64) << 8
        | ((word >> 0u64) as u8 as u64)) by (bit_vector);
}

#[verusfmt::skip]
proof fn lemma_bytes_of_word(word: u64, b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8)
    by (bit_vector)
    requires
        word == (b0 as u64) << 56 | (b1 as u64) << 48 | (b2 as u64) << 40 | (b3 as u64) << 32
            | (b4 as u64) << 24 | (b5 as u64) << 16 | (b6 as u64) << 8 | b7 as u64,
    ensures
        (word >> 56u64) as u8 == b0,
        (word >> 48u64) as u8 == b1,
        (word >> 40u64) as u8 == b2,
        (word >> 32u64) as u8 == b3,
        (word >> 24u64) as u8 == b4,
        (word >> 16u64) as u8 == b5,
        (word >> 8u64) as u8 == b6,
        (word >> 0u64) as u8 == b7,
{
}

/// A word whose bytes are those of `encoding` from `offset` is its inline
/// key there.
pub(in crate::proof) proof fn lemma_inline_key_of_bytes(word: u64, encoding: Seq<u8>, offset: int)
    requires
        forall|index: int|
            0 <= index < 8 ==> #[trigger] byte_of(word, index) == padded_byte(
                encoding,
                offset + index,
            ),
    ensures
        word == inline_key_at(encoding, offset),
{
    lemma_word_of_its_bytes(word);
}

/// The bytes of the inline key at `offset` are those of `encoding` from
/// `offset`.
pub(in crate::proof) proof fn lemma_bytes_of_inline_key(encoding: Seq<u8>, offset: int)
    ensures
        forall|index: int|
            0 <= index < 8 ==> #[trigger] byte_of(inline_key_at(encoding, offset), index)
                == padded_byte(encoding, offset + index),
{
    lemma_bytes_of_word(
        inline_key_at(encoding, offset),
        padded_byte(encoding, offset),
        padded_byte(encoding, offset + 1),
        padded_byte(encoding, offset + 2),
        padded_byte(encoding, offset + 3),
        padded_byte(encoding, offset + 4),
        padded_byte(encoding, offset + 5),
        padded_byte(encoding, offset + 6),
        padded_byte(encoding, offset + 7),
    );
}

proof fn lemma_byte_of_shifted(word: u64, shift: u64, index: u64)
    by (bit_vector)
    requires
        shift < 8,
        index < 8,
    ensures
        ((word << (8 * shift) as u64) >> (56 - 8 * index) as u64) as u8 == if shift + index < 8 {
            (word >> (56 - 8 * (shift + index)) as u64) as u8
        } else {
            0u8
        },
{
}

/// An encoding with no bytes past `start + 8` has its inline key at
/// `start + shift` in that at `start`, shifted up `shift` bytes.
pub(in crate::proof) proof fn lemma_shifted_inline_key(encoding: Seq<u8>, start: int, shift: u64)
    requires
        encoding.len() <= start + 8,
        shift < 8,
    ensures
        inline_key_at(encoding, start) << (8 * shift) as u64 == inline_key_at(
            encoding,
            start + shift,
        ),
{
    let word = inline_key_at(encoding, start);
    lemma_bytes_of_inline_key(encoding, start);
    assert forall|index: int| 0 <= index < 8 implies #[trigger] byte_of(
        word << (8 * shift) as u64,
        index,
    ) == padded_byte(encoding, start + shift + index) by {
        lemma_byte_of_shifted(word, shift, index as u64);
        if shift + index < 8 {
            assert(byte_of(word, shift + index) == padded_byte(encoding, start + shift + index));
        }
    }
    lemma_inline_key_of_bytes(word << (8 * shift) as u64, encoding, start + shift);
}

/// Past its end, an encoding's inline keys are zero.
#[verusfmt::skip]
pub(in crate::proof) proof fn lemma_inline_key_past_end(encoding: Seq<u8>, offset: int)
    requires
        encoding.len() <= offset,
    ensures
        inline_key_at(encoding, offset) == 0,
{
    assert(0u64 == (0u8 as u64) << 56 | (0u8 as u64) << 48 | (0u8 as u64) << 40
        | (0u8 as u64) << 32 | (0u8 as u64) << 24 | (0u8 as u64) << 16 | (0u8 as u64) << 8
        | 0u8 as u64) by (bit_vector);
}

// Byte strings' inline keys.
/// Eight bytes of `encoding` from `offset`, big-endian, are its inline key
/// there.
pub(in crate::proof) proof fn lemma_eight_bytes(encoding: Seq<u8>, offset: int)
    requires
        0 <= offset,
        offset + 8 <= encoding.len(),
    ensures
        BigEndian::spec_u64(encoding.subrange(offset, offset + 8)) == inline_key_at(
            encoding,
            offset,
        ),
{
}

proof fn lemma_bytes_of_u32(word: u32, b0: u8, b1: u8, b2: u8, b3: u8)
    by (bit_vector)
    requires
        word == (b0 as u32) << 24 | (b1 as u32) << 16 | (b2 as u32) << 8 | b3 as u32,
    ensures
        (word >> 24u32) as u8 == b0,
        (word >> 16u32) as u8 == b1,
        (word >> 8u32) as u8 == b2,
        (word >> 0u32) as u8 == b3,
{
}

proof fn lemma_or_itself(byte: u8)
    by (bit_vector)
    ensures
        byte | byte == byte,
        byte | 0 == byte,
        0 | byte == byte,
{
}

/// Byte `index` of the word that a string of `len` bytes, 4 to 7, makes of
/// its first 4 and its last 4: that byte of the first 4, or of the last, or
/// both where they overlap.
#[verusfmt::skip]
proof fn lemma_byte_of_halves(first: u32, last: u32, len: usize, index: u64)
    by (bit_vector)
    requires
        4 <= len < 8,
        index < 8,
    ensures
        (((first as u64) << 32 | (last as u64) << (8 * (8 - len)) as usize)
            >> (56 - 8 * index) as u64) as u8
            == (if index < 4 { (first >> (24 - 8 * index) as u32) as u8 } else { 0u8 })
            | (if len - 4 <= index < len {
                (last >> (24 - 8 * (index - (len - 4))) as u32) as u8
            } else {
                0u8
            }),
{
}

/// A string of 4 to 7 bytes is its inline key at 0 in its first 4 bytes and
/// its last 4, which overlap.
pub(super) proof fn lemma_four_to_seven_bytes(bytes: Seq<u8>)
    requires
        4 <= bytes.len() < 8,
    ensures
        (BigEndian::spec_u32(bytes.subrange(0, 4)) as u64) << 32 | (BigEndian::spec_u32(
            bytes.subrange(bytes.len() - 4, bytes.len() as int),
        ) as u64) << (8 * (8 - bytes.len())) as usize == inline_key_at(bytes, 0),
{
    let len = bytes.len() as usize;
    let first = BigEndian::spec_u32(bytes.subrange(0, 4));
    let last = BigEndian::spec_u32(bytes.subrange(len - 4, len as int));
    let word = (first as u64) << 32 | (last as u64) << (8 * (8 - len)) as usize;
    lemma_bytes_of_u32(first, bytes[0], bytes[1], bytes[2], bytes[3]);
    lemma_bytes_of_u32(last, bytes[len - 4], bytes[len - 3], bytes[len - 2], bytes[len - 1]);
    assert forall|index: int| 0 <= index < 8 implies #[trigger] byte_of(word, index) == padded_byte(
        bytes,
        index,
    ) by {
        lemma_byte_of_halves(first, last, len, index as u64);
        lemma_or_itself(padded_byte(bytes, index));
    }
    lemma_inline_key_of_bytes(word, bytes, 0);
}

/// A string of 2 or 3 bytes is its inline key at 0 in its first 2 bytes and
/// its last, which may overlap.
#[verusfmt::skip]
pub(super) proof fn lemma_two_or_three_bytes(bytes: Seq<u8>)
    requires
        2 <= bytes.len() < 4,
    ensures
        (BigEndian::spec_u16(bytes.subrange(0, 2)) as u64) << 48
            | (bytes[bytes.len() - 1] as u64) << (8 * (8 - bytes.len())) as usize
            == inline_key_at(bytes, 0),
{
    let (b0, b1, b2) = (bytes[0], bytes[1], padded_byte(bytes, 2));
    if bytes.len() == 2 {
        assert((((b0 as u16) << 8 | b1 as u16) as u64) << 48 | (b1 as u64) << 48usize
            == (b0 as u64) << 56 | (b1 as u64) << 48 | (0u8 as u64) << 40 | (0u8 as u64) << 32
            | (0u8 as u64) << 24 | (0u8 as u64) << 16 | (0u8 as u64) << 8 | 0u8 as u64)
            by (bit_vector);
    } else {
        assert((((b0 as u16) << 8 | b1 as u16) as u64) << 48 | (b2 as u64) << 40usize
            == (b0 as u64) << 56 | (b1 as u64) << 48 | (b2 as u64) << 40 | (0u8 as u64) << 32
            | (0u8 as u64) << 24 | (0u8 as u64) << 16 | (0u8 as u64) << 8 | 0u8 as u64)
            by (bit_vector);
    }
}

/// A string of one byte is its inline key at 0 in that byte.
#[verusfmt::skip]
pub(super) proof fn lemma_one_byte(bytes: Seq<u8>)
    requires
        bytes.len() == 1,
    ensures
        (bytes[0] as u64) << 56 == inline_key_at(bytes, 0),
{
    let b0 = bytes[0];
    assert((b0 as u64) << 56 == (b0 as u64) << 56 | (0u8 as u64) << 48 | (0u8 as u64) << 40
        | (0u8 as u64) << 32 | (0u8 as u64) << 24 | (0u8 as u64) << 16 | (0u8 as u64) << 8
        | 0u8 as u64) by (bit_vector);
}

// Integers' inline keys.
/// A `u64` is its inline key at 0, and its inline key at `offset` is it
/// shifted up `offset` bytes.
pub(super) proof fn lemma_u64_inline_key(key: u64, offset: int)
    requires
        0 <= offset,
    ensures
        inline_key_at(key.encoding(), 0) == key,
        inline_key_at(key.encoding(), offset) == if offset < 8 {
            key << (8 * offset) as u64
        } else {
            0
        },
{
    lemma_inline_key_of_bytes(key, key.encoding(), 0);
    if offset < 8 {
        lemma_shifted_inline_key(key.encoding(), 0, offset as u64);
    } else {
        lemma_inline_key_past_end(key.encoding(), offset);
    }
}

#[verusfmt::skip]
proof fn lemma_byte_of_u128_window(key: u128, offset: u64, index: u64)
    by (bit_vector)
    requires
        offset < 16,
        index < 8,
    ensures
        ((((key << (8 * offset) as u64) >> 64u64) as u64) >> (56 - 8 * index) as u64) as u8
            == if offset + index < 16 {
                (key >> (120 - 8 * (offset + index)) as u64) as u8
            } else {
                0u8
            },
{
}

/// A `u128`'s inline key at `offset` is its top 8 bytes once shifted up
/// `offset` bytes.
pub(super) proof fn lemma_u128_inline_key(key: u128, offset: int)
    requires
        0 <= offset,
    ensures
        inline_key_at(key.encoding(), offset) == if offset < 16 {
            ((key << (8 * offset) as u64) >> 64u64) as u64
        } else {
            0
        },
{
    if offset < 16 {
        let inline_key = ((key << (8 * offset) as u64) >> 64u64) as u64;
        assert forall|index: int| 0 <= index < 8 implies #[trigger] byte_of(inline_key, index)
            == padded_byte(key.encoding(), offset + index) by {
            lemma_byte_of_u128_window(key, offset as u64, index as u64);
        }
        lemma_inline_key_of_bytes(inline_key, key.encoding(), offset);
    } else {
        lemma_inline_key_past_end(key.encoding(), offset);
    }
}

// Order.
/// Words order like their bytes, big-endian.
#[verusfmt::skip]
proof fn lemma_words_order_like_bytes(left: u64, right: u64)
    ensures
        (left < right) == (
            byte_of(left, 0) < byte_of(right, 0) || byte_of(left, 0) == byte_of(right, 0) && (
            byte_of(left, 1) < byte_of(right, 1) || byte_of(left, 1) == byte_of(right, 1) && (
            byte_of(left, 2) < byte_of(right, 2) || byte_of(left, 2) == byte_of(right, 2) && (
            byte_of(left, 3) < byte_of(right, 3) || byte_of(left, 3) == byte_of(right, 3) && (
            byte_of(left, 4) < byte_of(right, 4) || byte_of(left, 4) == byte_of(right, 4) && (
            byte_of(left, 5) < byte_of(right, 5) || byte_of(left, 5) == byte_of(right, 5) && (
            byte_of(left, 6) < byte_of(right, 6) || byte_of(left, 6) == byte_of(right, 6) && (
            byte_of(left, 7) < byte_of(right, 7))))))))),
{
    assert((left < right) == (
        ((left >> 56u64) as u8) < ((right >> 56u64) as u8) || ((left >> 56u64) as u8) == ((right >> 56u64) as u8) && (
        ((left >> 48u64) as u8) < ((right >> 48u64) as u8) || ((left >> 48u64) as u8) == ((right >> 48u64) as u8) && (
        ((left >> 40u64) as u8) < ((right >> 40u64) as u8) || ((left >> 40u64) as u8) == ((right >> 40u64) as u8) && (
        ((left >> 32u64) as u8) < ((right >> 32u64) as u8) || ((left >> 32u64) as u8) == ((right >> 32u64) as u8) && (
        ((left >> 24u64) as u8) < ((right >> 24u64) as u8) || ((left >> 24u64) as u8) == ((right >> 24u64) as u8) && (
        ((left >> 16u64) as u8) < ((right >> 16u64) as u8) || ((left >> 16u64) as u8) == ((right >> 16u64) as u8) && (
        ((left >> 8u64) as u8) < ((right >> 8u64) as u8) || ((left >> 8u64) as u8) == ((right >> 8u64) as u8) && (
        ((left >> 0u64) as u8) < ((right >> 0u64) as u8)))))))))) by (bit_vector);
}

/// Encodings that agree before `offset + shift` and differ there sort like
/// their bytes there.
proof fn lemma_sorts_before_at(left: Seq<u8>, right: Seq<u8>, offset: int, shift: int)
    requires
        0 <= offset,
        0 <= shift < 8,
        agree_before(left, right, offset),
        forall|index: int|
            0 <= index < shift ==> #[trigger] padded_byte(left, offset + index) == padded_byte(
                right,
                offset + index,
            ),
        padded_byte(left, offset + shift) < padded_byte(right, offset + shift),
    ensures
        sorts_before(left, right),
{
    assert forall|index: int| 0 <= index < offset + shift implies #[trigger] padded_byte(
        left,
        index,
    ) == padded_byte(right, index) by {
        if index >= offset {
            assert(padded_byte(left, offset + (index - offset)) == padded_byte(
                right,
                offset + (index - offset),
            ));
        }
    }
}

/// Where two encodings agree before `offset` and their inline keys there
/// differ, the encodings sort like the inline keys.
pub(in crate::proof) proof fn lemma_inline_keys_order_encodings(
    left: Seq<u8>,
    right: Seq<u8>,
    offset: int,
)
    requires
        0 <= offset,
        agree_before(left, right, offset),
        inline_key_at(left, offset) < inline_key_at(right, offset),
    ensures
        sorts_before(left, right),
{
    let (left_key, right_key) = (inline_key_at(left, offset), inline_key_at(right, offset));
    lemma_bytes_of_inline_key(left, offset);
    lemma_bytes_of_inline_key(right, offset);
    lemma_words_order_like_bytes(left_key, right_key);
    let shift = if byte_of(left_key, 0) != byte_of(right_key, 0) {
        0
    } else if byte_of(left_key, 1) != byte_of(right_key, 1) {
        1
    } else if byte_of(left_key, 2) != byte_of(right_key, 2) {
        2
    } else if byte_of(left_key, 3) != byte_of(right_key, 3) {
        3
    } else if byte_of(left_key, 4) != byte_of(right_key, 4) {
        4
    } else if byte_of(left_key, 5) != byte_of(right_key, 5) {
        5
    } else if byte_of(left_key, 6) != byte_of(right_key, 6) {
        6
    } else {
        7
    };
    lemma_sorts_before_at(left, right, offset, shift);
}

/// Encodings that agree before `offset + shift` and differ there have
/// inline keys at `offset` that order like their bytes there.
proof fn lemma_encodings_order_inline_keys(left: Seq<u8>, right: Seq<u8>, offset: int, shift: int)
    requires
        0 <= offset,
        0 <= shift < 8,
        agree_before(left, right, offset + shift),
        padded_byte(left, offset + shift) < padded_byte(right, offset + shift),
    ensures
        inline_key_at(left, offset) < inline_key_at(right, offset),
{
    let (left_key, right_key) = (inline_key_at(left, offset), inline_key_at(right, offset));
    lemma_bytes_of_inline_key(left, offset);
    lemma_bytes_of_inline_key(right, offset);
    lemma_words_order_like_bytes(left_key, right_key);
}

/// Byte strings that sort before others, both padded with zeros, come
/// before them in std's order.
pub(in crate::proof) proof fn lemma_byte_strings_order_like_bytes(left: Seq<u8>, right: Seq<u8>)
    requires
        sorts_before(left, right),
    ensures
        lexicographically_before(left, right),
{
    let index = choose|index: int|
        0 <= index && agree_before(left, right, index) && #[trigger] padded_byte(left, index)
            < padded_byte(right, index);
    assert forall|before: int| 0 <= before < index && before < left.len() implies left[before]
        == right[before] by {
        assert(padded_byte(left, before) == padded_byte(right, before));
    }
}

pub(super) proof fn lemma_u64s_order_like_encodings(left: u64, right: u64)
    requires
        sorts_before(left.encoding(), right.encoding()),
    ensures
        left < right,
{
    let index = choose|index: int|
        0 <= index && agree_before(left.encoding(), right.encoding(), index)
            && #[trigger] padded_byte(left.encoding(), index) < padded_byte(
            right.encoding(),
            index,
        );
    lemma_encodings_order_inline_keys(left.encoding(), right.encoding(), 0, index);
    lemma_u64_inline_key(left, 0);
    lemma_u64_inline_key(right, 0);
}

#[verusfmt::skip]
proof fn lemma_u128s_order_like_halves(left: u128, right: u128)
    by (bit_vector)
    ensures
        (((left << 0u64) >> 64u64) as u64) < (((right << 0u64) >> 64u64) as u64) ==> left < right,
        (((left << 0u64) >> 64u64) as u64) == (((right << 0u64) >> 64u64) as u64)
            && (((left << 64u64) >> 64u64) as u64) < (((right << 64u64) >> 64u64) as u64)
            ==> left < right,
{
}

pub(super) proof fn lemma_u128s_order_like_encodings(left: u128, right: u128)
    requires
        sorts_before(left.encoding(), right.encoding()),
    ensures
        left < right,
{
    let index = choose|index: int|
        0 <= index && agree_before(left.encoding(), right.encoding(), index)
            && #[trigger] padded_byte(left.encoding(), index) < padded_byte(
            right.encoding(),
            index,
        );
    lemma_u128_inline_key(left, 0);
    lemma_u128_inline_key(right, 0);
    lemma_u128s_order_like_halves(left, right);
    if index < 8 {
        lemma_encodings_order_inline_keys(left.encoding(), right.encoding(), 0, index);
    } else {
        lemma_u128_inline_key(left, 8);
        lemma_u128_inline_key(right, 8);
        lemma_encodings_order_inline_keys(left.encoding(), right.encoding(), 8, index - 8);
    }
}

pub(super) proof fn lemma_flipped_sign_orders_i64(left: i64, right: i64)
    by (bit_vector)
    ensures
        ((left as u64) ^ (1u64 << 63)) < ((right as u64) ^ (1u64 << 63)) ==> left < right,
{
}

pub(super) proof fn lemma_flipped_sign_orders_i128(left: i128, right: i128)
    by (bit_vector)
    ensures
        ((left as u128) ^ (1u128 << 127)) < ((right as u128) ^ (1u128 << 127)) ==> left < right,
{
}

} // verus!
