//! Operations on many tiny maps, all alive at once, as JSON objects are
//! after parsing. The cost that matters is per map: its memory, its
//! construction, and the pointer chase into it.

use super::{Operation, PreparedCell, Stopwatch};
use crate::command_line::Named;
use crate::keys::{BenchKey, KeyMaker};
use crate::map_adapters::MapUnderTest;
use crate::parallel_setup::ParallelSetup;
use crate::random::{RandomStream, SeededRng};
use crate::values::{BenchValue, ValueMaker};
use crate::workload::MapShape;

/// Vocabulary indices for each of many maps, one map after another.
#[derive(Default)]
pub(crate) struct PicksPerMap {
    picks: Vec<u16>,
    /// Where each map's picks end in `picks`.
    ends: Vec<usize>,
}

impl PicksPerMap {
    pub(crate) fn with_capacity(map_count: usize, pick_count: usize) -> Self {
        Self {
            picks: Vec::with_capacity(pick_count),
            ends: Vec::with_capacity(map_count),
        }
    }

    pub(crate) fn push_map(&mut self, picks: &[u16]) {
        self.picks.extend_from_slice(picks);
        self.ends.push(self.picks.len());
    }

    pub(crate) fn map_count(&self) -> usize {
        self.ends.len()
    }

    pub(crate) fn pick_count(&self) -> usize {
        self.picks.len()
    }

    /// Each map's picks, in map order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &[u16]> {
        let starts = std::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(&self.ends)
            .map(|(start, &end)| &self.picks[start..end])
    }

    /// How many times each index of a vocabulary of `vocabulary_size` keys
    /// is picked.
    pub(crate) fn pick_counts(&self, vocabulary_size: usize) -> Vec<usize> {
        let mut counts = vec![0; vocabulary_size];
        for &pick in &self.picks {
            counts[usize::from(pick)] += 1;
        }
        counts
    }
}

/// The keys of many tiny maps, as indices into a vocabulary that they
/// share, as JSON objects of one kind share field names.
pub(crate) struct ManyMapsKeys<K> {
    /// Lookups probe with these, which stay in cache like the string
    /// literals a program looks fields up with.
    vocabulary: Vec<K>,
    /// Each map's keys, distinct, in insertion order.
    maps: PicksPerMap,
}

impl<K: BenchKey> ManyMapsKeys<K> {
    pub(crate) fn new(vocabulary: Vec<K>, maps: PicksPerMap) -> Self {
        assert!(
            (maps.picks.iter()).all(|&pick| usize::from(pick) < vocabulary.len()),
            "every pick is in the vocabulary"
        );
        Self { vocabulary, maps }
    }

    /// `shape`'s maps, each of keys drawn at random from a vocabulary of
    /// `MapShape::VOCABULARY_SIZE` generated keys, in random order.
    pub(crate) fn generated(mut keys: KeyMaker<K>, shape: MapShape) -> Self {
        let vocabulary_size = u16::try_from(MapShape::VOCABULARY_SIZE).expect("picks fit in u16");
        let vocabulary = (0..u64::from(vocabulary_size))
            .map(|index| keys.key(2 * index))
            .collect();
        let mut rng = SeededRng::new(RandomStream::InsertionOrder);
        let mut indices: Vec<u16> = (0..vocabulary_size).collect();
        let entries = shape.map_count() * shape.entries_per_map();
        let mut maps = PicksPerMap::with_capacity(shape.map_count(), entries);
        for _ in 0..shape.map_count() {
            rng.choose_to_front(&mut indices, shape.entries_per_map());
            maps.push_map(&indices[..shape.entries_per_map()]);
        }
        Self::new(vocabulary, maps)
    }

    #[cfg(test)]
    pub(crate) fn map_count(&self) -> usize {
        self.maps.map_count()
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.maps.pick_count()
    }

    fn key(&self, pick: u16) -> &K {
        &self.vocabulary[usize::from(pick)]
    }

    /// For each map, as many vocabulary keys that it doesn't hold as it
    /// holds. Each is the key of an entry drawn at random from all maps,
    /// drawn again while this map holds it, so that misses probe the same
    /// mix of keys as hits do.
    fn absent_picks(&self) -> PicksPerMap {
        let occurring_keys = (self.maps.pick_counts(self.vocabulary.len()).iter())
            .filter(|&&count| count > 0)
            .count();
        let mut rng = SeededRng::new(RandomStream::AbsentKeys);
        let mut absent = PicksPerMap::default();
        let mut map_absent = Vec::new();
        for map_picks in self.maps.iter() {
            assert!(
                map_picks.len() < occurring_keys,
                "a map holds every key, so no key misses it"
            );
            map_absent.clear();
            while map_absent.len() < map_picks.len() {
                let pick = self.maps.picks[rng.below(self.maps.pick_count())];
                if !map_picks.contains(&pick) {
                    map_absent.push(pick);
                }
            }
            absent.push_map(&map_absent);
        }
        absent
    }
}

