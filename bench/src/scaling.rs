//! `bbtree-scaling`: maps that threads share (`concurrent_maps.rs`) as
//! threads are added: `BCTreeMap`, `BBTreeMap` behind a `RwLock`, and other
//! crates' concurrent maps (`--maps` picks some), with `u64`, `u128` or
//! string keys (`--keys`).
//!
//! Each worker is pinned to a CPU of its own, one per physical core first,
//! then their SMT siblings, in the order the process's affinity mask allows
//! them: run it under `taskset` to give it some of the machine. Workloads, on
//! maps of `--entries` keys to `u64`, built by inserts in random key order:
//!
//! - `get`: lookups of keys the map holds, for `--seconds`;
//! - `get-miss`: lookups of keys it doesn't, for `--seconds`;
//! - `get-batch`, `get-miss-batch`: the same, [`LOOKUP_BATCH`] keys a call,
//!   through the batch API of the maps that have one (`BCTreeMap`,
//!   `BBTreeMap`), separate lookups on the others;
//! - `mixed`: 90% lookups, 10% updates of their values, for `--seconds`;
//! - `scan`: scans of [`SCAN_LENGTH`] entries from keys the map holds, for
//!   `--seconds`, on the maps that keep order;
//! - `churn`: a key the map holds removed and inserted again, two
//!   operations, for `--seconds`;
//! - `insert`: the whole map inserted into an empty one, the keys split
//!   between the workers, timed to the last one.
//!
//! What it measures of them, by `--measure`:
//!
//! - `throughput`: millions of operations a second, all workers together;
//! - `latency`: each operation's time, timed alone, as quantiles: the
//!   lookups, hits and misses, the mix's lookups, the churn's removals and
//!   inserts together, and the inserts (`latency.rs`);
//! - `memory`, in the binary whose allocator counts live bytes
//!   (`bbtree-scaling-memory`): the bytes each map holds once built, its
//!   keys' included, and how much the process's resident set grew, at the
//!   end and at its peak.
//!
//! Every key is made before anything is timed ([`KeyKind`]), and each
//! worker gets its own list of keys to look up, in random order, which it
//! reads front to back: making or finding a key costs no map anything.

use std::fmt::Write as _;
use std::hint::black_box;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Barrier, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use bbtree::{BBTreeMap, BCTreeMap};
use concurrent_map::ConcurrentMap;
use crossbeam_skiplist::SkipMap;
use dashmap::DashMap;
use scc::TreeIndex;

use crate::concurrent_maps::{ArtMapOf, MapHandle, ScalingKey, SharedMap};
use crate::keys::KeyMaker;
use crate::latency::{LatencyHistogram, TickClock};
use crate::memory::{CountingAllocator, ResidentSet};
use crate::random::SeededRng;

/// How many entries a scan visits.
const SCAN_LENGTH: usize = 100;

/// How many keys a batched lookup takes: as many as bbtree's executor
/// tracks with one bit each.
const LOOKUP_BATCH: usize = 64;

/// How many keys each worker's lookup list holds, a power of two.
const QUERIES_PER_WORKER: usize = 1 << 18;

/// How many keys the map doesn't hold a run makes, at most, for misses.
const MAX_ABSENT_KEYS: usize = 1 << 20;

/// The workloads.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Workload {
    Get,
    GetMiss,
    GetBatch,
    GetMissBatch,
    Mixed,
    Scan,
    Churn,
    Insert,
}

impl Workload {
    const ALL: [Self; 8] = [
        Self::Get,
        Self::GetMiss,
        Self::GetBatch,
        Self::GetMissBatch,
        Self::Mixed,
        Self::Scan,
        Self::Churn,
        Self::Insert,
    ];

    fn named(name: &str) -> Result<Self, String> {
        (Self::ALL.into_iter())
            .find(|workload| workload.name() == name)
            .ok_or_else(|| {
                let names: Vec<_> = Self::ALL.iter().map(|workload| workload.name()).collect();
                format!("unknown workload {name}; workloads: {}", names.join(","))
            })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::GetMiss => "get-miss",
            Self::GetBatch => "get-batch",
            Self::GetMissBatch => "get-miss-batch",
            Self::Mixed => "mixed",
            Self::Scan => "scan",
            Self::Churn => "churn",
            Self::Insert => "insert",
        }
    }
}

/// A run's keys: those the maps hold, in random key order, and as many
/// others, up to [`MAX_ABSENT_KEYS`], for lookups that miss.
struct KeySet<K> {
    present: Vec<K>,
    absent: Vec<K>,
}

impl<K> KeySet<K> {
    /// The first `entries` of `keys` present, the rest absent.
    fn split(mut keys: Vec<K>, entries: usize) -> Self {
        let absent = keys.split_off(entries);
        Self {
            present: keys,
            absent,
        }
    }
}

