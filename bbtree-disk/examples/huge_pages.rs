//! A `BBTreeMapFile` mapped from disk, as after a restart, in each size of OS
//! pages ([`OsPages`]): the mapping reads the file in the pages it asks
//! for. Small pages read a few KiB for each page a lookup first reaches,
//! and miss the TLB more once in memory; huge ones read 2 MiB, and then
//! look up at a map's speed.
//!
//! `cargo run --release --example huge_pages -- [entries] [directory]`:
//! random `u64` keys and values, 10M by default; the file goes to
//! `directory`, the system's temporary one by default, which must be on a
//! disk, not in memory (tmpfs). Linux only: elsewhere the file stays in the
//! page cache, in the pages it is in.

mod timed_lookups;

use std::hint::black_box;
use std::path::PathBuf;

use bbtree::unsync::BBTreeMap;
use bbtree_disk::{BBTreeMapFile, OsPages};
use timed_lookups::{Numbers, TimedLookups};

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// A `BBTreeMapFile`'s file, which mappings read from disk; removed when dropped.
struct ColdFile(PathBuf);

impl ColdFile {
    /// Writes `map` to the file at `path`.
    fn create(path: PathBuf, map: &BBTreeMap<u64, u64>) -> Self {
        BBTreeMapFile::<u64, u64>::create(&path, map).unwrap();
        Self(path)
    }

    /// The map, mapped in `pages` from disk: the file is dropped from
    /// the page cache first, and every page read in before this returns.
    fn map_from_disk(&self, pages: OsPages) -> BBTreeMapFile<u64, u64> {
        let file = std::fs::File::open(&self.0).unwrap();
        file.sync_all().unwrap();
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: a valid descriptor; it is advice only.
            let error =
                unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
            assert_eq!(error, 0);
        }
        // SAFETY: the file holds what the writer wrote; nothing changes it.
        let mapped =
            unsafe { BBTreeMapFile::<u64, u64>::mmap(&self.0).pages(pages).open() }.unwrap();
        black_box((mapped.iter()).fold(0_u64, |sum, (_, value)| sum.wrapping_add(*value)));
        mapped
    }
}

impl Drop for ColdFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// How much of the process's memory the kernel maps with 2 MiB entries: of
/// files, as the mapped `BBTreeMapFile`, and anonymous, as the map.
fn huge_page_mappings() -> String {
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    (rollup.lines())
        .filter(|line| line.starts_with("FilePmdMapped") || line.starts_with("AnonHugePages"))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(", ")
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let count: usize = arguments
        .next()
        .map_or(10_000_000, |count| count.parse().unwrap());
    let directory = arguments
        .next()
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let (map, lookups) = TimedLookups::of_u64_map(count, &mut Numbers::seeded());
    let file = ColdFile::create(
        directory.join(format!("bbtree-huge-pages-{}", std::process::id())),
        &map,
    );

    lookups.report("map", |key| map.get(key).copied());
    for pages in [OsPages::Small, OsPages::Huge] {
        let mapped = file.map_from_disk(pages);
        lookups.report(&format!("BBTreeMapFile, mapped, {pages:?} pages"), |key| {
            mapped.get(key).copied()
        });
        println!("  ({})", huge_page_mappings());
    }
}
