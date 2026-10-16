//! `bbtree-scaling`: how throughput grows as threads are added, on maps that
//! threads share: `BCTreeMap`; `BBTreeMap` behind a `RwLock`, the way to
//! share it without one; and other crates' concurrent ordered maps:
//! crossbeam's `SkipMap`, scc's `TreeIndex`, Congee, and concurrent-map
//! (`--maps` picks some).
//!
//! Each worker is pinned to a CPU of its own, one per physical core first,
//! then their SMT siblings, in the order the process's affinity mask allows
//! them: run it under `taskset` to give it some of the machine. Workloads, on
//! `u64 → u64` maps of `--entries` random keys, built by random inserts:
//!
//! - `get`: lookups of keys the map holds, for `--seconds`;
//! - `mixed`: 90% such lookups, 10% updates of their values, for
//!   `--seconds`;
//! - `insert`: the whole map inserted into an empty one, the keys split
//!   between the workers, timed to the last one.
//!
//! Keys are `SeededRng::mix` of their index, a bijection, so they are
//! distinct and random; workers draw indexes from generators of their own,
//! which every map pays alike.

use std::collections::BTreeMap;
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
use congee::Congee;
use crossbeam_skiplist::SkipMap;
use scc::TreeIndex;

use crate::random::SeededRng;

/// A map that threads share, each worker through a handle of its own: a
/// reference for most, a clone for a map whose reclamation keeps state per
/// handle.
trait SharedMap: Sized {
    const NAME: &'static str;

    /// What one worker reads and writes the map through.
    type Handle<'m>: MapHandle + Send
    where
        Self: 'm;

    fn new() -> Self;

    fn handle(&self) -> Self::Handle<'_>;
}

/// One worker's way into a shared map.
trait MapHandle {
    fn insert(&self, key: u64, value: u64);

    fn get(&self, key: u64) -> Option<u64>;

    /// Adds one to the value of `key`, if the map holds it.
    fn bump(&self, key: u64);
}

impl<T: MapHandle> MapHandle for &T {
    fn insert(&self, key: u64, value: u64) {
        (**self).insert(key, value);
    }

    fn get(&self, key: u64) -> Option<u64> {
        (**self).get(key)
    }

    fn bump(&self, key: u64) {
        (**self).bump(key);
    }
}

/// Implements [`SharedMap`] for a map that threads share by reference,
/// named `$name`.
macro_rules! shared_by_reference {
    ($map:ty, $name:literal, $new:expr) => {
        impl SharedMap for $map {
            const NAME: &'static str = $name;

            type Handle<'m> = &'m Self;

            fn new() -> Self {
                $new
            }

            fn handle(&self) -> &Self {
                self
            }
        }
    };
}

shared_by_reference!(BCTreeMap<u64, u64>, "bctree", BCTreeMap::new());

impl MapHandle for BCTreeMap<u64, u64> {
    fn insert(&self, key: u64, value: u64) {
        BCTreeMap::insert(self, key, value);
    }

    fn get(&self, key: u64) -> Option<u64> {
        self.read(&key, |_, &value| value)
    }

    fn bump(&self, key: u64) {
        self.update(&key, |_, value| *value += 1);
    }
}

shared_by_reference!(
    RwLock<BBTreeMap<u64, u64>>,
    "bbtree-rwlock",
    RwLock::new(BBTreeMap::new())
);

impl MapHandle for RwLock<BBTreeMap<u64, u64>> {
    fn insert(&self, key: u64, value: u64) {
        self.write().expect("no thread panics").insert(key, value);
    }

    fn get(&self, key: u64) -> Option<u64> {
        self.read().expect("no thread panics").get(&key).copied()
    }

    fn bump(&self, key: u64) {
        if let Some(value) = self.write().expect("no thread panics").get_mut(&key) {
            *value += 1;
        }
    }
}

// crossbeam's lock-free skiplist. It hands out values shared only, and
// replaces an entry by removing it and inserting another, which a lookup
// meanwhile misses: values are atomics, which an update adds to.
shared_by_reference!(
    SkipMap<u64, AtomicU64>,
    "crossbeam-skipmap",
    SkipMap::new()
);

impl MapHandle for SkipMap<u64, AtomicU64> {
    fn insert(&self, key: u64, value: u64) {
        SkipMap::insert(self, key, AtomicU64::new(value));
    }

