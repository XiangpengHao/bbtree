//! Value kinds (doc/02-field_study.md §6) and their deterministic
//! generators.

use crate::command_line::Named;
use crate::random::SeededRng;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ValueKind {
    /// `u64`: counters, IDs.
    U64,
    /// `[u64; 4]`: a 32-byte struct, `serde_json::Value`-sized.
    Struct32,
}

impl Named for ValueKind {
    const ALL: &'static [Self] = &[Self::U64, Self::Struct32];

    fn name(self) -> &'static str {
        match self {
            Self::U64 => "u64",
            Self::Struct32 => "struct32",
        }
    }
}

/// What the benchmark needs from a value type. Setup makes values on worker
/// threads (`parallel_setup.rs`).
pub(crate) trait BenchValue: Default + Send + Sync + 'static {
    /// A cheap read of the value, folded into checksums so that the
    /// optimizer can't skip visiting it.
    fn fingerprint(&self) -> u64;

    /// The update of the counter-increment pattern (`entry_upsert`):
    /// `+= 1` on a number, or on a struct's first field.
    fn bump(&mut self);
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

/// Makes the value of each entry from its index, on any thread.
pub(crate) struct ValueMaker<V> {
    make: Box<dyn Fn(u64) -> V + Send + Sync>,
}

impl<V> ValueMaker<V> {
    fn new(make: impl Fn(u64) -> V + Send + Sync + 'static) -> Self {
        Self {
            make: Box::new(make),
        }
    }

    pub(crate) fn value(&self, index: u64) -> V {
        (self.make)(index)
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
