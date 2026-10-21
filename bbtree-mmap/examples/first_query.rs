//! The time from a program's start to its map's first answer when the
//! map's entries are on disk, as after a restart: std's `BTreeMap` reads
//! them back and builds itself again; a `BBTreeMapFile` maps its file and
//! looks the key up where it lies, reading only the pages the lookup
//! reaches.
//!
//! `cargo run --release --example first_query -- [entries] [directory]`:
//! random `u64` keys and values, 10M by default; the files go to
//! `directory`, the system's temporary one by default, which must be on a
//! disk, not in memory (tmpfs). Each restart is a process of its own, run
//! once with the file out of the page cache, as after a reboot (Linux
//! only: elsewhere the file stays in it), and once with the file in it.
//! Prints CSV, a row a restart: the time to load the file (std's map
//! reads it, the `BBTreeMapFile` maps it), to build the map (std's only),
//! and to the first answer, each from the restart's start.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use bbtree_mmap::BBTreeMapFile;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The map's entries in key order: distinct random `u64` keys, each with a
/// value made from it.
struct Entries(Vec<(u64, u64)>);

impl Entries {
    fn random(count: usize) -> Self {
        let mut entries: Vec<(u64, u64)> = (0..count as u64)
            .map(|index| {
                let key = Self::mixed(index);
                (key, Self::value_of(key))
            })
            .collect();
        entries.sort_unstable();
        Self(entries)
    }

    /// SplitMix64's output for its `index`th state: random-looking, and
    /// distinct for distinct indexes.
    fn mixed(index: u64) -> u64 {
        let mut z = index.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn value_of(key: u64) -> u64 {
        key.rotate_left(17)
    }
}

/// How a restarted program gets its map back, each from a file of its own.
#[derive(Clone, Copy)]
enum Restart {
    /// std's `BTreeMap`, from a file of its entries in key order, each key
    /// and value 8 little-endian bytes: read whole, then built from them,
    /// in order, std's bulk build.
    StdBTreeMap,
    /// A `BBTreeMapFile`, mapped: opening it reads its first page, the
    /// lookup the pages it reaches.
    BBTreeMapFile,
}

impl Restart {
    const ALL: [Self; 2] = [Self::StdBTreeMap, Self::BBTreeMapFile];
    /// The argument that makes the program one restart, in a process of
    /// its own.
    const CHILD: &str = "--restart";

    fn name(self) -> &'static str {
        match self {
            Self::StdBTreeMap => "std-btree",
            Self::BBTreeMapFile => "bbtree-mmap",
        }
    }

    fn named(name: &str) -> Self {
        (Self::ALL.into_iter())
            .find(|restart| restart.name() == name)
            .unwrap_or_else(|| panic!("no restart named {name}"))
    }

    /// Writes the file this restart gets `entries` back from, into
    /// `directory`; returns its path.
    fn write(self, directory: &Path, entries: &Entries) -> PathBuf {
        let path = directory.join(format!(
            "bbtree-first-query-{}-{}",
            self.name(),
            std::process::id()
        ));
        match self {
            Self::StdBTreeMap => {
                let bytes: Vec<u8> = (entries.0.iter())
                    .flat_map(|(key, value)| [key.to_le_bytes(), value.to_le_bytes()])
                    .flatten()
                    .collect();
                fs::write(&path, bytes).unwrap();
            }
            Self::BBTreeMapFile => {
                BBTreeMapFile::<u64, u64>::create(&path, entries.0.iter().copied()).unwrap();
            }
        }
        path
    }

    /// Gets the map back from the file at `path` and looks `key` up, timed
    /// from the start: what a restarted program does before its first
    /// answer.
    fn first_answer(self, path: &Path, key: u64) -> FirstAnswer {
        let started = Instant::now();
        match self {
            Self::StdBTreeMap => {
                let bytes = fs::read(path).unwrap();
                let loaded = started.elapsed();
                let (entries, _) = bytes.as_chunks::<8>().0.as_chunks::<2>();
                let map: BTreeMap<u64, u64> = (entries.iter())
                    .map(|[key, value]| (u64::from_le_bytes(*key), u64::from_le_bytes(*value)))
                    .collect();
                let built = started.elapsed();
                let value = map.get(&key).copied();
                FirstAnswer::new(loaded, built, started.elapsed(), value)
            }
            Self::BBTreeMapFile => {
                // SAFETY: nothing changes the file while it is mapped.
                let map = unsafe { BBTreeMapFile::<u64, u64>::mmap(path).open() }.unwrap();
                let loaded = started.elapsed();
                let value = map.get(&key).copied();
                FirstAnswer::new(loaded, loaded, started.elapsed(), value)
            }
        }
    }

