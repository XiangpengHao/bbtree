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
mod proof;
mod tree;
pub mod unsync;

pub use allocator::{AllocError, Allocator, Global};
pub use inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWidth, InlineWord, WithInlineKey};
pub use map::BBTreeMap;

/// Read-only views of a map's nodes, as the map stores them, from
/// [`unsync::BBTreeMap::inspect`]: for the visualizer (`vis/`), behind the
/// `inspect` feature, which promises no stability.
#[cfg(feature = "inspect")]
pub mod inspect {
    pub use crate::tree::inspect::{
        BatchPoll, BatchTrace, LaneFetch, NodeId, NodeView, PageMap, SlotArray, SlotContents,
        TraceOutcome, TraceStep, TreeView,
    };
}

/// [`BBTreeMap`], an ordered map that threads share, and the types that go
/// with it.
pub mod bbtree_map {
    pub use crate::allocator::InsertError;
    pub use crate::map::{BBTreeMap, IntoIter, IterMut, RangeCloned};
}
