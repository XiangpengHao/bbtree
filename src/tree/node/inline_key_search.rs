//! A node's inline keys, read in place for a search (D18): the column of
//! a key's first bytes past the node's prefix that leaves and internal nodes
//! keep beside their keys, and how a search finds the keys whose inline
//! keys equal its own, by counting over the whole column or by a binary
//! search, as the target and the width call for.

use core::cmp::Ordering;
use core::hint;
use core::ops::Range;

use super::inline_keys::InlineSlot;
use super::slots::InlineKeySlots;
use super::vector_count::U64EqualityMask;
use crate::inline_key::InlineWord;

/// A node's inline keys, read in place (D18): every slot up to the
/// capacity, at this node kind's width, of which slots `0..len` hold the inline keys
/// of keys `0..len`, and the rest stale ones, which a search reads and masks
/// out. Its methods speak in windows: they narrow a window to the bytes the
/// node keeps, and give an inline key back as those bytes, zeros after.
///
/// Each typed node kind of a map has one width, so after monomorphization
/// each method is that of one width. They are not generic, so they need
/// `#[inline]` to inline into the searches, which other crates instantiate,
/// and `always`: before the width folds, each looks like a match of three
/// arms with bounds checks, which the compiler kept out of line, a call and
/// a match on every level of every insert.
#[derive(Clone, Copy)]
pub(in crate::tree) enum NodeInlineKeys<'a> {
    Two(LiveSlots<'a, u16>),
    Four(LiveSlots<'a, u32>),
    Eight(LiveSlots<'a, u64>),
}

/// `$body`, with `$live` bound to the slots of `$inline_keys`, whichever
/// their width.
macro_rules! with_live_slots {
    ($inline_keys:expr, |$live:ident| $body:expr) => {
        match $inline_keys {
            NodeInlineKeys::Two($live) => $body,
            NodeInlineKeys::Four($live) => $body,
            NodeInlineKeys::Eight($live) => $body,
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
                InlineKeySlots::Two(slots) => {
                    Self::Two(LiveSlots::new(slots.run(0, capacity).as_ref(), len))
                }
                InlineKeySlots::Four(slots) => {
                    Self::Four(LiveSlots::new(slots.run(0, capacity).as_ref(), len))
                }
                InlineKeySlots::Eight(slots) => {
                    Self::Eight(LiveSlots::new(slots.run(0, capacity).as_ref(), len))
                }
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
        with_live_slots!(self, |live| live.live()[index].window())
    }

    /// How inline key `index` orders against the bytes the node keeps of
    /// `window`. Equal says nothing of the keys.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    #[inline(always)]
    pub(super) fn cmp_at(self, index: usize, window: InlineWord) -> Ordering {
        with_live_slots!(self, |live| live
            .cmp_at(index, InlineSlot::of_window(window)))
    }

    /// The positions of the inline keys equal to the bytes the node keeps
    /// of `window`; see [`LiveSlots::equal_run`].
    #[inline(always)]
    pub(in crate::tree) fn equal_run(self, window: InlineWord) -> Range<usize> {
        with_live_slots!(self, |live| live.equal_run(InlineSlot::of_window(window)))
    }

    /// An equality mask only for widths and targets that benefit from it.
    /// Unsupported shapes keep the original ordered range and empty check.
    #[inline(always)]
    pub(super) fn point_mask(self, window: InlineWord) -> Option<U64EqualityMask> {
        with_live_slots!(self, |live| live.point_mask(InlineSlot::of_window(window)))
    }

    /// Whether every slot past `len` holds the greatest inline key
    /// (invariant 4).
    #[cfg(any(test, fuzzing))]
    pub(in crate::tree) fn pads_with_greatest(self) -> bool {
        with_live_slots!(self, |live| live.pads_with_greatest())
    }
}

/// A node's inline keys at one width, as [`NodeInlineKeys`] reads them:
/// `slots` up to the capacity, of which the first `len` are live.
#[derive(Clone, Copy)]
pub(in crate::tree) struct LiveSlots<'a, W> {
    slots: &'a [W],
    len: usize,
}

impl<'a, W: InlineSlot> LiveSlots<'a, W> {
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
        let greatest = W::of_window(InlineWord::MAX);
        self.slots[self.len..].iter().all(|&slot| slot == greatest)
    }

    /// How live slot `index` orders against `target`.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    fn cmp_at(self, index: usize, target: W) -> Ordering {
        self.live()[index].cmp(&target)
    }

    /// The positions of the inline keys equal to `target`: exactly those
    /// for sorted inline keys, which the `InlineKey` contract gives, and a
    /// range within `0..len` for any. If none equals it, the empty range at
    /// the first inline key greater than it.
    ///
    /// Found by a count of every slot: in explicit vectors where the target
    /// has them for slots of this width
    /// ([`VectorCount`](super::vector_count::VectorCount)), otherwise as the
    /// compiler vectorizes it ([`Self::equal_run_by_count`]) where it
    /// compares slots of this width in vectors
    /// ([`InlineSlot::COUNTS_IN_VECTORS`]); and by a binary search
    /// otherwise ([`Self::equal_run_by_search`]): without such compares,
    /// the count is an emulated sequence that costs more than the
    /// mispredicts it avoids. Measured on 1M-entry maps of 8-byte inline
    /// keys (04, Q6).
    /// Wide four-byte arrays on baseline x86-64 also use binary search:
    /// beyond the one-word packed reduction, full-capacity counts cost
    /// more than dependent probes in warm routing nodes.
    #[inline(always)]
    fn equal_run(self, target: W) -> Range<usize> {
        if cfg!(all(
            target_arch = "x86_64",
            not(target_feature = "avx512bw"),
            not(target_feature = "popcnt")
        )) && size_of::<W>() == 4
            && self.slots.len() > 32
        {
            return self.equal_run_by_search(target);
        }
        if let Some(run) = W::equal_run_in_vectors(self.slots, self.len, target) {
            return run;
        }
        if W::COUNTS_IN_VECTORS {
            self.equal_run_by_count(target)
        } else {
            self.equal_run_by_search(target)
        }
    }

    /// Equality-only masks are optional; ordered fallback is chosen by
    /// the leaf without wrapping its existing range in another result.
    #[inline(always)]
    fn point_mask(self, target: W) -> Option<U64EqualityMask> {
        W::point_mask_in_vectors(self.slots, target)
    }

    /// One pass over every slot up to the capacity counts the inline keys
    /// below `target` and those equal to it. The slots from `len` on hold
    /// the greatest inline key (invariant 4), so they never count below it,
    /// and count equal only to a greatest `target`, whose run the cut at
    /// `len` ends where the live slots do. With no branch, no dependent load
    /// and no mask per slot, and a trip count that is the capacity, one of
    /// two constants per map, the compiler unrolls it into a few vector
    /// compares whose masks it counts. Within `0..len` for any slots, by the
    /// cuts.
    fn equal_run_by_count(self, target: W) -> Range<usize> {
        let below = self.slots.iter().filter(|&&slot| slot < target).count();
        let equal = self.slots.iter().filter(|&&slot| slot == target).count();
        let below = below.min(self.len);
        below..(below + equal).min(self.len)
    }

    /// A branchless binary search finds where the run starts, and a scan
    /// from there finds where it ends: one compare, which inline keys that
    /// don't tie make predictable, rather than a second search, whose exit
    /// depends on where the first one ended. Within `0..len` for any slots:
    /// the search and the scan stay within the live slots.
    fn equal_run_by_search(self, target: W) -> Range<usize> {
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

    use core::cmp::Ordering;
    use core::ptr::NonNull;
    use std::vec;

    use super::{InlineKeySlots, InlineSlot, LiveSlots, NodeInlineKeys};
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
        W::of_window(InlineWord::MAX)
    }

    /// On sorted inline keys, the run is exactly the equal ones, wherever
    /// they are and however many, over several words of slots, the
    /// greatest too, by every way of finding it, with the slots past `len`
    /// holding the greatest (invariant 4), and by the vector count whatever
    /// they hold.
    fn assert_equal_runs_of_sorted_inline_keys<W: InlineSlot + From<u8>>(seed: u64) {
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
            let node = LiveSlots::new(&slots, len);
            let unpadded = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            for target in (0..18).map(W::from).chain([greatest()]) {
                let start = inline_keys.partition_point(|&probe| probe < target);
                let end = inline_keys.partition_point(|&probe| probe <= target);
                assert_eq!(node.equal_run(target), start..end);
                assert_eq!(node.equal_run_by_count(target), start..end);
                assert_eq!(node.equal_run_by_search(target), start..end);
                for slots in [&slots, &unpadded] {
                    if let Some(run) = W::equal_run_in_vectors(slots, len, target) {
                        assert_eq!(run, start..end);
                    }
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn equal_runs_of_sorted_inline_keys() {
        assert_equal_runs_of_sorted_inline_keys::<u16>(51);
        assert_equal_runs_of_sorted_inline_keys::<u32>(53);
        assert_equal_runs_of_sorted_inline_keys::<u64>(54);
    }

    /// On any inline keys, the run stays within the live ones.
    fn assert_equal_runs_stay_in_bounds<W: InlineSlot + From<u8>>(seed: u64) {
        let rng = TestRng::new(seed);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let len = rng.below(40);
            let inline_keys: Vec<W> = (0..len).map(|_| W::from(rng.below(4) as u8)).collect();
            let slots = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            let node = LiveSlots::new(&slots, len);
            for target in (0..5).map(W::from) {
                let runs = [
                    node.equal_run(target),
                    node.equal_run_by_count(target),
                    node.equal_run_by_search(target),
                ];
                for run in runs
                    .into_iter()
                    .chain(W::equal_run_in_vectors(&slots, len, target))
                {
                    assert!(run.start <= run.end && run.end <= len);
                }
            }
        }
    }

    #[test]
    fn equal_runs_stay_in_bounds_for_unsorted_inline_keys() {
        assert_equal_runs_stay_in_bounds::<u16>(52);
        assert_equal_runs_stay_in_bounds::<u32>(55);
        assert_equal_runs_stay_in_bounds::<u64>(56);
    }

    /// A node's inline keys at each width keep the first bytes of the
    /// windows written, compare by them, and find the runs of windows that
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
                assert_eq!(inline_keys.cmp_at(index, window), Ordering::Equal);
                let below = windows.iter().filter(|&&other| other & kept_mask < kept);
                let ties = windows.iter().filter(|&&other| other & kept_mask == kept);
                let (below, ties) = (below.count(), ties.count());
                assert_eq!(inline_keys.equal_run(window), below..below + ties);
            }
        }
    }
}
