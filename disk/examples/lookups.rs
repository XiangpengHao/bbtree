//! Lookups in a map and in its file map, read into memory and mapped from a
//! file: the file map's lookups run the map's code on the file map's bytes, so
//! they should take the map's time.
//!
//! `cargo run --release --example lookups -- [entries] [directory]`: random
//! `u64` keys and values, 10M of them by default; the file map goes to a file
//! in `directory`, the system's temporary one by default. Prints the time
//! per lookup of hits and of misses for each.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use bbtree::file::{FileMap, FileWriter};
use bbtree::unsync::BBTreeMap;
use bbtree_disk::{MappedFile, OsPages};

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// A xorshift64* generator, seeded: the same keys and lookups every run.
struct Numbers(u64);

impl Numbers {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// What a lookup cell reads its keys from.
trait Lookups {
    fn value(&self, key: &u64) -> Option<u64>;
}

impl Lookups for BBTreeMap<u64, u64> {
    fn value(&self, key: &u64) -> Option<u64> {
        self.get(key).copied()
    }
}

impl Lookups for FileMap<'_, u64, u64> {
    fn value(&self, key: &u64) -> Option<u64> {
        self.get(key).copied()
    }
}

/// Times one pass of lookups of `keys`, after an untimed pass over a
/// tenth of them; returns nanoseconds per lookup and the hits.
fn time_lookups(map: &impl Lookups, keys: &[u64]) -> (f64, usize) {
    let warm = keys
        .iter()
        .take(keys.len() / 10)
        .filter_map(|key| map.value(key))
        .count();
    black_box(warm);
    let start = Instant::now();
    let (hits, sum) = keys.iter().fold((0, 0_u64), |(hits, sum), key| {
        match black_box(map).value(key) {
            Some(value) => (hits + 1, sum.wrapping_add(value)),
            None => (hits, sum),
        }
    });
    let elapsed = start.elapsed();
    black_box(sum);
    (elapsed.as_nanos() as f64 / keys.len() as f64, hits)
}

/// Writes the file at `path` to disk and drops it from the page cache: a
/// mapping of it then reads it from disk, as after a restart, in the pages
/// the mapping asks for.
fn drop_from_page_cache(path: &std::path::Path) {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path).unwrap();
    file.sync_all().unwrap();
    // SAFETY: a valid descriptor; it is advice only.
    let error = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    assert_eq!(error, 0);
}

/// How much of the process's memory the kernel maps with 2 MiB entries,
/// of files and anonymous: the mapped file map's, and the map's.
fn huge_page_mappings() -> String {
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    (rollup.lines())
        .filter(|line| line.starts_with("FilePmdMapped") || line.starts_with("AnonHugePages"))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Times `map`'s hits and misses, and prints them on a line of their own.
fn report(name: &str, map: &impl Lookups, hits: &[u64], misses: &[u64]) {
    let (hit_ns, found) = time_lookups(map, hits);
    let (miss_ns, missed) = time_lookups(map, misses);
    assert_eq!((found, missed), (hits.len(), 0));
    println!("{name:<34} {hit_ns:>7.1} ns {miss_ns:>7.1} ns");
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let count: usize = arguments
        .next()
        .map_or(10_000_000, |count| count.parse().unwrap());
    let directory = arguments
        .next()
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let path = directory.join(format!("bbtree-lookups-{count}.bbtree"));

    let mut numbers = Numbers(0x5eed);
    // Even keys; odd ones are misses.
    let entries: Vec<(u64, u64)> = (0..count)
        .map(|_| {
            let key = numbers.next() & !1;
            (key, key.rotate_left(17))
        })
        .collect();
    let map: BBTreeMap<u64, u64> = entries.iter().copied().collect();
    let hits: Vec<u64> = (0..count)
        .map(|_| entries[numbers.next() as usize % count].0)
        .collect();
    let misses: Vec<u64> = (0..count).map(|_| numbers.next() | 1).collect();
    drop(entries);

    let started = Instant::now();
    let writer = FileWriter::from(&map);
    MappedFile::create(&path, &writer).unwrap();
    println!(
        "{} entries: a file map of {:.1} MB, {:.2} bytes per entry, written in {:.0} ms",
        map.len(),
        writer.byte_len() as f64 / 1e6,
        writer.byte_len() as f64 / map.len() as f64,
        started.elapsed().as_secs_f64() * 1e3,
    );
    drop(writer);

    println!("{:<34} {:>10} {:>10}", "", "get hit", "get miss");
    report("map (unsync, bulk built)", &map, &hits, &misses);
    let read = MappedFile::<u64, u64>::read_path(&path).unwrap();
    report("file map, read into memory", read.map(), &hits, &misses);
    drop(read);
    for pages in [OsPages::Small, OsPages::Huge] {
        // Read from disk through the mapping, which takes the pages in the
        // size it asks for.
        drop_from_page_cache(&path);
        // SAFETY: the file holds what the writer wrote; nothing changes it.
        let mapped = unsafe { MappedFile::<u64, u64>::mmap_path(&path, pages) }.unwrap();
        // Every page in memory before timing.
        black_box(
            mapped
                .map()
                .iter()
                .fold(0_u64, |sum, (_, value)| sum.wrapping_add(*value)),
        );
        let name = format!("file map, mapped, {pages:?} pages");
        report(&name, mapped.map(), &hits, &misses);
        println!("  ({})", huge_page_mappings());
    }
    std::fs::remove_file(&path).unwrap();
}
