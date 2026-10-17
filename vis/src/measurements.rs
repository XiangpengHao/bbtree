//! The post's measurements, as `bench/tools/blog-runs.sh` took them on a
//! machine and `bench/tools/blog-collect.sh` merged them: compiled in from
//! the CSVs, so a new run is a rebuild. Each map keeps one name and one
//! color across every chart.

/// A map the bench measured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MeasuredMap {
    /// bbtree's `BBTreeMap`, which threads share: the map the post is about,
    /// on one thread and as threads are added.
    BBTree,
    StdBTree,
    /// std's `BTreeMap` behind a `RwLock`.
    StdBTreeRwLock,
    SkipMap,
    SccTreeIndex,
    ConcurrentMap,
    /// std's hash map, which keeps no order: the speed of a lookup that
    /// needn't be ordered.
    StdHashMap,
}

impl MeasuredMap {
    /// The maps of one thread, in the order the charts draw them: the
    /// ordered ones (the concurrent ones used from one thread), then std's
    /// hash map apart.
    pub(crate) const SINGLE_THREAD: [Self; 6] = [
        Self::BBTree,
        Self::StdBTree,
        Self::SccTreeIndex,
        Self::ConcurrentMap,
        Self::SkipMap,
        Self::StdHashMap,
    ];

    /// The maps threads share, in the order the charts draw them.
    pub(crate) const CONCURRENT: [Self; 5] = [
        Self::BBTree,
        Self::StdBTreeRwLock,
        Self::SccTreeIndex,
        Self::ConcurrentMap,
        Self::SkipMap,
    ];

    /// The maps whose memory the post shows: a `RwLock` adds nothing to
    /// std's.
    pub(crate) const MEMORY: [Self; 5] = [
        Self::BBTree,
        Self::StdBTree,
        Self::SkipMap,
        Self::SccTreeIndex,
        Self::ConcurrentMap,
    ];

    /// The map the bench names `name`.
    fn from_bench_name(name: &str) -> Option<Self> {
        Some(match name {
            "bbtree" => Self::BBTree,
            "std-btree" => Self::StdBTree,
            "std-btree-rwlock" => Self::StdBTreeRwLock,
            "crossbeam-skipmap" => Self::SkipMap,
            "scc-treeindex" => Self::SccTreeIndex,
            "concurrent-map" => Self::ConcurrentMap,
            "std-hash" => Self::StdHashMap,
            _ => return None,
        })
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::BBTree => "bbtree",
            Self::StdBTree => "std BTreeMap",
            Self::StdBTreeRwLock => "RwLock<BTreeMap>",
            Self::SkipMap => "crossbeam SkipMap",
            Self::SccTreeIndex => "scc TreeIndex",
            Self::ConcurrentMap => "concurrent-map",
            Self::StdHashMap => "std HashMap",
        }
    }

    /// The fill of the map's dots and bars, a CSS variable, one a map
    /// whatever the chart shows: bbtree in the site's highlighter,
    /// every other a dark, desaturated slate, apart by lightness, so that
    /// the story's series stands out.
    pub(crate) fn fill_color(self) -> &'static str {
        match self {
            Self::BBTree => "var(--ours)",
            _ => self.line_color(),
        }
    }

    /// The color of the map's lines and rings: its fill, but for the
    /// highlighter's, too light for a thin line, whose lines are its dark
    /// gold.
    pub(crate) fn line_color(self) -> &'static str {
        match self {
            Self::BBTree => "var(--ours-line)",
            Self::StdBTree | Self::StdBTreeRwLock | Self::StdHashMap => "var(--theirs-1)",
            Self::SccTreeIndex => "var(--theirs-2)",
            Self::ConcurrentMap => "var(--theirs-3)",
            Self::SkipMap => "var(--theirs-4)",
        }
    }

    /// Its name and where its code lives, for the maps outside std.
    pub(crate) fn repository_link(self) -> Option<(String, &'static str)> {
        let url = match self {
            Self::BBTree => crate::site::REPOSITORY,
            Self::SkipMap => "https://github.com/crossbeam-rs/crossbeam",
            Self::SccTreeIndex => "https://codeberg.org/wvwwvwwv/scalable-concurrent-containers",
            Self::ConcurrentMap => "https://github.com/komora-io/concurrent-map",
            Self::StdBTree | Self::StdBTreeRwLock | Self::StdHashMap => return None,
        };
        Some((self.label().to_string(), url))
    }

    /// Whether it is bbtree: the series the story is about.
    pub(crate) fn is_ours(self) -> bool {
        self == Self::BBTree
    }

    /// Whether it keeps its keys in order, to scan ranges of them: all but
    /// the hash map.
    pub(crate) fn is_ordered(self) -> bool {
        self != Self::StdHashMap
    }
}

