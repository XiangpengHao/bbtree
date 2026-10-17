//! An ordered map and set for one thread at a time: [`BBTreeMap`] and
//! [`BBTreeSet`], drop-in replacements for std's `BTreeMap` and `BTreeSet`
//! (D1), on the same nodes as [`crate::BBTreeMap`], which threads share.
//! They take `&mut self` to change, lend references into their nodes, and
//! latch nothing.

pub(crate) mod map;
pub(crate) mod set;

pub use map::BBTreeMap;
pub use set::BBTreeSet;

/// An ordered map based on a B-tree, [`BBTreeMap`], and the types that go
/// with it: the counterpart of `std::collections::btree_map`.
pub mod bbtree_map {
    pub use super::map::{
        BBTreeMap, Entry, ExtractIf, IntoIter, IntoKeys, IntoValues, Iter, IterMut, Keys,
        OccupiedEntry, Range, RangeMut, VacantEntry, Values, ValuesMut,
    };
}

/// An ordered set based on a B-tree, [`BBTreeSet`], and the types that go
/// with it: the counterpart of `std::collections::btree_set`.
pub mod bbtree_set {
    pub use super::set::{
        BBTreeSet, Difference, ExtractIf, Intersection, IntoIter, Iter, Range, SymmetricDifference,
        Union,
    };
}
