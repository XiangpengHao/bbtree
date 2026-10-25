//! Point operations on `bbtree::unsync::BBTreeMap`, as `point_ops_bench.cpp`
//! times the C++ map: the same keys, in the same orders.
//!
//! usage: bbtree-reference-bench point [entries] [u64|string|all]

use std::hint::black_box;
use std::time::Instant;

use bbtree::unsync::BBTreeMap;

/// The splitmix64 finalizer, as `Keys::mix` in `workload.hpp`.
fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn path(value: u64) -> String {
    format!("/data/warehouse/table_{:02}/part-{:016x}.parquet", value % 37, value)
}

fn report(cell: &str, size: usize, ns: f64) {
    println!("{cell:<28} {size:>10} {ns:>10.1} ns/op");
}

fn ns_per(start: Instant, operations: usize) -> f64 {
    start.elapsed().as_nanos() as f64 / operations as f64
}

fn run<K: Ord + Clone>(name: &str, entries: usize, make_key: impl Fn(u64) -> K) {
    let ids: Vec<u64> = (0..entries as u64).map(mix).collect();
    let keys: Vec<K> = ids.iter().map(|&id| make_key(id)).collect();
    let hits: Vec<K> = (0..entries as u64)
        .map(|index| make_key(ids[(mix(index ^ 0x5555_5555_5555_5555) % entries as u64) as usize]))
        .collect();
    let misses: Vec<K> = (0..entries as u64)
        .map(|index| make_key(mix(entries as u64 + index)))
        .collect();

    let mut map = BBTreeMap::<K, u64>::new();
    let input = keys.clone();
    let start = Instant::now();
    for (index, key) in input.into_iter().enumerate() {
        map.insert(key, index as u64);
    }
    report(&format!("{name} insert_random"), entries, ns_per(start, entries));

    // `GET_ROUNDS` repeats the lookups, as in `point_ops_bench.cpp`.
    let rounds: usize = std::env::var("GET_ROUNDS").map_or(1, |rounds| rounds.parse().unwrap());
    let start = Instant::now();
    let mut sum = 0_u64;
    for _ in 0..rounds {
        for key in &hits {
            sum = sum.wrapping_add(*map.get(key).unwrap());
        }
    }
    black_box(sum);
    report(&format!("{name} get_hit"), entries, ns_per(start, entries));

    let start = Instant::now();
    let mut found = 0_usize;
    for key in &misses {
        found += usize::from(map.get(key).is_some());
    }
    black_box(found);
    report(&format!("{name} get_miss"), entries, ns_per(start, entries));

    let start = Instant::now();
    let mut removed = 0_usize;
    for key in &hits {
        removed += usize::from(map.remove(key).is_some());
    }
    black_box(removed);
    report(&format!("{name} remove_random"), entries, ns_per(start, entries));

    map.clear();
    let mut sorted = keys;
    sorted.sort();
    let start = Instant::now();
    for (index, key) in sorted.into_iter().enumerate() {
        map.insert(key, index as u64);
    }
    report(&format!("{name} insert_sequential"), entries, ns_per(start, entries));
}

/// Scans, as `scan_bench.cpp` times the C++ map.
fn scan(entries: usize) {
    let keys: Vec<u64> = (0..entries as u64).map(mix).collect();
    let mut map = BBTreeMap::<u64, u64>::new();
    for (index, &key) in keys.iter().enumerate() {
        map.insert(key, index as u64);
    }
    let start = Instant::now();
    let mut sum = 0_u64;
    let mut iter = map.iter();
    while let Some((_, value)) = iter.next() {
        sum = sum.wrapping_add(*value);
    }
    black_box(sum);
    report("u64 iterate_next", entries, ns_per(start, entries));

    let start = Instant::now();
    let sum = map.iter().fold(0_u64, |sum, (_, value)| sum.wrapping_add(*value));
    black_box(sum);
    report("u64 iterate_fold", entries, ns_per(start, entries));

    let scans = entries.min(200_000);
    let starts: Vec<u64> = (0..scans as u64)
        .map(|index| keys[(mix(index ^ 0x5555_5555_5555_5555) % entries as u64) as usize])
        .collect();
    let rounds: usize = std::env::var("SCAN_ROUNDS").map_or(1, |rounds| rounds.parse().unwrap());
    let start = Instant::now();
    let mut sum = 0_u64;
    for &from in starts.iter().cycle().take(rounds * starts.len()) {
        let mut range = map.range(from..);
        for _ in 0..64 {
            let Some((_, value)) = range.next() else { break };
            sum = sum.wrapping_add(*value);
        }
    }
    black_box(sum);
    report("u64 range64", entries, ns_per(start, scans));
}

