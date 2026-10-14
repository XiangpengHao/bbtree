//! A node's inline keys, read in place for a search (D18): the column of
//! a key's first bytes past the node's prefix that leaves and internal nodes
//! keep beside their keys, and how a search finds the keys whose inline
//! keys equal its own, by counting over the whole column or by a binary
//! search, as the build target and the width call for.

use core::hint;
use core::ops::Range;

use super::inline_keys::InlineSlot;
use super::slots::InlineKeySlots;
use super::vector_count::U64EqualityMask;
use crate::inline_key::InlineWord;

/// A node's inline keys, read in place (D18): every slot up to the
/// capacity, at this node kind's width, of which slots `0..len` hold the
/// inline keys of keys `0..len`, and the rest the greatest inline key
/// (invariant 4), which a count reads and the cuts at `len` leave out. Its
/// methods narrow an inline key to the bytes the node keeps, and give one
/// back as those bytes, zeros after.
///
/// Each typed node kind of a map has one width, so after monomorphization
/// each method is that of one width. They are not generic, so they need
/// `#[inline]` to inline into the searches, which other crates instantiate,
/// and `always`: before the width folds, each looks like a match of three
/// arms with bounds checks, which the compiler kept out of line, a call and
/// a match on every level of every insert.
#[derive(Clone, Copy)]
pub(in crate::tree) enum NodeInlineKeys<'a> {
    Two(InlineKeysOfWidth<'a, u16>),
    Four(InlineKeysOfWidth<'a, u32>),
    Eight(InlineKeysOfWidth<'a, u64>),
}

/// `$body`, with `$of_width` bound to the slots of `$inline_keys`, whichever
/// their width.
macro_rules! with_inline_keys_of_width {
    ($inline_keys:expr, |$of_width:ident| $body:expr) => {
        match $inline_keys {
            NodeInlineKeys::Two($of_width) => $body,
            NodeInlineKeys::Four($of_width) => $body,
            NodeInlineKeys::Eight($of_width) => $body,
        }
    };
}

impl<'a> NodeInlineKeys<'a> {
    /// The first `capacity` of `slots`, of which the first `len` are live.
    ///
    /// # Safety
    ///
    /// `capacity` is the slots' own, all of them are initialized, and none
    /// changes for `'a`.
    #[inline(always)]
    pub(super) unsafe fn new(slots: InlineKeySlots, capacity: usize, len: usize) -> Self {
        // SAFETY: forwarded, at the slots' width.
        unsafe {
            match slots {
                InlineKeySlots::Two(slots) => Self::Two(InlineKeysOfWidth::new(
                    slots.slice_ptr(capacity).as_ref(),
                    len,
                )),
                InlineKeySlots::Four(slots) => Self::Four(InlineKeysOfWidth::new(
                    slots.slice_ptr(capacity).as_ref(),
                    len,
                )),
                InlineKeySlots::Eight(slots) => Self::Eight(InlineKeysOfWidth::new(
                    slots.slice_ptr(capacity).as_ref(),
                    len,
                )),
            }
        }
    }

    /// Inline key `index`: the bytes the node keeps of key `index`'s
    /// window, zeros after them.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    #[inline(always)]
    pub(in crate::tree) fn inline_key(self, index: usize) -> InlineWord {
        with_inline_keys_of_width!(self, |of_width| of_width.live()[index].inline_key())
    }

    /// The positions of the inline keys equal to the bytes the node keeps
    /// of `inline_key`; see [`InlineKeysOfWidth::equal_range`].
    #[inline(always)]
    pub(in crate::tree) fn equal_range(self, inline_key: InlineWord) -> Range<usize> {
        with_inline_keys_of_width!(self, |of_width| of_width
            .equal_range(InlineSlot::of_inline_key(inline_key)))
    }

    /// The slots equal to the bytes the node keeps of `inline_key`, as a bit
    /// mask over the capacity, where the build target has vectors for it
    /// (AVX-512F, 8-byte slots); `None` otherwise.
    #[inline(always)]
    pub(super) fn equality_mask(self, inline_key: InlineWord) -> Option<U64EqualityMask> {
        with_inline_keys_of_width!(self, |of_width| of_width
            .equality_mask(InlineSlot::of_inline_key(inline_key)))
    }

    /// Whether every slot past `len` holds the greatest inline key
    /// (invariant 4).
    #[cfg(any(test, fuzzing))]
    pub(in crate::tree) fn pads_with_greatest(self) -> bool {
        with_inline_keys_of_width!(self, |of_width| of_width.pads_with_greatest())
    }
}

/// A node's inline keys at one width, as [`NodeInlineKeys`] reads them:
/// `slots` up to the capacity, of which the first `len` are live.
#[derive(Clone, Copy)]
pub(in crate::tree) struct InlineKeysOfWidth<'a, W> {
    slots: &'a [W],
    len: usize,
}

impl<'a, W: InlineSlot> InlineKeysOfWidth<'a, W> {
    fn new(slots: &'a [W], len: usize) -> Self {
        debug_assert!(len <= slots.len());
        Self { slots, len }
    }

    /// The live slots.
    fn live(self) -> &'a [W] {
        &self.slots[..self.len]
    }

    #[cfg(any(test, fuzzing))]
    fn pads_with_greatest(self) -> bool {
        let greatest = W::of_inline_key(InlineWord::MAX);
        self.slots[self.len..].iter().all(|&slot| slot == greatest)
    }

    /// The positions of the inline keys equal to `target`: exactly those
    /// for sorted inline keys, which the `InlineKey` contract gives, and a
    /// range within `0..len` for any. If none equals it, the empty range at
    /// the first inline key greater than it.
    ///
    /// Found by a count of every slot: in explicit vectors where the build
    /// target has them for slots of this width
    /// ([`VectorCount`](super::vector_count::VectorCount)), otherwise as the
    /// compiler vectorizes it ([`Self::equal_range_by_count`]) where it
    /// compares slots of this width in vectors
    /// ([`InlineSlot::COUNT_AUTOVECTORIZES`]); and by a binary search
    /// otherwise ([`Self::equal_range_by_search`]): without such compares,
    /// the count is an emulated sequence that costs more than the mispredicts
    /// it avoids. Measured on 1M-entry maps of 8-byte inline keys (04, Q6).
    /// Wide four-byte arrays on baseline x86-64 also use binary search:
    /// beyond the one-word packed reduction, full-capacity counts cost more
    /// than dependent probes in warm routing nodes.
    #[inline(always)]
    fn equal_range(self, target: W) -> Range<usize> {
        if cfg!(all(
            target_arch = "x86_64",
            not(target_feature = "avx512bw"),
            not(target_feature = "popcnt")
        )) && size_of::<W>() == 4
            && self.slots.len() > 32
        {
            return self.equal_range_by_search(target);
        }
        if let Some(equal_range) = W::equal_range_in_vectors(self.slots, self.len, target) {
            return equal_range;
        }
        if W::COUNT_AUTOVECTORIZES {
            self.equal_range_by_count(target)
        } else {
            self.equal_range_by_search(target)
        }
    }

    /// The slots equal to `target`, as a bit mask, where the build target
    /// has vectors for slots of this width; `None` otherwise.
    #[inline(always)]
    fn equality_mask(self, target: W) -> Option<U64EqualityMask> {
        W::equality_mask_in_vectors(self.slots, target)
    }

    /// One pass over every slot up to the capacity counts the inline keys
    /// below `target` and those equal to it. The slots from `len` on hold
    /// the greatest inline key (invariant 4), so they never count below it,
    /// and count equal only to a greatest `target`, whose equal range the cut
    /// at `len` ends where the live slots do. With no branch, no dependent load
    /// and no mask per slot, and a trip count that is the capacity, one of
    /// two constants per map, the compiler unrolls it into a few vector
    /// compares whose masks it counts. Within `0..len` for any slots, by the
    /// cuts.
    fn equal_range_by_count(self, target: W) -> Range<usize> {
        let below = self.slots.iter().filter(|&&slot| slot < target).count();
        let equal = self.slots.iter().filter(|&&slot| slot == target).count();
        let below = below.min(self.len);
        below..(below + equal).min(self.len)
    }

    /// A branchless binary search finds where the equal range starts, and a
    /// scan from there finds where it ends: one compare, which inline keys
    /// that don't tie make predictable, rather than a second search, whose
    /// exit depends on where the first one ended. Within `0..len` for any
    /// slots: the search and the scan stay within the live slots.
    fn equal_range_by_search(self, target: W) -> Range<usize> {
        let live = self.live();
        let start = Self::count_below(live, target);
        let mut end = start;
        while live.get(end) == Some(&target) {
            end += 1;
        }
        start..end
    }

    /// How many of `inline_keys`, sorted, are below `target`, by a
    /// branchless binary search, as in `KeyList::branchless_search_index`;
    /// in `0..=len` for any inline keys, by construction: the loop keeps
    /// `base + size <= len`, and probes only `mid = base + size / 2 < base +
    /// size`.
    fn count_below(inline_keys: &[W], target: W) -> usize {
        let mut size = inline_keys.len();
        if size == 0 {
            return 0;
        }
        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            // SAFETY: `mid < base + size <= inline_keys.len()`.
            let probe = unsafe { *inline_keys.get_unchecked(mid) };
            base = hint::select_unpredictable(probe < target, mid, base);
            size -= half;
        }
        // SAFETY: `base < base + size <= inline_keys.len()`, with `size == 1`
        // here.
        let last = unsafe { *inline_keys.get_unchecked(base) };
        base + usize::from(last < target)
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use core::ptr::NonNull;
    use std::vec;

    use super::{InlineKeySlots, InlineKeysOfWidth, InlineSlot, NodeInlineKeys};
    use crate::inline_key::{InlineWidth, InlineWord};
    use crate::tree::test_support::TestRng;

    /// `stale_count` random slots after `live`, as no node holds past its
    /// length: a search must stay in bounds whatever they hold.
    fn slots_with_stale_tail<W: InlineSlot + From<u8>>(
        rng: &TestRng,
        live: &[W],
        stale_count: usize,
    ) -> Vec<W> {
        let stale = (0..stale_count).map(|_| W::from(rng.below(20) as u8));
        live.iter().copied().chain(stale).collect()
    }

    /// The greatest inline key, which slots past a node's length hold.
    fn greatest<W: InlineSlot>() -> W {
        W::of_inline_key(InlineWord::MAX)
    }

    /// On sorted inline keys, the equal range is exactly the equal ones,
    /// wherever they are and however many, over several words of slots, the
    /// greatest too, by every way of finding it, with the slots past `len`
    /// holding the greatest (invariant 4), and by the vector count whatever
    /// they hold.
    fn assert_equal_ranges_of_sorted_inline_keys<W: InlineSlot + From<u8>>(seed: u64) {
        let rng = TestRng::new(seed);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let len = rng.below(140);
            let mut inline_keys: Vec<W> = (0..len)
                .map(|_| match rng.below(9) {
                    8 => greatest(),
                    small => W::from(small as u8 * 2),
                })
                .collect();
            inline_keys.sort_unstable();
            let stale = core::iter::repeat_n(greatest(), rng.below(8));
            let slots: Vec<W> = inline_keys.iter().copied().chain(stale).collect();
            let node = InlineKeysOfWidth::new(&slots, len);
            let unpadded = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            for target in (0..18).map(W::from).chain([greatest()]) {
                let start = inline_keys.partition_point(|&probe| probe < target);
                let end = inline_keys.partition_point(|&probe| probe <= target);
                assert_eq!(node.equal_range(target), start..end);
                assert_eq!(node.equal_range_by_count(target), start..end);
                assert_eq!(node.equal_range_by_search(target), start..end);
                for slots in [&slots, &unpadded] {
                    if let Some(equal_range) = W::equal_range_in_vectors(slots, len, target) {
                        assert_eq!(equal_range, start..end);
                    }
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn equal_ranges_of_sorted_inline_keys() {
        assert_equal_ranges_of_sorted_inline_keys::<u16>(51);
        assert_equal_ranges_of_sorted_inline_keys::<u32>(53);
        assert_equal_ranges_of_sorted_inline_keys::<u64>(54);
    }

    /// On any inline keys, the equal range stays within the live ones.
    fn assert_equal_ranges_stay_in_bounds<W: InlineSlot + From<u8>>(seed: u64) {
        let rng = TestRng::new(seed);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let len = rng.below(40);
            let inline_keys: Vec<W> = (0..len).map(|_| W::from(rng.below(4) as u8)).collect();
            let slots = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            let node = InlineKeysOfWidth::new(&slots, len);
            for target in (0..5).map(W::from) {
                let equal_ranges = [
                    node.equal_range(target),
                    node.equal_range_by_count(target),
                    node.equal_range_by_search(target),
                ];
                for equal_range in equal_ranges
                    .into_iter()
                    .chain(W::equal_range_in_vectors(&slots, len, target))
                {
                    assert!(equal_range.start <= equal_range.end && equal_range.end <= len);
                }
            }
        }
    }

    #[test]
    fn equal_ranges_stay_in_bounds_for_unsorted_inline_keys() {
        assert_equal_ranges_stay_in_bounds::<u16>(52);
        assert_equal_ranges_stay_in_bounds::<u32>(55);
        assert_equal_ranges_stay_in_bounds::<u64>(56);
    }

    /// A node's inline keys at each width keep the first bytes of the windows
    /// written, compare by them, and find the equal_ranges of windows that
    /// tie in them: three sorted windows that share 2 bytes, two of which
    /// share 4.
    #[test]
    fn inline_keys_of_each_width_keep_the_first_bytes() {
        let windows: [InlineWord; 3] = [
            0x0102_0304_0506_0708,
            0x0102_0304_ffff_ffff,
            0x0102_ffff_0000_0000,
        ];
        for (width, kept_bytes) in [
            (InlineWidth::Two, 2),
            (InlineWidth::Four, 4),
            (InlineWidth::Eight, 8),
        ] {
            let capacity = 1024 / kept_bytes;
            // All ones: the greatest inline key at every width, which slots
            // past a node's length hold (invariant 4).
            let mut buffer = vec![u64::MAX; 128];
            let anchor = NonNull::new(buffer.as_mut_ptr().cast::<u8>()).unwrap();
            // SAFETY: `capacity` slots of `kept_bytes` fill the 1 KB buffer,
            // which is 8-aligned and outlives them.
            let slots = unsafe { InlineKeySlots::new(width, anchor, capacity) };
            for (index, &window) in windows.iter().enumerate() {
                // SAFETY: `index < capacity`.
                unsafe { slots.write(index, window) };
            }
            // SAFETY: the buffer was filled, so every slot is initialized,
            // and nothing writes it while `inline_keys` lives.
            let inline_keys = unsafe { NodeInlineKeys::new(slots, capacity, windows.len()) };
            let kept_mask = !(InlineWord::MAX
                .checked_shr(8 * kept_bytes as u32)
                .unwrap_or(0));
            for (index, &window) in windows.iter().enumerate() {
                let kept = window & kept_mask;
                assert_eq!(inline_keys.inline_key(index), kept);
                let below = windows.iter().filter(|&&other| other & kept_mask < kept);
                let ties = windows.iter().filter(|&&other| other & kept_mask == kept);
                let (below, ties) = (below.count(), ties.count());
                assert_eq!(inline_keys.equal_range(window), below..below + ties);
            }
        }
    }
}
