//! Deterministic pseudo-random numbers, so that every map in a cell sees the
//! same keys, values and access orders.

use std::mem;
use std::ops::RangeInclusive;

use crate::parallel_setup::ParallelSetup;

/// What a random sequence is for. Each purpose has its own seed, so that,
/// say, the lookup order doesn't shift when key generation draws one more
/// number. The seed is the variant's position: new ones go last.
#[derive(Clone, Copy)]
pub(crate) enum RandomStream {
    KeyContent,
    InsertionOrder,
    LookupOrder,
    RemovalOrder,
    MapOrder,
    /// Which keys of a dataset a map of less than all of them holds.
    DatasetSample,
    /// Which keys lookups that miss tiny maps probe with.
    AbsentKeys,
}

/// SplitMix64 (Steele, Lea and Flood, 2014): small, fast, and random enough
/// for benchmark data.
pub(crate) struct SeededRng {
    state: u64,
}

impl SeededRng {
    const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

    pub(crate) fn new(stream: RandomStream) -> Self {
        Self {
            state: Self::mix(stream as u64),
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(Self::GOLDEN_GAMMA);
        Self::mix(self.state)
    }

    /// A number in `0..bound`, by Lemire's multiply-and-shift; the bias is
    /// below 2⁻³² for any bound the benchmark uses.
    pub(crate) fn below(&mut self, bound: usize) -> usize {
        let bound = bound as u128;
        ((u128::from(self.next_u64()) * bound) >> 64) as usize
    }

    pub(crate) fn in_range(&mut self, range: RangeInclusive<usize>) -> usize {
        range.start() + self.below(range.end() - range.start() + 1)
    }

    /// One of `table`'s values, each as likely as its weight.
    pub(crate) fn weighted<T: Copy>(&mut self, table: &[(T, u32)]) -> T {
        let total: u32 = table.iter().map(|&(_, weight)| weight).sum();
        let mut ticket = self.below(total as usize) as u32;
        for &(value, weight) in table {
            if ticket < weight {
                return value;
            }
            ticket -= weight;
        }
        unreachable!("the ticket is below the total weight")
    }

    /// `length` characters drawn from `alphabet`, which must be ASCII.
    pub(crate) fn ascii_text(&mut self, length: usize, alphabet: &[u8]) -> String {
        (0..length)
            .map(|_| char::from(alphabet[self.below(alphabet.len())]))
            .collect()
    }

    /// Fisher–Yates. Many items are shuffled on all cores, with the same
    /// result and the same draws: the swaps run on their indices, which
    /// are small and contiguous, then the items move to their places.
    pub(crate) fn shuffle<T: Send>(&mut self, items: &mut Vec<T>) {
        if !ParallelSetup::spreads(items.len()) {
            self.fisher_yates(items);
            return;
        }
        let mut order: Vec<usize> = (0..items.len()).collect();
        self.fisher_yates(&mut order);
        *items = ParallelSetup::permute(mem::take(items), &order);
    }

    fn fisher_yates<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }

    /// Moves `count` distinct items, chosen at random, to the front of
    /// `items`: the first `count` steps of Fisher–Yates.
    pub(crate) fn choose_to_front<T>(&mut self, items: &mut [T], count: usize) {
        for index in 0..count {
            let chosen = index + self.below(items.len() - index);
            items.swap(index, chosen);
        }
    }

    /// The indices of the items that `choose_to_front` would move to the
    /// front of `item_count` items, in its order.
    pub(crate) fn choose_indices(&mut self, item_count: usize, count: usize) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..item_count).collect();
        self.choose_to_front(&mut indices, count);
        indices.truncate(count);
        indices
    }

    /// SplitMix64's output function. It is a bijection on `u64`, so distinct
    /// inputs give distinct, random-looking outputs.
    pub(crate) fn mix(mut x: u64) -> u64 {
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffling_many_items_gives_the_serial_result() {
        let count = 100_000;
        assert!(ParallelSetup::spreads(count));
        let mut shuffled: Vec<u64> = (0..count as u64).collect();
        let mut serially_shuffled = shuffled.clone();
        let (mut rng, mut serial_rng) = (
            SeededRng::new(RandomStream::InsertionOrder),
            SeededRng::new(RandomStream::InsertionOrder),
        );
        rng.shuffle(&mut shuffled);
        serial_rng.fisher_yates(&mut serially_shuffled);
        assert_eq!(shuffled, serially_shuffled);
        assert_eq!(rng.next_u64(), serial_rng.next_u64());
    }

    #[test]
    fn chosen_indices_are_those_chosen_to_the_front() {
        let mut items: Vec<usize> = (0..1_000).collect();
        SeededRng::new(RandomStream::DatasetSample).choose_to_front(&mut items, 100);
        let indices = SeededRng::new(RandomStream::DatasetSample).choose_indices(1_000, 100);
        assert_eq!(indices, items[..100]);
    }
}
