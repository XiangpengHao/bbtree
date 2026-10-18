//! A leaf's hash tags (D36, invariant 5): which of a lookup's candidates
//! have its hash tag, as none, one, two or many. Tags never order keys.

#![forbid(unsafe_code)]

pub(super) use super::vector_count::SlotMatches;
use super::vector_count::U8EqualitySearch;
use core::ops::Range;

/// A leaf's hash tags, every slot up to its capacity, each live one its
/// key's [`InlineKey::hash_tag`] (D36).
///
/// [`InlineKey::hash_tag`]: crate::InlineKey::hash_tag
pub(super) struct HashTags<'a> {
    slots: &'a [u8],
}

impl<'a> HashTags<'a> {
    /// The tag of every std byte string of 255 bytes or more: a byte
    /// string's tag is its length, saturated (`[u8]`'s `InlineKey` impl).
    const SATURATED_LENGTH: u8 = u8::MAX;

    pub(super) fn new(slots: &'a [u8]) -> Self {
        Self { slots }
    }

    /// Whether `tag`, a std byte string's, is its exact length, as every
    /// tag below [`Self::SATURATED_LENGTH`] is: two byte strings with such
    /// a tag have equal lengths.
    #[inline(always)]
    pub(super) fn is_exact_length(tag: u8) -> bool {
        tag < Self::SATURATED_LENGTH
    }

    /// The positions among `candidates`, live keys, whose tag is `target`:
    /// none, one, two, or more. The vector search reads every slot up to
    /// the capacity, a constant trip count, and masks out the rest.
    #[inline(always)]
    pub(super) fn matching(&self, candidates: Range<usize>, target: u8) -> SlotMatches {
        if let Some(matches) = U8EqualitySearch::matching(self.slots, candidates.clone(), target) {
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

    use super::{HashTags, SlotMatches};
    use crate::inline_key::InlineKey;

    #[test]
    fn byte_string_tags_are_lengths_exact_below_saturation() {
        for len in [0, 1, 4, 253, 254] {
            let tag = vec![b'x'; len].hash_tag();
            assert_eq!(usize::from(tag), len);
            assert!(HashTags::is_exact_length(tag));
        }
        for len in [255, 256, 257, 65_536] {
            let tag = vec![b'x'; len].hash_tag();
            assert_eq!(tag, u8::MAX);
            assert!(!HashTags::is_exact_length(tag));
        }
    }

    #[test]
    fn length_filter_checks_only_the_candidates() {
        for capacity in 0..=65 {
            let mut slots = vec![7; capacity];
            for target_index in 0..capacity {
                slots[target_index] = 255;
                let tags = HashTags::new(&slots);
                assert!(
                    matches!(tags.matching(0..capacity, 255), SlotMatches::One(index) if index == target_index)
                );
                assert!(matches!(
                    tags.matching(0..target_index, 255),
                    SlotMatches::None
                ));
                assert!(matches!(
                    tags.matching(target_index + 1..capacity, 255),
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
    fn two_matches_keep_their_order_and_stay_among_the_candidates() {
        for capacity in 0..=65 {
            let mut slots = vec![7; capacity];
            assert!(matches!(
                HashTags::new(&slots).matching(0..0, 7),
                SlotMatches::None
            ));
            for lower in 0..capacity {
                slots[lower] = 255;
                for upper in lower + 1..capacity {
                    slots[upper] = 255;
                    let tags = HashTags::new(&slots);
                    assert!(matches!(
                        tags.matching(0..capacity, 255),
                        SlotMatches::Two { lower: first, upper: second }
                            if (first, second) == (lower, upper)
                    ));
                    assert!(matches!(
                        tags.matching(lower..upper, 255),
                        SlotMatches::One(index) if index == lower
                    ));
                    assert!(matches!(
                        tags.matching(lower + 1..upper + 1, 255),
                        SlotMatches::One(index) if index == upper
                    ));
                    slots[upper] = 7;
                }
                slots[lower] = 7;
            }
        }

        // The scalar fallback keeps full indexes, even beyond u16::MAX.
        let mut slots = vec![7; 65537];
        slots[17] = 255;
        slots[65536] = 255;
        assert!(matches!(
            HashTags::new(&slots).matching(0..slots.len(), 255),
            SlotMatches::Two {
                lower: 17,
                upper: 65536
            }
        ));
        slots[65535] = 255;
        assert!(matches!(
            HashTags::new(&slots).matching(0..slots.len(), 255),
            SlotMatches::Many
        ));
    }
}
