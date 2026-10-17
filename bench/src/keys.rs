//! Key kinds (doc/02-field_study.md §6) and their deterministic generators,
//! and the key kinds read from real-world datasets (`datasets.rs`).

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use bbtree::{WithInlineKey, unsync};

use crate::cell_runner::{CellMeasurement, CellRun};
use crate::command_line::Named;
use crate::datasets::DataFile;
use crate::operations::PreparedCell;
use crate::random::{RandomStream, SeededRng};
use crate::values::BenchValue;
use crate::workload::MapSize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyKind {
    /// Random `u64`s: hashed IDs, cache keys.
    U64Random,
    /// `String`s as long as JSON object keys (median 7 B, p90 14 B).
    JsonString,
    /// `String`s like crate names with a version (median 16 B).
    CrateString,
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
        Self::JsonString,
        Self::CrateString,
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
            Self::JsonString => "str-json",
            Self::CrateString => "str-16",
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
            Self::U64Random | Self::JsonString | Self::CrateString => &[],
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

    fn is_generated(self) -> bool {
        self.data_files().is_empty()
    }

    /// Whether the keys are integers with an `InlineKey` impl, which only
    /// `bbtree-unsync-inline` uses.
    pub(crate) fn is_inline_keyed_integer(self) -> bool {
        matches!(self, Self::U64Random)
    }

    /// The sizes the keys come in. Generated keys come in every size whose
    /// shape is fixed, and JSON objects in their own shape. The small
    /// datasets' keys come in `all` and the fixed sizes they have enough keys
    /// for. The large ones' come in 1M and 10M, or `all` instead of 10M when
    /// they hold fewer keys than that.
    pub(crate) fn sizes(self) -> &'static [MapSize] {
        match self {
            Self::CrateNames | Self::KernelPaths => &[MapSize::OneThousand, MapSize::All],
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

    /// Measures `prepared` on `bbtree-unsync-inline`, an
    /// `unsync::BBTreeMap` whose keys have inline keys through their
    /// `InlineKey` impl: for the key types of
    /// [`KeyKind::is_inline_keyed_integer`] only.
    fn measure_with_inline_keys<V: BenchValue>(
        run: &CellRun,
        prepared: impl PreparedCell<Self, V>,
    ) -> Result<CellMeasurement, String> {
        let _ = (run, prepared);
        Err("bbtree-unsync-inline runs on integer keys only".to_owned())
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
        run.measure::<unsync::BBTreeMap<u64, V, WithInlineKey>, u64, V>(prepared)
    }
}

impl BenchKey for String {
    fn fingerprint(&self) -> u64 {
        self.len() as u64
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
}

impl KeyMaker<u64> {
    pub(crate) fn random_u64() -> Self {
        Self::new(SeededRng::mix)
    }
}

impl KeyMaker<String> {
    pub(crate) fn json_keys() -> Self {
        DistinctStrings::new(JsonKeyShape).into_key_maker()
    }

    pub(crate) fn crate_versions() -> Self {
        DistinctStrings::new(CrateVersionShape).into_key_maker()
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
/// cargo's package IDs).
struct CrateVersionShape;

impl CrateVersionShape {
    const NAME_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz-_";
}

impl StringShape for CrateVersionShape {
    fn candidate(&self, rng: &mut SeededRng) -> String {
        let name_length = rng.in_range(3..=15);
        let name = rng.ascii_text(name_length, Self::NAME_CHARACTERS);
        let (major, minor, patch) = (rng.below(3), rng.below(40), rng.below(20));
        format!("{name} {major}.{minor}.{patch}")
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
    fn makers_make_distinct_keys() {
        fn assert_distinct<K: BenchKey>(mut maker: KeyMaker<K>) {
            let mut keys: Vec<K> = (0..50_000).map(|index| maker.key(index)).collect();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), 50_000);
        }
        assert_distinct(KeyMaker::random_u64());
        assert_distinct(KeyMaker::json_keys());
        assert_distinct(KeyMaker::crate_versions());
    }
}