pub(crate) struct ManyMapsCell<K, V> {
    keys: ManyMapsKeys<K>,
    values: ValueMaker<V>,
}

impl<K: BenchKey, V: BenchValue> ManyMapsCell<K, V> {
    pub(crate) fn new(keys: ManyMapsKeys<K>, values: ValueMaker<V>) -> Self {
        Self { keys, values }
    }

    /// Fresh entries for every map, one map after another.
    fn entries(&self) -> Vec<(K, V)> {
        ParallelSetup::map(&self.keys.maps.picks, |index, &pick| {
            (self.keys.key(pick).clone(), self.values.value(index as u64))
        })
    }

    /// Builds each map from its share of `entries`, in their order.
    fn build_maps<M: MapUnderTest<K, V>>(&self, entries: Vec<(K, V)>) -> Vec<M> {
        let mut entries = entries.into_iter();
        (self.keys.maps.iter())
            .map(|map_picks| {
                let mut map = M::new();
                for (key, value) in entries.by_ref().take(map_picks.len()) {
                    map.insert(key, value);
                }
                map
            })
            .collect()
    }

    fn time_builds<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) -> usize {
        let input = self.entries();
        let maps: Vec<M> = stopwatch.time(|| self.build_maps(input));
        let entry_count: usize = maps.iter().map(M::entry_count).sum();
        assert_eq!(entry_count, self.keys.entry_count());
        entry_count
    }

    /// Looks up each map's `probes` in it, visiting the maps in random
    /// order. Returns how many lookups it timed and how many of them hit.
    fn time_lookups<M: MapUnderTest<K, V>>(
        &self,
        probes: &PicksPerMap,
        stopwatch: &mut Stopwatch,
    ) -> (usize, usize) {
        let maps: Vec<M> = self.build_maps(self.entries());
        let probes_of_maps: Vec<&[u16]> = probes.iter().collect();
        let mut map_order: Vec<usize> = (0..maps.len()).collect();
        SeededRng::new(RandomStream::MapOrder).shuffle(&mut map_order);
        // Each visit's map and probes, found beforehand, so that the timed
        // loop reads only them.
        let visits: Vec<(&M, &[u16])> = (map_order.iter())
            .map(|&map_index| (&maps[map_index], probes_of_maps[map_index]))
            .collect();
        let (hits, _checksum) = stopwatch.time(|| {
            let (mut hits, mut sum) = (0, 0_u64);
            for &(map, map_probes) in &visits {
                for &pick in map_probes {
                    if let Some(value) = map.get(self.keys.key(pick)) {
                        hits += 1;
                        sum = sum.wrapping_add(value.fingerprint());
                    }
                }
            }
            (hits, sum)
        });
        (probes.pick_count(), hits)
    }

    fn time_iteration<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) -> usize {
        let maps: Vec<M> = self.build_maps(self.entries());
        stopwatch
            .time(|| (maps.iter()).fold(0_u64, |sum, map| sum.wrapping_add(map.checksum_all())));
        self.keys.entry_count()
    }
}

impl<K: BenchKey, V: BenchValue> PreparedCell<K, V> for ManyMapsCell<K, V> {
    fn time<M: MapUnderTest<K, V>>(
        &mut self,
        operation: Operation,
        stopwatch: &mut Stopwatch,
    ) -> usize {
        match operation {
            Operation::BuildMany => self.time_builds::<M>(stopwatch),
            Operation::GetMany => {
                let (lookups, hits) = self.time_lookups::<M>(&self.keys.maps, stopwatch);
                assert_eq!(hits, lookups, "every get_many key is in its map");
                lookups
            }
            Operation::GetManyMiss => {
                let absent = self.keys.absent_picks();
                let (lookups, hits) = self.time_lookups::<M>(&absent, stopwatch);
                assert_eq!(hits, 0, "no get_many_miss key is in its map");
                lookups
            }
            Operation::Iterate => self.time_iteration::<M>(stopwatch),
            _ => unreachable!("{} runs on one map", operation.name()),
        }
    }

    fn build_input(&self, operation: Operation) -> Vec<(K, V)> {
        assert_eq!(
            operation,
            Operation::BuildMany,
            "only build_many builds many maps"
        );
        self.entries()
    }

    fn build<M: MapUnderTest<K, V>>(&self, _operation: Operation, input: Vec<(K, V)>) -> Vec<M> {
        self.build_maps(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{MapSize, Scale};

    #[test]
    fn misses_are_vocabulary_keys_that_the_map_lacks() {
        let shape = MapSize::ThirtyTwoEntryMaps
            .fixed_shape(Scale::Quick)
            .unwrap();
        let keys = ManyMapsKeys::generated(KeyMaker::random_u64(), shape);
        let absent = keys.absent_picks();
        assert_eq!(absent.map_count(), keys.map_count());
        for (map_picks, map_absent) in keys.maps.iter().zip(absent.iter()) {
            assert_eq!(map_absent.len(), map_picks.len());
            assert!(map_absent.iter().all(|pick| !map_picks.contains(pick)));
        }
    }
}
