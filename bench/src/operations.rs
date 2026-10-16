//! The operations of doc/02-field_study.md §6, on one map
//! (`single_map.rs`) or on many tiny maps (`many_maps.rs`), and the
//! stopwatch that times them.

mod many_maps;
mod single_map;

use std::hint::black_box;
use std::time::{Duration, Instant};

use crate::command_line::Named;
use crate::keys::BenchKey;
use crate::map_adapters::MapUnderTest;
use crate::perf_counters::PerfControl;
use crate::values::BenchValue;
use crate::workload::MapSize;

pub(crate) use many_maps::{ManyMapsCell, ManyMapsKeys, PicksPerMap};
pub(crate) use single_map::{BatchCell, SingleMapCell, SingleMapKeys};

/// What a cell does. Time is reported per operation, which is one call of
/// the map's API unless noted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Operation {
    /// Inserts every key into an empty map, in random order.
    RandomInsert,
    /// Inserts every key into an empty map, in ascending order.
    SequentialInsert,
    /// Looks up keys that are in the map.
    GetHit,
    /// Looks up keys that aren't in the map, which fall between keys that are.
    GetMiss,
    /// Visits every entry, in key order for ordered maps. Per entry.
    Iterate,
    /// Visits 64 entries in key order from a random key in the map. Per scan.
    Range64,
    /// Removes every key, in random order.
    RandomRemove,
    /// The counter-increment pattern, `*map.entry(key).or_default() += 1`,
    /// for every key twice in random order: half inserts, half updates.
    EntryUpsert,
    /// Pops every entry, smallest first: a drain, as timers and queues do.
    PopFirst,
    /// `FromIterator` from entries sorted by key. Per entry.
    BulkBuild,
    /// Builds many tiny maps, all kept alive, inserting each map's keys in
    /// its own order: random for generated maps, document order for JSON
    /// objects. Per insert.
    BuildMany,
    /// Looks up every key of every tiny map, visiting the maps in random
    /// order.
    GetMany,
    /// Looks up, in every tiny map, as many keys that it lacks as it holds,
    /// visiting the maps in random order. The keys are the other maps', so
    /// they mix like those of `GetMany`.
    GetManyMiss,
    /// `RandomInsert` through `insert_many`, 16 entries a call. Per entry.
    BatchRandomInsert,
    /// `SequentialInsert` through `insert_many`, 16 entries a call. Per
    /// entry.
    BatchSequentialInsert,
    /// `RandomInsert` through one `extend`. Per entry.
    ExtendRandom,
    /// `SequentialInsert` through one `extend`. Per entry.
    ExtendSequential,
    /// `GetHit` through `get_many`, 32 keys a call. Per key.
    BatchGetHit32,
    /// `GetMiss` through `get_many`, 32 keys a call. Per key.
    BatchGetMiss32,
    /// `GetHit` through `get_many`, 64 keys a call. Per key.
    BatchGetHit64,
    /// `GetMiss` through `get_many`, 64 keys a call. Per key.
    BatchGetMiss64,
}

impl Named for Operation {
    const ALL: &'static [Self] = &[
        Self::RandomInsert,
        Self::BatchRandomInsert,
        Self::BatchSequentialInsert,
        Self::ExtendRandom,
        Self::ExtendSequential,
        Self::BatchGetHit32,
        Self::BatchGetMiss32,
        Self::BatchGetHit64,
        Self::BatchGetMiss64,
        Self::SequentialInsert,
        Self::GetHit,
        Self::GetMiss,
        Self::Iterate,
        Self::Range64,
        Self::RandomRemove,
        Self::EntryUpsert,
        Self::PopFirst,
        Self::BulkBuild,
        Self::BuildMany,
        Self::GetMany,
        Self::GetManyMiss,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::RandomInsert => "random_insert",
            Self::BatchRandomInsert => "batch_random_insert",
            Self::BatchSequentialInsert => "batch_sequential_insert",
            Self::ExtendRandom => "extend_random",
            Self::ExtendSequential => "extend_sequential",
            Self::BatchGetHit32 => "batch_get_hit32",
            Self::BatchGetMiss32 => "batch_get_miss32",
            Self::BatchGetHit64 => "batch_get_hit64",
            Self::BatchGetMiss64 => "batch_get_miss64",

            Self::SequentialInsert => "sequential_insert",
            Self::GetHit => "get_hit",
            Self::GetMiss => "get_miss",
            Self::Iterate => "iterate",
            Self::Range64 => "range64",
            Self::RandomRemove => "random_remove",
            Self::EntryUpsert => "entry_upsert",
            Self::PopFirst => "pop_first",
            Self::BulkBuild => "bulk_build",
            Self::BuildMany => "build_many",
            Self::GetMany => "get_many",
            Self::GetManyMiss => "get_many_miss",
        }
    }
}

