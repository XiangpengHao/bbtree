//! Performance sanity check: `BBTreeMap` against std's `BTreeMap` on
//! `u64` → `u64` inserts and lookups, timed with `Instant`, nothing else.
//!
//! ```text
//! cargo run --release --example insert_get_vs_std
//! ```

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use bbtree::BBTreeMap;

/// Operations per workload.
const OPERATIONS: usize = 1_000_000;

/// Each workload runs this many times per map; the fastest run counts.
const RUNS: usize = 5;

/// The map operations the workloads time.
trait TimedMap: Default {
    const NAME: &'static str;
    fn insert(&mut self, key: u64, value: u64);
    fn get(&self, key: u64) -> Option<&u64>;
}

impl TimedMap for BBTreeMap<u64, u64> {
    const NAME: &'static str = "bbtree";

    fn insert(&mut self, key: u64, value: u64) {
        BBTreeMap::insert(self, key, value);
    }

    fn get(&self, key: u64) -> Option<&u64> {
        BBTreeMap::get(self, &key)
    }
}

impl TimedMap for BTreeMap<u64, u64> {
    const NAME: &'static str = "std";

    fn insert(&mut self, key: u64, value: u64) {
        BTreeMap::insert(self, key, value);
    }

    fn get(&self, key: u64) -> Option<&u64> {
        BTreeMap::get(self, &key)
    }
}

/// A deterministic xorshift64* generator, so both maps see the same keys.
struct KeyGenerator(u64);

impl Iterator for KeyGenerator {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        Some(self.0.wrapping_mul(0x2545_F491_4F6C_DD1D))
    }
}

/// The keys every workload uses.
struct Keys {
    /// Random keys, inserted in this order.
    random: Vec<u64>,
    /// Keys `0..OPERATIONS`, inserted in this order.
    sequential: Vec<u64>,
    /// Random picks from `random`, looked up in this order: every one hits.
    lookups: Vec<u64>,
}

impl Keys {
    fn new() -> Self {
        let random: Vec<u64> = KeyGenerator(0x9E37_79B9_7F4A_7C15)
            .take(OPERATIONS)
            .collect();
        let lookups = KeyGenerator(0xD1B5_4A32_D192_ED03)
            .take(OPERATIONS)
            .map(|pick| random[(pick % OPERATIONS as u64) as usize])
            .collect();
        Self {
            random,
            sequential: (0..OPERATIONS as u64).collect(),
            lookups,
        }
    }
}

#[derive(Clone, Copy)]
enum Workload {
    RandomInsert,
    SequentialInsert,
    RandomGet,
}

impl Workload {
    const ALL: [Self; 3] = [Self::RandomInsert, Self::SequentialInsert, Self::RandomGet];

    fn name(self) -> &'static str {
        match self {
            Self::RandomInsert => "1M random inserts",
            Self::SequentialInsert => "1M sequential inserts",
            Self::RandomGet => "1M random gets",
        }
    }

    /// The fastest of `RUNS` runs on `Map`, in nanoseconds per operation.
    fn nanos_per_operation<Map: TimedMap>(self, keys: &Keys) -> f64 {
        let fastest = (0..RUNS)
            .map(|_| self.time_one_run::<Map>(keys))
            .min()
            .expect("at least one run");
        fastest.as_nanos() as f64 / OPERATIONS as f64
    }

    /// Times one run. Building the map for lookups and dropping the map
    /// afterwards are not timed.
    fn time_one_run<Map: TimedMap>(self, keys: &Keys) -> Duration {
        let mut map = Map::default();
        let elapsed = match self {
            Self::RandomInsert => Self::time_inserts(&mut map, &keys.random),
            Self::SequentialInsert => Self::time_inserts(&mut map, &keys.sequential),
            Self::RandomGet => {
                for &key in &keys.random {
                    map.insert(key, key);
                }
                let start = Instant::now();
                let mut sum = 0_u64;
                for key in &keys.lookups {
                    sum = sum.wrapping_add(*map.get(black_box(*key)).expect("every key hits"));
                }
                black_box(sum);
                start.elapsed()
            }
        };
        drop(black_box(map));
        elapsed
    }

    fn time_inserts<Map: TimedMap>(map: &mut Map, keys: &[u64]) -> Duration {
        let start = Instant::now();
        for &key in keys {
            map.insert(black_box(key), key);
        }
        start.elapsed()
    }
}

fn main() {
    let keys = Keys::new();
    println!(
        "{:<24}{:>14}{:>14}{:>12}",
        "u64 -> u64",
        format!("{} ns/op", BBTreeMap::<u64, u64>::NAME),
        format!("{} ns/op", BTreeMap::<u64, u64>::NAME),
        "std/bbtree"
    );
    for workload in Workload::ALL {
        let ours = workload.nanos_per_operation::<BBTreeMap<u64, u64>>(&keys);
        let theirs = workload.nanos_per_operation::<BTreeMap<u64, u64>>(&keys);
        println!(
            "{:<24}{:>14.1}{:>14.1}{:>12.2}",
            workload.name(),
            ours,
            theirs,
            theirs / ours
        );
    }
}
