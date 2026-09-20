//! Operations on one map.
//!
//! A cell times at least the operation budget: operations that consume the
//! map (builds, removals, pops) rebuild it for another round, outside the
//! timed region, and lookups, scans and passes run more of themselves on the
//! same map. Large maps run one round; lookups on them stop at the budget.

use std::hint::black_box;

use super::{Operation, PreparedCell, Stopwatch};
use crate::command_line::Named;
use crate::keys::{BenchKey, KeyMaker};
use crate::map_adapters::MapUnderTest;
use crate::random::{RandomStream, SeededRng};
use crate::values::{BenchValue, ValueMaker};

pub(crate) struct SingleMapCell<K, V> {
    keys: KeyMaker<K>,
    values: ValueMaker<V>,
    /// The map's keys, distinct, in random insertion order.
    present: Vec<K>,
    budget: usize,
}

impl<K: BenchKey, V: BenchValue> SingleMapCell<K, V> {
    pub(crate) fn new(
        mut keys: KeyMaker<K>,
        values: ValueMaker<V>,
        entries: usize,
        budget: usize,
    ) -> Self {
        let mut present: Vec<K> = (0..entries as u64)
            .map(|index| keys.key(2 * index))
            .collect();
        SeededRng::new(RandomStream::InsertionOrder).shuffle(&mut present);
        Self {
            keys,
            values,
            present,
            budget,
        }
    }

    fn entry_count(&self) -> usize {
        self.present.len()
    }

    /// How many rounds reach the budget: one for large maps.
    fn rounds(&self) -> usize {
        self.budget.div_ceil(self.entry_count())
    }

    /// Fresh entries, in insertion order.
    fn entries_in_insertion_order(&self) -> Vec<(K, V)> {
        (self.present.iter().enumerate())
            .map(|(index, key)| (key.clone(), self.values.value(index as u64)))
            .collect()
    }

    fn entries_in_key_order(&self) -> Vec<(K, V)> {
        let mut entries = self.entries_in_insertion_order();
        entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        entries
    }

    fn insert_all<M: MapUnderTest<K, V>>(entries: Vec<(K, V)>) -> M {
        let mut map = M::new();
        for (key, value) in entries {
            map.insert(key, value);
        }
        map
    }

    /// The map that operations other than builds start from: every key
    /// inserted in random order, as most maps are built.
    fn built_map<M: MapUnderTest<K, V>>(&self) -> M {
        Self::insert_all(self.entries_in_insertion_order())
    }

    fn build_map<M: MapUnderTest<K, V>>(operation: Operation, input: Vec<(K, V)>) -> M {
        match operation {
            Operation::BulkBuild => M::from_sorted_entries(input),
            _ => Self::insert_all(input),
        }
    }

    /// Up to `budget` of the map's keys, a random sample, cloned so that
    /// lookups probe with keys of their own.
    fn present_sample(&self) -> Vec<K> {
        let count = self.budget.min(self.entry_count());
        let stride = self.entry_count() / count;
        self.present
            .iter()
            .step_by(stride)
            .take(count)
            .cloned()
            .collect()
    }

    /// Up to `budget` keys that the map doesn't hold, spread over its key
    /// range.
    fn absent_sample(&mut self) -> Vec<K> {
        let count = self.budget.min(self.entry_count());
        let stride = self.entry_count() / count;
        (0..count)
            .map(|position| self.keys.key(2 * (position * stride) as u64 + 1))
            .collect()
    }

    /// The keys range scans may start at: those with at least
    /// `RANGE_SCAN_LENGTH` − 1 keys after them, so that every scan visits
    /// the full length.
    fn range_starts(&self) -> Vec<K> {
        let mut starts = self.present.clone();
        starts.sort_unstable();
        let start_count = starts
            .len()
            .saturating_sub(Operation::RANGE_SCAN_LENGTH - 1);
        assert!(start_count > 0, "range64 needs at least 64 entries");
        starts.truncate(start_count);
        starts
    }

