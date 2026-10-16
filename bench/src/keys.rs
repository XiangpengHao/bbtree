//! Key kinds (doc/02-field_study.md §6) and their deterministic generators,
//! and the key kinds read from real-world datasets (`datasets.rs`).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use bbtree::{BBTreeMap, InlineKey, InlineWord, WithInlineKey};
use bytes::Bytes;

use crate::cell_runner::{CellMeasurement, CellRun};
use crate::command_line::Named;
use crate::datasets::DataFile;
use crate::map_adapters::CountedBCTree;
use crate::operations::PreparedCell;
use crate::random::{RandomStream, SeededRng};
use crate::values::BenchValue;
use crate::workload::MapSize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyKind {
    /// Random `u64`s: hashed IDs, cache keys.
    U64Random,
    /// Dense `u64`s: slots, page numbers, sequence numbers.
    U64Sequential,
    /// Random `u32`s: small IDs (`StateID`, `RowID`, indices).
    U32,
    /// `String`s as long as JSON object keys (median 7 B, p90 14 B).
    JsonString,
    /// `String`s like crate names with a version (median 16 B).
    CrateString,
    /// `String` paths into cargo's registry (median 103 B, p90 122 B), all
    /// behind the same 64-byte prefix.
    PathString,
    /// Random `[u8; 32]`s: hashes and public keys.
    Hash32,
    /// LSM memtable keys with 16-byte user keys.
    Memtable16,
    /// LSM memtable keys with 24-byte user keys.
    Memtable24,
    /// `String`s: every crate name on crates.io (median 12 B, 36% of sorted
    /// neighbours share their first 8 bytes).
    CrateNames,
    /// `String`s: the file paths of the Linux source tree (median 36 B, 99%
    /// of sorted neighbours share their first 8 bytes).
    KernelPaths,
    /// `String`s: English Wikipedia's article titles (median 18 B, 19M of
    /// them).
    WikiTitles,
    /// `String`s: the URLs of pages in a Common Crawl crawl, kept as crawled
    /// (`https://www.example.com/...`): long keys behind long shared
    /// prefixes. TODO(phase 2): statistics.
    Urls,
    /// `String`s: every French address, as the Base Adresse Nationale's
    /// geocoder labels them (`83bis Chemin du Peuple 01330
    /// Ambérieux-en-Dombes`; median 39 B, 97% of sorted neighbours share
    /// their first 8 bytes, 26M of them).
    Addresses,
    /// `String`s: the names of GeoNames' places. TODO(phase 2): statistics.
    GeoNames,
    /// `String`s: the titles of computer science publications in dblp
    /// (median 78 B, 87% of sorted neighbours share their first 8 bytes,
    /// 8.4M of them).
    DblpTitles,
    /// `String`s: the keys of the objects of real JSON documents, each
    /// object a tiny map (median 2 keys of 8 B).
    JsonObjectKeys,
}

impl Named for KeyKind {
    const ALL: &'static [Self] = &[
        Self::U64Random,
        Self::U64Sequential,
        Self::U32,
        Self::JsonString,
        Self::CrateString,
        Self::PathString,
        Self::Hash32,
        Self::Memtable16,
        Self::Memtable24,
        Self::CrateNames,
        Self::KernelPaths,
        Self::WikiTitles,
        Self::Urls,
        Self::Addresses,
        Self::GeoNames,
        Self::DblpTitles,
        Self::JsonObjectKeys,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::U64Random => "u64",
            Self::U64Sequential => "u64-seq",
            Self::U32 => "u32",
            Self::JsonString => "str-json",
            Self::CrateString => "str-16",
            Self::PathString => "str-path",
            Self::Hash32 => "hash32",
            Self::Memtable16 => "memtable16",
            Self::Memtable24 => "memtable24",
            Self::CrateNames => "crate-names",
            Self::KernelPaths => "kernel-paths",
            Self::WikiTitles => "wiki-titles",
            Self::Urls => "urls",
            Self::Addresses => "addresses",
            Self::GeoNames => "geonames",
            Self::DblpTitles => "dblp-titles",
            Self::JsonObjectKeys => "json-keys",
        }
    }
}

