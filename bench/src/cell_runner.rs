//! The child side of the suite runner: measures one cell in this process and
//! reports it on stdout, as one line of `name=value` fields.
//!
//! The cell's kinds are runtime values; measuring needs them as types. The
//! dispatch goes key type, then value type, then map type, and each step's
//! `match` is where a kind maps to its type and its generator.

use std::collections::{BTreeMap, HashMap};

use bbtree::BBTreeMap;

use crate::command_line::{CellRequest, Named};
use crate::keys::{BenchKey, KeyKind, KeyMaker};
use crate::map_adapters::{MapKind, MapUnderTest};
use crate::memory::{ResidentSet, RunMode};
use crate::operations::{ManyMapsCell, PreparedCell, SingleMapCell, Stopwatch};
use crate::perf_counters::PerfControl;
use crate::values::{BenchValue, ValueKind, ValueMaker};

/// What a child measured.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CellMeasurement {
    Timing {
        operations: u64,
        elapsed_ns: u64,
        peak_rss_bytes: u64,
    },
    Memory {
        entries: u64,
        /// Everything the maps hold on the heap, keys' and values' own
        /// allocations included, plus the maps' inline headers.
        map_bytes: u64,
        /// How much the resident set grew while the maps were built.
        rss_growth_bytes: u64,
        peak_rss_bytes: u64,
    },
}

impl CellMeasurement {
    fn to_line(self) -> String {
        match self {
            Self::Timing {
                operations,
                elapsed_ns,
                peak_rss_bytes,
            } => format!(
                "operations={operations} elapsed_ns={elapsed_ns} peak_rss_bytes={peak_rss_bytes}"
            ),
            Self::Memory {
                entries,
                map_bytes,
                rss_growth_bytes,
                peak_rss_bytes,
            } => format!(
                "entries={entries} map_bytes={map_bytes} rss_growth_bytes={rss_growth_bytes} \
                 peak_rss_bytes={peak_rss_bytes}"
            ),
        }
    }

    pub(crate) fn parse(line: &str) -> Result<Self, String> {
        let fields: HashMap<&str, u64> = (line.split_whitespace())
            .map(|field| {
                let (name, value) = field.split_once('=')?;
                Some((name, value.parse().ok()?))
            })
            .collect::<Option<_>>()
            .ok_or_else(|| format!("malformed measurement `{line}`"))?;
        let field = |name| {
            (fields.get(name).copied()).ok_or_else(|| format!("no {name} in measurement `{line}`"))
        };
        if fields.contains_key("operations") {
            Ok(Self::Timing {
                operations: field("operations")?,
                elapsed_ns: field("elapsed_ns")?,
                peak_rss_bytes: field("peak_rss_bytes")?,
            })
        } else {
            Ok(Self::Memory {
                entries: field("entries")?,
                map_bytes: field("map_bytes")?,
                rss_growth_bytes: field("rss_growth_bytes")?,
                peak_rss_bytes: field("peak_rss_bytes")?,
            })
        }
    }

    /// The number summaries report: nanoseconds per operation, or bytes
    /// per entry.
    pub(crate) fn summary_value(self) -> f64 {
        match self {
            Self::Timing {
                operations,
                elapsed_ns,
                ..
            } => elapsed_ns as f64 / operations as f64,
            Self::Memory {
                entries, map_bytes, ..
            } => map_bytes as f64 / entries as f64,
        }
    }
}

/// One cell, measured in this process.
pub(crate) struct CellRun {
    request: CellRequest,
    mode: RunMode,
}

impl CellRun {
    pub(crate) fn new(request: CellRequest, mode: RunMode) -> Self {
        Self { request, mode }
    }

    pub(crate) fn measure_and_report(self) -> Result<(), String> {
        let cell = self.request.cell();
        let operation = cell.workload().operation();
        if !cell.is_applicable() {
            return Err(format!("{} lacks {}", cell.map().name(), operation.name()));
        }
        if matches!(self.mode, RunMode::Memory(_)) && !operation.builds_maps() {
            return Err(format!(
                "memory mode measures builds, not {}",
                operation.name()
            ));
        }
        let measurement = self.with_key_type()?;
        println!("{}", measurement.to_line());
        Ok(())
    }

