//! JSON and registry-path shapes copied from bench/src/keys.rs, with the
//! KeyContent SplitMix64 stream from bench/src/random.rs (seed = mix(0)).
//! Keep this dependency-free diagnostic's shapes in sync with those files.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::RangeInclusive;

/// Owns every allocation while the timed workload copies only `&str`s.
pub(super) struct StringKeys {
    keys: Vec<String>,
}

impl StringKeys {
    pub(super) fn new(kind: &str, size: usize, operation: &str) -> Self {
        let extra = match operation {
            "get_miss" => size,
            "random_insert" => size / 4,
            _ => 0,
        };
        let count = size.checked_add(extra).expect("key count fits usize");
        let keys = match kind {
            "borrowed-json" => Self::generate(JsonKeyShape, count),
            "borrowed-path" => {
                let shape = RegistryPathShape::new(size, &mut SeededRng::new());
                Self::generate(shape, count)
            }
            _ => unreachable!("validated string kind"),
        };
        Self { keys }
    }

    fn generate(shape: impl StringShape + 'static, count: usize) -> Vec<String> {
        let mut generator = DistinctStrings::new(shape);
        (0..count).map(|_| generator.next_distinct()).collect()
    }

    pub(super) fn key(&self, index: usize) -> &str {
        &self.keys[index]
    }
}

/// SplitMix64 (Steele, Lea and Flood, 2014): small, fast, and random enough
/// for benchmark data.
struct SeededRng {
    state: u64,
}

impl SeededRng {
    const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

    fn new() -> Self {
        Self {
            state: Self::mix(0),
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(Self::GOLDEN_GAMMA);
        Self::mix(self.state)
    }

    /// A number in `0..bound`, by Lemire's multiply-and-shift; the bias is
    /// below 2⁻³² for any bound the benchmark uses.
    fn below(&mut self, bound: usize) -> usize {
        let bound = bound as u128;
        ((u128::from(self.next_u64()) * bound) >> 64) as usize
    }

    fn in_range(&mut self, range: RangeInclusive<usize>) -> usize {
        range.start() + self.below(range.end() - range.start() + 1)
    }

    /// One of `table`'s values, each as likely as its weight.
    fn weighted<T: Copy>(&mut self, table: &[(T, u32)]) -> T {
        let total: u32 = table.iter().map(|&(_, weight)| weight).sum();
        let mut ticket = self.below(total as usize) as u32;
        for &(value, weight) in table {
            if ticket < weight {
                return value;
            }
            ticket -= weight;
        }
        unreachable!("the ticket is below the total weight")
    }

    /// `length` characters drawn from `alphabet`, which must be ASCII.
    fn ascii_text(&mut self, length: usize, alphabet: &[u8]) -> String {
        (0..length)
            .map(|_| char::from(alphabet[self.below(alphabet.len())]))
            .collect()
    }

    /// SplitMix64's output function. It is a bijection on `u64`, so distinct
    /// inputs give distinct, random-looking outputs.
    fn mix(mut x: u64) -> u64 {
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }
}

/// How the candidates of one string key kind look.
trait StringShape {
    fn candidate(&self, rng: &mut SeededRng) -> String;
}

/// Strings of one shape, made distinct: a candidate seen before gets random
/// identifier characters appended until it is new. Short JSON keys collide
/// often (there are only 63² two-character ones), the other shapes rarely.
struct DistinctStrings<Shape> {
    shape: Shape,
    rng: SeededRng,
    seen: HashSet<u64>,
}

impl<Shape: StringShape + 'static> DistinctStrings<Shape> {
    const TAIL_CHARACTERS: &[u8] =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";

    fn new(shape: Shape) -> Self {
        Self {
            shape,
            rng: SeededRng::new(),
            seen: HashSet::new(),
        }
    }

    fn next_distinct(&mut self) -> String {
        let mut key = self.shape.candidate(&mut self.rng);
        // The set holds hashes rather than strings: a hash collision only
        // lengthens a key that was already new, which is harmless.
        while !self.seen.insert(Self::content_hash(&key)) {
            key.push_str(&self.rng.ascii_text(1, Self::TAIL_CHARACTERS));
        }
        key
    }

