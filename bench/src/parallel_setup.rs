//! A cell's untimed setup on all cores: copying its keys into input pools,
//! making values, sorting, and moving shuffled items into place. Timed
//! regions stay on the cell's own thread, and cells still run one at a time.
//!
//! Work on fewer than `ParallelSetup::MIN_ITEMS` items stays on the calling
//! thread, so that small maps' keys and values stay in its heap: small maps'
//! timed regions free them (removals, pops, upserts of present keys), and a
//! free from another thread's heap takes mimalloc's slower cross-thread path.
//! Larger inputs' allocations land in the worker threads' heaps. Of the
//! suites' timed regions on large maps, none frees a heap-allocated key or
//! value, so the difference falls in untimed teardown.

use std::cmp::Ordering;

use rayon::prelude::*;

/// The setup steps that run on all cores for large inputs. They give the same
/// results as their serial versions, in the same order.
pub(crate) struct ParallelSetup;

impl ParallelSetup {
    /// The fewest items worth spreading over all cores: below it, the work
    /// takes well under a millisecond on one.
    const MIN_ITEMS: usize = 1 << 16;

    /// Whether work on `item_count` items runs on all cores.
    pub(crate) fn spreads(item_count: usize) -> bool {
        item_count >= Self::MIN_ITEMS
    }

    /// `make(index, item)` for each of `items`, in order.
    pub(crate) fn map<T: Sync, U: Send>(
        items: &[T],
        make: impl Fn(usize, &T) -> U + Sync + Send,
    ) -> Vec<U> {
        if Self::spreads(items.len()) {
            (items.par_iter().enumerate())
                .map(|(index, item)| make(index, item))
                .collect()
        } else {
            (items.iter().enumerate())
                .map(|(index, item)| make(index, item))
                .collect()
        }
    }

    /// A copy of each of `items`, in order.
    pub(crate) fn cloned<T: Clone + Send + Sync>(items: &[T]) -> Vec<T> {
        Self::map(items, |_, item| item.clone())
    }

    /// Sorts `items` as `slice::sort_unstable_by` does. Which of equal items
    /// comes first may differ, so callers sort items that are distinct.
    pub(crate) fn sort_unstable_by<T: Send>(
        items: &mut [T],
        compare: impl Fn(&T, &T) -> Ordering + Sync,
    ) {
        if Self::spreads(items.len()) {
            items.par_sort_unstable_by(compare);
        } else {
            items.sort_unstable_by(compare);
        }
    }

    /// `items` in the order `order` lists their indices, which it lists
    /// once each: the item at `order[position]` goes to `position`.
    pub(crate) fn permute<T: Send>(items: Vec<T>, order: &[usize]) -> Vec<T> {
        assert_eq!(items.len(), order.len(), "the order lists every item");
        let mut destinations = vec![usize::MAX; order.len()];
        for (position, &index) in order.iter().enumerate() {
            destinations[index] = position;
        }
        // Sorting the items by their destination moves each into place,
        // without cloning them.
        let mut placed: Vec<(usize, T)> = if Self::spreads(items.len()) {
            destinations.into_par_iter().zip(items).collect()
        } else {
            destinations.into_iter().zip(items).collect()
        };
        Self::sort_unstable_by(&mut placed, |(left, _), (right, _)| left.cmp(right));
        assert!(
            (placed.iter().enumerate())
                .all(|(position, &(destination, _))| destination == position),
            "the order is a permutation"
        );
        if Self::spreads(placed.len()) {
            placed.into_par_iter().map(|(_, item)| item).collect()
        } else {
            placed.into_iter().map(|(_, item)| item).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permute_moves_each_item_to_its_position() {
        for count in [10, ParallelSetup::MIN_ITEMS + 10] {
            let items: Vec<String> = (0..count).map(|index| index.to_string()).collect();
            let order: Vec<usize> = (0..count)
                .map(|position| (position * 7 + 3) % count)
                .collect();
            let permuted = ParallelSetup::permute(items.clone(), &order);
            for (position, &index) in order.iter().enumerate() {
                assert_eq!(permuted[position], items[index]);
            }
        }
    }

    #[test]
    #[should_panic(expected = "permutation")]
    fn permute_refuses_an_order_that_repeats_an_index() {
        ParallelSetup::permute(vec![1, 2, 3], &[0, 0, 2]);
    }

    #[test]
    fn map_keeps_the_order_in_parallel() {
        let items: Vec<usize> = (0..ParallelSetup::MIN_ITEMS * 2).collect();
        let doubled = ParallelSetup::map(&items, |index, item| index + item);
        assert!(
            doubled
                .iter()
                .enumerate()
                .all(|(index, &sum)| sum == 2 * index)
        );
    }
}