    fn with_key_type(&self) -> Result<CellMeasurement, String> {
        let workload = self.request.cell().workload();
        match workload.key() {
            KeyKind::U64Random => self.with_value_type(KeyMaker::random_u64()),
            KeyKind::U64Sequential => self.with_value_type(KeyMaker::sequential_u64()),
            KeyKind::U32 => self.with_value_type(KeyMaker::random_u32()),
            KeyKind::JsonString => self.with_value_type(KeyMaker::json_keys()),
            KeyKind::CrateString => self.with_value_type(KeyMaker::crate_versions()),
            KeyKind::PathString => {
                let shape = workload.size().shape(self.request.scale());
                self.with_value_type(KeyMaker::registry_paths(shape.distinct_keys()))
            }
            KeyKind::Hash32 => self.with_value_type(KeyMaker::random_hashes()),
            KeyKind::Memtable16 => self.with_value_type(KeyMaker::memtable(16)),
            KeyKind::Memtable24 => self.with_value_type(KeyMaker::memtable(24)),
        }
    }

    fn with_value_type<K: BenchKey>(&self, keys: KeyMaker<K>) -> Result<CellMeasurement, String> {
        match self.request.cell().workload().value() {
            ValueKind::Unit => self.with_map_type(keys, ValueMaker::unit()),
            ValueKind::U64 => self.with_map_type(keys, ValueMaker::counters()),
            ValueKind::Struct32 => self.with_map_type(keys, ValueMaker::structs()),
            ValueKind::String => self.with_map_type(keys, ValueMaker::short_strings()),
            ValueKind::Bytes100 => self.with_map_type(keys, ValueMaker::byte_records(100)),
            ValueKind::Bytes1k => self.with_map_type(keys, ValueMaker::byte_records(1024)),
        }
    }

    fn with_map_type<K: BenchKey, V: BenchValue>(
        &self,
        keys: KeyMaker<K>,
        values: ValueMaker<V>,
    ) -> Result<CellMeasurement, String> {
        match self.request.cell().map() {
            MapKind::BBTree => self.with_prepared_cell::<BBTreeMap<K, V>, K, V>(keys, values),
            MapKind::StdBTree => self.with_prepared_cell::<BTreeMap<K, V>, K, V>(keys, values),
            MapKind::Hashbrown => {
                self.with_prepared_cell::<hashbrown::HashMap<K, V>, K, V>(keys, values)
            }
            MapKind::StdHash => self.with_prepared_cell::<HashMap<K, V>, K, V>(keys, values),
        }
    }

    /// Generates the cell's inputs, before anything is measured.
    fn with_prepared_cell<M, K, V>(
        &self,
        keys: KeyMaker<K>,
        values: ValueMaker<V>,
    ) -> Result<CellMeasurement, String>
    where
        M: MapUnderTest<K, V>,
        K: BenchKey,
        V: BenchValue,
    {
        let scale = self.request.scale();
        let size = self.request.cell().workload().size();
        let shape = size.shape(scale);
        if size.has_many_maps() {
            self.measure::<M, K, V>(ManyMapsCell::new(keys, values, shape))
        } else {
            let entries = shape.entries_per_map();
            let budget = scale.operation_budget();
            self.measure::<M, K, V>(SingleMapCell::new(keys, values, entries, budget))
        }
    }

    fn measure<M, K, V>(
        &self,
        mut prepared: impl PreparedCell<K, V>,
    ) -> Result<CellMeasurement, String>
    where
        M: MapUnderTest<K, V>,
        K: BenchKey,
        V: BenchValue,
    {
        let operation = self.request.cell().workload().operation();
        match self.mode {
            RunMode::Timing => {
                let perf_control = (self.request.perf_control())
                    .map(PerfControl::open)
                    .transpose()?;
                let mut stopwatch = Stopwatch::new(perf_control);
                let operations = prepared.time::<M>(operation, &mut stopwatch);
                Ok(CellMeasurement::Timing {
                    operations: operations as u64,
                    elapsed_ns: u64::try_from(stopwatch.elapsed().as_nanos())
                        .expect("under 584 years"),
                    peak_rss_bytes: ResidentSet::read().peak_bytes(),
                })
            }
            RunMode::Memory(allocator) => {
                let input = prepared.build_input(operation);
                let entries = input.len() as u64;
                let rss_before = ResidentSet::read().current_bytes();
                let maps: Vec<M> = prepared.build(operation, input);
                let rss_after = ResidentSet::read();
                Ok(CellMeasurement::Memory {
                    entries,
                    map_bytes: allocator.bytes_freed_by_dropping(maps) as u64,
                    rss_growth_bytes: rss_after.current_bytes().saturating_sub(rss_before),
                    peak_rss_bytes: rss_after.peak_bytes(),
                })
            }
        }
    }
}
