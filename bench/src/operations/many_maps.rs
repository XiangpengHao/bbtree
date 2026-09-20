//! Operations on many tiny maps, all alive at once, as JSON objects are
//! after parsing. The cost that matters is per map: its memory, its
//! construction, and the pointer chase into it.

use super::{Operation, PreparedCell, Stopwatch};
use crate::command_line::Named;
use crate::keys::{BenchKey, KeyMaker};
use crate::map_adapters::MapUnderTest;
use crate::random::{RandomStream, SeededRng};
use crate::values::{BenchValue, ValueMaker};
use crate::workload::MapShape;

pub(crate) struct ManyMapsCell<K, V> {
    /// The keys every map draws its own from (`MapShape::VOCABULARY_SIZE`).
    /// Lookups probe with these, which stay in cache like the string
    /// literals a program looks fields up with.
    vocabulary: Vec<K>,
    /// Each map's keys as vocabulary indices, in insertion order, one map
    /// after another.
    key_picks: Vec<u16>,
    values: ValueMaker<V>,
    shape: MapShape,
}

impl<K: BenchKey, V: BenchValue> ManyMapsCell<K, V> {
    pub(crate) fn new(mut keys: KeyMaker<K>, values: ValueMaker<V>, shape: MapShape) -> Self {
        let vocabulary_size = u16::try_from(MapShape::VOCABULARY_SIZE).expect("picks fit in u16");
        let vocabulary = (0..u64::from(vocabulary_size))
            .map(|index| keys.key(2 * index))
            .collect();
        let mut rng = SeededRng::new(RandomStream::InsertionOrder);
        let mut indices: Vec<u16> = (0..vocabulary_size).collect();
        let mut key_picks = Vec::with_capacity(shape.total_entries());
        for _ in 0..shape.map_count() {
            rng.choose_to_front(&mut indices, shape.entries_per_map());
            key_picks.extend_from_slice(&indices[..shape.entries_per_map()]);
        }
        Self {
            vocabulary,
            key_picks,
            values,
            shape,
        }
    }

    fn keys_of_map(&self, map_index: usize) -> &[u16] {
        let entries_per_map = self.shape.entries_per_map();
        &self.key_picks[map_index * entries_per_map..][..entries_per_map]
    }

    fn vocabulary_key(&self, pick: u16) -> &K {
        &self.vocabulary[usize::from(pick)]
    }

    /// Fresh entries for every map, one map after another.
    fn entries(&self) -> Vec<(K, V)> {
        (self.key_picks.iter().enumerate())
            .map(|(index, &pick)| {
                (
                    self.vocabulary_key(pick).clone(),
                    self.values.value(index as u64),
                )
            })
            .collect()
    }

    fn build_maps<M: MapUnderTest<K, V>>(shape: MapShape, entries: Vec<(K, V)>) -> Vec<M> {
        let mut entries = entries.into_iter();
        (0..shape.map_count())
            .map(|_| {
                let mut map = M::new();
                for (key, value) in entries.by_ref().take(shape.entries_per_map()) {
                    map.insert(key, value);
                }
                map
            })
            .collect()
    }

    fn time_builds<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) {
        let input = self.entries();
        let maps: Vec<M> = stopwatch.time(|| Self::build_maps(self.shape, input));
        let entry_count: usize = maps.iter().map(M::entry_count).sum();
        assert_eq!(entry_count, self.shape.total_entries());
    }

    fn time_lookups<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) {
        let maps: Vec<M> = Self::build_maps(self.shape, self.entries());
        let mut map_order: Vec<usize> = (0..maps.len()).collect();
        SeededRng::new(RandomStream::MapOrder).shuffle(&mut map_order);
        let (hits, _checksum) = stopwatch.time(|| {
            let (mut hits, mut sum) = (0, 0_u64);
            for &map_index in &map_order {
                for &pick in self.keys_of_map(map_index) {
                    if let Some(value) = maps[map_index].get(self.vocabulary_key(pick)) {
                        hits += 1;
                        sum = sum.wrapping_add(value.fingerprint());
                    }
                }
            }
            (hits, sum)
        });
        assert_eq!(
            hits,
            self.shape.total_entries(),
            "every get_many key is in its map"
        );
    }

    fn time_iteration<M: MapUnderTest<K, V>>(&self, stopwatch: &mut Stopwatch) {
        let maps: Vec<M> = Self::build_maps(self.shape, self.entries());
        stopwatch
            .time(|| (maps.iter()).fold(0_u64, |sum, map| sum.wrapping_add(map.checksum_all())));
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
            Operation::GetMany => self.time_lookups::<M>(stopwatch),
            Operation::Iterate => self.time_iteration::<M>(stopwatch),
            _ => unreachable!("{} runs on one map", operation.name()),
        }
        self.shape.total_entries()
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
        Self::build_maps(self.shape, input)
    }
}