impl Operation {
    /// The operations on one map, all of which the headline suite runs.
    pub(crate) const ON_ONE_MAP: &'static [Self] = &[
        Self::RandomInsert,
        Self::SequentialInsert,
        Self::GetHit,
        Self::GetMiss,
        Self::Iterate,
        Self::Range64,
        Self::RandomRemove,
        Self::EntryUpsert,
        Self::PopFirst,
        Self::BulkBuild,
    ];

    /// Batches, and the single-key operations they compare with.
    pub(crate) const BATCH: &'static [Self] = &[
        Self::GetHit,
        Self::GetMiss,
        Self::RandomInsert,
        Self::SequentialInsert,
        Self::BatchRandomInsert,
        Self::BatchSequentialInsert,
        Self::ExtendRandom,
        Self::ExtendSequential,
        Self::BatchGetHit32,
        Self::BatchGetMiss32,
        Self::BatchGetHit64,
        Self::BatchGetMiss64,
    ];

    /// The operations on many tiny maps: build them, then read them as a
    /// deserializer's caller or a serializer would.
    pub(crate) const ON_MANY_MAPS: &'static [Self] = &[
        Self::BuildMany,
        Self::GetMany,
        Self::GetManyMiss,
        Self::Iterate,
    ];

    const RANGE_SCAN_LENGTH: usize = 64;

    pub(crate) fn applies_to(self, size: MapSize) -> bool {
        let operations = if size.has_many_maps() {
            Self::ON_MANY_MAPS
        } else {
            Self::ON_ONE_MAP
        };
        operations.contains(&self) || (!size.has_many_maps() && Self::BATCH.contains(&self))
    }

    /// Whether the operation calls a batch API, which `BatchCell` times.
    pub(crate) fn is_batch(self) -> bool {
        matches!(
            self,
            Self::BatchRandomInsert
                | Self::BatchSequentialInsert
                | Self::ExtendRandom
                | Self::ExtendSequential
                | Self::BatchGetHit32
                | Self::BatchGetMiss32
                | Self::BatchGetHit64
                | Self::BatchGetMiss64
        )
    }

    /// Whether the operation builds maps from inputs made beforehand: the
    /// operations whose maps memory mode measures.
    pub(crate) fn builds_maps(self) -> bool {
        matches!(
            self,
            Self::RandomInsert
                | Self::SequentialInsert
                | Self::BulkBuild
                | Self::BuildMany
                | Self::BatchRandomInsert
                | Self::BatchSequentialInsert
                | Self::ExtendRandom
                | Self::ExtendSequential
        )
    }
}

/// A cell's inputs, generated before anything is measured, and the
/// operations that run on them.
pub(crate) trait PreparedCell<K: BenchKey, V: BenchValue> {
    /// Times `operation` on maps of type `M`; returns how many operations
    /// it timed.
    fn time<M: MapUnderTest<K, V>>(
        &mut self,
        operation: Operation,
        stopwatch: &mut Stopwatch,
    ) -> usize;

    /// The entries that build `operation` consumes, in its order.
    fn build_input(&self, operation: Operation) -> Vec<(K, V)>;

    /// Builds the maps from `input` as build `operation` does.
    fn build<M: MapUnderTest<K, V>>(&self, operation: Operation, input: Vec<(K, V)>) -> Vec<M>;
}

/// Adds up the time of a cell's timed regions. Under `--perf`, it also
/// switches perf's counters on and off around each region, so the counters
/// cover the same work as the time.
pub(crate) struct Stopwatch {
    elapsed: Duration,
    perf_control: Option<PerfControl>,
}

impl Stopwatch {
    pub(crate) fn new(perf_control: Option<PerfControl>) -> Self {
        Self {
            elapsed: Duration::ZERO,
            perf_control,
        }
    }

    /// Runs `region` timed. Its result goes through `black_box`, so the
    /// work can't be optimized away; dropping the result is not timed.
    pub(crate) fn time<R>(&mut self, region: impl FnOnce() -> R) -> R {
        if let Some(perf_control) = &mut self.perf_control {
            perf_control.start_counting();
        }
        let start = Instant::now();
        let result = black_box(region());
        let elapsed = start.elapsed();
        if let Some(perf_control) = &mut self.perf_control {
            perf_control.stop_counting();
        }
        self.elapsed += elapsed;
        result
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.elapsed
    }
}
