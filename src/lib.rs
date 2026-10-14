//! bbtree: the blessed B-tree.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod allocator;
mod inline_key;
mod map;
mod set;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the tree's tests use the rest of its API")
)]
mod tree;

pub use allocator::{AllocError, Allocator, Global};
pub use inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWidth, InlineWord, WithInlineKey};
pub use map::BBTreeMap;
pub use set::BBTreeSet;

/// Read-only views of a map's nodes, from [`BBTreeMap::inspect`], as the map
/// stores them: for the visualizer (`vis/`), behind the `inspect` feature,
/// which promises no stability.
#[cfg(feature = "inspect")]
pub mod inspect {
    pub use crate::tree::inspect::{
        NodeId, NodeView, PageMap, SlotArray, SlotContents, TraceOutcome, TraceStep, TreeView,
    };
}

/// An ordered map based on a B-tree, [`BBTreeMap`], and the types that go
/// with it: the counterpart of `std::collections::btree_map`.
pub mod bbtree_map {
    pub use crate::map::{
        BBTreeMap, Entry, ExtractIf, IntoIter, IntoKeys, IntoValues, Iter, IterMut, Keys,
        OccupiedEntry, Range, RangeMut, VacantEntry, Values, ValuesMut,
    };
}

/// An ordered set based on a B-tree, [`BBTreeSet`], and the types that go
/// with it: the counterpart of `std::collections::btree_set`.
pub mod bbtree_set {
    pub use crate::set::{
        BBTreeSet, Difference, ExtractIf, Intersection, IntoIter, Iter, Range, SymmetricDifference,
        Union,
    };
}
