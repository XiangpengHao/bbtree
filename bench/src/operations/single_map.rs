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
use crate::parallel_setup::ParallelSetup;
use crate::random::{RandomStream, SeededRng};
use crate::values::{BenchValue, ValueMaker};

/// A one-map cell's keys: those the map holds, and those that lookups that
/// miss probe with.
pub(crate) struct SingleMapKeys<K> {
    /// Distinct.
    present: Vec<K>,
    /// Distinct, none of them in `present`, and spread over its key range.
    absent: Vec<K>,
}

impl<K: BenchKey> SingleMapKeys<K> {
    pub(crate) fn new(present: Vec<K>, absent: Vec<K>) -> Self {
        Self { present, absent }
    }

    /// `entries` generated keys at even indices, and keys at odd indices for
    /// misses: as many as `miss_limit` or the entries, whichever is fewer,
    /// spread over the range.
    pub(crate) fn generated(mut keys: KeyMaker<K>, entries: usize, miss_limit: usize) -> Self {
        let present = (0..entries as u64)
            .map(|index| keys.key(2 * index))
            .collect();
        let miss_count = miss_limit.min(entries);
        let stride = entries / miss_count;
        let absent = (0..miss_count)
            .map(|position| keys.key(2 * (position * stride) as u64 + 1))
            .collect();
        Self { present, absent }
    }

    #[cfg(test)]
    pub(crate) fn present(&self) -> &[K] {
        &self.present
    }

    #[cfg(test)]
    pub(crate) fn absent(&self) -> &[K] {
        &self.absent
    }
}

pub(crate) struct SingleMapCell<K, V> {
    /// The map's keys, distinct, in random insertion order.
    present: Vec<K>,
    /// Keys that the map doesn't hold, which lookups that miss probe with.
    absent: Vec<K>,
    values: ValueMaker<V>,
    budget: usize,
}