/// What a run measures.
#[derive(Clone, Copy)]
enum Measure {
    /// Operations a second.
    Throughput,
    /// Each operation's time.
    Latency(TickClock),
    /// The bytes each map holds once built, from the counting allocator.
    Memory(&'static CountingAllocator),
}

/// The keys a run uses, all distinct, made before anything is timed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    /// Random `u64`s.
    U64,
    /// Random `u128`s.
    U128,
    /// JSON object keys, short identifiers, as the bench's `str-json`
    /// (`keys.rs`).
    JsonStrings,
    /// Source paths in cargo's registry, about 100 bytes sharing a long
    /// prefix, as the bench's `str-path`.
    PathStrings,
}

impl KeyKind {
    const ALL: [Self; 4] = [Self::U64, Self::U128, Self::JsonStrings, Self::PathStrings];

    fn name(self) -> &'static str {
        match self {
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::JsonStrings => "str-json",
            Self::PathStrings => "str-path",
        }
    }

    fn named(name: &str) -> Result<Self, String> {
        (Self::ALL.into_iter())
            .find(|kind| kind.name() == name)
            .ok_or_else(|| {
                let names: Vec<_> = Self::ALL.iter().map(|kind| kind.name()).collect();
                format!("unknown keys {name}; keys: {}", names.join(","))
            })
    }

    /// `entries` integer keys, `make`'s of their indexes: distinct, as it is
    /// a bijection.
    fn integers<K>(entries: usize, make: impl Fn(u64) -> K) -> Vec<K> {
        (0..entries as u64).map(make).collect()
    }

    /// `entries` strings from `maker`, each distinct.
    fn strings(entries: usize, mut maker: KeyMaker<String>) -> Vec<String> {
        (0..entries as u64).map(|index| maker.key(index)).collect()
    }
}

/// The CPUs the workers run on, in the order they are added: one per
/// physical core, then the cores' SMT siblings.
struct CpuOrder {
    cpus: Vec<usize>,
}

impl CpuOrder {
    /// The CPUs the process's affinity mask allows, cores first.
    fn of_this_process() -> Result<Self, String> {
        // SAFETY: a zeroed `cpu_set_t` is an empty set, which the call fills
        // for this process.
        let allowed = unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) != 0 {
                return Err("sched_getaffinity failed".to_owned());
            }
            (0..libc::CPU_SETSIZE as usize)
                .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
                .collect::<Vec<usize>>()
        };
        let mut seen_cores = std::collections::HashSet::new();
        let (mut first, mut siblings) = (Vec::new(), Vec::new());
        for cpu in allowed {
            let core = Self::core_of(cpu)?;
            if seen_cores.insert(core) {
                first.push(cpu);
            } else {
                siblings.push(cpu);
            }
        }
        first.extend(siblings);
        Ok(Self { cpus: first })
    }

    /// The physical core `cpu` belongs to, with its package.
    fn core_of(cpu: usize) -> Result<(u32, u32), String> {
        let read = |name: &str| {
            let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/{name}");
            std::fs::read_to_string(&path)
                .map_err(|error| format!("reading {path}: {error}"))?
                .trim()
                .parse::<u32>()
                .map_err(|error| format!("parsing {path}: {error}"))
        };
        Ok((read("physical_package_id")?, read("core_id")?))
    }

    fn physical_cores(&self) -> usize {
        let cores: std::collections::HashSet<_> = (self.cpus.iter())
            .map(|&cpu| Self::core_of(cpu).expect("read before"))
            .collect();
        cores.len()
    }

    /// The thread counts to measure: every count up to the physical cores,
    /// then every second one up to all the CPUs.
    fn thread_counts(&self) -> Vec<usize> {
        let cores = self.physical_cores();
        let mut counts: Vec<usize> = (1..=cores).collect();
        counts.extend((cores + 2..=self.cpus.len()).step_by(2));
        if *counts.last().expect("a CPU") != self.cpus.len() {
            counts.push(self.cpus.len());
        }
        counts
    }

    /// Pins the calling thread to worker `worker`'s CPU.
    fn pin(&self, worker: usize) {
        let cpu = self.cpus[worker];
        // SAFETY: a zeroed `cpu_set_t` is an empty set, and `cpu` is below
        // `CPU_SETSIZE`, as the process's mask had it.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(cpu, &mut set);
            let pinned = libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set);
            assert_eq!(pinned, 0, "pinning a worker to CPU {cpu}");
        }
    }
}

/// A worker's own generator of indexes below the map's size.
struct IndexStream {
    state: u64,
}

impl IndexStream {
    fn new(worker: usize, round: u64) -> Self {
        Self {
            state: SeededRng::mix((round << 32) ^ worker as u64 ^ 0xC0FFEE),
        }
    }

