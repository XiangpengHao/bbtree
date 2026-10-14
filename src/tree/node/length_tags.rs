//! A leaf's length tags (D36, invariant 5): which of a lookup's candidates
//! have the key's length, as none, one, two or many. Tags never order keys.

#![forbid(unsafe_code)]

pub(super) use super::vector_count::SlotMatches;
use super::vector_count::U16EqualitySearch;
use core::ops::Range;

/// A leaf's length tags, every slot up to its capacity, each live one its
/// key's byte length truncated to 16 bits (D36).
pub(super) struct LengthTags<'a> {
    slots: &'a [u16],
}

impl<'a> LengthTags<'a> {
    pub(super) fn new(slots: &'a [u16]) -> Self {
        Self { slots }
    }

    /// The tag of a key of `len` bytes: its length, truncated to 16 bits,
    /// which only lets keys of other lengths collide.
    #[inline(always)]
    pub(super) fn tag_of(len: usize) -> u16 {
        len as u16
    }

    /// The positions among `candidates`, live keys, whose tag is `target`:
    /// none, one, two, or more. The vector search reads every slot up to
    /// the capacity, a constant trip count, and masks out the rest.
    #[inline(always)]
    pub(super) fn matching(&self, candidates: Range<usize>, target: u16) -> SlotMatches {
        if let Some(matches) = U16EqualitySearch::matching(self.slots, candidates.clone(), target) {
            return matches;
        }
        let mut matches = self.slots[candidates.clone()]
            .iter()
            .enumerate()
            .filter(|(_, tag)| **tag == target);
        let Some((lower, _)) = matches.next() else {
            return SlotMatches::None;
        };
        let Some((upper, _)) = matches.next() else {
            return SlotMatches::One(candidates.start + lower);
        };
        if matches.next().is_some() {
            SlotMatches::Many
        } else {
            SlotMatches::Two {
                lower: candidates.start + lower,
                upper: candidates.start + upper,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::vec;

    use super::{LengthTags, SlotMatches};

    #[test]
    fn length_filter_checks_only_the_candidates() {
        for capacity in 0..=65 {
            let mut slots = vec![7; capacity];
            for target_index in 0..capacity {
                slots[target_index] = 65535;
                let tags = LengthTags::new(&slots);
                assert!(
                    matches!(tags.matching(0..capacity, 65535), SlotMatches::One(index) if index == target_index)
                );
                assert!(matches!(
                    tags.matching(0..target_index, 65535),
                    SlotMatches::None
                ));
                assert!(matches!(
                    tags.matching(target_index + 1..capacity, 65535),
                    SlotMatches::None
                ));
                assert!(matches!(tags.matching(0..0, 7), SlotMatches::None));
                if capacity > 3 {
                    assert!(matches!(tags.matching(0..capacity, 7), SlotMatches::Many));
                }
                slots[target_index] = 7;
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn two_matches_keep_their_order_and_stay_among_the_candidates() {
        for capacity in 0..=65 {
            let mut slots = vec![7; capacity];
            assert!(matches!(
                LengthTags::new(&slots).matching(0..0, 7),
                SlotMatches::None
            ));
            for lower in 0..capacity {
                slots[lower] = 65535;
                for upper in lower + 1..capacity {
                    slots[upper] = 65535;
                    let tags = LengthTags::new(&slots);
                    assert!(matches!(
                        tags.matching(0..capacity, 65535),
                        SlotMatches::Two { lower: first, upper: second }
                            if (first, second) == (lower, upper)
                    ));
                    assert!(matches!(
                        tags.matching(lower..upper, 65535),
                        SlotMatches::One(index) if index == lower
                    ));
                    assert!(matches!(
                        tags.matching(lower + 1..upper + 1, 65535),
                        SlotMatches::One(index) if index == upper
                    ));
                    slots[upper] = 7;
                }
                slots[lower] = 7;
            }
        }

        // The scalar fallback keeps full indexes, even beyond u16::MAX.
        let mut slots = vec![7; 65537];
        slots[17] = 65535;
        slots[65536] = 65535;
        assert!(matches!(
            LengthTags::new(&slots).matching(0..slots.len(), 65535),
            SlotMatches::Two {
                lower: 17,
                upper: 65536
            }
        ));
        slots[65535] = 65535;
        assert!(matches!(
            LengthTags::new(&slots).matching(0..slots.len(), 65535),
            SlotMatches::Many
        ));
    }
}
