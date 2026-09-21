//! The suites: lists of workloads, each of which runs on every map. Together
//! they cover doc/02-field_study.md §6 without its full cross product.

use crate::command_line::{CellFilter, Named};
use crate::keys::KeyKind;
use crate::map_adapters::MapKind;
use crate::operations::Operation;
use crate::values::ValueKind;
use crate::workload::{MapSize, Workload};

/// A suite runs within 10 minutes at full scale, one repetition, on the
/// benchmark machine (Ryzen 9 9900X).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Suite {
    /// `u64 → u64` at 10M, every operation.
    Headline,
    /// Each generated key kind → `u64`, at 1M, and the fixed-size keys and
    /// JSON keys at 10M too: inserts and lookups.
    Keys,
    /// Each large real-world key set → `u64`, at 1M and 10M (`all` for sets
    /// of fewer keys), on the ordered maps: inserts and lookups.
    RealKeys,
    /// `u64` → each value kind, at 1M: inserts and lookups.
    Values,
    /// Many tiny JSON-object-like maps: JSON keys → 32-byte structs, in
    /// maps of generated keys and in the objects of real JSON documents.
    Tiny,
    /// LSM memtables: `(Bytes, u64)` → 100-byte records, at 1M.
    Memtable,
    /// `u64` and JSON-key sets, at 1M.
    Sets,
    /// `u64 → u64` and `String → u64` at 1k, every operation; and crate names
    /// and kernel paths → `u64` at 1k: inserts and lookups.
    Small,
}

impl Named for Suite {
    const ALL: &'static [Self] = &[
        Self::Headline,
        Self::Keys,
        Self::RealKeys,
        Self::Values,
        Self::Tiny,
        Self::Memtable,
        Self::Sets,
        Self::Small,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Headline => "headline",
            Self::Keys => "keys",
            Self::RealKeys => "realkeys",
            Self::Values => "values",
            Self::Tiny => "tiny",
            Self::Memtable => "memtable",
            Self::Sets => "sets",
            Self::Small => "small",
        }
    }
}

impl Suite {
    /// The key kinds the keys suite also runs at 10M: the fixed-size ones,
    /// whose 1M maps fit in the L3 cache or nearly, and JSON keys, the
    /// commonest strings. The other strings and the memtable keys outgrow
    /// L3 at 1M already, and at 10M they would add about 15 minutes, mostly
    /// building their trees.
    const KEYS_AT_TEN_MILLION: [KeyKind; 5] = [
        KeyKind::U64Random,
        KeyKind::U64Sequential,
        KeyKind::U32,
        KeyKind::Hash32,
        KeyKind::JsonString,
    ];

    /// The key kinds read from large datasets of one key per line: those of
    /// the string-index literature (LITS, HOPE, HOT), but Wikipedia's titles
    /// of today.
    const REAL_KEYS: [KeyKind; 5] = [
        KeyKind::WikiTitles,
        KeyKind::Urls,
        KeyKind::Addresses,
        KeyKind::GeoNames,
        KeyKind::DblpTitles,
    ];