    #[inline(always)]
    fn next_below(&mut self, bound: usize) -> usize {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let random = SeededRng::mix(self.state);
        ((u128::from(random) * bound as u128) >> 64) as usize
    }
}

/// One worker's keys to look up, in random order, which it reads front to
/// back, from the start again at the end; and its stream for the mix's
/// choices.
struct WorkerQueries<K> {
    keys: Vec<K>,
    next: usize,
    choices: IndexStream,
}

impl<K: Clone> WorkerQueries<K> {
    /// [`QUERIES_PER_WORKER`] of `keys`, picked at random for `worker` of
    /// `workers`.
    fn new(keys: &[K], worker: usize, workers: usize) -> Self {
        let mut picks = IndexStream::new(worker, workers as u64);
        Self {
            keys: (0..QUERIES_PER_WORKER)
                .map(|_| keys[picks.next_below(keys.len())].clone())
                .collect(),
            next: 0,
            choices: IndexStream::new(worker, !(workers as u64)),
        }
    }

    #[inline(always)]
    fn next_key(&mut self) -> &K {
        let index = self.next;
        self.next = (index + 1) & (QUERIES_PER_WORKER - 1);
        &self.keys[index]
    }

    /// The next `N` keys, as `N` calls to [`Self::next_key`] give them.
    #[inline(always)]
    fn next_keys<const N: usize>(&mut self) -> [&K; N] {
        let start = self.next;
        self.next = (start + N) & (QUERIES_PER_WORKER - 1);
        let keys = &self.keys;
        std::array::from_fn(|lane| &keys[(start + lane) & (QUERIES_PER_WORKER - 1)])
    }

    /// Whether to update rather than look up: one time in ten.
    #[inline(always)]
    fn updates(&mut self) -> bool {
        self.choices.next_below(10) == 0
    }
}

/// What a worker's operations left to check: their values added up, which
/// the compiler must not drop, and how many scans ended early.
#[derive(Default)]
struct WorkerTally {
    checksum: u64,
    short_scans: u64,
}

impl WorkerTally {
    /// Runs one iteration of `workload` through `handle` on the next of
    /// `queries`, checking what it returns: a lookup finds its key, or
    /// doesn't for a miss.
    #[inline(always)]
    fn operate<K: ScalingKey>(
        &mut self,
        workload: Workload,
        handle: &impl MapHandle<K>,
        queries: &mut WorkerQueries<K>,
    ) {
        match workload {
            Workload::Get => {
                let value = handle.get(queries.next_key());
                self.checksum = self
                    .checksum
                    .wrapping_add(value.expect("every key is there"));
            }
            Workload::GetMiss => {
                assert!(
                    handle.get(queries.next_key()).is_none(),
                    "an absent key found"
                );
            }
            Workload::GetBatch => {
                let keys = queries.next_keys::<LOOKUP_BATCH>();
                for value in handle.get_batch(keys) {
                    self.checksum = self
                        .checksum
                        .wrapping_add(value.expect("every key is there"));
                }
            }
            Workload::GetMissBatch => {
                let keys = queries.next_keys::<LOOKUP_BATCH>();
                assert!(
                    handle.get_batch(keys).iter().all(Option::is_none),
                    "an absent key found"
                );
            }
            Workload::Mixed if queries.updates() => handle.bump(queries.next_key()),
            Workload::Mixed => {
                let value = handle.get(queries.next_key());
                self.checksum = self
                    .checksum
                    .wrapping_add(value.expect("every key is there"));
            }
            Workload::Scan => {
                let (visited, sum) = handle.scan(queries.next_key(), SCAN_LENGTH);
                self.checksum = self.checksum.wrapping_add(sum);
                self.short_scans += u64::from(visited < SCAN_LENGTH);
            }
            Workload::Churn => {
                let key = queries.next_key();
                let copy = key.clone();
                handle.remove(key);
                handle.insert(copy, 0);
            }
            Workload::Insert => unreachable!("inserts fill an empty map"),
        }
    }
}

impl Workload {
    /// How many operations one iteration of it does: two for the churn, a
    /// removal and an insert; a batch's keys for a batched lookup.
    fn operations_per_iteration(self) -> u64 {
        match self {
            Self::Churn => 2,
            Self::GetBatch | Self::GetMissBatch => LOOKUP_BATCH as u64,
            _ => 1,
        }
    }
}

/// One measured throughput: a map, a workload and a thread count.
struct Point {
    map: &'static str,
    workload: Workload,
    threads: usize,
    operations: u64,
    elapsed: Duration,
}

impl Point {
    fn mops(&self) -> f64 {
        self.operations as f64 / self.elapsed.as_secs_f64() / 1e6
    }
}

