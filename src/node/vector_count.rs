//! Search over a node's inline keys (D18) in explicit vectors, for slots of
//! 2 and 4 bytes. AVX-512 broadcasts the search's inline key to every lane,
//! one load and two compares per 64 bytes of slots, the compares' bits
//! gathered 64 slots to a word, the live ones kept with one `bzhi` by `len`,
//! and counted with one `popcnt`.
//!
//! The compiler's own vectors for the same count depend on the capacity:
//! for capacities that aren't a multiple of the vector, such as the 23
//! slots of a `u64 → [u64; 4]` leaf, it compared in vectors but then took
//! the mask apart a bit at a time, more than doubling the instructions of
//! the whole lookup (05-fence-key.md).
//!
//! On x86-64 without hardware popcount, SSE2 compares eight 2-byte or four
//! 4-byte slots at once. Its masks have one bit per byte, so each lane
//! repeats its result. Sorted slots make the masks for `< target` and
//! `<= target` runs of low one-bits; their first zeros locate the equal
//! range without a software popcount. For 33–64 two-byte or 17–32 four-byte
//! slots, packing comparison lanes to half their width keeps one mask.
//! Multiple words would add extraction, clipping, and accumulation work;
//! larger capacities and targets with POPCNT keep the compiler's vector
//! count unless AVX-512 applies above.

use core::num::NonZeroU64;
use core::ops::Range;

pub(super) enum SlotMatches {
    None,
    One(usize),
    Two { lower: usize, upper: usize },
    Many,
}

/// Equality-only slot search whose byte masks fit in one machine word.
/// Unlike ordered inline windows, these slots need not be sorted.
pub(super) struct U16EqualityMask;

impl U16EqualityMask {
    #[inline]
    pub(super) fn matching(slots: &[u16], run: Range<usize>, target: u16) -> Option<SlotMatches> {
        #[cfg(target_arch = "x86_64")]
        {
            use core::arch::x86_64::{
                _mm_cmpeq_epi16, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi16,
            };
            const LANES: usize = 8;
            if !(LANES..=32).contains(&slots.len()) {
                return None;
            }
            let mut bits = 0_u64;
            // SAFETY: SSE2 is part of every x86-64 target. Each load starts
            // at most `len - LANES`, so all eight initialized slots fit.
            // The last partial chunk overlaps the preceding one; equal
            // positions produce the same bits and can be combined with OR.
            unsafe {
                let target = _mm_set1_epi16(target as i16);
                for offset in (0..slots.len()).step_by(LANES) {
                    let start = offset.min(slots.len() - LANES);
                    let chunk = _mm_loadu_si128(slots.as_ptr().add(start).cast());
                    let equal = _mm_movemask_epi8(_mm_cmpeq_epi16(chunk, target)) as u64;
                    bits |= equal << (2 * start);
                }
            }
            // Retain one of each slot's two byte bits, then only the
            // primary live run. Clamping also bounds every shift below.
            let start = 2 * run.start.min(slots.len());
            let end = 2 * run.end.min(slots.len());
            bits &= 0x5555_5555_5555_5555
                & u64::MAX.checked_shl(start as u32).unwrap_or(0)
                & u64::MAX.checked_shr((64 - end) as u32).unwrap_or(0);
            if bits == 0 {
                return Some(SlotMatches::None);
            }
            let remaining = bits & (bits - 1);
            Some(if remaining == 0 {
                SlotMatches::One(bits.trailing_zeros() as usize / 2)
            } else if remaining.is_power_of_two() {
                SlotMatches::Two {
                    lower: bits.trailing_zeros() as usize / 2,
                    upper: remaining.trailing_zeros() as usize / 2,
                }
            } else {
                SlotMatches::Many
            })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = (slots, run, target);
            None
        }
    }
}

/// Point lookups over 8-byte slots need only an equality mask. Ordered
/// searches keep their compiler-vectorized count; unlike narrow slots,
/// that count still constructs both comparison masks before rejecting an
/// empty run. All supported capacities fit their equality bits in one word.
pub(super) struct U64EqualityMask {
    bits: u64,
}