/// The keys a measurement used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MeasuredKeys {
    U64,
    Uuid,
    WikiTitles,
    JsonKeys,
}

impl MeasuredKeys {
    pub(crate) const ALL: [Self; 4] = [Self::U64, Self::Uuid, Self::WikiTitles, Self::JsonKeys];

    fn from_bench_name(name: &str) -> Option<Self> {
        Some(match name {
            "u64" => Self::U64,
            "uuid" => Self::Uuid,
            "wiki-titles" => Self::WikiTitles,
            "str-json" => Self::JsonKeys,
            _ => return None,
        })
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::U64 => "u64",
            Self::Uuid => "UUID",
            Self::WikiTitles => "Wikipedia titles",
            Self::JsonKeys => "JSON keys",
        }
    }

    /// What the keys are, in a few words.
    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::U64 => "random 64-bit integers",
            Self::Uuid => "random version-4 UUIDs, [u8; 16]",
            Self::WikiTitles => "English Wikipedia's article titles, 21 bytes on average",
            Self::JsonKeys => "short identifiers, as JSON objects' keys",
        }
    }
}

/// What a measurement timed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MeasuredOperation {
    /// Lookups, half of which hit and half miss.
    Get,
    /// The same, 64 keys a call, through the batch API where a map has one.
    GetBatch,
    Insert,
    /// Inserts 16 entries a call, through the batch API where a map has one.
    InsertBatch,
    /// Adds one to the value of a key the map holds, in place.
    Update,
    /// 90% lookups, 10% updates of their values.
    Mixed,
    /// 100 entries in order from a key.
    Scan,
    /// 90% scans of 100 entries in order from a key, 10% updates of a
    /// key's value.
    ScanMixed,
}

impl MeasuredOperation {
    /// The same operation through bbtree's batch API
    /// (`get_many_copied`, `insert_many`), if it has one.
    pub(crate) fn batched(self) -> Option<Self> {
        match self {
            Self::Get => Some(Self::GetBatch),
            Self::Insert => Some(Self::InsertBatch),
            _ => None,
        }
    }

    fn from_bench_name(name: &str) -> Option<Self> {
        Some(match name {
            "get-half" => Self::Get,
            "get-half-batch" => Self::GetBatch,
            "insert" => Self::Insert,
            "insert-batch" => Self::InsertBatch,
            "update" => Self::Update,
            "mixed" => Self::Mixed,
            "scan" => Self::Scan,
            "scan-mixed" => Self::ScanMixed,
            _ => return None,
        })
    }
}

/// Which of the post's runs a throughput came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Run {
    /// One thread, pinned.
    SingleThread,
    /// Threads added.
    Concurrent,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct ThroughputRow {
    run: Run,
    map: MeasuredMap,
    keys: MeasuredKeys,
    operation: MeasuredOperation,
    threads: u32,
    mops: f64,
}

/// What a map held of the heap for its entries, its keys' own allocations
/// included: once built, and at the most while it was built.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct MapMemory {
    pub(crate) built_bytes: f64,
    pub(crate) peak_bytes: f64,
}