/// One measured latency distribution, in nanoseconds.
struct LatencyPoint {
    map: &'static str,
    workload: Workload,
    threads: usize,
    operations: u64,
    /// The 50th, 90th, 99th and 99.9th percentiles, and the slowest.
    nanos: [f64; 5],
}

/// What one map held once built.
struct MemoryPoint {
    map: &'static str,
    entries: usize,
    /// Live bytes that dropping the map freed, its keys' included.
    heap_bytes: usize,
    /// The growth of the process's resident set while it was built.
    resident_growth: u64,
    /// The growth of the process's peak resident set while it was built.
    peak_growth: u64,
}

/// The command line's settings, and what it measured so far.
struct ScalingRun {
    /// The maps to measure, by name; every one if `None`.
    maps: Option<Vec<String>>,
    /// The workloads to run, each where the measure has it.
    workloads: Vec<Workload>,
    keys: KeyKind,
    measure: Measure,
    entries: usize,
    duration: Duration,
    cpus: CpuOrder,
    thread_counts: Vec<usize>,
    out: PathBuf,
    points: Vec<Point>,
    latencies: Vec<LatencyPoint>,
    memory: Vec<MemoryPoint>,
}

impl ScalingRun {
    /// Every map it measures, by name, in the order it measures them.
    const MAPS: [&'static str; 7] = [
        <BCTreeMap<u64, u64> as SharedMap<u64>>::NAME,
        <RwLock<BBTreeMap<u64, u64>> as SharedMap<u64>>::NAME,
        <SkipMap<u64, AtomicU64> as SharedMap<u64>>::NAME,
        <TreeIndex<u64, AtomicU64> as SharedMap<u64>>::NAME,
        <ArtMapOf<u64> as SharedMap<u64>>::NAME,
        <ConcurrentMap<u64, u64> as SharedMap<u64>>::NAME,
        <DashMap<u64, u64> as SharedMap<u64>>::NAME,
    ];

    /// The latency quantiles it reports.
    const QUANTILES: [f64; 4] = [0.5, 0.9, 0.99, 0.999];

    fn parse(
        arguments: &[String],
        counting_allocator: Option<&'static CountingAllocator>,
    ) -> Result<Self, String> {
        let cpus = CpuOrder::of_this_process()?;
        let mut run = Self {
            maps: None,
            workloads: Workload::ALL.to_vec(),
            keys: KeyKind::U64,
            measure: counting_allocator.map_or(Measure::Throughput, Measure::Memory),
            entries: 10_000_000,
            duration: Duration::from_millis(500),
            thread_counts: cpus.thread_counts(),
            cpus,
            out: PathBuf::from("results"),
            points: Vec::new(),
            latencies: Vec::new(),
            memory: Vec::new(),
        };
        let mut arguments = arguments.iter();
        while let Some(flag) = arguments.next() {
            let mut value = || arguments.next().ok_or(format!("{flag} needs a value"));
            match flag.as_str() {
                "--entries" => run.entries = value()?.parse().map_err(|e| format!("{e}"))?,
                "--seconds" => {
                    run.duration =
                        Duration::from_secs_f64(value()?.parse().map_err(|e| format!("{e}"))?)
                }
                "--threads" => {
                    run.thread_counts = (value()?.split(','))
                        .map(|count| count.parse().map_err(|e| format!("{e}")))
                        .collect::<Result<_, _>>()?
                }
                "--out" => run.out = PathBuf::from(value()?),
                "--keys" => run.keys = KeyKind::named(value()?)?,
                "--workloads" => {
                    run.workloads = (value()?.split(','))
                        .map(Workload::named)
                        .collect::<Result<_, _>>()?
                }
                "--maps" => {
                    let maps: Vec<String> = value()?.split(',').map(str::to_owned).collect();
                    if let Some(unknown) =
                        maps.iter().find(|map| !Self::MAPS.contains(&map.as_str()))
                    {
                        return Err(format!(
                            "unknown map {unknown}; maps: {}",
                            Self::MAPS.join(",")
                        ));
                    }
                    run.maps = Some(maps);
                }
                "--measure" => {
                    run.measure = match (value()?.as_str(), run.measure) {
                        ("throughput", Measure::Throughput) => Measure::Throughput,
                        ("latency", Measure::Throughput) => {
                            Measure::Latency(TickClock::calibrated(Duration::from_millis(200)))
                        }
                        ("memory", memory @ Measure::Memory(_)) => memory,
                        (measure, Measure::Memory(_)) => {
                            return Err(format!(
                                "{measure} needs the timing binary, bbtree-scaling"
                            ));
                        }
                        (measure, _) => {
                            return Err(format!(
                                "{measure}: this binary measures throughput or latency; \
                                 memory needs bbtree-scaling-memory"
                            ));
                        }
                    }
                }
                _ => {
                    return Err(format!(
                        "unknown option {flag}; options: --entries N, --seconds S, \
                         --threads 1,2,4, --maps A,B, --keys u64|u128|str-json|str-path, \
                         --workloads get,get-miss,mixed,scan,churn,insert, \
                         --measure throughput|latency|memory, --out DIR"
                    ));
                }
            }
        }
        if let Some(&too_many) =
            (run.thread_counts.iter()).find(|&&count| count > run.cpus.cpus.len())
        {
            return Err(format!(
                "{too_many} threads, but the affinity mask allows {} CPUs",
                run.cpus.cpus.len()
            ));
        }
        Ok(run)
    }