impl U64EqualityMask {
    #[inline(always)]
    fn matching(slots: &[u64], target: u64) -> Option<Self> {
        #[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
        {
            use core::arch::x86_64::{
                _mm512_mask_cmpeq_epi64_mask, _mm512_maskz_loadu_epi64, _mm512_set1_epi64,
            };
            if slots.len() > 64 {
                return None;
            }
            const LANES: usize = 8;
            let mut bits = 0_u64;
            // SAFETY: the cfg checks AVX-512F. Each mask selects only the
            // initialized slots remaining in this slice. There are at most
            // eight lanes per load, and capacity <= 64 bounds every shift.
            unsafe {
                let target = _mm512_set1_epi64(target as i64);
                for start in (0..slots.len()).step_by(LANES) {
                    let lanes = (slots.len() - start).min(LANES);
                    let in_slots = ((1_u64 << lanes) - 1) as u8;
                    let chunk =
                        _mm512_maskz_loadu_epi64(in_slots, slots.as_ptr().add(start).cast());
                    bits |=
                        u64::from(_mm512_mask_cmpeq_epi64_mask(in_slots, chunk, target)) << start;
                }
            }
            Some(Self { bits })
        }
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx512f")))]
        {
            let _ = (slots, target);
            None
        }
    }

    /// The live matching hull, or `None` before extracting any bounds
    /// when no live bit matches. Sorted slots give an exact run; arbitrary
    /// slots still give bounds within `0..len`.
    #[inline(always)]
    pub(super) fn run(self, len: usize) -> Option<Range<usize>> {
        let live_bits = len.min(64);
        #[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
        // SAFETY: the cfg checks BMI2 independently of vector features.
        let bits = unsafe { core::arch::x86_64::_bzhi_u64(self.bits, live_bits as u32) };
        #[cfg(not(all(target_arch = "x86_64", target_feature = "bmi2")))]
        let bits = self.bits & u64::MAX.checked_shr((64 - live_bits) as u32).unwrap_or(0);
        let bits = NonZeroU64::new(bits)?;
        // The mask already implies these bounds. Keep them explicit so
        // safe key indexing need not rediscover the bit-position proof.
        let end = (64 - bits.leading_zeros() as usize).min(len);
        let start = (bits.trailing_zeros() as usize).min(end);
        Some(start..end)
    }
}

/// Inline-key slots that the target counts in explicit vectors.
pub(super) trait VectorCount: Copy {
    /// An equality mask for point lookup where it avoids ordered work.
    /// Narrow slots and unsupported targets retain the ordered reduction.
    #[inline(always)]
    fn point_mask_in_vectors(_slots: &[Self], _target: Self) -> Option<U64EqualityMask> {
        None
    }

    /// The positions of the first `len` of `slots` equal to `target`: those
    /// below it, then those equal to it. Exactly the equal ones for sorted
    /// slots, and a range within `0..len` for any. `None` where the target
    /// has no such vectors, or the slots count faster otherwise.
    ///
    /// The loads and compares run over the capacity, `slots.len()`, a
    /// constant per node kind, so their masks are constants and they don't
    /// wait for `len`, which clips the results. Slots past `len` are read,
    /// but their contents do not affect the answer.
    fn equal_run_in_vectors(slots: &[Self], len: usize, target: Self) -> Option<Range<usize>>;
}

