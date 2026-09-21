//! What one measurement runs: a workload (key kind × value kind × size ×
//! operation) on one map, at full or quick scale.

use crate::command_line::Named;
use crate::keys::KeyKind;
use crate::map_adapters::MapKind;
use crate::operations::Operation;
use crate::values::ValueKind;

/// The sizes of doc/02-field_study.md §6: many tiny maps alive at once,
/// where the cost per map matters, or one map.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MapSize {
    /// 1M maps of 4 entries: JSON objects (median 2 keys, 92% have ≤ 8).
    FourEntryMaps,
    /// 100k maps of 32 entries: JSON objects at p99.
    ThirtyTwoEntryMaps,
    /// About 1M maps, each holding the keys of one object of real JSON
    /// documents: the documents' objects, repeated.
    RealJsonObjects,
    /// One map of 1k entries, like cargo's per-package maps: rebuilt or
    /// reused until the cell has timed enough operations.
    OneThousand,
    HundredThousand,
    OneMillion,
    /// DRAM-resident: memtables and indexes.
    TenMillion,
    /// One map of every key of a dataset, but those held out for misses.
    All,
}

impl Named for MapSize {
    const ALL: &'static [Self] = &[
        Self::FourEntryMaps,
        Self::ThirtyTwoEntryMaps,
        Self::RealJsonObjects,
        Self::OneThousand,
        Self::HundredThousand,
        Self::OneMillion,
        Self::TenMillion,
        Self::All,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::FourEntryMaps => "1Mx4",
            Self::ThirtyTwoEntryMaps => "100kx32",
            Self::RealJsonObjects => "1Mxreal",
            Self::OneThousand => "1k",
            Self::HundredThousand => "100k",
            Self::OneMillion => "1M",
            Self::TenMillion => "10M",
            Self::All => "all",
        }
    }
}

impl MapSize {
    /// The sizes whose shape is fixed, which are those of generated keys.
    pub(crate) const FIXED_SHAPES: &'static [Self] = &[
        Self::FourEntryMaps,
        Self::ThirtyTwoEntryMaps,
        Self::OneThousand,
        Self::HundredThousand,
        Self::OneMillion,
        Self::TenMillion,
    ];

    /// How many maps `1Mxreal` builds, about: its JSON objects repeat until
    /// there are at least this many.
    pub(crate) const REAL_JSON_OBJECT_MAPS: usize = 1_000_000;

    /// The shape of the sizes that fix it; `None` for those whose dataset
    /// does.
    pub(crate) fn fixed_shape(self, scale: Scale) -> Option<MapShape> {
        let (map_count, entries_per_map) = match self {
            Self::FourEntryMaps => (scale.shrink(1_000_000), 4),
            Self::ThirtyTwoEntryMaps => (scale.shrink(100_000), 32),
            Self::OneThousand => (1, scale.shrink(1_000)),
            Self::HundredThousand => (1, scale.shrink(100_000)),
            Self::OneMillion => (1, scale.shrink(1_000_000)),
            Self::TenMillion => (1, scale.shrink(10_000_000)),
            Self::RealJsonObjects | Self::All => return None,
        };
        Some(MapShape {
            map_count,
            entries_per_map,
        })
    }

    pub(crate) fn has_many_maps(self) -> bool {
        matches!(
            self,
            Self::FourEntryMaps | Self::ThirtyTwoEntryMaps | Self::RealJsonObjects
        )
    }
}

/// How many maps a cell builds, and how many entries each one holds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MapShape {
    map_count: usize,
    entries_per_map: usize,
}

impl MapShape {
    /// How many distinct keys many tiny maps draw their keys from, as JSON
    /// objects of one kind share field names.
    pub(crate) const VOCABULARY_SIZE: usize = 256;

    pub(crate) fn map_count(self) -> usize {
        self.map_count
    }

    pub(crate) fn entries_per_map(self) -> usize {
        self.entries_per_map
    }

    /// How many distinct keys a cell of this shape inserts: one map's
    /// entries, or the vocabulary that many maps draw from.
    pub(crate) fn distinct_keys(self) -> usize {
        if self.map_count == 1 {
            self.entries_per_map
        } else {
            Self::VOCABULARY_SIZE
        }
    }
}

/// Full-size runs, or `--quick` ones for CI and development.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scale {
    Full,
    /// 10× fewer entries, tiny maps and timed operations.
    Quick,
}

impl Scale {
    pub(crate) fn shrink(self, count: usize) -> usize {
        match self {
            Self::Full => count,
            Self::Quick => count / 10,
        }
    }

    /// The fewest operations a cell times. Small maps are rebuilt or reused
    /// until they reach it; lookups on large maps stop at it, since a random
    /// sample of the keys times as well as all of them.
    pub(crate) fn operation_budget(self) -> usize {
        self.shrink(1_000_000)
    }
}

/// One row of the report: what runs on every map.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Workload {
    key: KeyKind,
    value: ValueKind,
    size: MapSize,
    operation: Operation,
}

impl Workload {
    /// # Panics
    ///
    /// If the keys don't come in this size, or the operation doesn't apply
    /// to maps of this size.
    pub(crate) fn new(key: KeyKind, value: ValueKind, size: MapSize, operation: Operation) -> Self {
        assert!(
            key.sizes().contains(&size),
            "{} keys don't come in size {}",
            key.name(),
            size.name()
        );
        assert!(
            operation.applies_to(size),
            "{} does not apply to size {}",
            operation.name(),
            size.name()
        );
        Self {
            key,
            value,
            size,
            operation,
        }
    }

    /// Every combination of the given kinds, sizes and operations, nested in
    /// that order.
    pub(crate) fn product(
        keys: &[KeyKind],
        values: &[ValueKind],
        sizes: &[MapSize],
        operations: &[Operation],
    ) -> Vec<Self> {
        let mut workloads = Vec::new();
        for &key in keys {
            for &value in values {
                for &size in sizes {
                    for &operation in operations {
                        workloads.push(Self::new(key, value, size, operation));
                    }
                }
            }
        }
        workloads
    }

    pub(crate) fn key(self) -> KeyKind {
        self.key
    }

    pub(crate) fn value(self) -> ValueKind {
        self.value
    }

    pub(crate) fn size(self) -> MapSize {
        self.size
    }

    pub(crate) fn operation(self) -> Operation {
        self.operation
    }
}

/// One workload on one map: the unit the suite runner starts a child
/// process for, once per repetition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Cell {
    map: MapKind,
    workload: Workload,
}

impl Cell {
    pub(crate) fn new(map: MapKind, workload: Workload) -> Self {
        Self { map, workload }
    }

    pub(crate) fn map(self) -> MapKind {
        self.map
    }

    pub(crate) fn workload(self) -> Workload {
        self.workload
    }

    /// Whether the map has the API the operation needs. Cells that aren't
    /// applicable are reported as `n/a`, not run.
    pub(crate) fn is_applicable(self) -> bool {
        self.map.supports(self.workload.operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_shapes_are_the_sizes_with_a_shape() {
        for &size in MapSize::ALL {
            assert_eq!(
                size.fixed_shape(Scale::Full).is_some(),
                MapSize::FIXED_SHAPES.contains(&size),
                "{}",
                size.name()
            );
        }
    }
}
