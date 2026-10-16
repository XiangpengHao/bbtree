//! Search over a node's inline keys (D18) in explicit vectors, for slots of
//! 2 and 4 bytes, and for 28 to 31 slots of 8 bytes. AVX-512 broadcasts the
//! search's inline key to every lane, one load and two compares per 64
//! bytes of slots, the compares' bits gathered 64 slots to a word, the live
//! ones kept with one `bzhi` by `len`, and counted with one `popcnt`.
//!
//! The compiler's own vectors for the same count depend on the capacity:
//! for capacities that aren't a multiple of the vector, such as the 23
//! slots of a `u64 → [u64; 4]` leaf, or 28 to 31 slots of 8 bytes, it
//! compared in vectors but then took the mask apart a bit at a time, more
//! than doubling the instructions of the whole lookup (05-fence-key.md).
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
//!
//! On AArch64, NEON counts every width ([`NeonCount`]): the compiler's own
//! count there widened each compared lane to 64 bits, one at a time, to add
//! it to a `usize`, about six instructions a slot, 1,360 a 2M `u64` hit on
//! an M1 Pro against 228 a 10M one with AVX-512.
//!
//! Two equality searches live here too: an exact-key lookup over 8-byte
//! slots takes an AVX-512 equality mask ([`U64EqualityMask`], D41), and a
//! leaf's length tags, which are unsorted, are filtered in SSE2 vectors
//! ([`U16EqualitySearch`], D36).

use core::num::NonZeroU64;
use core::ops::Range;

use super::slots::Slots;

/// Which of a lookup's candidates have its length tag: none, one, two (at
/// these positions), or more.
pub(super) enum SlotMatches {
    None,
    One(usize),
    Two { lower: usize, upper: usize },
    Many,
}

/// The length tags (D36) equal to a target among a lookup's candidates, in
/// SSE2 vectors whose byte masks fit in one word: tags are unsorted, so this
/// finds matches, not a range.
pub(super) struct U16EqualitySearch;

impl U16EqualitySearch {
    #[inline(always)]
    pub(super) fn matching(
        slots: &[u16],
        candidates: Range<usize>,
        target: u16,
    ) -> Option<SlotMatches> {
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
            // at most `slots.len() - LANES`, so all eight initialized slots
            // fit.
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
            // live candidates. Clamping also bounds every shift below.
            let start = 2 * candidates.start.min(slots.len());
            let end = 2 * candidates.end.min(slots.len());
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
            let _ = (slots, candidates, target);
            None
        }
    }
}

/// Exact-key lookups over 8-byte slots need only an equality mask. An ordered
/// search builds both comparison masks; this one can reject an empty equal
/// range without counting the slots below the target. All supported
/// capacities fit their equality bits in one word.
pub(in crate::tree) struct U64EqualityMask {
    bits: u64,
}