impl MapMemory {
    /// Whether it held visibly more at its peak than once built.
    pub(crate) fn peaked(&self) -> bool {
        self.peak_bytes > self.built_bytes * 1.005
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct MemoryRow {
    map: MeasuredMap,
    keys: MeasuredKeys,
    memory: MapMemory,
}

/// One workload of a real system, before and after one of its maps became
/// bbtree's: the system's own benchmark on one thread, run by an
/// integration in `~/coding/bbtree-integrations` (outside the repository).
/// The CSV keeps the thread count it ran at; the post shows one thread.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct IntegrationRow {
    pub(crate) system: String,
    /// The cluster it joins: lookup, insert, a mix.
    pub(crate) workload: String,
    /// What exactly ran, in a few words.
    pub(crate) detail: String,
    /// The map the system ships, and its nanoseconds per operation.
    pub(crate) before: String,
    pub(crate) before_nanos: f64,
    /// bbtree's map in its place, and its nanoseconds per operation.
    pub(crate) after: String,
    pub(crate) after_nanos: f64,
}

impl IntegrationRow {
    /// How many times faster the system ran with bbtree's map.
    pub(crate) fn speedup(&self) -> f64 {
        self.before_nanos / self.after_nanos
    }

    /// Where the system's code lives.
    pub(crate) fn repository(&self) -> Option<&'static str> {
        match self.system.as_str() {
            "fjall" => Some("https://github.com/fjall-rs/fjall"),
            "Turso" => Some("https://github.com/tursodatabase/turso"),
            "SlateDB" => Some("https://github.com/slatedb/slatedb"),
            "Tonbo" => Some("https://github.com/tonbo-io/tonbo"),
            "Lance" => Some("https://github.com/lance-format/lance"),
            _ => None,
        }
    }
}

/// The least and the most of some speedups, shown as "1.9–4.7×".
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct SpeedupRange {
    least: f64,
    most: f64,
}

impl SpeedupRange {
    fn of(speedups: impl Iterator<Item = f64>) -> Option<Self> {
        speedups.fold(None, |range: Option<Self>, speedup| {
            Some(match range {
                Some(range) => Self {
                    least: range.least.min(speedup),
                    most: range.most.max(speedup),
                },
                None => Self {
                    least: speedup,
                    most: speedup,
                },
            })
        })
    }
}

impl std::fmt::Display for SpeedupRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.1}–{:.1}×", self.least, self.most)
    }
}

/// The machine a set of measurements came from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Machine {
    pub(crate) name: &'static str,
    pub(crate) detail: &'static str,
}

/// Every measurement of one machine.
#[derive(PartialEq, Debug)]
pub(crate) struct Measurements {
    machine: Machine,
    throughput: Vec<ThroughputRow>,
    memory: Vec<MemoryRow>,
    integrations: Vec<IntegrationRow>,
}

impl Measurements {
    /// The runs on the desktop the post was written on.
    pub(crate) fn of_ryzen_9900x() -> Self {
        Self::parse(
            Machine {
                name: "Ryzen 9 9900X",
                detail: "Zen 5, 12 cores, 24 threads, 2 × 32 MiB L3, DDR5, mimalloc",
            },
            include_str!("../data/9900x-20261017/throughput.csv"),
            include_str!("../data/9900x-20261017/memory.csv"),
            include_str!("../data/9900x-20261017/integrations.csv"),
        )
    }

    /// A machine's single-thread runs alone (`blog-runs.sh arch`), for
    /// the architectures card.
    pub(crate) fn of_single_thread(machine: Machine, throughput: &str) -> Self {
        Self::parse(machine, throughput, "", "")
    }

    fn parse(machine: Machine, throughput: &str, memory: &str, integrations: &str) -> Self {
        Self {
            machine,
            throughput: Self::rows(throughput, |fields| {
                Some(ThroughputRow {
                    run: match fields[0] {
                        "single-thread" => Run::SingleThread,
                        "concurrent" => Run::Concurrent,
                        _ => return None,
                    },
                    map: MeasuredMap::from_bench_name(fields[1])?,
                    keys: MeasuredKeys::from_bench_name(fields[2])?,
                    operation: MeasuredOperation::from_bench_name(fields[3])?,
                    threads: fields[4].parse().ok()?,
                    mops: fields[7].parse().ok()?,
                })
            }),
            memory: Self::rows(memory, |fields| {
                Some(MemoryRow {
                    map: MeasuredMap::from_bench_name(fields[0])?,
                    keys: MeasuredKeys::from_bench_name(fields[1])?,
                    memory: MapMemory {
                        built_bytes: fields[3].parse().ok()?,
                        peak_bytes: fields.get(7)?.parse().ok()?,
                    },
                })
            }),
            integrations: Self::rows(integrations, |fields| {
                // One thread only: with more, the systems are bound by their
                // own locks, not the map.
                if fields[3] != "1" {
                    return None;
                }
                Some(IntegrationRow {
                    system: fields[0].to_string(),
                    workload: fields[1].to_string(),
                    detail: fields[2].to_string(),
                    before: fields[4].to_string(),
                    before_nanos: fields[5].parse().ok()?,
                    after: fields[6].to_string(),
                    after_nanos: fields[7].parse().ok()?,
                })
            }),
        }
    }

