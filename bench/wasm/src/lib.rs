//! The architectures card's gets in WebAssembly (`wasm32-unknown-unknown`,
//! its default features): bbtree against std's `BTreeMap`, one thread,
//! lookups half of which miss, on the keys the native bench made and
//! dumped (`bbtree-scaling --dump-keys`), timed by the host's clock. The
//! host (`run.mjs`, in Node) copies a dump into the module's memory and
//! calls [`measure`] once a map; the cells are `blog-runs.sh arch`'s.

use std::collections::BTreeMap;
use std::hint::black_box;

use bbtree::BBTreeMap;

#[link(wasm_import_module = "env")]
unsafe extern "C" {
    /// The host's clock, in milliseconds: `performance.now()`.
    safe fn now_ms() -> f64;
}

/// Room for `len` bytes, for the host to write a dump or read a result
/// in. Never freed: the module lives for one run.
#[unsafe(no_mangle)]
pub extern "C" fn reserve(len: usize) -> *mut u8 {
    Vec::<u8>::with_capacity(len).leak().as_mut_ptr()
}

/// Times gets on `map` (0: bbtree, 1: std's `BTreeMap`) for `seconds`,
/// with the dump of `len` bytes at `dump` of keys of `kind` (0: `u64`, 1:
/// UUIDs, 2: strings), and writes the operations, the nanoseconds they
/// took and how many hit, three `f64`s, at `out`.
///
/// # Safety
///
/// `dump` holds `len` bytes the host wrote, and `out` has room for three
/// `f64`s, unaligned; both in room from [`reserve`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn measure(
    kind: u32,
    map: u32,
    dump: *const u8,
    len: usize,
    seconds: f64,
    out: *mut f64,
) {
    // SAFETY: the caller's.
    let dump = unsafe { std::slice::from_raw_parts(dump, len) };
    let text = std::str::from_utf8(dump).expect("a dump is text");
    let map = MeasuredMap::from_code(map);
    let cell = match kind {
        0 => KeyDump::parse(text, |line| line.parse::<u64>().expect("a u64")).measure(map, seconds),
        1 => KeyDump::parse(text, KeyDump::<[u8; 16]>::uuid).measure(map, seconds),
        2 => KeyDump::parse(text, str::to_string).measure(map, seconds),
        _ => panic!("no key kind {kind}"),
    };
    // SAFETY: the caller's.
    unsafe {
        out.cast::<[f64; 3]>()
            .write_unaligned([cell.operations, cell.elapsed_ns, cell.hits])
    };
}

/// The two maps compared.
#[derive(Clone, Copy, PartialEq, Debug)]
enum MeasuredMap {
    BBTree,
    StdBTree,
}

impl MeasuredMap {
    fn from_code(code: u32) -> Self {
        match code {
            0 => Self::BBTree,
            1 => Self::StdBTree,
            _ => panic!("no map {code}"),
        }
    }
}

/// What a map's gets did.
struct GetCell {
    operations: f64,
    elapsed_ns: f64,
    hits: f64,
}

/// A run's keys, as the native bench dumped them: those the map holds, in
/// the order they go in, and others for lookups that miss.
struct KeyDump<K> {
    present: Vec<K>,
    absent: Vec<K>,
}

impl KeyDump<[u8; 16]> {
    /// A UUID from its 32 hex digits.
    fn uuid(line: &str) -> [u8; 16] {
        std::array::from_fn(|index| {
            u8::from_str_radix(&line[2 * index..2 * index + 2], 16).expect("hex")
        })
    }
}

impl<K: Ord + Clone> KeyDump<K> {
    /// The dump's keys, each line read by `key`, after the first line's
    /// counts of present and absent keys.
    fn parse(text: &str, key: impl Fn(&str) -> K) -> Self {
        let mut lines = text.lines();
        let counts = lines.next().expect("a dump starts with its counts");
        let (present, absent) = counts.split_once(' ').expect("two counts");
        let present: usize = present.parse().expect("a count");
        let absent: usize = absent.parse().expect("a count");
        let mut keys = lines.map(key);
        Self {
            present: keys.by_ref().take(present).collect(),
            absent: keys.take(absent).collect(),
        }
    }

    /// As many keys the map holds as it doesn't, in random order, as the
    /// native bench's `get-half` draws them.
    fn queries(&self) -> Vec<K> {
        let count = self.absent.len().min(self.present.len());
        let mut queries: Vec<K> = (self.present[..count].iter())
            .chain(&self.absent[..count])
            .cloned()
            .collect();
        // Fisher–Yates, by a fixed SplitMix64 sequence.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for index in (1..queries.len()).rev() {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut mixed = state;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            mixed ^= mixed >> 31;
            queries.swap(index, (mixed % (index as u64 + 1)) as usize);
        }
        queries
    }

    /// Builds `map` from the present keys, in their order, then times
    /// gets of [`Self::queries`] for `seconds`.
    fn measure(self, map: MeasuredMap, seconds: f64) -> GetCell {
        let queries = self.queries();
        let values = (0..).map(|value: u64| value);
        match map {
            MeasuredMap::BBTree => {
                let map: BBTreeMap<K, u64> = self.present.into_iter().zip(values).collect();
                Self::time_gets(&queries, seconds, |key| map.get(key).copied())
            }
            MeasuredMap::StdBTree => {
                let map: BTreeMap<K, u64> = self.present.into_iter().zip(values).collect();
                Self::time_gets(&queries, seconds, |key| map.get(key).copied())
            }
        }
    }

    /// Gets of `queries`, round and round, until `seconds` have passed,
    /// the clock read every [`CHUNK`] gets.
    fn time_gets(queries: &[K], seconds: f64, get: impl Fn(&K) -> Option<u64>) -> GetCell {
        const CHUNK: usize = 4096;
        let (mut operations, mut hits, mut checksum) = (0u64, 0u64, 0u64);
        let mut next = 0;
        let started = now_ms();
        let mut elapsed_ms = 0.0;
        while elapsed_ms < seconds * 1e3 {
            for _ in 0..CHUNK {
                if let Some(value) = get(&queries[next]) {
                    hits += 1;
                    checksum = checksum.wrapping_add(value);
                }
                next = if next + 1 == queries.len() {
                    0
                } else {
                    next + 1
                };
            }
            operations += CHUNK as u64;
            elapsed_ms = now_ms() - started;
        }
        black_box(checksum);
        GetCell {
            operations: operations as f64,
            elapsed_ns: elapsed_ms * 1e6,
            hits: hits as f64,
        }
    }
}
