//! What the std functions that the code Verus proves calls do, and how
//! long a slice can be, as std documents them: trusted, not proven, and
//! erased by a normal build like every spec
//! (`doc/08-fuzzing-verification.md`). vstd has no specs for these.
use vstd::prelude::*;

verus! {

// Verus wants each spec as visible as the std function it specifies, so they
// are `pub`, in a private module.
pub assume_specification<T, const N: usize>[ <[T]>::first_chunk::<N> ](slice: &[T]) -> (chunk:
    Option<&[T; N]>)
    ensures
        chunk.is_some() <==> slice@.len() >= N,
        chunk.is_some() ==> chunk.unwrap()@ == slice@.subrange(0, N as int),
;

pub assume_specification<T, const N: usize>[ <[T]>::last_chunk::<N> ](slice: &[T]) -> (chunk:
    Option<&[T; N]>)
    ensures
        chunk.is_some() <==> slice@.len() >= N,
        chunk.is_some() ==> chunk.unwrap()@ == slice@.subrange(
            slice@.len() - N,
            slice@.len() as int,
        ),
;

pub assume_specification[ u64::checked_shl ](word: u64, shift: u32) -> (shifted: Option<u64>)
    ensures
        shifted.is_some() <==> shift < 64,
        shifted.is_some() ==> shifted.unwrap() == word << shift,
;

/// A slice of bytes has at most `isize::MAX` of them, as every Rust
/// allocation does.
pub(super) axiom fn axiom_byte_slice_fits_isize(bytes: &[u8])
    ensures
        bytes@.len() <= isize::MAX,
;

/// `==` on arrays of bytes, whose impl, generic over the arrays' elements,
/// Verus can't give a spec.
pub(super) struct ByteArrays;

impl ByteArrays {
    #[verifier::external_body]
    #[inline(always)]
    pub(super) fn equal<const N: usize>(left: &[u8; N], right: &[u8; N]) -> (equal: bool)
        ensures
            equal == (left@ == right@),
    {
        left == right
    }
}

/// `from_be_bytes`, whose signatures, sized by `size_of::<Self>()`, Verus
/// can't match to give them specs.
pub(super) struct BigEndian;

impl BigEndian {
    /// Eight bytes as a big-endian word.
    #[verusfmt::skip]
    pub(super) open spec fn spec_u64(bytes: Seq<u8>) -> u64 {
        (bytes[0] as u64) << 56 | (bytes[1] as u64) << 48 | (bytes[2] as u64) << 40
            | (bytes[3] as u64) << 32 | (bytes[4] as u64) << 24 | (bytes[5] as u64) << 16
            | (bytes[6] as u64) << 8 | bytes[7] as u64
    }

    /// Four bytes as a big-endian word.
    pub(super) open spec fn spec_u32(bytes: Seq<u8>) -> u32 {
        (bytes[0] as u32) << 24 | (bytes[1] as u32) << 16 | (bytes[2] as u32) << 8 | bytes[3] as u32
    }

    /// Two bytes as a big-endian word.
    pub(super) open spec fn spec_u16(bytes: Seq<u8>) -> u16 {
        (bytes[0] as u16) << 8 | bytes[1] as u16
    }

    #[verifier::external_body]
    #[inline(always)]
    pub(super) fn u64_of(bytes: [u8; 8]) -> (word: u64)
        ensures
            word == Self::spec_u64(bytes@),
    {
        u64::from_be_bytes(bytes)
    }

    #[verifier::external_body]
    #[inline(always)]
    pub(super) fn u32_of(bytes: [u8; 4]) -> (word: u32)
        ensures
            word == Self::spec_u32(bytes@),
    {
        u32::from_be_bytes(bytes)
    }

    #[verifier::external_body]
    #[inline(always)]
    pub(super) fn u16_of(bytes: [u8; 2]) -> (word: u16)
        ensures
            word == Self::spec_u16(bytes@),
    {
        u16::from_be_bytes(bytes)
    }
}

} // verus!