impl KeyKind {
    /// The data files the keys are read from: none for generated keys.
    pub(crate) fn data_files(self) -> &'static [DataFile] {
        match self {
            Self::U64Random
            | Self::U64Sequential
            | Self::U32
            | Self::JsonString
            | Self::CrateString
            | Self::PathString
            | Self::Hash32
            | Self::Memtable16
            | Self::Memtable24 => &[],
            Self::CrateNames => &[DataFile::CrateNames],
            Self::KernelPaths => &[DataFile::KernelPaths],
            Self::WikiTitles => &[DataFile::WikiTitles],
            Self::Urls => &[DataFile::CommonCrawlUrls],
            Self::Addresses => &[DataFile::BanAddresses],
            Self::GeoNames => &[DataFile::GeoNames],
            Self::DblpTitles => &[DataFile::DblpTitles],
            Self::JsonObjectKeys => &[DataFile::JsonObjectKeys, DataFile::JsonKeyVocabulary],
        }
    }

    pub(crate) fn is_generated(self) -> bool {
        self.data_files().is_empty()
    }

    /// Whether the keys are integers with an `InlineKey` impl, which only
    /// `bbtree-inline` uses.
    pub(crate) fn is_inline_keyed_integer(self) -> bool {
        matches!(self, Self::U64Random | Self::U64Sequential)
    }

    /// The sizes the keys come in. Generated keys come in every size whose
    /// shape is fixed, and JSON objects in their own shape. The small
    /// datasets' keys come in `all` and the fixed sizes they have enough keys
    /// for. The large ones' come in 1M and 10M, or `all` instead of 10M when
    /// they hold fewer keys than that.
    pub(crate) fn sizes(self) -> &'static [MapSize] {
        match self {
            Self::CrateNames => &[MapSize::OneThousand, MapSize::HundredThousand, MapSize::All],
            Self::KernelPaths => &[MapSize::OneThousand, MapSize::All],
            Self::WikiTitles | Self::Urls | Self::Addresses => &[
                MapSize::TenThousand,
                MapSize::OneMillion,
                MapSize::TenMillion,
            ],
            // TODO(phase 2): GeoNames' count decides whether 10M or `all`.
            Self::GeoNames | Self::DblpTitles => &[MapSize::OneMillion, MapSize::All],
            Self::JsonObjectKeys => &[MapSize::RealJsonObjects],
            generated => {
                assert!(generated.is_generated(), "{} has no sizes", self.name());
                MapSize::FIXED_SHAPES
            }
        }
    }
}

/// What the benchmark needs from a key type. Setup shares keys with, and
/// sends them between, its worker threads (`parallel_setup.rs`).
pub(crate) trait BenchKey: Ord + Hash + Clone + Send + Sync + 'static {
    /// A cheap read of the key, folded into checksums so that the optimizer
    /// can't skip visiting it.
    fn fingerprint(&self) -> u64;

    /// Measures `prepared` on `bbtree`, a `BBTreeMap` set up as users of the
    /// key type would set it up: with the default strategy, `Auto`, unless
    /// the impl says otherwise.
    fn measure_on_bbtree<V: BenchValue>(
        run: &CellRun,
        prepared: impl PreparedCell<Self, V>,
    ) -> Result<CellMeasurement, String> {
        run.measure::<BBTreeMap<Self, V>, Self, V>(prepared)
    }

    /// Measures `prepared` on `bctree`, a `BCTreeMap` with the strategy
    /// `bbtree` uses for the key type, on one thread.
    fn measure_on_bctree<V: BenchValue>(
        run: &CellRun,
        prepared: impl PreparedCell<Self, V>,
    ) -> Result<CellMeasurement, String> {
        run.measure::<CountedBCTree<Self, V>, Self, V>(prepared)
    }

    /// Measures `prepared` on `bbtree-inline`, a `BBTreeMap` whose keys have
    /// inline keys through their `InlineKey` impl: for the key types of
    /// [`KeyKind::is_inline_keyed_integer`] only.
    fn measure_with_inline_keys<V: BenchValue>(
        run: &CellRun,
        prepared: impl PreparedCell<Self, V>,
    ) -> Result<CellMeasurement, String> {
        let _ = (run, prepared);
        Err("bbtree-inline runs on integer keys only".to_owned())
    }
}

impl BenchKey for u64 {
    fn fingerprint(&self) -> u64 {
        *self
    }

    fn measure_with_inline_keys<V: BenchValue>(
        run: &CellRun,
        prepared: impl PreparedCell<Self, V>,
    ) -> Result<CellMeasurement, String> {
        run.measure::<BBTreeMap<u64, V, WithInlineKey>, u64, V>(prepared)
    }
}

impl BenchKey for u32 {
    fn fingerprint(&self) -> u64 {
        u64::from(*self)
    }
}

impl BenchKey for String {
    fn fingerprint(&self) -> u64 {
        self.len() as u64
    }
}

