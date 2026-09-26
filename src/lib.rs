//! bbtree: the blessed B-tree.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod inline_key;
mod map;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "mutable and owning iteration, entries and fence keys (05-fence-key.md), not yet written, use the rest of the node API"
    )
)]
mod node;

pub use inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWord, WithInlineKey};
pub use map::BBTreeMap;

/// An ordered map based on a B-tree, [`BBTreeMap`], and the types that go
/// with it: the counterpart of `std::collections::btree_map`.
pub mod bbtree_map {
    pub use crate::map::{BBTreeMap, Iter, Keys, Range, Values};
}