    /// The rows of `csv` past its header that `row` reads; a row it can't
    /// read (a map or workload the post doesn't show) is left out.
    fn rows<T>(csv: &str, row: impl Fn(&[&str]) -> Option<T>) -> Vec<T> {
        (csv.lines().skip(1))
            .filter_map(|line| row(&line.split(',').collect::<Vec<_>>()))
            .collect()
    }

    pub(crate) fn machine(&self) -> Machine {
        self.machine
    }

    /// Nanoseconds per operation of `operation` on `map` with `keys`, back
    /// to back on one thread.
    pub(crate) fn single_thread_nanos(
        &self,
        map: MeasuredMap,
        keys: MeasuredKeys,
        operation: MeasuredOperation,
    ) -> Option<f64> {
        (self.throughput.iter())
            .find(|row| {
                row.run == Run::SingleThread
                    && row.map == map
                    && row.keys == keys
                    && row.operation == operation
            })
            .map(|row| 1e3 / row.mops)
    }

    /// Millions of operations a second of `operation` on `map` with
    /// `keys`, by thread count, threads added.
    pub(crate) fn concurrent_mops(
        &self,
        map: MeasuredMap,
        keys: MeasuredKeys,
        operation: MeasuredOperation,
    ) -> Vec<(u32, f64)> {
        (self.throughput.iter())
            .filter(|row| {
                row.run == Run::Concurrent
                    && row.map == map
                    && row.keys == keys
                    && row.operation == operation
            })
            .map(|row| (row.threads, row.mops))
            .collect()
    }

    /// What `map` held of the heap with `keys`.
    pub(crate) fn memory(&self, map: MeasuredMap, keys: MeasuredKeys) -> Option<MapMemory> {
        (self.memory.iter())
            .find(|row| row.map == map && row.keys == keys)
            .map(|row| row.memory)
    }

    /// How many times faster `faster` ran than `slower` on one thread,
    /// each a map and the operation it ran, the least and the most across
    /// the kinds of keys.
    pub(crate) fn single_thread_speedups(
        &self,
        faster: (MeasuredMap, MeasuredOperation),
        slower: (MeasuredMap, MeasuredOperation),
    ) -> Option<SpeedupRange> {
        SpeedupRange::of(MeasuredKeys::ALL.into_iter().filter_map(|keys| {
            Some(
                self.single_thread_nanos(slower.0, keys, slower.1)?
                    / self.single_thread_nanos(faster.0, keys, faster.1)?,
            )
        }))
    }

    /// How many times faster the systems ran with bbtree, the least and
    /// the most across their workloads.
    pub(crate) fn integration_speedups(&self) -> Option<SpeedupRange> {
        SpeedupRange::of(self.integrations.iter().map(IntegrationRow::speedup))
    }

    /// The systems measured before and after, in the order of their rows.
    pub(crate) fn integrated_systems(&self) -> Vec<String> {
        let mut systems: Vec<String> = Vec::new();
        for row in &self.integrations {
            if !systems.contains(&row.system) {
                systems.push(row.system.clone());
            }
        }
        systems
    }

    /// `system`'s workloads before and after.
    pub(crate) fn integration_rows(&self, system: &str) -> Vec<IntegrationRow> {
        (self.integrations.iter())
            .filter(|row| row.system == system)
            .cloned()
            .collect()
    }
}
