//! Lookups in a map and in its `BBTreeMapFile`, read into memory and mapped from a
//! file: the `BBTreeMapFile`'s lookups run the map's code on the file's bytes, so
//! they take the map's time.
//!
//! `cargo run --release --example lookups -- [entries]`: random `u64` keys
//! and values, 10M by default. `huge_pages` maps a `BBTreeMapFile` from disk, in
//! either size of OS pages.

mod timed_lookups;

use std::time::Instant;

use bbtree_disk::BBTreeMapFile;
use timed_lookups::{Numbers, TimedLookups};

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .map_or(10_000_000, |count| count.parse().unwrap());
    let (map, lookups) = TimedLookups::of_u64_map(count, &mut Numbers::seeded());
    let path = std::env::temp_dir().join(format!("bbtree-lookups-{}", std::process::id()));

    let started = Instant::now();
    BBTreeMapFile::<u64, u64>::create(&path, &map).unwrap();
    let written = started.elapsed();
    let file_len = std::fs::metadata(&path).unwrap().len() as f64;
    println!(
        "{} entries: a file of {:.1} MB, {:.2} bytes an entry, written in {:.0} ms",
        map.len(),
        file_len / 1e6,
        file_len / map.len() as f64,
        written.as_secs_f64() * 1e3,
    );

    let read = BBTreeMapFile::<u64, u64>::read(&path).unwrap();
    // SAFETY: nothing changes the file while it is mapped.
    let mapped = unsafe { BBTreeMapFile::<u64, u64>::mmap(&path).open() }.unwrap();
    lookups.report("map", |key| map.get(key).copied());
    lookups.report("BBTreeMapFile, read into memory", |key| {
        read.get(key).copied()
    });
    lookups.report("BBTreeMapFile, mapped", |key| mapped.get(key).copied());
    drop(mapped);
    std::fs::remove_file(&path).unwrap();
}
