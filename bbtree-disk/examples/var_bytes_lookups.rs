//! Lookups of var-length keys and values, kept in heap pages after the
//! leaves, against plain ones kept in the leaves, and against a map.
//!
//! `cargo run --release --example var_bytes_lookups -- [entries]`: 10M by default.
//! Two key sets, each with random lookups that hit and that miss:
//!
//! - random `u64` keys and values: a map, a `BBTreeMapFile<u64, u64>`, which
//!   keeps them in their slots, and a file map of their 8 big-endian bytes,
//!   var-length, in heap pages after the leaves, looked up by those bytes:
//!   what keeping bytes apart costs;
//! - random text keys of 8 to 40 bytes, after a shared 5-byte prefix, with
//!   `u64` values: a map of `String`s and a `BBTreeMapFile<String, u64>`.
//!
//! `BBTreeMapFile`s are read into memory.

mod timed_lookups;

use bbtree::unsync::BBTreeMap;
use bbtree_disk::BBTreeMapFile;
use timed_lookups::{Numbers, TimedLookups};

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

impl Numbers {
    /// Text of 8 to 40 letters and digits after `user/`.
    fn text(&mut self) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let len = 8 + self.next() as usize % 33;
        let letters = (0..len).map(|_| ALPHABET[self.next() as usize % ALPHABET.len()] as char);
        "user/".chars().chain(letters).collect()
    }
}

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .map_or(10_000_000, |count| count.parse().unwrap());
    let mut numbers = Numbers::seeded();
    u64_lookups(count, &mut numbers);
    text_lookups(count, &mut numbers);
}

/// Random `u64` keys and values: a map, a `BBTreeMapFile`, which keeps them in
/// their slots, and a file map of var-length ones.
fn u64_lookups(count: usize, numbers: &mut Numbers) {
    let (map, lookups) = TimedLookups::of_u64_map(count, numbers);
    let path =
        std::env::temp_dir().join(format!("bbtree-var-bytes-lookups-{}", std::process::id()));
    let var_entries =
        (map.iter()).map(|(key, value)| (key.to_be_bytes().to_vec(), value.to_be_bytes().to_vec()));
    BBTreeMapFile::<Vec<u8>, Vec<u8>>::create(&path, var_entries).unwrap();
    let var_map = BBTreeMapFile::<Vec<u8>, Vec<u8>>::read(&path).unwrap();
    let var_len = std::fs::metadata(&path).unwrap().len();
    BBTreeMapFile::<u64, u64>::create(&path, &map).unwrap();
    let disk = BBTreeMapFile::<u64, u64>::read(&path).unwrap();
    let disk_len = std::fs::metadata(&path).unwrap().len();
    std::fs::remove_file(&path).unwrap();
    println!(
        "{count} u64 entries: in slots, {:.2} bytes an entry; var-length, {:.2}",
        disk_len as f64 / count as f64,
        var_len as f64 / count as f64
    );
    lookups.report("u64: map", |key| map.get(key).copied());
    lookups.report("u64: BBTreeMapFile<u64, u64>", |key| disk.get(key).copied());
    lookups.report("u64: file map, var-length", |key| {
        let value = var_map.get(key.to_be_bytes().as_slice())?;
        Some(u64::from_be_bytes(value.try_into().unwrap()))
    });
}

/// Random text keys with `u64` values: a map of `String`s, and a `BBTreeMapFile`.
fn text_lookups(count: usize, numbers: &mut Numbers) {
    let mut texts: Vec<String> = (0..count).map(|_| numbers.text()).collect();
    texts.sort_unstable();
    texts.dedup();
    let map: BBTreeMap<String, u64> = texts
        .iter()
        .map(|text| (text.clone(), text.len() as u64))
        .collect();
    let hits: Vec<String> = (0..count)
        .map(|_| texts[numbers.next() as usize % texts.len()].clone())
        .collect();
    // Absent: a byte no key has, at the end.
    let misses: Vec<String> = (0..count)
        .map(|_| texts[numbers.next() as usize % texts.len()].clone() + "!")
        .collect();
    drop(texts);
    let lookups = TimedLookups::new(hits, misses);
    let path = std::env::temp_dir().join(format!("bbtree-text-lookups-{}", std::process::id()));
    BBTreeMapFile::<String, u64>::create(&path, &map).unwrap();
    let disk = BBTreeMapFile::<String, u64>::read(&path).unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();
    std::fs::remove_file(&path).unwrap();
    println!(
        "{} text entries: a BBTreeMapFile of {:.2} bytes an entry",
        map.len(),
        file_len as f64 / map.len() as f64
    );
    lookups.report("text: map of String", |key| map.get(key.as_str()).copied());
    lookups.report("text: BBTreeMapFile<String, u64>", |key| {
        disk.get(key.as_str()).copied()
    });
}
