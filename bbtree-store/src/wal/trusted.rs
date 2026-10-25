//! What the WAL's proofs trust (`design/bbtree-store/01-wal.md`,
//! "Verification"): CRC32C, as a function of its bytes, and little-endian
//! words, as vstd's spec functions describe them. Not proven, and erased by
//! a normal build like every spec. Reading words is vstd's own trusted
//! `u32_from_le_bytes` and `u64_from_le_bytes`.
use std::vec::Vec;

use vstd::prelude::*;

#[cfg(verus_only)]
use vstd::bytes::{spec_u32_to_le_bytes, spec_u64_to_le_bytes};

verus! {

/// CRC32C. The proofs know nothing of its value but that it is a function
/// of its bytes.
pub(super) struct Crc32c;

impl Crc32c {
    pub(super) uninterp spec fn spec_of(bytes: Seq<u8>) -> u32;

    #[verifier::external_body]
    pub(super) fn of(bytes: &[u8]) -> (crc: u32)
        ensures
            crc == Self::spec_of(bytes@),
    {
        crc32c::crc32c(bytes)
    }
}

/// Little-endian words written into byte vectors.
pub(super) struct LittleEndian;

impl LittleEndian {
    #[verifier::external_body]
    pub(super) fn push_u32(out: &mut Vec<u8>, word: u32)
        ensures
            final(out)@ == old(out)@ + spec_u32_to_le_bytes(word),
    {
        out.extend_from_slice(&word.to_le_bytes());
    }

    #[verifier::external_body]
    pub(super) fn push_u64(out: &mut Vec<u8>, word: u64)
        ensures
            final(out)@ == old(out)@ + spec_u64_to_le_bytes(word),
    {
        out.extend_from_slice(&word.to_le_bytes());
    }

    /// Writes `word` over the four bytes at `at`.
    #[verifier::external_body]
    pub(super) fn write_u32(out: &mut [u8], at: usize, word: u32)
        requires
            at + 4 <= old(out)@.len(),
        ensures
            final(out)@ == old(out)@.subrange(0, at as int) + spec_u32_to_le_bytes(word) + old(
                out,
            )@.subrange(at + 4, old(out)@.len() as int),
    {
        out[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
}

} // verus!