impl U64EqualityMask {
    #[inline(always)]
    fn matching(slots: Slots<u64>, target: u64) -> Option<Self> {
        #[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
        {
            use core::arch::x86_64::{
                _mm512_mask_cmpeq_epi64_mask, _mm512_maskz_loadu_epi64, _mm512_set1_epi64,
            };
            if slots.capacity() > 64 {
                return None;
            }
            const LANES: usize = 8;
            let mut bits = 0_u64;
            // SAFETY: the cfg checks AVX-512F. Each mask selects only the
            // initialized slots remaining in this slice. There are at most
            // eight lanes per load, and capacity <= 64 bounds every shift.
            unsafe {
                let target = _mm512_set1_epi64(target as i64);
                for start in (0..slots.capacity()).step_by(LANES) {
                    let lanes = (slots.capacity() - start).min(LANES);
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
    /// when no live bit matches. Sorted slots give an exact equal range;
    /// arbitrary slots still give bounds within `0..len`.
    #[inline(always)]
    pub(super) fn equal_range(self, len: usize) -> Option<Range<usize>> {
        let bits = Self::keep_low_bits(self.bits, len.min(64) as u32);
        let bits = NonZeroU64::new(bits)?;
        // The mask already implies these bounds. Keep them explicit so
        // safe key indexing need not rediscover the bit-position proof.
        let end = (64 - bits.leading_zeros() as usize).min(len);
        let start = (bits.trailing_zeros() as usize).min(end);
        Some(start..end)
    }

    /// A batch needs only a unique live position, not both ends of a range.
    /// Ignore stale matching slots before testing whether one bit remains.
    #[inline(always)]
    pub(super) fn unique(self, len: usize) -> SlotMatches {
        let bits = Self::keep_low_bits(self.bits, len.min(64) as u32);
        if bits == 0 {
            SlotMatches::None
        } else if bits.is_power_of_two() {
            SlotMatches::One(bits.trailing_zeros() as usize)
        } else {
            SlotMatches::Many
        }
    }

    /// `bits` with all but its `count` lowest cleared, `count <= 64`: one
    /// `bzhi` where BMI2 is there.
    #[inline(always)]
    fn keep_low_bits(bits: u64, count: u32) -> u64 {
        debug_assert!(count <= u64::BITS);
        #[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
        // SAFETY: the cfg checks BMI2.
        return unsafe { core::arch::x86_64::_bzhi_u64(bits, count) };
        #[cfg(not(all(target_arch = "x86_64", target_feature = "bmi2")))]
        return bits & u64::MAX.checked_shr(u64::BITS - count).unwrap_or(0);
    }
}

/// Inline-key slots that the build target counts in explicit vectors.
pub(in crate::tree) trait VectorCount: Copy {
    /// An equality mask for exact-key lookup where it avoids ordered work.
    /// Narrow slots and unsupported targets retain the ordered reduction.
    #[inline(always)]
    fn equality_mask_in_vectors(_slots: Slots<Self>, _target: Self) -> Option<U64EqualityMask> {
        None
    }

    /// The positions of the first `len` of `slots` equal to `target`: those
    /// below it, then those equal to it. Exactly the equal ones for sorted
    /// slots, and a range within `0..len` for any. `None` where the target
    /// has no such vectors, or the slots count faster otherwise.
    ///
    /// The loads and compares run over the capacity, `slots.len()`, a
    /// constant per node kind, so their masks are constants and they don't
    /// wait for `len`, which clips the results. Slots past `len` are read:
    /// x86's counts mask them out, so their contents don't affect the
    /// answer; NEON's count them, so it is exact only where they hold the
    /// greatest inline key (invariant 4), as the compiler's count is.
    fn equal_range_in_vectors(slots: Slots<Self>, len: usize, target: Self)
    -> Option<Range<usize>>;
}

/// The AVX-512 count of `$slots`' first `$len` slots below and equal to
/// `$target`, over the capacity, `$slots.len()`, with the intrinsics for
/// one slot width: `$lane`s under a `$mask` per vector. Its caller checks
/// the target features those intrinsics need.
#[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
macro_rules! count_avx512_lanes {
    ($slots:ident, $len:ident, $target:ident, $lane:ty, $mask:ty,
     $set1:ident, $load:ident, $below:ident, $equal:ident) => {{
        use core::arch::x86_64::{$below, $equal, $load, $set1};

        const LANES: usize = <$mask>::BITS as usize;
        const WORD_LANES: usize = u64::BITS as usize;
        let (mut below, mut equal) = (0, 0);
        let mut word_start = 0;
        // SAFETY: the caller checks the target features the intrinsics
        // need. Each mask selects the
        // lanes of `start..slots.len()`, within the slots, and a masked load
        // reads only the lanes it selects.
        unsafe {
            let target = $set1($target as $lane);
            while word_start < $slots.capacity() {
                let (mut below_bits, mut equal_bits) = (0_u64, 0_u64);
                let mut offset = 0;
                while offset < WORD_LANES && word_start + offset < $slots.capacity() {
                    let start = word_start + offset;
                    let lanes = ($slots.capacity() - start).min(LANES);
                    let in_slots = ((1_u64 << lanes) - 1) as $mask;
                    let chunk = $load(in_slots, $slots.as_ptr().add(start).cast());
                    below_bits |= u64::from($below(in_slots, chunk, target)) << offset;
                    equal_bits |= u64::from($equal(in_slots, chunk, target)) << offset;
                    offset += LANES;
                }
                let live = $len.saturating_sub(word_start).min(WORD_LANES) as u32;
                below += U64EqualityMask::keep_low_bits(below_bits, live).count_ones() as usize;
                equal += U64EqualityMask::keep_low_bits(equal_bits, live).count_ones() as usize;
                word_start += WORD_LANES;
            }
        }
        Some(below..below + equal)
    }};
}

/// The NEON count of a node's slots below and equal to a target, over the
/// capacity, for every slot width: one compare of each kind per 16 bytes of
/// slots, whose all-ones lanes are subtracted into counters of the slots'
/// own width, added up across lanes once, at the end. Slots past `len` count
/// too, so the counts are exact where they hold the greatest inline key
/// (invariant 4), which no target is below; masking them out per vector, as
/// x86 does, cost 8–18% of a 10M-entry lookup on an M1 Pro.
#[cfg(target_arch = "aarch64")]
struct NeonCount;

#[cfg(target_arch = "aarch64")]
impl NeonCount {
    /// [`VectorCount::equal_range_in_vectors`]; `None` for fewer slots than
    /// a vector holds.
    #[inline(always)]
    fn equal_range<W: NeonLanes>(slots: Slots<W>, len: usize, target: W) -> Option<Range<usize>> {
        if slots.capacity() < W::LANES {
            return None;
        }
        // The capacity is a constant per node kind, so this split folds.
        let tail = slots.capacity() % W::LANES;
        let whole = slots.capacity() - tail;
        // SAFETY: every load reads `W::LANES` slots from at most
        // `capacity - W::LANES`: the whole vectors, then, for a tail, the
        // last `W::LANES` slots, which overlap the vector before. Every slot
        // up to the capacity is initialized (type invariant of the callers'
        // slots).
        let (below, equal) = unsafe {
            let target = W::splat(target);
            let (mut below, mut equal) = (W::index(0), W::index(0));
            for start in (0..whole).step_by(W::LANES) {
                let chunk = W::load(slots.as_ptr().add(start));
                below = W::subtract(below, W::below(chunk, target));
                equal = W::subtract(equal, W::equal(chunk, target));
            }
            if tail != 0 {
                let first = slots.capacity() - W::LANES;
                let chunk = W::load(slots.as_ptr().add(first));
                // The lanes the whole vectors didn't count: `whole..`.
                let fresh = W::below(W::index(whole - 1), W::positions(first));
                below = W::subtract(below, W::and(W::below(chunk, target), fresh));
                equal = W::subtract(equal, W::and(W::equal(chunk, target), fresh));
            }
            (W::sum(below), W::sum(equal))
        };
        let below = below.min(len);
        Some(below..(below + equal).min(len))
    }
}

/// A slot width's NEON lanes, for [`NeonCount`]: 16-byte vectors of
/// `LANES` slots. A node's capacity is below 2^16 slots, so a slot's width
/// holds any position, a lane's count, and the sum of the lanes' counts.
#[cfg(target_arch = "aarch64")]
trait NeonLanes: Copy {
    const LANES: usize;
    type Vector: Copy;

    /// `self` in every lane.
    fn splat(self) -> Self::Vector;

    /// `index` in every lane.
    fn index(index: usize) -> Self::Vector;

    /// The positions of the `LANES` slots from `first`: `first`, `first +
    /// 1`, and so on.
    fn positions(first: usize) -> Self::Vector;

    /// The `LANES` slots from `first`.
    ///
    /// # Safety
    ///
    /// They are initialized and allocated.
    unsafe fn load(first: *const Self) -> Self::Vector;

    /// All ones in the lanes where `slots` is below `target`.
    fn below(slots: Self::Vector, target: Self::Vector) -> Self::Vector;

    /// All ones in the lanes where `slots` equals `target`.
    fn equal(slots: Self::Vector, target: Self::Vector) -> Self::Vector;

    fn and(left: Self::Vector, right: Self::Vector) -> Self::Vector;

    fn subtract(left: Self::Vector, right: Self::Vector) -> Self::Vector;

    /// The lanes added up.
    fn sum(lanes: Self::Vector) -> usize;
}

/// Implements [`NeonLanes`] for one slot width with its intrinsics.
#[cfg(target_arch = "aarch64")]
macro_rules! neon_lanes {
    ($slot:ty, $vector:ident, $splat:ident, $load:ident, $greater:ident, $equal:ident,
     $and:ident, $add:ident, $subtract:ident, $sum:ident) => {
        impl NeonLanes for $slot {
            const LANES: usize = 16 / size_of::<$slot>();
            type Vector = core::arch::aarch64::$vector;

            #[inline(always)]
            fn splat(self) -> Self::Vector {
                // SAFETY: NEON is part of every AArch64 target.
                unsafe { core::arch::aarch64::$splat(self) }
            }

            #[inline(always)]
            fn index(index: usize) -> Self::Vector {
                debug_assert!(index <= usize::from(u16::MAX));
                (index as $slot).splat()
            }

            #[inline(always)]
            fn positions(first: usize) -> Self::Vector {
                const LANE_INDEXES: [$slot; 16 / size_of::<$slot>()] = {
                    let mut indexes = [0; 16 / size_of::<$slot>()];
                    let mut lane = 0;
                    while lane < indexes.len() {
                        indexes[lane] = lane as $slot;
                        lane += 1;
                    }
                    indexes
                };
                // SAFETY: NEON is part of every AArch64 target, and the load
                // reads the `LANES` indexes.
                unsafe {
                    let indexes = core::arch::aarch64::$load(LANE_INDEXES.as_ptr());
                    core::arch::aarch64::$add(indexes, Self::index(first))
                }
            }

            #[inline(always)]
            unsafe fn load(first: *const Self) -> Self::Vector {
                // SAFETY: forwarded.
                unsafe { core::arch::aarch64::$load(first) }
            }

            #[inline(always)]
            fn below(slots: Self::Vector, target: Self::Vector) -> Self::Vector {
                // SAFETY: NEON is part of every AArch64 target.
                unsafe { core::arch::aarch64::$greater(target, slots) }
            }

            #[inline(always)]
            fn equal(slots: Self::Vector, target: Self::Vector) -> Self::Vector {
                // SAFETY: NEON is part of every AArch64 target.
                unsafe { core::arch::aarch64::$equal(slots, target) }
            }

            #[inline(always)]
            fn and(left: Self::Vector, right: Self::Vector) -> Self::Vector {
                // SAFETY: NEON is part of every AArch64 target.
                unsafe { core::arch::aarch64::$and(left, right) }
            }

            #[inline(always)]
            fn subtract(left: Self::Vector, right: Self::Vector) -> Self::Vector {
                // SAFETY: NEON is part of every AArch64 target.
                unsafe { core::arch::aarch64::$subtract(left, right) }
            }

            #[inline(always)]
            fn sum(lanes: Self::Vector) -> usize {
                // SAFETY: NEON is part of every AArch64 target.
                (unsafe { core::arch::aarch64::$sum(lanes) }) as usize
            }
        }
    };
}

#[cfg(target_arch = "aarch64")]
neon_lanes!(
    u16,
    uint16x8_t,
    vdupq_n_u16,
    vld1q_u16,
    vcgtq_u16,
    vceqq_u16,
    vandq_u16,
    vaddq_u16,
    vsubq_u16,
    vaddvq_u16
);
#[cfg(target_arch = "aarch64")]
neon_lanes!(
    u32,
    uint32x4_t,
    vdupq_n_u32,
    vld1q_u32,
    vcgtq_u32,
    vceqq_u32,
    vandq_u32,
    vaddq_u32,
    vsubq_u32,
    vaddvq_u32
);
#[cfg(target_arch = "aarch64")]
neon_lanes!(
    u64,
    uint64x2_t,
    vdupq_n_u64,
    vld1q_u64,
    vcgtq_u64,
    vceqq_u64,
    vandq_u64,
    vaddq_u64,
    vsubq_u64,
    vaddvq_u64
);

/// Implements [`VectorCount`] for slots of one width, with the AVX-512 and
/// SSE2 intrinsics for its lanes.
macro_rules! vector_count {
    ($slot:ty, $lane:ty, $mask:ty, $set1:ident, $load:ident, $below:ident, $equal:ident,
     $sse_set1:ident, $sse_greater:ident, $sse_equal:ident, $sse_pack:ident) => {
        impl VectorCount for $slot {
            #[inline(always)]
            fn equal_range_in_vectors(
                slots: Slots<Self>,
                len: usize,
                target: Self,
            ) -> Option<Range<usize>> {
                // The AVX-512BW the 2- and 4-byte lanes' intrinsics need.
                #[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
                {
                    count_avx512_lanes!(
                        slots, len, target, $lane, $mask, $set1, $load, $below, $equal
                    )
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
                    // The slots whose byte masks fit in one word, unpacked.
                    const UNPACKED_MAX_SLOTS: usize = 64 / SLOT_BYTES;
                    // One packing stage doubles the slots that fit in a
                    // single mask. Leave shorter arrays unchanged; larger
                    // ones would need another packing stage or mask word.
                    let packed = slots.capacity() > UNPACKED_MAX_SLOTS;
                    let max_slots = if packed {
                        2 * UNPACKED_MAX_SLOTS
                    } else {
                        UNPACKED_MAX_SLOTS
                    };
                    if slots.capacity() < LANES || slots.capacity() > max_slots {
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
                            while offset < slots.capacity() {
                                let start = offset.min(slots.capacity() - 2 * LANES);
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
                            while offset < slots.capacity() {
                                let start = offset.min(slots.capacity() - LANES);
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
                    // lower <= upper; clipping each to `len` keeps both in
                    // bounds.
                    let below = (below_bits.trailing_ones() as usize / bits_per_slot).min(len);
                    let through = (through_bits.trailing_ones() as usize / bits_per_slot).min(len);
                    Some(below..through)
                }
                #[cfg(target_arch = "aarch64")]
                {
                    NeonCount::equal_range(slots, len, target)
                }
                #[cfg(any(
                    not(any(target_arch = "x86_64", target_arch = "aarch64")),
                    all(
                        target_arch = "x86_64",
                        not(target_feature = "avx512bw"),
                        target_feature = "popcnt"
                    )
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

/// 8-byte slots, those of std byte strings' leaves and of 8-byte `InlineKey`
/// impls, keep the compiler's count with AVX-512, which measured faster than
/// explicit vectors on string maps (05-fence-key.md), but for 28 to 31 slots,
/// the capacity of an internal node with 8-byte inline keys, and of a
/// `String` set's leaf: there the compiler takes the compares' masks apart a
/// bit at a time, and the count goes the narrow slots' way. NEON counts every
/// capacity.
impl VectorCount for u64 {
    #[inline(always)]
    fn equality_mask_in_vectors(slots: Slots<Self>, target: Self) -> Option<U64EqualityMask> {
        U64EqualityMask::matching(slots, target)
    }

    #[inline(always)]
    fn equal_range_in_vectors(
        slots: Slots<Self>,
        len: usize,
        target: Self,
    ) -> Option<Range<usize>> {
        // The capacity is a constant per node kind, so the check folds.
        #[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
        {
            if !(28..=31).contains(&slots.capacity()) {
                return None;
            }
            count_avx512_lanes!(
                slots,
                len,
                target,
                i64,
                u8,
                _mm512_set1_epi64,
                _mm512_maskz_loadu_epi64,
                _mm512_mask_cmplt_epu64_mask,
                _mm512_mask_cmpeq_epu64_mask
            )
        }
        #[cfg(target_arch = "aarch64")]
        {
            NeonCount::equal_range(slots, len, target)
        }
        #[cfg(not(any(
            all(target_arch = "x86_64", target_feature = "avx512f"),
            target_arch = "aarch64"
        )))]
        {
            let _ = (slots, len, target);
            None
        }
    }
}

/// Whether the build target's explicit counts are exact whatever the slots
/// past `len` hold, as x86's are; NEON's are only where those hold the
/// greatest inline key (invariant 4), and in bounds for any.
#[cfg(test)]
pub(super) const EXACT_PAST_LEN: bool = !cfg!(target_arch = "aarch64");

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::{EXACT_PAST_LEN, SlotMatches, Slots, U64EqualityMask};
    use crate::inline_key::InlineWord;
    use crate::tree::node::inline_keys::InlineSlot;

    #[test]
    fn unique_matches_ignore_bits_past_len() {
        for bits in [0, 1, 1 << 63, 3, 3 << 31, u64::MAX] {
            for len in 0..=80 {
                let matches: Vec<_> = (0..len.min(64))
                    .filter(|index| bits & (1 << index) != 0)
                    .collect();
                match (U64EqualityMask { bits }).unique(len) {
                    SlotMatches::None => assert!(matches.is_empty()),
                    SlotMatches::One(index) => assert_eq!(matches, [index]),
                    SlotMatches::Many => assert!(matches.len() > 1),
                    SlotMatches::Two { .. } => unreachable!("only uniqueness is tested"),
                }
            }
        }
    }

    /// Every short capacity and live length, including vector and mask
    /// boundaries, with the tail padded with the greatest inline key
    /// (invariant 4), and with a stale tail, unsorted, that alternates the
    /// extreme values; live values cross the signed-compare boundary and
    /// contain ties.
    fn assert_vector_boundaries<W: InlineSlot>() {
        let values = [
            0,
            0x0001_0000_0000_0000,
            0x7fff_0000_0000_0000,
            0x8000_0000_0000_0000,
            0xfffe_0000_0000_0000,
            u64::MAX,
        ]
        .map(W::of_inline_key);
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
                let greatest = W::of_inline_key(InlineWord::MAX);
                let padded: Vec<W> = (slots[..len].iter().copied())
                    .chain(core::iter::repeat_n(greatest, capacity - len))
                    .collect();
                for target in values {
                    let start = slots[..len].partition_point(|&slot| slot < target);
                    let end = slots[..len].partition_point(|&slot| slot <= target);
                    if let Some(equal_range) =
                        W::equal_range_in_vectors(Slots::of_slice(&padded), len, target)
                    {
                        assert_eq!(equal_range, start..end, "capacity {capacity}, live {len}");
                    }
                    if let Some(equal_range) =
                        W::equal_range_in_vectors(Slots::of_slice(&slots), len, target)
                    {
                        if EXACT_PAST_LEN {
                            assert_eq!(equal_range, start..end, "capacity {capacity}, live {len}");
                        } else {
                            assert!(equal_range.start <= equal_range.end && equal_range.end <= len);
                        }
                    }
                }
                slots[..len].reverse();
                for target in values {
                    if let Some(equal_range) =
                        W::equal_range_in_vectors(Slots::of_slice(&slots), len, target)
                    {
                        assert!(equal_range.start <= equal_range.end && equal_range.end <= len);
                    }
                }
                // An inconsistent ordering can leave noncontiguous matches.
                // The search need not find them, but its range stays in bounds.
                for (index, slot) in slots[..len].iter_mut().enumerate() {
                    *slot = values[(index * 5 + 2) % values.len()];
                }
                for target in values {
                    if let Some(equal_range) =
                        W::equal_range_in_vectors(Slots::of_slice(&slots), len, target)
                    {
                        assert!(equal_range.start <= equal_range.end && equal_range.end <= len);
                    }
                }
            }
        }
    }

    #[test]
    fn vector_counts_at_every_boundary() {
        assert_vector_boundaries::<u16>();
        assert_vector_boundaries::<u32>();
        assert_vector_boundaries::<u64>();
    }

    /// 8-byte slots count in explicit vectors only at the capacities whose
    /// count the compiler takes apart bit by bit; the others keep its code.
    #[test]
    #[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
    fn explicit_u64_counts_cover_only_28_to_31_slots() {
        use super::VectorCount;

        for capacity in 0..=64 {
            let slots = std::vec![u64::MAX; capacity];
            let equal_range =
                u64::equal_range_in_vectors(Slots::of_slice(&slots), capacity, u64::MAX);
            assert_eq!(equal_range.is_some(), (28..=31).contains(&capacity));
            if let Some(equal_range) = equal_range {
                assert_eq!(equal_range, 0..capacity);
            }
        }
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
                            u16::equal_range_in_vectors(Slots::of_slice(&slots), len, target),
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
                            u32::equal_range_in_vectors(Slots::of_slice(&slots), len, target),
                            Some(start..end),
                            "capacity {capacity}, live {len}, uniform {uniform:?}, target {target}"
                        );
                    }
                    // Inconsistent encodings need not give an exact
                    // equal_range, but neither bound may escape the live
                    // prefix.
                    for (index, slot) in slots[..len].iter_mut().enumerate() {
                        *slot = values[(index * 5 + 2) % values.len()];
                    }
                    for target in values {
                        let equal_range =
                            u32::equal_range_in_vectors(Slots::of_slice(&slots), len, target)
                                .unwrap();
                        assert!(equal_range.start <= equal_range.end && equal_range.end <= len);
                    }
                }
            }
        }
        // Another packing stage or mask word is a separate policy.
        for capacity in [33, 36, 64] {
            let slots = std::vec![0; capacity];
            assert_eq!(
                u32::equal_range_in_vectors(Slots::of_slice(&slots), capacity, 0),
                None
            );
        }
    }

    /// Equality masks need not retain an empty equal_range's insertion
    /// position, but must retain every live match and ignore stale matching
    /// tail slots.
    fn assert_equality_mask_boundaries<W: InlineSlot>() {
        let values = [0, 1_u64 << 48, 1 << 63, u64::MAX].map(W::of_inline_key);
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
                        let result = W::equality_mask_in_vectors(Slots::of_slice(&slots), target);
                        assert_eq!(result.is_some(), supported, "capacity {capacity}");
                        if let Some(mask) = result {
                            let equal_range = mask.equal_range(len).unwrap_or(0..0);
                            let live = &slots[..len];
                            let expected = match live.iter().position(|&slot| slot == target) {
                                None => 0..0,
                                Some(start) => {
                                    start
                                        ..live.iter().rposition(|&slot| slot == target).unwrap() + 1
                                }
                            };
                            assert_eq!(equal_range, expected, "capacity {capacity}, live {len}");
                            assert!(equal_range.start <= equal_range.end && equal_range.end <= len);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn equality_masks_bound_live_matches_across_lane_edges() {
        assert_equality_mask_boundaries::<u16>();
        assert_equality_mask_boundaries::<u32>();
        assert_equality_mask_boundaries::<u64>();
    }
}