/// Implements [`VectorCount`] for slots of one width, with the AVX-512 and
/// SSE2 intrinsics for its lanes.
macro_rules! vector_count {
    ($slot:ty, $lane:ty, $mask:ty, $set1:ident, $load:ident, $below:ident, $equal:ident,
     $sse_set1:ident, $sse_greater:ident, $sse_equal:ident, $sse_pack:ident) => {
        impl VectorCount for $slot {
            #[inline(always)]
            fn equal_run_in_vectors(
                slots: &[Self],
                len: usize,
                target: Self,
            ) -> Option<Range<usize>> {
                #[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
                {
                    #[cfg(target_feature = "bmi2")]
                    use core::arch::x86_64::_bzhi_u64;
                    use core::arch::x86_64::{$below, $equal, $load, $set1};

                    const LANES: usize = <$mask>::BITS as usize;
                    const WORD_LANES: usize = u64::BITS as usize;
                    let (mut below, mut equal) = (0, 0);
                    let mut word_start = 0;
                    // SAFETY: the target has AVX-512BW, which the cfg
                    // checks; BMI2 is checked separately below. Each mask
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
                            let keep_live = |bits: u64| {
                                #[cfg(target_feature = "bmi2")]
                                {
                                    _bzhi_u64(bits, live)
                                }
                                #[cfg(not(target_feature = "bmi2"))]
                                {
                                    bits & u64::MAX.checked_shr(64 - live).unwrap_or(0)
                                }
                            };
                            below += keep_live(below_bits).count_ones() as usize;
                            equal += keep_live(equal_bits).count_ones() as usize;
                            word_start += WORD_LANES;
                        }
                    }
                    Some(below..below + equal)
                }
                #[cfg(all(
                    target_arch = "x86_64",
                    not(target_feature = "avx512bw"),
                    not(target_feature = "popcnt")
                ))]
                {
                    use core::arch::x86_64::{
                        _mm_loadu_si128, _mm_movemask_epi8, _mm_xor_si128, $sse_equal,
                        $sse_greater, $sse_pack, $sse_set1,
                    };

                    const SLOT_BYTES: usize = size_of::<$slot>();
                    const LANES: usize = 16 / SLOT_BYTES;
                    const WORD_LANES: usize = 64 / SLOT_BYTES;
                    // One packing stage doubles the slots that fit in a
                    // single mask. Leave shorter arrays unchanged; larger
                    // ones would need another packing stage or mask word.
                    let packed = slots.len() > WORD_LANES;
                    let max_slots = if packed { 2 * WORD_LANES } else { WORD_LANES };
                    if slots.len() < LANES || slots.len() > max_slots {
                        return None;
                    }
                    let bits_per_slot = if packed { SLOT_BYTES / 2 } else { SLOT_BYTES };
                    let (mut below_bits, mut through_bits) = (0_u64, 0_u64);
                    // SAFETY: SSE2 is available on every x86-64 target.
                    // Each load starts at most `slots.len() - LANES`, and
                    // reads exactly LANES slots. The packed path takes two
                    // adjacent loads from at most `slots.len() - 2 * LANES`.
                    // Loads may overlap previous ones; duplicate comparison
                    // bits agree. All slots are initialized, including those
                    // past `len`. Comparison lanes are exactly 0 or -1,
                    // unchanged by signed saturation to half their width.
                    // The capacity gate keeps every packed bit in one u64.
                    unsafe {
                        let sign_bit: $slot = 1 << (<$slot>::BITS - 1);
                        let flip_sign = $sse_set1(sign_bit as $lane);
                        let target = $sse_set1((target ^ sign_bit) as $lane);
                        let mut offset = 0;
                        if packed {
                            while offset < slots.len() {
                                let start = offset.min(slots.len() - 2 * LANES);
                                let first = _mm_loadu_si128(slots.as_ptr().add(start).cast());
                                let second =
                                    _mm_loadu_si128(slots.as_ptr().add(start + LANES).cast());
                                let first = _mm_xor_si128(first, flip_sign);
                                let second = _mm_xor_si128(second, flip_sign);
                                let lower = $sse_pack(
                                    $sse_greater(target, first),
                                    $sse_greater(target, second),
                                );
                                let above = $sse_pack(
                                    $sse_greater(first, target),
                                    $sse_greater(second, target),
                                );
                                below_bits |=
                                    (_mm_movemask_epi8(lower) as u64) << (start * bits_per_slot);
                                through_bits |= u64::from(!(_mm_movemask_epi8(above) as u16))
                                    << (start * bits_per_slot);
                                offset += 2 * LANES;
                            }
                        } else {
                            while offset < slots.len() {
                                let start = offset.min(slots.len() - LANES);
                                let chunk = _mm_loadu_si128(slots.as_ptr().add(start).cast());
                                let chunk = _mm_xor_si128(chunk, flip_sign);
                                let lower = _mm_movemask_epi8($sse_greater(target, chunk)) as u64;
                                let equal = _mm_movemask_epi8($sse_equal(target, chunk)) as u64;
                                below_bits |= lower << (start * SLOT_BYTES);
                                through_bits |= (lower | equal) << (start * SLOT_BYTES);
                                offset += LANES;
                            }
                        }
                    }
                    // For sorted live slots, these low-bit runs are the lower
                    // and upper bounds. For any slots, `<` implies `<=`, so
                    // lower <= upper; clipping each to `len` keeps both in bounds.
                    let below = (below_bits.trailing_ones() as usize / bits_per_slot).min(len);
                    let through = (through_bits.trailing_ones() as usize / bits_per_slot).min(len);
                    Some(below..through)
                }
                #[cfg(any(
                    not(target_arch = "x86_64"),
                    all(not(target_feature = "avx512bw"), target_feature = "popcnt")
                ))]
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
    _mm512_mask_cmpeq_epu16_mask,
    _mm_set1_epi16,
    _mm_cmpgt_epi16,
    _mm_cmpeq_epi16,
    _mm_packs_epi16
);
vector_count!(
    u32,
    i32,
    u16,
    _mm512_set1_epi32,
    _mm512_maskz_loadu_epi32,
    _mm512_mask_cmplt_epu32_mask,
    _mm512_mask_cmpeq_epu32_mask,
    _mm_set1_epi32,
    _mm_cmpgt_epi32,
    _mm_cmpeq_epi32,
    _mm_packs_epi32
);