    /// Makes the keys, and measures every map the command line picked with
    /// them.
    fn measure_with_keys(&mut self) {
        let entries = self.entries;
        let all = entries + entries.min(MAX_ABSENT_KEYS);
        let started = Instant::now();
        match self.keys {
            KeyKind::U64 => {
                let keys = KeySet::split(KeyKind::integers(all, SeededRng::mix), entries);
                self.report_keys(&keys.present, started, |_| 8);
                self.measure_picked_maps(&keys);
            }
            KeyKind::U128 => {
                let keys = KeyKind::integers(all, |index| {
                    u128::from(SeededRng::mix(index)) << 64 | u128::from(SeededRng::mix(!index))
                });
                let keys = KeySet::split(keys, entries);
                self.report_keys(&keys.present, started, |_| 16);
                self.measure_picked_maps(&keys);
            }
            KeyKind::JsonStrings => {
                let keys = KeySet::split(KeyKind::strings(all, KeyMaker::json_keys()), entries);
                self.report_keys(&keys.present, started, String::len);
                self.measure_picked_maps(&keys);
            }
            KeyKind::PathStrings => {
                let maker = KeyMaker::registry_paths(all);
                let keys = KeySet::split(KeyKind::strings(all, maker), entries);
                self.report_keys(&keys.present, started, String::len);
                self.measure_picked_maps(&keys);
            }
        }
    }

    /// Prints how many `keys` there are, how long they are on average by
    /// `len`, and how long making them took since `started`.
    fn report_keys<K>(&self, keys: &[K], started: Instant, len: impl Fn(&K) -> usize) {
        let bytes: usize = keys.iter().map(len).sum();
        println!(
            "{} {} keys, {:.1} bytes each on average, made in {:.1?}",
            keys.len(),
            self.keys.name(),
            bytes as f64 / keys.len() as f64,
            started.elapsed()
        );
    }

    /// Every map the command line picked, in the order of [`Self::MAPS`],
    /// with `keys`.
    fn measure_picked_maps<K: ScalingKey>(&mut self, keys: &KeySet<K>) {
        if self.picks(<BCTreeMap<K, u64> as SharedMap<K>>::NAME) {
            self.measure::<K, BCTreeMap<K, u64>>(keys);
        }
        if self.picks(<RwLock<BBTreeMap<K, u64>> as SharedMap<K>>::NAME) {
            self.measure::<K, RwLock<BBTreeMap<K, u64>>>(keys);
        }
        if self.picks(<SkipMap<K, AtomicU64> as SharedMap<K>>::NAME) {
            self.measure::<K, SkipMap<K, AtomicU64>>(keys);
        }
        if self.picks(<TreeIndex<K, AtomicU64> as SharedMap<K>>::NAME) {
            self.measure::<K, TreeIndex<K, AtomicU64>>(keys);
        }
        if self.picks(<ArtMapOf<K> as SharedMap<K>>::NAME) {
            self.measure::<K, ArtMapOf<K>>(keys);
        }
        if self.picks(<ConcurrentMap<K, u64> as SharedMap<K>>::NAME) {
            self.measure::<K, ConcurrentMap<K, u64>>(keys);
        }
        if self.picks(<DashMap<K, u64> as SharedMap<K>>::NAME) {
            self.measure::<K, DashMap<K, u64>>(keys);
        }
    }

    /// Whether the command line picked the map named `name`.
    fn picks(&self, name: &str) -> bool {
        (self.maps.as_ref()).is_none_or(|maps| maps.iter().any(|map| map == name))
    }

    /// What the run measures, of `M`, with `keys`.
    fn measure<K: ScalingKey, M: SharedMap<K>>(&mut self, keys: &KeySet<K>) {
        match self.measure {
            Measure::Throughput => self.throughput::<K, M>(keys),
            Measure::Latency(clock) => self.latency::<K, M>(keys, clock),
            Measure::Memory(allocator) => self.memory::<K, M>(keys, allocator),
        }
    }

