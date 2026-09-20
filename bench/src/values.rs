//! Value kinds (doc/02-field_study.md §6) and their deterministic
//! generators.

use crate::command_line::Named;
use crate::random::SeededRng;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ValueKind {
    /// `()`: sets, 41% of `BTreeMap`/`BTreeSet` mentions.
    Unit,
    /// `u64`: counters, IDs.
    U64,
    /// `[u64; 4]`: a 32-byte struct, `serde_json::Value`-sized.
    Struct32,
    /// `String` of 16–32 bytes: string-to-string maps.
    String,
    /// `Box<[u8]>` of 100 bytes: a KV record (RocksDB's `db_bench`).
    Bytes100,
    /// `Box<[u8]>` of 1 KB: a KV record (YCSB).
    Bytes1k,
}

impl Named for ValueKind {
    const ALL: &'static [Self] = &[
        Self::Unit,
        Self::U64,
        Self::Struct32,
        Self::String,
        Self::Bytes100,
        Self::Bytes1k,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::U64 => "u64",
            Self::Struct32 => "struct32",
            Self::String => "string",
            Self::Bytes100 => "bytes100",
            Self::Bytes1k => "bytes1k",
        }
    }
}

/// What the benchmark needs from a value type.
pub(crate) trait BenchValue: Default + 'static {
    /// A cheap read of the value, folded into checksums so that the
    /// optimizer can't skip visiting it.
    fn fingerprint(&self) -> u64;

    /// The update of the counter-increment pattern (`entry_upsert`):
    /// `+= 1` on a number; values without one stay as they are.
    fn bump(&mut self);
}

impl BenchValue for () {
    fn fingerprint(&self) -> u64 {
        0
    }

    fn bump(&mut self) {}
}

impl BenchValue for u64 {
    fn fingerprint(&self) -> u64 {
        *self
    }

    fn bump(&mut self) {
        *self += 1;
    }
}

impl BenchValue for [u64; 4] {
    fn fingerprint(&self) -> u64 {
        self[0]
    }

    fn bump(&mut self) {
        self[0] += 1;
    }
}

impl BenchValue for String {
    fn fingerprint(&self) -> u64 {
        self.len() as u64
    }

    fn bump(&mut self) {}
}

impl BenchValue for Box<[u8]> {
    fn fingerprint(&self) -> u64 {
        self.len() as u64
    }

    fn bump(&mut self) {}
}

/// Makes the value of each entry from its index.
pub(crate) struct ValueMaker<V> {
    make: Box<dyn Fn(u64) -> V>,
}

impl<V> ValueMaker<V> {
    fn new(make: impl Fn(u64) -> V + 'static) -> Self {
        Self {
            make: Box::new(make),
        }
    }

    pub(crate) fn value(&self, index: u64) -> V {
        (self.make)(index)
    }
}

impl ValueMaker<()> {
    pub(crate) fn unit() -> Self {
        Self::new(|_index| ())
    }
}

impl ValueMaker<u64> {
    pub(crate) fn counters() -> Self {
        Self::new(|index| index)
    }
}

impl ValueMaker<[u64; 4]> {
    pub(crate) fn structs() -> Self {
        Self::new(|index| [index, !index, SeededRng::mix(index), 0])
    }
}

impl ValueMaker<String> {
    /// 16 to 32 hexadecimal digits, so each value is its own heap block of
    /// exactly its length.
    pub(crate) fn short_strings() -> Self {
        Self::new(|index| {
            let digits = SeededRng::mix(index);
            let mut text = format!("{digits:016x}{:016x}", !digits);
            text.truncate(16 + (digits % 17) as usize);
            text.shrink_to_fit();
            text
        })
    }
}

impl ValueMaker<Box<[u8]>> {
    pub(crate) fn byte_records(length: usize) -> Self {
        Self::new(move |index| vec![index.to_le_bytes()[0]; length].into_boxed_slice())
    }
}