    fn get(&self, key: u64) -> Option<u64> {
        SkipMap::get(self, &key).map(|entry| entry.value().load(Ordering::Relaxed))
    }

    fn bump(&self, key: u64) {
        if let Some(entry) = SkipMap::get(self, &key) {
            entry.value().fetch_add(1, Ordering::Relaxed);
        }
    }
}

// scc's B+-tree, read-optimized, which hands out values shared only:
// values are atomics, as for the skiplist. Lookups peek, which takes no
// lock; updates read, which sees every change to a value.
shared_by_reference!(
    TreeIndex<u64, AtomicU64>,
    "scc-treeindex",
    TreeIndex::new()
);

impl MapHandle for TreeIndex<u64, AtomicU64> {
    fn insert(&self, key: u64, value: u64) {
        self.upsert_sync(key, AtomicU64::new(value));
    }

    fn get(&self, key: u64) -> Option<u64> {
        self.peek_with(&key, |_, value| value.load(Ordering::Relaxed))
    }

    fn bump(&self, key: u64) {
        self.read_sync(&key, |_, value| value.fetch_add(1, Ordering::Relaxed));
    }
}

// Congee, an adaptive radix tree with optimistic lock coupling, for
// 8-byte keys: each call pins an epoch, as a call that keeps no guard.
shared_by_reference!(Congee<usize, usize>, "congee", Congee::default());

impl MapHandle for Congee<usize, usize> {
    fn insert(&self, key: u64, value: u64) {
        let guard = self.pin();
        (Congee::insert(self, key as usize, value as usize, &guard)).expect("memory for a node");
    }

    fn get(&self, key: u64) -> Option<u64> {
        let guard = self.pin();
        Congee::get(self, &(key as usize), &guard).map(|value| value as u64)
    }

    fn bump(&self, key: u64) {
        let guard = self.pin();
        self.compute_if_present(&(key as usize), |value| Some(value + 1), &guard);
    }
}

// sled's lock-free B+-tree. Its reclamation keeps state per handle, which
// isn't `Sync`: each worker clones the map.
impl SharedMap for ConcurrentMap<u64, u64> {
    const NAME: &'static str = "concurrent-map";

    type Handle<'m> = Self;

    fn new() -> Self {
        ConcurrentMap::default()
    }

    fn handle(&self) -> Self {
        self.clone()
    }
}

impl MapHandle for ConcurrentMap<u64, u64> {
    fn insert(&self, key: u64, value: u64) {
        ConcurrentMap::insert(self, key, value);
    }

    fn get(&self, key: u64) -> Option<u64> {
        ConcurrentMap::get(self, &key)
    }

    fn bump(&self, key: u64) {
        self.fetch_and_update(key, |value| value.map(|value| value + 1));
    }
}

/// The workloads, in the order they run.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Workload {
    Get,
    Mixed,
    Insert,
}

impl Workload {
    const ALL: [Self; 3] = [Self::Get, Self::Mixed, Self::Insert];

    fn name(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Mixed => "mixed",
            Self::Insert => "insert",
        }
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

/// One measured point: a map, a workload and a thread count.
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

/// The command line's settings, and the points measured so far.
struct ScalingRun {
    /// The maps to measure, by name; every one if `None`.
    maps: Option<Vec<String>>,
    entries: usize,
    duration: Duration,
    cpus: CpuOrder,
    thread_counts: Vec<usize>,
    out: PathBuf,
    points: Vec<Point>,
}

impl ScalingRun {
    /// Every map it measures, by name, in the order it measures them.
    const MAPS: [&'static str; 6] = [
        <BCTreeMap<u64, u64> as SharedMap>::NAME,
        <RwLock<BBTreeMap<u64, u64>> as SharedMap>::NAME,
        <SkipMap<u64, AtomicU64> as SharedMap>::NAME,
        <TreeIndex<u64, AtomicU64> as SharedMap>::NAME,
        <Congee<usize, usize> as SharedMap>::NAME,
        <ConcurrentMap<u64, u64> as SharedMap>::NAME,
    ];