    /// The maps the suite's workloads run on. The real-keys suite compares
    /// the ordered maps only: at 10M entries of long strings, building each
    /// map takes seconds, and the hash maps wouldn't fit its time budget.
    pub(crate) fn maps(self) -> &'static [MapKind] {
        match self {
            Self::RealKeys => &[MapKind::BBTree, MapKind::StdBTree],
            Self::Headline
            | Self::Keys
            | Self::Values
            | Self::Tiny
            | Self::Memtable
            | Self::Sets
            | Self::Small => MapKind::ALL,
        }
    }

    pub(crate) fn workloads(self) -> Vec<Workload> {
        let inserts_and_lookups = [Operation::InsertRandom, Operation::GetHit];
        let inserts_hits_and_misses = [
            Operation::InsertRandom,
            Operation::GetHit,
            Operation::GetMiss,
        ];
        match self {
            Self::Headline => Workload::product(
                &[KeyKind::U64Random],
                &[ValueKind::U64],
                &[MapSize::TenMillion],
                Operation::ON_ONE_MAP,
            ),
            Self::Keys => (KeyKind::ALL.iter())
                .filter(|key| key.is_generated())
                .flat_map(|&key| {
                    let sizes: &[MapSize] = if Self::KEYS_AT_TEN_MILLION.contains(&key) {
                        &[MapSize::OneMillion, MapSize::TenMillion]
                    } else {
                        &[MapSize::OneMillion]
                    };
                    Workload::product(&[key], &[ValueKind::U64], sizes, &inserts_hits_and_misses)
                })
                .collect(),
            Self::RealKeys => (Self::REAL_KEYS.iter())
                .flat_map(|&key| {
                    Workload::product(
                        &[key],
                        &[ValueKind::U64],
                        key.sizes(),
                        &inserts_hits_and_misses,
                    )
                })
                .collect(),
            Self::Values => Workload::product(
                &[KeyKind::U64Random],
                ValueKind::ALL,
                &[MapSize::OneMillion],
                &inserts_and_lookups,
            ),
            Self::Tiny => {
                let mut workloads = Workload::product(
                    &[KeyKind::JsonString],
                    &[ValueKind::Struct32],
                    &[MapSize::FourEntryMaps, MapSize::ThirtyTwoEntryMaps],
                    &[Operation::BuildMany, Operation::GetMany, Operation::Iterate],
                );
                workloads.extend(Workload::product(
                    &[KeyKind::JsonObjectKeys],
                    &[ValueKind::Struct32],
                    &[MapSize::RealJsonObjects],
                    Operation::ON_MANY_MAPS,
                ));
                workloads
            }
            Self::Memtable => Workload::product(
                &[KeyKind::Memtable16, KeyKind::Memtable24],
                &[ValueKind::Bytes100],
                &[MapSize::OneMillion],
                &[
                    Operation::InsertRandom,
                    Operation::GetHit,
                    Operation::Range64,
                ],
            ),
            Self::Sets => Workload::product(
                &[KeyKind::U64Random, KeyKind::JsonString],
                &[ValueKind::Unit],
                &[MapSize::OneMillion],
                &inserts_and_lookups,
            ),
            Self::Small => {
                let mut workloads = Workload::product(
                    &[KeyKind::U64Random, KeyKind::CrateString],
                    &[ValueKind::U64],
                    &[MapSize::OneThousand],
                    Operation::ON_ONE_MAP,
                );
                workloads.extend(Workload::product(
                    &[KeyKind::CrateNames, KeyKind::KernelPaths],
                    &[ValueKind::U64],
                    &[MapSize::OneThousand],
                    &inserts_hits_and_misses,
                ));
                workloads
            }
        }
    }
}

/// A row of the report: a workload, under the first suite that lists it.
#[derive(Clone, Copy)]
pub(crate) struct SuiteRow {
    suite: Suite,
    workload: Workload,
}

impl SuiteRow {
    /// The workloads of `suites` that `filter` admits, in suite order. A
    /// workload that several suites list runs once.
    pub(crate) fn select(suites: &[Suite], filter: &CellFilter) -> Vec<Self> {
        let mut rows: Vec<Self> = Vec::new();
        for &suite in suites {
            for workload in suite.workloads() {
                let listed = rows.iter().any(|row| row.workload == workload);
                if filter.admits(workload) && !listed {
                    rows.push(Self { suite, workload });
                }
            }
        }
        rows
    }

    pub(crate) fn suite(self) -> Suite {
        self.suite
    }

    pub(crate) fn workload(self) -> Workload {
        self.workload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suites_list_each_workload_once() {
        for &suite in Suite::ALL {
            let workloads = suite.workloads();
            for (index, workload) in workloads.iter().enumerate() {
                assert!(
                    !workloads[..index].contains(workload),
                    "{} lists {workload:?} twice",
                    suite.name()
                );
            }
        }
    }
}