/// 8-byte slots, those of std byte strings, keep the compiler's count: the
/// same count in explicit vectors measured slower on 1M and 10M string maps
/// (05-fence-key.md).
impl VectorCount for u64 {
    #[inline(always)]
    fn point_mask_in_vectors(slots: &[Self], target: Self) -> Option<U64EqualityMask> {
        U64EqualityMask::matching(slots, target)
    }

    #[inline(always)]
    fn equal_run_in_vectors(_slots: &[Self], _len: usize, _target: Self) -> Option<Range<usize>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use crate::node::inline_keys::InlineSlot;

    /// Every short capacity and live length, including vector and mask
    /// boundaries. The tail is unsorted and alternates the extreme values;
    /// live values cross the signed-compare boundary and contain ties.
    fn assert_vector_boundaries<W: InlineSlot>() {
        let values = [
            0,
            0x0001_0000_0000_0000,
            0x7fff_0000_0000_0000,
            0x8000_0000_0000_0000,
            0xfffe_0000_0000_0000,
            u64::MAX,
        ]
        .map(W::of_window);
        for capacity in 0..=80 {
            for len in 0..=capacity {
                let mut slots: Vec<W> = (0..capacity)
                    .map(|index| {
                        if index < len {
                            values[index * values.len() / len]
                        } else if index % 2 == 0 {
                            values[0]
                        } else {
                            values[values.len() - 1]
                        }
                    })
                    .collect();
                for target in values {
                    let start = slots[..len].partition_point(|&slot| slot < target);
                    let end = slots[..len].partition_point(|&slot| slot <= target);
                    if let Some(run) = W::equal_run_in_vectors(&slots, len, target) {
                        assert_eq!(run, start..end, "capacity {capacity}, live {len}");
                    }
                }
                slots[..len].reverse();
                for target in values {
                    if let Some(run) = W::equal_run_in_vectors(&slots, len, target) {
                        assert!(run.start <= run.end && run.end <= len);
                    }
                }
                // An inconsistent ordering can leave noncontiguous matches.
                // The search need not find them, but its range stays in bounds.
                for (index, slot) in slots[..len].iter_mut().enumerate() {
                    *slot = values[(index * 5 + 2) % values.len()];
                }
                for target in values {
                    if let Some(run) = W::equal_run_in_vectors(&slots, len, target) {
                        assert!(run.start <= run.end && run.end <= len);
                    }
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn vector_boundaries_ignore_stale_tails() {
        assert_vector_boundaries::<u16>();
        assert_vector_boundaries::<u32>();
    }

    #[cfg(all(
        target_arch = "x86_64",
        not(target_feature = "avx512bw"),
        not(target_feature = "popcnt")
    ))]
    #[test]
    fn packed_u16_bounds_cover_a_full_mask_word() {
        use super::VectorCount;

        for capacity in [33, 38, 55, 56, 64] {
            for len in 0..=capacity {
                for value in [0, u16::MAX] {
                    let mut slots = std::vec![value; capacity];
                    for (index, slot) in slots[len..].iter_mut().enumerate() {
                        *slot = if index % 2 == 0 { 0 } else { u16::MAX };
                    }
                    for target in [0, 1, 0x8000, u16::MAX] {
                        let start = slots[..len].partition_point(|&slot| slot < target);
                        let end = slots[..len].partition_point(|&slot| slot <= target);
                        assert_eq!(
                            u16::equal_run_in_vectors(&slots, len, target),
                            Some(start..end),
                            "capacity {capacity}, live {len}, value {value}, target {target}"
                        );
                    }
                }
            }
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        not(target_feature = "avx512bw"),
        not(target_feature = "popcnt")
    ))]
    #[test]
    fn packed_u32_bounds_cover_partial_pairs_and_unsigned_values() {
        use super::VectorCount;

        let values = [0, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX - 1, u32::MAX];
        for capacity in 17..=32 {
            for len in 0..=capacity {
                for uniform in [None, Some(0), Some(u32::MAX)] {
                    let mut slots: Vec<u32> = (0..capacity)
                        .map(|index| {
                            if index < len {
                                uniform.unwrap_or_else(|| values[index * values.len() / len])
                            } else if index % 2 == 0 {
                                0
                            } else {
                                u32::MAX
                            }
                        })
                        .collect();
                    for target in values {
                        let start = slots[..len].partition_point(|&slot| slot < target);
                        let end = slots[..len].partition_point(|&slot| slot <= target);
                        assert_eq!(
                            u32::equal_run_in_vectors(&slots, len, target),
                            Some(start..end),
                            "capacity {capacity}, live {len}, uniform {uniform:?}, target {target}"
                        );
                    }
                    // Inconsistent encodings need not give an exact run,
                    // but neither bound may escape the live prefix.
                    for (index, slot) in slots[..len].iter_mut().enumerate() {
                        *slot = values[(index * 5 + 2) % values.len()];
                    }
                    for target in values {
                        let run = u32::equal_run_in_vectors(&slots, len, target).unwrap();
                        assert!(run.start <= run.end && run.end <= len);
                    }
                }
            }
        }
        // Another packing stage or mask word is a separate policy.
        for capacity in [33, 36, 64] {
            let slots = std::vec![0; capacity];
            assert_eq!(u32::equal_run_in_vectors(&slots, capacity, 0), None);
        }
    }