    fn parse(arguments: &[String]) -> Result<Self, String> {
        let cpus = CpuOrder::of_this_process()?;
        let mut run = Self {
            maps: None,
            entries: 10_000_000,
            duration: Duration::from_millis(500),
            thread_counts: cpus.thread_counts(),
            cpus,
            out: PathBuf::from("results"),
            points: Vec::new(),
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
                _ => {
                    return Err(format!(
                        "unknown option {flag}; options: --entries N, --seconds S, \
                         --threads 1,2,4, --maps A,B, --out DIR"
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

    /// The key at `index`: distinct for distinct indexes.
    #[inline(always)]
    fn key(index: usize) -> u64 {
        SeededRng::mix(index as u64)
    }

    /// Runs `work(worker, handle)` on as many pinned workers as `handles`,
    /// each with its own, at once, and returns their results and the time
    /// from their start to the last one's end.
    fn on_workers<H: MapHandle + Send, R: Send>(
        &self,
        handles: Vec<H>,
        work: impl Fn(usize, &H) -> R + Sync,
    ) -> (Vec<R>, Duration) {
        let barrier = Barrier::new(handles.len() + 1);
        thread::scope(|scope| {
            let workers: Vec<_> = (handles.into_iter().enumerate())
                .map(|(worker, handle)| {
                    let (barrier, work) = (&barrier, &work);
                    scope.spawn(move || {
                        self.cpus.pin(worker);
                        barrier.wait();
                        work(worker, &handle)
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

    /// A map of every key, inserted in random order by all the CPUs.
    fn built<M: SharedMap>(&self) -> M {
        let map = M::new();
        let workers = self.cpus.cpus.len();
        let mut order: Vec<usize> = (0..self.entries).collect();
        SeededRng::new(crate::random::RandomStream::InsertionOrder).shuffle(&mut order);
        let chunk = self.entries.div_ceil(workers);
        self.on_workers(Self::handles(&map, workers), |worker, handle| {
            for &index in order.iter().skip(worker * chunk).take(chunk) {
                handle.insert(Self::key(index), index as u64);
            }
        });
        map
    }

    /// A handle of `map` for each of `workers` workers.
    fn handles<M: SharedMap>(map: &M, workers: usize) -> Vec<M::Handle<'_>> {
        (0..workers).map(|_| map.handle()).collect()
    }

    /// Runs `operation` on `threads` workers until `self.duration` is up,
    /// and records the point.
    fn time_for_duration<M: SharedMap>(
        &mut self,
        map: &M,
        workload: Workload,
        threads: usize,
        operation: impl Fn(&M::Handle<'_>, &mut IndexStream) -> u64 + Sync,
    ) {
        const BATCH: usize = 256;
        let stop = AtomicBool::new(false);
        let duration = self.duration;
        let (results, elapsed) = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(duration);
                stop.store(true, Ordering::Relaxed);
            });
            self.on_workers(Self::handles(map, threads), |worker, handle| {
                let mut stream = IndexStream::new(worker, threads as u64);
                let (mut operations, mut checksum) = (0_u64, 0_u64);
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..BATCH {
                        checksum = checksum.wrapping_add(operation(handle, &mut stream));
                    }
                    operations += BATCH as u64;
                }
                black_box(checksum);
                operations
            })
        });
        self.record::<M>(workload, threads, results.into_iter().sum(), elapsed);
    }

    fn record<M: SharedMap>(
        &mut self,
        workload: Workload,
        threads: usize,
        operations: u64,
        elapsed: Duration,
    ) {
        let point = Point {
            map: M::NAME,
            workload,
            threads,
            operations,
            elapsed,
        };
        println!(
            "{:<14} {:<7} {:>3} threads: {:>8.2} Mops/s",
            point.map,
            point.workload.name(),
            point.threads,
            point.mops()
        );
        self.points.push(point);
    }

    /// Every workload on each map the command line picked, in the order of
    /// [`Self::MAPS`].
    fn measure_picked_maps(&mut self) {
        if self.picks(<BCTreeMap<u64, u64> as SharedMap>::NAME) {
            self.measure::<BCTreeMap<u64, u64>>();
        }
        if self.picks(<RwLock<BBTreeMap<u64, u64>> as SharedMap>::NAME) {
            self.measure::<RwLock<BBTreeMap<u64, u64>>>();
        }
        if self.picks(<SkipMap<u64, AtomicU64> as SharedMap>::NAME) {
            self.measure::<SkipMap<u64, AtomicU64>>();
        }
        if self.picks(<TreeIndex<u64, AtomicU64> as SharedMap>::NAME) {
            self.measure::<TreeIndex<u64, AtomicU64>>();
        }
        if self.picks(<Congee<usize, usize> as SharedMap>::NAME) {
            self.measure::<Congee<usize, usize>>();
        }
        if self.picks(<ConcurrentMap<u64, u64> as SharedMap>::NAME) {
            self.measure::<ConcurrentMap<u64, u64>>();
        }
    }

    /// Whether the command line picked the map named `name`.
    fn picks(&self, name: &str) -> bool {
        (self.maps.as_ref()).is_none_or(|maps| maps.iter().any(|map| map == name))
    }

    /// Every workload on `M`, at every thread count.
    fn measure<M: SharedMap>(&mut self) {
        let map: M = self.built();
        let entries = self.entries;
        for threads in self.thread_counts.clone() {
            self.time_for_duration(&map, Workload::Get, threads, |handle, stream| {
                handle
                    .get(Self::key(stream.next_below(entries)))
                    .expect("every key is there")
            });
        }
        for threads in self.thread_counts.clone() {
            self.time_for_duration(&map, Workload::Mixed, threads, |handle, stream| {
                let key = Self::key(stream.next_below(entries));
                if stream.next_below(10) == 0 {
                    handle.bump(key);
                    0
                } else {
                    handle.get(key).expect("every key is there")
                }
            });
        }
        drop(map);
        for threads in self.thread_counts.clone() {
            let map = M::new();
            let chunk = entries.div_ceil(threads);
            let (_, elapsed) = self.on_workers(Self::handles(&map, threads), |worker, handle| {
                for index in (worker * chunk..entries).take(chunk) {
                    handle.insert(Self::key(index), index as u64);
                }
            });
            self.record::<M>(Workload::Insert, threads, entries as u64, elapsed);
        }
    }

    /// The points as a markdown table per workload, and as CSV.
    fn report(&self) -> (String, String) {
        let mut csv = String::from("map,workload,threads,operations,elapsed_ns,mops\n");
        let mut table: BTreeMap<(Workload, usize), BTreeMap<&str, f64>> = BTreeMap::new();
        for point in &self.points {
            let _ = writeln!(
                csv,
                "{},{},{},{},{},{:.3}",
                point.map,
                point.workload.name(),
                point.threads,
                point.operations,
                point.elapsed.as_nanos(),
                point.mops()
            );
            (table.entry((point.workload, point.threads)).or_default())
                .insert(point.map, point.mops());
        }
        let mut markdown = String::new();
        let mut maps: Vec<&str> = Vec::new();
        for point in &self.points {
            if !maps.contains(&point.map) {
                maps.push(point.map);
            }
        }
        for workload in Workload::ALL {
            let _ = writeln!(
                markdown,
                "\n### {} (Mops/s)\n\n| threads | {} |\n|--:|{}",
                workload.name(),
                maps.join(" | "),
                "--:|".repeat(maps.len())
            );
            for (&(_, threads), row) in table.range((workload, 0)..=(workload, usize::MAX)) {
                let cells: Vec<String> = (maps.iter())
                    .map(|map| {
                        row.get(map)
                            .map_or("–".to_owned(), |mops| format!("{mops:.2}"))
                    })
                    .collect();
                let _ = writeln!(markdown, "| {threads} | {} |", cells.join(" | "));
            }
        }
        (markdown, csv)
    }

    fn write(&self, markdown: &str, csv: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.out)
            .map_err(|error| format!("creating {}: {error}", self.out.display()))?;
        let path = self.out.join("scaling.csv");
        std::fs::write(&path, csv)
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
        let path = self.out.join("scaling.md");
        std::fs::write(&path, markdown)
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
        println!("\nwrote {}", self.out.display());
        Ok(())
    }
}

/// Runs `bbtree-scaling`'s command line.
pub fn run() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let outcome = ScalingRun::parse(&arguments).and_then(|mut run| {
        println!(
            "{} entries, {:?} per timed point, CPUs in order {:?}, threads {:?}",
            run.entries, run.duration, run.cpus.cpus, run.thread_counts
        );
        run.measure_picked_maps();
        let (markdown, csv) = run.report();
        println!("{markdown}");
        run.write(&markdown, &csv)
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