impl<K: BenchKey, V: BenchValue> SingleMapCell<K, V> {
    pub(crate) fn new(keys: SingleMapKeys<K>, values: ValueMaker<V>, budget: usize) -> Self {
        let SingleMapKeys {
            mut present,
            absent,
        } = keys;
        SeededRng::new(RandomStream::InsertionOrder).shuffle(&mut present);
        Self {
            present,
            absent,
            values,
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
        ParallelSetup::map(&self.present, |index, key| {
            (key.clone(), self.values.value(index as u64))
        })
    }

    fn entries_in_key_order(&self) -> Vec<(K, V)> {
        let mut entries = self.entries_in_insertion_order();
        ParallelSetup::sort_unstable_by(&mut entries, |(left, _), (right, _)| left.cmp(right));
        entries
    }

    fn insert_all<M: MapUnderTest<K, V>>(entries: Vec<(K, V)>) -> M {
        let mut map = M::new();
        for (key, value) in entries {
            map.insert(key, value);
        }
        map
    }

    #[inline(never)]
    fn insert_batches<M: MapUnderTest<K, V>, const N: usize>(input: Vec<(K, V)>) -> M {
        let mut map = M::new();
        let mut entries = input.into_iter();
        while entries.len() >= N {
            let batch =
                core::array::from_fn::<_, N, _>(|_| entries.next().expect("a full batch remains"));
            map.insert_batch(batch);
        }
        for entry in entries {
            map.insert_batch([entry]);
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
        let sample: Vec<&K> = self.present.iter().step_by(stride).take(count).collect();
        ParallelSetup::map(&sample, |_, &key| key.clone())
    }

    /// The keys range scans may start at: those with at least
    /// `RANGE_SCAN_LENGTH` − 1 keys after them, so that every scan visits
    /// the full length.
    fn range_starts(&self) -> Vec<K> {
        let mut starts = ParallelSetup::cloned(&self.present);
        ParallelSetup::sort_unstable_by(&mut starts, K::cmp);
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

    /// The same map, probe order and checksum as individual lookups.
    #[inline(never)]
    fn time_batch_lookups<M: MapUnderTest<K, V>, const N: usize>(
        &self,
        hit: bool,
        stopwatch: &mut Stopwatch,
    ) -> usize {
        let sample = if hit {
            self.present_sample()
        } else {
            Vec::new()
        };
        // Match time_lookups' allocation order too: sample first, then
        // build the map, so mimalloc gives both paths comparable layouts.
        let map: M = self.built_map();
        let probes = self.probes(if hit { &sample } else { &self.absent });
        let (batches, remainder) = probes.as_chunks::<N>();
        let (hits, _checksum) = stopwatch.time(|| {
            let mut hits = 0;
            let mut checksum = 0_u64;
            for keys in batches {
                for value in map.get_batch(*keys).into_iter().flatten() {
                    hits += 1;
                    checksum = checksum.wrapping_add(value.fingerprint());
                }
            }
            for key in remainder {
                if let Some(value) = map.get_batch([*key])[0] {
                    hits += 1;
                    checksum = checksum.wrapping_add(value.fingerprint());
                }
            }
            (hits, checksum)
        });
        assert_eq!(hits, if hit { probes.len() } else { 0 });
        probes.len()
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
            let mut removal_order = ParallelSetup::cloned(&self.present);
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
            let mut upserted_keys = ParallelSetup::cloned(&self.present);
            upserted_keys.extend(ParallelSetup::cloned(&self.present));
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
            Operation::RandomInsert | Operation::SequentialInsert | Operation::BulkBuild => {
                self.time_builds::<M>(operation, stopwatch)
            }
            Operation::GetHit => {
                let pool = self.present_sample();
                let (lookups, hits) = self.time_lookups::<M>(&pool, stopwatch);
                assert_eq!(hits, lookups, "every get_hit key is in the map");
                lookups
            }
            Operation::GetMiss => {
                let (lookups, hits) = self.time_lookups::<M>(&self.absent, stopwatch);
                assert_eq!(hits, 0, "no get_miss key is in the map");
                lookups
            }
            Operation::Iterate => self.time_iteration::<M>(stopwatch),
            Operation::Range64 => self.time_range_scans::<M>(stopwatch),
            Operation::RandomRemove => self.time_removals::<M>(stopwatch),
            Operation::EntryUpsert => self.time_upserts::<M>(stopwatch),
            Operation::PopFirst => self.time_pops::<M>(stopwatch),
            _ => {
                unreachable!("{} runs on many maps", operation.name())
            }
        }
    }

    fn build_input(&self, operation: Operation) -> Vec<(K, V)> {
        match operation {
            Operation::RandomInsert => self.entries_in_insertion_order(),
            Operation::SequentialInsert | Operation::BulkBuild => self.entries_in_key_order(),
            _ => unreachable!("{} doesn't build a map", operation.name()),
        }
    }

    fn build<M: MapUnderTest<K, V>>(&self, operation: Operation, input: Vec<(K, V)>) -> Vec<M> {
        vec![Self::build_map(operation, input)]
    }
}

/// Separate dispatch keeps the new cells out of the individual-operation
/// runner: putting them in its match changed register allocation in the
/// removal and entry loops, invalidating instruction comparisons.
pub(crate) struct BatchCell<K, V> {
    individual: SingleMapCell<K, V>,
}

impl<K, V> From<SingleMapCell<K, V>> for BatchCell<K, V> {
    fn from(individual: SingleMapCell<K, V>) -> Self {
        Self { individual }
    }
}

impl<K: BenchKey, V: BenchValue> BatchCell<K, V> {
    fn build_map<M: MapUnderTest<K, V>>(operation: Operation, input: Vec<(K, V)>) -> M {
        match operation {
            Operation::BatchRandomInsert | Operation::BatchSequentialInsert => {
                SingleMapCell::insert_batches::<M, 16>(input)
            }
            Operation::ExtendRandom | Operation::ExtendSequential => {
                let mut map = M::new();
                map.extend_entries(input.into_iter());
                map
            }
            _ => unreachable!("{} doesn't build a batch", operation.name()),
        }
    }
}

impl<K: BenchKey, V: BenchValue> PreparedCell<K, V> for BatchCell<K, V> {
    fn time<M: MapUnderTest<K, V>>(
        &mut self,
        operation: Operation,
        stopwatch: &mut Stopwatch,
    ) -> usize {
        match operation {
            Operation::BatchGetHit32 => {
                self.individual.time_batch_lookups::<M, 32>(true, stopwatch)
            }
            Operation::BatchGetMiss32 => self
                .individual
                .time_batch_lookups::<M, 32>(false, stopwatch),
            Operation::BatchGetHit64 => {
                self.individual.time_batch_lookups::<M, 64>(true, stopwatch)
            }
            Operation::BatchGetMiss64 => self
                .individual
                .time_batch_lookups::<M, 64>(false, stopwatch),
            _ => {
                for _ in 0..self.individual.rounds() {
                    let input = self.build_input(operation);
                    let map: M = stopwatch.time(|| Self::build_map(operation, input));
                    assert_eq!(map.entry_count(), self.individual.entry_count());
                }
                self.individual.rounds() * self.individual.entry_count()
            }
        }
    }

    fn build_input(&self, operation: Operation) -> Vec<(K, V)> {
        match operation {
            Operation::BatchRandomInsert | Operation::ExtendRandom => {
                self.individual.entries_in_insertion_order()
            }
            Operation::BatchSequentialInsert | Operation::ExtendSequential => {
                self.individual.entries_in_key_order()
            }
            _ => unreachable!("{} doesn't build a batch", operation.name()),
        }
    }

    fn build<M: MapUnderTest<K, V>>(&self, operation: Operation, input: Vec<(K, V)>) -> Vec<M> {
        vec![Self::build_map(operation, input)]
    }
}