    /// The order lookups probe in: `budget` keys drawn at random from
    /// `pool`. On small maps every key is probed many times, in an order too
    /// long for the branch predictor to learn.
    fn probes<'pool>(&self, pool: &'pool [K]) -> Vec<&'pool K> {
        let mut rng = SeededRng::new(RandomStream::LookupOrder);
        (0..self.budget)
            .map(|_| &pool[rng.below(pool.len())])
            .collect()
    }

    fn time_builds<M: MapUnderTest<K, V>>(
        &self,
        operation: Operation,
        stopwatch: &mut Stopwatch,
    ) -> usize {
        for _ in 0..self.rounds() {
            let input = self.build_input(operation);
            let map: M = stopwatch.time(|| Self::build_map(operation, input));
            assert_eq!(map.entry_count(), self.entry_count());
        }
        self.rounds() * self.entry_count()
    }

    /// Returns how many lookups it timed and how many of them hit.
    fn time_lookups<M: MapUnderTest<K, V>>(
        &self,
        pool: &[K],
        stopwatch: &mut Stopwatch,
    ) -> (usize, usize) {
        let map: M = self.built_map();
        let probes = self.probes(pool);
        let (hits, _checksum) = stopwatch.time(|| {
            probes
                .iter()
                .fold((0, 0_u64), |(hits, sum), key| match map.get(key) {
                    Some(value) => (hits + 1, sum.wrapping_add(value.fingerprint())),
                    None => (hits, sum),
                })
        });
        (probes.len(), hits)
    }

    fn time_iteration<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) -> usize {
        let map: M = self.built_map();
        let passes = self.rounds();
        // `black_box` keeps the passes from being merged into one.
        stopwatch.time(|| {
            (0..passes).fold(0_u64, |sum, _| {
                sum.wrapping_add(black_box(&map).checksum_all())
            })
        });
        passes * self.entry_count()
    }

    fn time_range_scans<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) -> usize {
        let map: M = self.built_map();
        let starts = self.range_starts();
        let probes = self.probes(&starts);
        stopwatch.time(|| {
            probes.iter().fold(0_u64, |sum, start| {
                sum.wrapping_add(map.checksum_range(start, Operation::RANGE_SCAN_LENGTH))
            })
        });
        probes.len()
    }

    fn time_removals<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) -> usize {
        let mut rng = SeededRng::new(RandomStream::RemovalOrder);
        for _ in 0..self.rounds() {
            let mut map: M = self.built_map();
            let mut removal_order = self.present.clone();
            rng.shuffle(&mut removal_order);
            let removed = stopwatch.time(|| {
                (removal_order.iter())
                    .filter(|key| map.remove(key).is_some())
                    .count()
            });
            assert_eq!(removed, self.entry_count());
        }
        self.rounds() * self.entry_count()
    }

    fn time_upserts<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) -> usize {
        let mut rng = SeededRng::new(RandomStream::InsertionOrder);
        for _ in 0..self.rounds() {
            let mut upserted_keys: Vec<K> =
                self.present.iter().chain(&self.present).cloned().collect();
            rng.shuffle(&mut upserted_keys);
            let map: M = stopwatch.time(|| {
                let mut map = M::new();
                for key in upserted_keys {
                    map.bump_count(key);
                }
                map
            });
            assert_eq!(map.entry_count(), self.entry_count());
        }
        self.rounds() * 2 * self.entry_count()
    }

    fn time_pops<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) -> usize {
        for _ in 0..self.rounds() {
            let mut map: M = self.built_map();
            let (popped, _checksum) = stopwatch.time(|| {
                let (mut popped, mut sum) = (0, 0_u64);
                while let Some((key, value)) = map.pop_first() {
                    popped += 1;
                    sum = sum.wrapping_add(key.fingerprint() ^ value.fingerprint());
                }
                (popped, sum)
            });
            assert_eq!(popped, self.entry_count());
        }
        self.rounds() * self.entry_count()
    }
}

impl<K: BenchKey, V: BenchValue> PreparedCell<K, V> for SingleMapCell<K, V> {
    fn time<M: MapUnderTest<K, V>>(
        &mut self,
        operation: Operation,
        stopwatch: &mut Stopwatch,
    ) -> usize {
        match operation {
            Operation::InsertRandom | Operation::InsertSequential | Operation::BulkBuild => {
                self.time_builds::<M>(operation, stopwatch)
            }
            Operation::GetHit => {
                let pool = self.present_sample();
                let (lookups, hits) = self.time_lookups::<M>(&pool, stopwatch);
                assert_eq!(hits, lookups, "every get_hit key is in the map");
                lookups
            }
            Operation::GetMiss => {
                let pool = self.absent_sample();
                let (lookups, hits) = self.time_lookups::<M>(&pool, stopwatch);
                assert_eq!(hits, 0, "no get_miss key is in the map");
                lookups
            }
            Operation::Iterate => self.time_iteration::<M>(stopwatch),
            Operation::Range64 => self.time_range_scans::<M>(stopwatch),
            Operation::RemoveRandom => self.time_removals::<M>(stopwatch),
            Operation::EntryUpsert => self.time_upserts::<M>(stopwatch),
            Operation::PopFirst => self.time_pops::<M>(stopwatch),
            Operation::BuildMany | Operation::GetMany => {
                unreachable!("{} runs on many maps", operation.name())
            }
        }
    }

    fn build_input(&self, operation: Operation) -> Vec<(K, V)> {
        match operation {
            Operation::InsertRandom => self.entries_in_insertion_order(),
            Operation::InsertSequential | Operation::BulkBuild => self.entries_in_key_order(),
            _ => unreachable!("{} doesn't build a map", operation.name()),
        }
    }

    fn build<M: MapUnderTest<K, V>>(&self, operation: Operation, input: Vec<(K, V)>) -> Vec<M> {
        vec![Self::build_map(operation, input)]
    }
}