    /// Point masks need not retain an empty run's insertion position, but
    /// must retain every live match and ignore stale matching tail slots.
    fn assert_point_boundaries<W: InlineSlot>() {
        let values = [0, 1_u64 << 48, 1 << 63, u64::MAX].map(W::of_window);
        for capacity in 0..=80 {
            let supported = size_of::<W>() == 8
                && cfg!(all(target_arch = "x86_64", target_feature = "avx512f"))
                && capacity <= 64;
            for len in 0..=capacity {
                let mut slots: Vec<W> = (0..capacity)
                    .map(|index| {
                        if index < len {
                            values[index * values.len() / len]
                        } else {
                            values[index % values.len()]
                        }
                    })
                    .collect();
                for unsorted in [false, true] {
                    if unsorted {
                        // Split equal groups across the live prefix, so a
                        // first/last hull may include nonmatching slots.
                        slots[..len].reverse();
                        slots[..len].rotate_left(len / 2);
                    }
                    for target in values {
                        let result = W::point_mask_in_vectors(&slots, target);
                        assert_eq!(result.is_some(), supported, "capacity {capacity}");
                        if let Some(mask) = result {
                            let run = mask.run(len).unwrap_or(0..0);
                            let live = &slots[..len];
                            let expected = match live.iter().position(|&slot| slot == target) {
                                None => 0..0,
                                Some(start) => {
                                    start
                                        ..live.iter().rposition(|&slot| slot == target).unwrap() + 1
                                }
                            };
                            assert_eq!(run, expected, "capacity {capacity}, live {len}");
                            assert!(run.start <= run.end && run.end <= len);
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn point_masks_bound_live_matches_across_lane_edges() {
        assert_point_boundaries::<u16>();
        assert_point_boundaries::<u32>();
        assert_point_boundaries::<u64>();
    }
}
