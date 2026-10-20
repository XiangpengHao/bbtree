//! The suites CI runs: lists of workloads, each of which runs on every map
//! of its suite. The evaluation's suites, which cover design/02-field_study.md
//! §6, live in bbtree-eval.

use crate::command_line::{CellFilter, Named};
use crate::keys::KeyKind;
use crate::map_adapters::MapKind;
use crate::operations::Operation;
use crate::values::ValueKind;
use crate::workload::{MapSize, Workload};

/// Named collections of workloads, all of which CI runs: `small`, and
/// `realkeys` and `tiny` without their datasets, to keep the bench building
/// and running (.github/workflows/ci.yml); `ci` to report each change's
/// timings (.github/workflows/perf.yml). Select cells to keep each run below
/// five minutes on the benchmark machine (Ryzen 9 9900X).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Suite {
    /// Each large real-world key set → `u64`, at 1M and 10M (`all` for sets
    /// of fewer keys), on the ordered maps: inserts and lookups.
    RealKeys,
    /// Many tiny JSON-object-like maps: JSON keys → 32-byte structs, in
    /// maps of generated keys and in the objects of real JSON documents.
    Tiny,
    /// `u64 → u64` and `String → u64` at 1k, every operation; and crate names
    /// and kernel paths → `u64` at 1k: inserts and lookups.
    Small,
    /// What CI measures for every pull request and every commit to main
    /// (.github/workflows/perf.yml), bbtree's unsync map against std's
    /// `BTreeMap` only: `u64 → u64` at 1k and at 1M, past the CI machine's
    /// 12 MiB L3, every operation, and at 10M inserts and lookups; and
    /// strings → `u64` at 1k and 1M, inserts and lookups. Generated keys
    /// only, since the CI machine has no datasets.
    Ci,
}

impl Named for Suite {
    const ALL: &'static [Self] = &[Self::RealKeys, Self::Tiny, Self::Small, Self::Ci];

    fn name(self) -> &'static str {
        match self {
            Self::RealKeys => "realkeys",
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Ci => "ci",
        }
    }
}

impl Suite {
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
    /// map takes seconds, and the hash maps wouldn't fit its time budget. CI
    /// compares bbtree's unsync map with its baseline, std's `BTreeMap`.
    pub(crate) fn maps(self) -> &'static [MapKind] {
        match self {
            Self::RealKeys | Self::Ci => &[MapKind::Unsync, MapKind::StdBTree],
            Self::Tiny | Self::Small => MapKind::ALL,
        }
    }

    pub(crate) fn workloads(self) -> Vec<Workload> {
        let inserts_hits_and_misses = [
            Operation::RandomInsert,
            Operation::GetHit,
            Operation::GetMiss,
        ];
        match self {
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
            Self::Ci => {
                let mut workloads = Workload::product(
                    &[KeyKind::U64Random],
                    &[ValueKind::U64],
                    &[MapSize::OneThousand, MapSize::OneMillion],
                    Operation::ON_ONE_MAP,
                );
                workloads.extend(Workload::product(
                    &[KeyKind::U64Random],
                    &[ValueKind::U64],
                    &[MapSize::TenMillion],
                    &inserts_hits_and_misses,
                ));
                workloads.extend(Workload::product(
                    &[KeyKind::CrateString],
                    &[ValueKind::U64],
                    &[MapSize::OneThousand],
                    &inserts_hits_and_misses,
                ));
                workloads.extend(Workload::product(
                    &[KeyKind::JsonString],
                    &[ValueKind::U64],
                    &[MapSize::OneMillion],
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