    /// Runs `work(worker, input)` on as many pinned workers as `inputs`,
    /// each with its own, at once, and returns their results and the time
    /// from their start to the last one's end.
    fn on_workers<I: Send, R: Send>(
        &self,
        inputs: Vec<I>,
        work: impl Fn(usize, I) -> R + Sync,
    ) -> (Vec<R>, Duration) {
        let barrier = Barrier::new(inputs.len() + 1);
        thread::scope(|scope| {
            let workers: Vec<_> = (inputs.into_iter().enumerate())
                .map(|(worker, input)| {
                    let (barrier, work) = (&barrier, &work);
                    scope.spawn(move || {
                        self.cpus.pin(worker);
                        barrier.wait();
                        work(worker, input)
                    })
                })
                .collect();
            barrier.wait();
            let start = Instant::now();
            let results = (workers.into_iter())
                .map(|worker| worker.join().expect("no worker panics"))
                .collect();
            (results, start.elapsed())
        })
    }

    /// A map of every key, inserted by all the CPUs, each every so many:
    /// in random key order, as the keys are.
    fn built<K: ScalingKey, M: SharedMap<K>>(&self, keys: &[K]) -> M {
        let map = M::new();
        let workers = self.cpus.cpus.len();
        let handles = (0..workers).map(|_| map.handle()).collect();
        self.on_workers(handles, |worker, handle| {
            for (index, key) in keys.iter().enumerate().skip(worker).step_by(workers) {
                handle.insert(key.clone(), index as u64);
            }
        });
        map
    }