impl BenchKey for [u8; 32] {
    fn fingerprint(&self) -> u64 {
        u64::from(self[0])
    }
}

impl BenchKey for SequencedKey {
    fn fingerprint(&self) -> u64 {
        self.sequence
    }

    /// With `WithInlineKey`: `Auto` gives inline keys to std byte strings
    /// only, and a memtable on bbtree would give its keys theirs.
    fn measure_on_bbtree<V: BenchValue>(
        run: &CellRun,
        prepared: impl PreparedCell<Self, V>,
    ) -> Result<CellMeasurement, String> {
        run.measure::<BBTreeMap<Self, V, WithInlineKey>, Self, V>(prepared)
    }

    fn measure_on_bctree<V: BenchValue>(
        run: &CellRun,
        prepared: impl PreparedCell<Self, V>,
    ) -> Result<CellMeasurement, String> {
        run.measure::<CountedBCTree<Self, V, WithInlineKey>, Self, V>(prepared)
    }
}

/// An LSM memtable key, like SlateDB's `SequencedKey`: ordered by user key
/// ascending, then by sequence number descending, so that a user key's
/// newest version comes first.
#[derive(PartialEq, Eq, Hash, Debug)]
pub(crate) struct SequencedKey {
    user_key: Bytes,
    sequence: u64,
}

impl Ord for SequencedKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.user_key
            .cmp(&other.user_key)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for SequencedKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The encoding is the user key's bytes, which order like the keys; keys of
/// equal user keys tie, for `Ord` to order by sequence number. Appending the
/// sequence number would misorder user keys of different lengths.
impl InlineKey for SequencedKey {
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.user_key[..].inline_key_at(offset)
    }
}

/// A deep copy. The benchmark clones keys to make independent inputs, and a
/// memtable owns its keys' bytes; `Bytes::clone` would share them instead,
/// behind an atomic reference count.
impl Clone for SequencedKey {
    fn clone(&self) -> Self {
        Self {
            user_key: Bytes::copy_from_slice(&self.user_key),
            sequence: self.sequence,
        }
    }
}

/// Makes the keys of one kind: distinct indices give distinct keys.
///
/// Workloads insert the keys at even indices and use odd ones for keys that
/// are never inserted, so that both sets spread over the same key range.
/// Makers of random content ignore the index and make a new distinct key at
/// every call, which serves the same purpose.
pub(crate) struct KeyMaker<K> {
    make: Box<dyn FnMut(u64) -> K>,
}

impl<K> KeyMaker<K> {
    fn new(make: impl FnMut(u64) -> K + 'static) -> Self {
        Self {
            make: Box::new(make),
        }
    }

    pub(crate) fn key(&mut self, index: u64) -> K {
        (self.make)(index)
    }

    /// Fills `bytes` with random bytes behind an 8-byte prefix that is a
    /// bijection of `index`: distinct indices give distinct byte strings,
    /// all equally random-looking.
    fn fill_distinct_bytes(index: u64, rng: &mut SeededRng, bytes: &mut [u8]) {
        let (prefix, rest) = bytes.split_at_mut(size_of::<u64>());
        prefix.copy_from_slice(&SeededRng::mix(index).to_be_bytes());
        for byte in rest {
            *byte = rng.next_u64().to_le_bytes()[0];
        }
    }
}

impl KeyMaker<u64> {
    pub(crate) fn random_u64() -> Self {
        Self::new(SeededRng::mix)
    }

    pub(crate) fn sequential_u64() -> Self {
        Self::new(|index| index)
    }
}

impl KeyMaker<u32> {
    pub(crate) fn random_u32() -> Self {
        Self::new(|index| Self::mix32(u32::try_from(index).expect("fewer than 2^32 keys")))
    }

    /// MurmurHash3's finalizer: a bijection on `u32`.
    fn mix32(mut x: u32) -> u32 {
        x ^= x >> 16;
        x = x.wrapping_mul(0x85EB_CA6B);
        x ^= x >> 13;
        x = x.wrapping_mul(0xC2B2_AE35);
        x ^ (x >> 16)
    }
}

impl KeyMaker<[u8; 32]> {
    pub(crate) fn random_hashes() -> Self {
        let mut rng = SeededRng::new(RandomStream::KeyContent);
        Self::new(move |index| {
            let mut hash = [0; 32];
            Self::fill_distinct_bytes(index, &mut rng, &mut hash);
            hash
        })
    }
}

