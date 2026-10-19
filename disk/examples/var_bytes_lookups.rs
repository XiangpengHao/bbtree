//! Lookups of var-length keys and values, kept in the leaves' tails
//! against plain ones kept in the leaves, and against a map.
//!
//! `cargo run --release --example var_bytes_lookups -- [entries]`: 10M by default.
//! Two key sets, each with random lookups that hit and that miss:
//!
//! - random `u64` keys and values: a map, a file map of plain `u64`s, a file map
//!   of their 8 big-endian bytes, var-length (a query buffer reused, as a
//!   loop of lookups would), and an `IndexFile<u64, u64>` (a query buffer
//!   per lookup);
//! - random text keys of 8 to 40 bytes, after a shared 5-byte prefix, with
//!   `u64` values: a map of `String`s and an `IndexFile<String, u64>`.
//!
//! File maps are read into memory, in huge pages, as a map's nodes are.

use std::hint::black_box;
use std::time::Instant;

use bbtree::WithInlineKey;
use bbtree::file::{FileMap, FileWriter, VarBytes, VarBytesBuf};
use bbtree::unsync::BBTreeMap;
use bbtree_disk::IndexFile;

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

    /// Text of 8 to 40 letters and digits after `user/`.
    fn text(&mut self) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let len = 8 + self.next() as usize % 33;
        let letters = (0..len).map(|_| ALPHABET[self.next() as usize % ALPHABET.len()] as char);
        "user/".chars().chain(letters).collect()
    }
}

/// Times one pass of `lookup` over `keys`, after an untimed pass over a
/// tenth of them; returns nanoseconds per lookup and the hits.
fn time<Q>(keys: &[Q], mut lookup: impl FnMut(&Q) -> Option<u64>) -> (f64, usize) {
    let warm = keys
        .iter()
        .take(keys.len() / 10)
        .filter_map(&mut lookup)
        .count();
    black_box(warm);
    let start = Instant::now();
    let (hits, sum) = keys.iter().fold((0, 0_u64), |(hits, sum), key| {
        match lookup(black_box(key)) {
            Some(value) => (hits + 1, sum.wrapping_add(value)),
            None => (hits, sum),
        }
    });
    black_box(sum);
    (start.elapsed().as_nanos() as f64 / keys.len() as f64, hits)
}

/// Times `lookup`'s hits and misses, and prints them on a line of their own.
fn report<Q>(name: &str, hits: &[Q], misses: &[Q], mut lookup: impl FnMut(&Q) -> Option<u64>) {
    let (hit_ns, found) = time(hits, &mut lookup);
    let (miss_ns, missed) = time(misses, &mut lookup);
    assert_eq!((found, missed), (hits.len(), 0), "{name}");
    println!("{name:<40} {hit_ns:>7.1} ns {miss_ns:>7.1} ns");
}

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .map_or(10_000_000, |count| count.parse().unwrap());
    let mut numbers = Numbers(0x5eed);
    println!("{:<40} {:>10} {:>10}", "", "get hit", "get miss");
    u64_lookups(count, &mut numbers);
    text_lookups(count, &mut numbers);
}

/// Random `u64` keys and values: a map, a file map of plain slots, one of
/// var-length ones, and an index file.
fn u64_lookups(count: usize, numbers: &mut Numbers) {
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

    let plain_bytes = FileWriter::from(&map).to_buf();
    let plain_map = FileMap::<u64, u64>::open(&plain_bytes).unwrap();
    let var_writer = FileWriter::<VarBytes, VarBytes, WithInlineKey>::from_ascending(
        map.iter().map(|(key, value)| {
            (
                key.to_be_bytes().to_vec().into(),
                value.to_be_bytes().to_vec().into(),
            )
        }),
    )
    .unwrap();
    let var_bytes = var_writer.to_buf();
    drop(var_writer);
    let var_map = FileMap::<VarBytes, VarBytes, WithInlineKey>::open(&var_bytes).unwrap();
    let path =
        std::env::temp_dir().join(format!("bbtree-var-bytes-lookups-{}", std::process::id()));
    IndexFile::create(&path, &map).unwrap();
    let file = IndexFile::<u64, u64>::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    println!(
        "{count} u64 entries: plain, {:.2} bytes an entry; var-length, {:.2}",
        plain_bytes.len() as f64 / count as f64,
        var_bytes.len() as f64 / count as f64
    );
    report("u64: map", &hits, &misses, |key| map.get(key).copied());
    report("u64: file map, plain", &hits, &misses, |key| {
        plain_map.get(key).copied()
    });
    let mut query = VarBytesBuf::new(b"");
    report("u64: file map, var-length", &hits, &misses, |key| {
        query.set(&key.to_be_bytes());
        let value = var_map.get(&*query)?;
        Some(u64::from_be_bytes(value.bytes().try_into().unwrap()))
    });
    report("u64: IndexFile<u64, u64>", &hits, &misses, |key| {
        file.get(key)
    });
}

/// Random text keys with `u64` values: a map of `String`s, and an index
/// file.
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
    let path = std::env::temp_dir().join(format!("bbtree-text-lookups-{}", std::process::id()));
    IndexFile::create(&path, &map).unwrap();
    let file = IndexFile::<String, u64>::read(&path).unwrap();
    let file_len = std::fs::metadata(&path).unwrap().len();
    std::fs::remove_file(&path).unwrap();
    println!(
        "{} text entries: an index file {:.2} bytes an entry",
        map.len(),
        file_len as f64 / map.len() as f64
    );
    report("text: map of String", &hits, &misses, |key| {
        map.get(key.as_str()).copied()
    });
    report("text: IndexFile<String, u64>", &hits, &misses, |key| {
        file.get(key.as_str())
    });
}