    /// Runs [`Self::first_answer`] in a process of its own, as a restarted
    /// program, after leaving the file at `path` in or out of the page
    /// cache.
    fn first_answer_in_new_process(self, path: &Path, key: u64, cache: PageCache) -> FirstAnswer {
        cache.prepare(path);
        let output = Command::new(std::env::current_exe().unwrap())
            .arg(Self::CHILD)
            .arg(self.name())
            .arg(path)
            .arg(key.to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", self.name());
        FirstAnswer::parse(&String::from_utf8(output.stdout).unwrap())
    }
}

/// When a restart's steps ended, from its start, and the value it found.
struct FirstAnswer {
    loaded: Duration,
    built: Duration,
    answered: Duration,
    value: Option<u64>,
}

impl FirstAnswer {
    fn new(loaded: Duration, built: Duration, answered: Duration, value: Option<u64>) -> Self {
        Self {
            loaded,
            built,
            answered,
            value,
        }
    }

    /// The line a restart's process prints, for its parent to parse.
    fn line(&self) -> String {
        let nanos = [self.loaded, self.built, self.answered].map(|time| time.as_nanos());
        let value = self.value.map_or("none".into(), |value| value.to_string());
        format!("{} {} {} {value}", nanos[0], nanos[1], nanos[2])
    }

    fn parse(line: &str) -> Self {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let time = |field: &str| Duration::from_nanos(field.parse().unwrap());
        Self::new(
            time(fields[0]),
            time(fields[1]),
            time(fields[2]),
            fields[3].parse().ok(),
        )
    }

    /// Microseconds to load, to build after loading, and to the answer.
    fn csv(&self) -> String {
        let micros = |time: Duration| time.as_secs_f64() * 1e6;
        format!(
            "{:.1},{:.1},{:.1}",
            micros(self.loaded),
            micros(self.built - self.loaded),
            micros(self.answered),
        )
    }
}

/// Where a restart finds its file: out of the page cache, read from disk,
/// or in it.
#[derive(Clone, Copy, Debug)]
enum PageCache {
    Cold,
    Warm,
}

impl PageCache {
    fn prepare(self, path: &Path) {
        match self {
            Self::Cold => {
                let file = File::open(path).unwrap();
                file.sync_all().unwrap();
                #[cfg(target_os = "linux")]
                {
                    use std::os::fd::AsRawFd;
                    // SAFETY: a valid descriptor; it is advice only.
                    let error = unsafe {
                        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    };
                    assert_eq!(error, 0);
                }
            }
            Self::Warm => drop(fs::read(path).unwrap()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
        }
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [child, restart, path, key] = arguments.as_slice()
        && child == Restart::CHILD
    {
        let answer = Restart::named(restart).first_answer(Path::new(path), key.parse().unwrap());
        println!("{}", answer.line());
        return;
    }
    let count: usize = arguments
        .first()
        .map_or(10_000_000, |count| count.parse().unwrap());
    let directory = arguments
        .get(1)
        .map_or_else(std::env::temp_dir, PathBuf::from);

    let entries = Entries::random(count);
    // A key from the middle, the same for every restart.
    let key = entries.0[count / 2].0;
    let files = Restart::ALL.map(|restart| (restart, restart.write(&directory, &entries)));
    println!("map,page_cache,entries,file_mb,load_us,build_us,first_answer_us");
    for cache in [PageCache::Cold, PageCache::Warm] {
        for (restart, path) in &files {
            let answer = restart.first_answer_in_new_process(path, key, cache);
            assert_eq!(
                answer.value,
                Some(Entries::value_of(key)),
                "{}",
                restart.name()
            );
            let file_mb = fs::metadata(path).unwrap().len() as f64 / 1e6;
            println!(
                "{},{},{count},{file_mb:.1},{}",
                restart.name(),
                cache.name(),
                answer.csv()
            );
        }
    }
    for (_, path) in &files {
        fs::remove_file(path).unwrap();
    }
}