impl KeyMaker<SequencedKey> {
    /// Memtable keys whose user keys are `user_key_length` random bytes.
    /// User keys are distinct, so the sequence number only adds the size
    /// and the tie-break a memtable key has.
    pub(crate) fn memtable(user_key_length: usize) -> Self {
        let mut rng = SeededRng::new(RandomStream::KeyContent);
        Self::new(move |index| {
            let mut user_key = vec![0; user_key_length];
            Self::fill_distinct_bytes(index, &mut rng, &mut user_key);
            SequencedKey {
                user_key: Bytes::from(user_key),
                sequence: index,
            }
        })
    }
}

impl KeyMaker<String> {
    pub(crate) fn json_keys() -> Self {
        DistinctStrings::new(JsonKeyShape).into_key_maker()
    }

    pub(crate) fn crate_versions() -> Self {
        DistinctStrings::new(CrateVersionShape { separator: ' ' }).into_key_maker()
    }

    /// Paths for about `expected_keys` keys, which sets how many crate
    /// directories they share.
    pub(crate) fn registry_paths(expected_keys: usize) -> Self {
        let mut rng = SeededRng::new(RandomStream::KeyContent);
        DistinctStrings::new(RegistryPathShape::new(expected_keys, &mut rng)).into_key_maker()
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
            rng: SeededRng::new(RandomStream::KeyContent),
            seen: HashSet::new(),
        }
    }

    fn into_key_maker(mut self) -> KeyMaker<String> {
        KeyMaker::new(move |_index| self.next_distinct())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lengths(mut maker: KeyMaker<String>, count: u64) -> Vec<usize> {
        let mut lengths: Vec<usize> = (0..count).map(|index| maker.key(index).len()).collect();
        lengths.sort_unstable();
        lengths
    }

    fn percentile(sorted: &[usize], percent: usize) -> usize {
        sorted[sorted.len() * percent / 100]
    }

    #[test]
    fn json_keys_follow_the_measured_lengths() {
        let lengths = lengths(KeyMaker::json_keys(), 100_000);
        assert_eq!(percentile(&lengths, 50), 7);
        assert_eq!(percentile(&lengths, 90), 14);
    }

    #[test]
    fn crate_versions_are_about_16_bytes() {
        let lengths = lengths(KeyMaker::crate_versions(), 100_000);
        assert!((15..=17).contains(&percentile(&lengths, 50)));
    }

    #[test]
    fn paths_match_the_registry_distribution() {
        let lengths = lengths(KeyMaker::registry_paths(100_000), 100_000);
        let (median, p90) = (percentile(&lengths, 50), percentile(&lengths, 90));
        assert!((100..=106).contains(&median), "median {median}");
        assert!((118..=126).contains(&p90), "p90 {p90}");
    }

    #[test]
    fn makers_make_distinct_keys() {
        fn assert_distinct<K: BenchKey>(mut maker: KeyMaker<K>) {
            let mut keys: Vec<K> = (0..50_000).map(|index| maker.key(index)).collect();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), 50_000);
        }
        assert_distinct(KeyMaker::random_u64());
        assert_distinct(KeyMaker::sequential_u64());
        assert_distinct(KeyMaker::random_u32());
        assert_distinct(KeyMaker::json_keys());
        assert_distinct(KeyMaker::crate_versions());
        assert_distinct(KeyMaker::registry_paths(50_000));
        assert_distinct(KeyMaker::random_hashes());
        assert_distinct(KeyMaker::memtable(16));
    }

    #[test]
    fn memtable_keys_put_the_newest_version_first() {
        let older = SequencedKey {
            user_key: Bytes::from_static(b"user"),
            sequence: 1,
        };
        let newer = SequencedKey {
            user_key: Bytes::from_static(b"user"),
            sequence: 2,
        };
        let next_user_key = SequencedKey {
            user_key: Bytes::from_static(b"users"),
            sequence: 0,
        };
        assert!(newer < older);
        assert!(older < next_user_key);
    }

    #[test]
    fn memtable_inline_keys_order_like_the_keys() {
        let user = |bytes: &'static [u8], sequence| SequencedKey {
            user_key: Bytes::from_static(bytes),
            sequence,
        };
        let mut keys = vec![user(b"user", 1), user(b"user", 2), user(b"users", 0)];
        let mut maker = KeyMaker::memtable(16);
        keys.extend((0..1000).map(|index| maker.key(index)));
        for a in &keys {
            for b in &keys {
                let inline_order = a.inline_key_at(0).cmp(&b.inline_key_at(0));
                assert!(inline_order.is_eq() || inline_order == a.cmp(b));
            }
        }
    }
}
