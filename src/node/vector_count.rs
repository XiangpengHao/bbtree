//! The count over a node's inline keys (D18) in explicit AVX-512 vectors, for
//! slots of 2 and 4 bytes: the search's inline key broadcast to every lane,
//! one load and two compares per 64 bytes of slots, the compares' bits
//! gathered 64 slots to a word, the live ones kept with one `bzhi` by `len`,
//! and counted with one `popcnt`.
//!
//! The compiler's own vectors for the same count depend on the capacity:
//! for capacities that aren't a multiple of the vector, such as the 23
//! slots of a `u64 → [u64; 4]` leaf, it compared in vectors but then took
//! the mask apart a bit at a time, more than doubling the instructions of
//! the whole lookup (05-fence-key.md).

use core::ops::Range;

/// Inline-key slots that the target counts in explicit vectors.
pub(super) trait VectorCount: Copy {
    /// The positions of the first `len` of `slots` equal to `target`: those
    /// below it, then those equal to it, counted. Exactly the equal ones for
    /// sorted slots, and a range within `0..len` for any, as both counts are
    /// of disjoint live slots. `None` where the target has no such vectors,
    /// or the slots count faster otherwise.
    ///
    /// The loads and compares run over the capacity, `slots.len()`, a
    /// constant per node kind, so their masks are constants and they don't
    /// wait for `len`, which only the word masks read; nor do they read
    /// what the slots past `len` hold.
    fn equal_run_in_vectors(slots: &[Self], len: usize, target: Self) -> Option<Range<usize>>;
}

/// Implements [`VectorCount`] for slots of one width, with the AVX-512
/// intrinsics for its lanes.
macro_rules! vector_count {
    ($slot:ty, $lane:ty, $mask:ty, $set1:ident, $load:ident, $below:ident, $equal:ident) => {
        impl VectorCount for $slot {
            #[inline(always)]
            fn equal_run_in_vectors(
                slots: &[Self],
                len: usize,
                target: Self,
            ) -> Option<Range<usize>> {
                #[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
                {
                    use core::arch::x86_64::{_bzhi_u64, $below, $equal, $load, $set1};

                    const LANES: usize = <$mask>::BITS as usize;
                    const WORD_LANES: usize = u64::BITS as usize;
                    let (mut below, mut equal) = (0, 0);
                    let mut word_start = 0;
                    // SAFETY: the target has AVX-512BW, which the cfg
                    // checks, and so BMI2, for every intrinsic; each mask
                    // selects the lanes of `start..slots.len()`, within
                    // `slots`, and a masked load reads only the lanes it
                    // selects.
                    unsafe {
                        let target = $set1(target as $lane);
                        while word_start < slots.len() {
                            let (mut below_bits, mut equal_bits) = (0_u64, 0_u64);
                            let mut offset = 0;
                            while offset < WORD_LANES && word_start + offset < slots.len() {
                                let start = word_start + offset;
                                let lanes = (slots.len() - start).min(LANES);
                                let in_slots = ((1_u64 << lanes) - 1) as $mask;
                                let chunk = $load(in_slots, slots.as_ptr().add(start).cast());
                                below_bits |= u64::from($below(in_slots, chunk, target)) << offset;
                                equal_bits |= u64::from($equal(in_slots, chunk, target)) << offset;
                                offset += LANES;
                            }
                            let live = len.saturating_sub(word_start).min(WORD_LANES) as u32;
                            below += _bzhi_u64(below_bits, live).count_ones() as usize;
                            equal += _bzhi_u64(equal_bits, live).count_ones() as usize;
                            word_start += WORD_LANES;
                        }
                    }
                    Some(below..below + equal)
                }
                #[cfg(not(all(target_arch = "x86_64", target_feature = "avx512bw")))]
                {
                    let _ = (slots, len, target);
                    None
                }
            }
        }
    };
}

vector_count!(
    u16,
    i16,
    u32,
    _mm512_set1_epi16,
    _mm512_maskz_loadu_epi16,
    _mm512_mask_cmplt_epu16_mask,
    _mm512_mask_cmpeq_epu16_mask
);
vector_count!(
    u32,
    i32,
    u16,
    _mm512_set1_epi32,
    _mm512_maskz_loadu_epi32,
    _mm512_mask_cmplt_epu32_mask,
    _mm512_mask_cmpeq_epu32_mask
);

/// 8-byte slots, those of std byte strings, keep the compiler's count: the
/// same count in explicit vectors measured slower on 1M and 10M string maps
/// (05-fence-key.md).
impl VectorCount for u64 {
    #[inline(always)]
    fn equal_run_in_vectors(_slots: &[Self], _len: usize, _target: Self) -> Option<Range<usize>> {
        None
    }
}