    /// Each of `threads` workers' handle of `map`, with its own share of
    /// `keys` to insert, which it owns: made before anything is timed.
    fn insert_shares<'m, K: ScalingKey, M: SharedMap<K>>(
        map: &'m M,
        keys: &[K],
        threads: usize,
    ) -> Vec<(M::Handle<'m>, Vec<K>)> {
        let share = keys.len().div_ceil(threads);
        (keys.chunks(share))
            .map(|share| (map.handle(), share.to_vec()))
            .collect()
    }

    /// Runs `operation` on `threads` workers, each with its handle of `map`,
    /// its lookup list from `keys` and a state of its own that `start`
    /// makes, until `self.duration` is up; returns each worker's operations
    /// and state, and the time it took.
    fn for_duration<K: ScalingKey, M: SharedMap<K>, T: Send>(
        &self,
        map: &M,
        keys: &[K],
        threads: usize,
        start: impl Fn() -> T + Sync,
        operation: impl Fn(&M::Handle<'_>, &mut WorkerQueries<K>, &mut T) + Sync,
    ) -> (Vec<(u64, T)>, Duration) {
        const BATCH: u64 = 256;
        let inputs = (0..threads)
            .map(|worker| (map.handle(), WorkerQueries::new(keys, worker, threads)))
            .collect();
        let stop = AtomicBool::new(false);
        let duration = self.duration;
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(duration);
                stop.store(true, Ordering::Relaxed);
            });
            self.on_workers(inputs, |_, (handle, mut queries)| {
                let (mut operations, mut state) = (0_u64, start());
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..BATCH {
                        operation(&handle, &mut queries, &mut state);
                    }
                    operations += BATCH;
                }
                (operations, state)
            })
        })
    }

    /// Records a throughput point.
    fn record(
        &mut self,
        map: &'static str,
        workload: Workload,
        threads: usize,
        operations: u64,
        elapsed: Duration,
    ) {
        let point = Point {
            map,
            workload,
            threads,
            operations,
            elapsed,
        };
        println!(
            "{:<18} {:<7} {:>3} threads: {:>8.2} Mops/s",
            point.map,
            point.workload.name(),
            point.threads,
            point.mops()
        );
        self.points.push(point);
    }

    /// Records a latency point from the workers' `histograms`.
    fn record_latency(
        &mut self,
        map: &'static str,
        clock: TickClock,
        workload: Workload,
        threads: usize,
        histograms: impl IntoIterator<Item = LatencyHistogram>,
    ) {
        let mut all = LatencyHistogram::new();
        for histogram in histograms {
            all.merge(&histogram);
        }
        let mut nanos = [0.0; 5];
        for (slot, fraction) in nanos.iter_mut().zip(Self::QUANTILES) {
            *slot = clock.nanos(all.quantile(fraction));
        }
        nanos[4] = clock.nanos(all.max());
        println!(
            "{map:<18} {:<7} {threads:>3} threads: p50 {:>6.0} p90 {:>6.0} p99 {:>7.0} \
             p99.9 {:>8.0} max {:>10.0} ns",
            workload.name(),
            nanos[0],
            nanos[1],
            nanos[2],
            nanos[3],
            nanos[4]
        );
        self.latencies.push(LatencyPoint {
            map,
            workload,
            threads,
            operations: all.total(),
            nanos,
        });
    }

    /// Every workload's throughput on `M` with `keys`, at every thread
    /// count.
    fn throughput<K: ScalingKey, M: SharedMap<K>>(&mut self, keys: &KeySet<K>) {
        let map: M = self.built(&keys.present);
        for workload in [
            Workload::Get,
            Workload::GetMiss,
            Workload::GetBatch,
            Workload::GetMissBatch,
            Workload::Mixed,
            Workload::Scan,
            Workload::Churn,
        ] {
            if !self.workloads.contains(&workload) || (workload == Workload::Scan && !M::ORDERED) {
                continue;
            }
            let queried = if matches!(workload, Workload::GetMiss | Workload::GetMissBatch) {
                &keys.absent
            } else {
                &keys.present
            };
            for threads in self.thread_counts.clone() {
                let (done, elapsed) = self.for_duration(
                    &map,
                    queried,
                    threads,
                    WorkerTally::default,
                    |handle, queries, tally| tally.operate(workload, handle, queries),
                );
                let mut operations = 0;
                let mut short_scans = 0;
                for (iterations, tally) in done {
                    operations += iterations * workload.operations_per_iteration();
                    short_scans += tally.short_scans;
                    black_box(tally.checksum);
                }
                if short_scans * 100 > operations {
                    println!(
                        "  {short_scans} of {operations} scans ended before {SCAN_LENGTH} entries"
                    );
                }
                self.record(M::NAME, workload, threads, operations, elapsed);
            }
        }
        drop(map);
        let insert_threads = if self.workloads.contains(&Workload::Insert) {
            self.thread_counts.clone()
        } else {
            Vec::new()
        };
        for threads in insert_threads {
            let map = M::new();
            let shares = Self::insert_shares(&map, &keys.present, threads);
            let (_, elapsed) = self.on_workers(shares, |worker, (handle, share)| {
                for (index, key) in share.into_iter().enumerate() {
                    handle.insert(key, (worker + index) as u64);
                }
            });
            let operations = keys.present.len() as u64;
            self.record(M::NAME, Workload::Insert, threads, operations, elapsed);
        }
    }

    /// The latencies of lookups, hits and misses, of the mix's lookups, of
    /// the churn's removals and inserts together, and of inserts into an
    /// empty map, on `M` with `keys`, at every thread count, each operation
    /// timed alone by `clock`.
    fn latency<K: ScalingKey, M: SharedMap<K>>(&mut self, keys: &KeySet<K>, clock: TickClock) {
        let map: M = self.built(&keys.present);
        for workload in [
            Workload::Get,
            Workload::GetMiss,
            Workload::Mixed,
            Workload::Churn,
        ] {
            if !self.workloads.contains(&workload) {
                continue;
            }
            let queried = if workload == Workload::GetMiss {
                &keys.absent
            } else {
                &keys.present
            };
            for threads in self.thread_counts.clone() {
                let (done, _) = self.for_duration(
                    &map,
                    queried,
                    threads,
                    LatencyHistogram::new,
                    |handle, queries, histogram| {
                        Self::time_one(workload, handle, queries, histogram, clock);
                    },
                );
                let histograms = done.into_iter().map(|(_, histogram)| histogram);
                self.record_latency(M::NAME, clock, workload, threads, histograms);
            }
        }
        drop(map);
        let insert_threads = if self.workloads.contains(&Workload::Insert) {
            self.thread_counts.clone()
        } else {
            Vec::new()
        };
        for threads in insert_threads {
            let map = M::new();
            let shares = Self::insert_shares(&map, &keys.present, threads);
            let (histograms, _) = self.on_workers(shares, |worker, (handle, share)| {
                let mut histogram = LatencyHistogram::new();
                for (index, key) in share.into_iter().enumerate() {
                    let start = clock.now();
                    handle.insert(key, (worker + index) as u64);
                    histogram.record(clock.now() - start);
                }
                histogram
            });
            self.record_latency(M::NAME, clock, Workload::Insert, threads, histograms);
        }
    }

    /// Runs one operation of `workload` through `handle` on the next of
    /// `queries`, and records how long it took by `clock` into `histogram`:
    /// a lookup, or the mix's lookup (its update untimed), or a removal and
    /// an insert together.
    #[inline(always)]
    fn time_one<K: ScalingKey>(
        workload: Workload,
        handle: &impl MapHandle<K>,
        queries: &mut WorkerQueries<K>,
        histogram: &mut LatencyHistogram,
        clock: TickClock,
    ) {
        match workload {
            Workload::Get | Workload::GetMiss => {
                let key = queries.next_key();
                let start = clock.now();
                let value = handle.get(key);
                histogram.record(clock.now() - start);
                assert_eq!(
                    value.is_some(),
                    workload == Workload::Get,
                    "a key where it belongs"
                );
            }
            Workload::Mixed if queries.updates() => handle.bump(queries.next_key()),
            Workload::Mixed => {
                let key = queries.next_key();
                let start = clock.now();
                let value = handle.get(key);
                histogram.record(clock.now() - start);
                black_box(value.expect("every key is there"));
            }
            Workload::Churn => {
                let key = queries.next_key();
                let copy = key.clone();
                let start = clock.now();
                handle.remove(key);
                handle.insert(copy, 0);
                histogram.record(clock.now() - start);
            }
            Workload::GetBatch | Workload::GetMissBatch | Workload::Scan | Workload::Insert => {
                unreachable!("not timed one at a time")
            }
        }
    }

    /// The bytes `M` holds once built with `keys`, by `allocator`'s count,
    /// its own copies of the keys included, and how much the process's
    /// resident set grew building it: at the end, and at its peak.
    fn memory<K: ScalingKey, M: SharedMap<K>>(
        &mut self,
        keys: &KeySet<K>,
        allocator: &'static CountingAllocator,
    ) {
        let keys = &keys.present;
        let resident = ResidentSet::read().current_bytes();
        let map: M = self.built(keys);
        let after = ResidentSet::read();
        let resident_growth = after.current_bytes().saturating_sub(resident);
        let peak_growth = after.peak_bytes().saturating_sub(resident);
        let heap_bytes = allocator.bytes_freed_by_dropping(map);
        let per_entry = |bytes: u64| bytes as f64 / keys.len() as f64;
        println!(
            "{:<18} {:>12} heap bytes, {:>6.1} per entry; resident set grew {:>6.1} bytes \
             per entry, {:>6.1} at its peak",
            M::NAME,
            heap_bytes,
            per_entry(heap_bytes as u64),
            per_entry(resident_growth),
            per_entry(peak_growth)
        );
        self.memory.push(MemoryPoint {
            map: M::NAME,
            entries: keys.len(),
            heap_bytes,
            resident_growth,
            peak_growth,
        });
    }

    /// What it measured as CSV.
    fn csv(&self) -> String {
        let keys = self.keys.name();
        let mut csv = String::new();
        match self.measure {
            Measure::Throughput => {
                csv.push_str("map,keys,workload,threads,operations,elapsed_ns,mops\n");
                for point in &self.points {
                    let _ = writeln!(
                        csv,
                        "{},{keys},{},{},{},{},{:.3}",
                        point.map,
                        point.workload.name(),
                        point.threads,
                        point.operations,
                        point.elapsed.as_nanos(),
                        point.mops()
                    );
                }
            }
            Measure::Latency(_) => {
                csv.push_str(
                    "map,keys,workload,threads,operations,p50_ns,p90_ns,p99_ns,p999_ns,max_ns\n",
                );
                for point in &self.latencies {
                    let [p50, p90, p99, p999, max] = point.nanos;
                    let _ = writeln!(
                        csv,
                        "{},{keys},{},{},{},{p50:.0},{p90:.0},{p99:.0},{p999:.0},{max:.0}",
                        point.map,
                        point.workload.name(),
                        point.threads,
                        point.operations
                    );
                }
            }
            Measure::Memory(_) => {
                csv.push_str(
                    "map,keys,entries,heap_bytes,heap_bytes_per_entry,resident_growth_bytes,\
                     peak_growth_bytes\n",
                );
                for point in &self.memory {
                    let _ = writeln!(
                        csv,
                        "{},{keys},{},{},{:.1},{},{}",
                        point.map,
                        point.entries,
                        point.heap_bytes,
                        point.heap_bytes as f64 / point.entries as f64,
                        point.resident_growth,
                        point.peak_growth
                    );
                }
            }
        }
        csv
    }

    /// Writes the CSV to `--out`, under a name for what it measured.
    fn write(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.out)
            .map_err(|error| format!("creating {}: {error}", self.out.display()))?;
        let name = match self.measure {
            Measure::Throughput => "scaling.csv",
            Measure::Latency(_) => "latency.csv",
            Measure::Memory(_) => "memory.csv",
        };
        let path = self.out.join(name);
        std::fs::write(&path, self.csv())
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
        println!("\nwrote {}", path.display());
        Ok(())
    }
}

/// Runs `bbtree-scaling`'s command line; with `counting_allocator`, the
/// global allocator, `bbtree-scaling-memory`'s, which measures memory.
pub fn run(counting_allocator: Option<&'static CountingAllocator>) -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let outcome = ScalingRun::parse(&arguments, counting_allocator).and_then(|mut run| {
        println!(
            "{} entries, {:?} per timed point, CPUs in order {:?}, threads {:?}",
            run.entries, run.duration, run.cpus.cpus, run.thread_counts
        );
        run.measure_with_keys();
        run.write()
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