/// The shared map under threads, as `shared_bench.cpp` times the C++ map.
fn shared(entries: usize, thread_counts: &[usize]) {
    let keys: Vec<u64> = (0..entries as u64).map(mix).collect();
    let hits: Vec<u64> = (0..entries as u64)
        .map(|index| keys[(mix(index ^ 0x5555_5555_5555_5555) % entries as u64) as usize])
        .collect();
    for &threads in thread_counts {
        let map = bbtree::BBTreeMap::<u64, u64>::new();
        let start = Instant::now();
        std::thread::scope(|scope| {
            for thread in 0..threads {
                let (map, keys) = (&map, &keys);
                scope.spawn(move || {
                    for index in (thread..entries).step_by(threads) {
                        map.insert(keys[index], index as u64).unwrap();
                    }
                });
            }
        });
        let ns = ns_per(start, entries);
        println!("shared insert   threads {threads:>2}  {:>8.1} Mops/s", 1000.0 / ns);
        // `GET_ROUNDS` repeats the lookups, as `BATCH_ROUNDS` the batched ones.
        let get_rounds: usize = std::env::var("GET_ROUNDS").map_or(1, |rounds| rounds.parse().unwrap());
        let start = Instant::now();
        std::thread::scope(|scope| {
            for thread in 0..threads {
                let (map, hits) = (&map, &hits);
                scope.spawn(move || {
                    let mut sum = 0_u64;
                    for index in (0..get_rounds).flat_map(|_| (thread..entries).step_by(threads)) {
                        sum = sum.wrapping_add(map.get_copied(&hits[index]).unwrap());
                    }
                    black_box(sum);
                });
            }
        });
        let ns = ns_per(start, entries);
        println!("shared get_hit  threads {threads:>2}  {:>8.1} Mops/s", 1000.0 / ns);
        const PER_CALL: usize = 64;
        let calls = entries / PER_CALL;
        let rounds: usize = std::env::var("BATCH_ROUNDS").map_or(1, |rounds| rounds.parse().unwrap());
        let start = Instant::now();
        std::thread::scope(|scope| {
            for thread in 0..threads {
                let (map, hits) = (&map, &hits);
                scope.spawn(move || {
                    let mut sum = 0_u64;
                    for call in (0..rounds).flat_map(|_| (thread..calls).step_by(threads)) {
                        let keys: [&u64; PER_CALL] =
                            core::array::from_fn(|index| &hits[call * PER_CALL + index]);
                        for value in map.get_many_copied(keys) {
                            sum = sum.wrapping_add(value.unwrap());
                        }
                    }
                    black_box(sum);
                });
            }
        });
        let ns = ns_per(start, calls * PER_CALL);
        println!("shared batch_get_hit threads {threads:>2}  {:>8.1} Mops/s", 1000.0 / ns);
    }
}

/// Batches, as `batch_bench.cpp` times the C++ map: 64 keys a call.
fn batch(entries: usize) {
    const PER_CALL: usize = 64;
    let keys: Vec<u64> = (0..entries as u64).map(mix).collect();
    let hits: Vec<u64> = (0..entries as u64)
        .map(|index| keys[(mix(index ^ 0x5555_5555_5555_5555) % entries as u64) as usize])
        .collect();
    let misses: Vec<u64> = (0..entries as u64).map(|index| mix(entries as u64 + index)).collect();
    let mut map = BBTreeMap::<u64, u64>::new();
    let start = Instant::now();
    for (chunk_index, chunk) in keys.chunks(PER_CALL).enumerate() {
        if let Ok(full) = <[u64; PER_CALL]>::try_from(chunk) {
            let base = (chunk_index * PER_CALL) as u64;
            let mut offset = 0;
            let entries_array = full.map(|key| {
                offset += 1;
                (key, base + offset - 1)
            });
            black_box(map.insert_many(entries_array));
        } else {
            for (offset, &key) in chunk.iter().enumerate() {
                map.insert(key, (chunk_index * PER_CALL + offset) as u64);
            }
        }
    }
    report("u64 batch_insert_random", entries, ns_per(start, entries));
    for (name, probes) in [("u64 batch_get_hit", &hits), ("u64 batch_get_miss", &misses)] {
        let start = Instant::now();
        let mut sum = 0_u64;
        let calls = entries / PER_CALL;
        for call in 0..calls {
            let refs: [&u64; PER_CALL] = core::array::from_fn(|index| &probes[call * PER_CALL + index]);
            for value in map.get_many(refs) {
                sum = sum.wrapping_add(value.copied().unwrap_or(1));
            }
        }
        black_box(sum);
        report(name, entries, ns_per(start, calls * PER_CALL));
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let suite = arguments.first().map(String::as_str).unwrap_or("point");
    let entries: usize = arguments.get(1).map_or(1_000_000, |n| n.parse().unwrap());
    let which = arguments.get(2).map(String::as_str).unwrap_or("all");
    match suite {
        "point" => {
            if which == "all" || which == "u64" {
                run("u64", entries, |id| id);
            }
            if which == "all" || which == "string" {
                run("string", entries, path);
            }
        }
        "scan" => scan(entries),
        "batch" => batch(entries),
        "shared" => {
            let counts: Vec<usize> = arguments
                .get(2)
                .map_or(vec![1, 4, 8, 12], |list| list.split(',').map(|n| n.parse().unwrap()).collect());
            shared(entries, &counts);
        }
        other => panic!("unknown suite {other}"),
    }
}