    /// SipHash with fixed keys, so every run makes the same strings.
    fn content_hash(key: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }
}

/// JSON object keys: identifier characters, with the lengths measured in the
/// field study.
struct JsonKeyShape;

impl JsonKeyShape {
    const CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";

    /// Key lengths in 19k JSON objects from 6 documents: (bytes, keys).
    const LENGTH_HISTOGRAM: [(usize, u32); 29] = [
        (1, 80),
        (2, 1440),
        (3, 2628),
        (4, 8867),
        (5, 6693),
        (6, 13055),
        (7, 3573),
        (8, 12255),
        (9, 2747),
        (10, 2597),
        (11, 4126),
        (12, 2288),
        (13, 1785),
        (14, 2797),
        (15, 1039),
        (16, 638),
        (17, 607),
        (18, 584),
        (19, 449),
        (20, 242),
        (21, 1254),
        (22, 175),
        (23, 739),
        (24, 207),
        (25, 173),
        (26, 173),
        (27, 1),
        (28, 520),
        (34, 173),
    ];
}

impl StringShape for JsonKeyShape {
    fn candidate(&self, rng: &mut SeededRng) -> String {
        let length = rng.weighted(&Self::LENGTH_HISTOGRAM);
        rng.ascii_text(length, Self::CHARACTERS)
    }
}

/// A crate name and a version, `serde 1.0.210`-like (median 16 B, as
/// cargo's package IDs), or `serde-1.0.210` as a registry directory.
struct CrateVersionShape {
    separator: char,
}

impl CrateVersionShape {
    const NAME_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz-_";
}

impl StringShape for CrateVersionShape {
    fn candidate(&self, rng: &mut SeededRng) -> String {
        let name_length = rng.in_range(3..=15);
        let name = rng.ascii_text(name_length, Self::NAME_CHARACTERS);
        let (major, minor, patch) = (rng.below(3), rng.below(40), rng.below(20));
        format!("{name}{}{major}.{minor}.{patch}", self.separator)
    }
}

/// Absolute source paths in cargo's registry: a 64-byte prefix every key
/// shares, a crate directory about `FILES_PER_CRATE` keys share, then module
/// directories and a file name. The shared prefixes make every comparison
/// read 64 bytes or more.
struct RegistryPathShape {
    crate_directories: Vec<String>,
}

impl RegistryPathShape {
    const PREFIX: &str = "/home/user/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/";

    /// About 30 in the field study: 15k source paths from 500 crates.
    const FILES_PER_CRATE: usize = 32;

    /// How many module directories a file is in, weighted.
    const MODULE_DEPTHS: [(usize, u32); 4] = [(0, 6), (1, 6), (2, 5), (3, 3)];

    const PATH_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz_";

    fn new(expected_keys: usize, rng: &mut SeededRng) -> Self {
        let crate_directory = CrateVersionShape { separator: '-' };
        let crate_count = expected_keys.div_ceil(Self::FILES_PER_CRATE);
        Self {
            crate_directories: (0..crate_count)
                .map(|_| crate_directory.candidate(rng))
                .collect(),
        }
    }

    fn push_path_component(path: &mut String, rng: &mut SeededRng, length: usize) {
        path.push_str(&rng.ascii_text(length, Self::PATH_CHARACTERS));
    }
}

impl StringShape for RegistryPathShape {
    fn candidate(&self, rng: &mut SeededRng) -> String {
        let mut path = String::from(Self::PREFIX);
        path.push_str(&self.crate_directories[rng.below(self.crate_directories.len())]);
        path.push_str("/src/");
        for _ in 0..rng.weighted(&Self::MODULE_DEPTHS) {
            let length = rng.in_range(3..=14);
            Self::push_path_component(&mut path, rng, length);
            path.push('/');
        }
        let length = rng.in_range(3..=12);
        Self::push_path_component(&mut path, rng, length);
        path.push_str(".rs");
        path
    }
}
