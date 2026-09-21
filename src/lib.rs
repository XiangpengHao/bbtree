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
        reason = "removal, iteration and fence keys (05-fence-key.md), not yet written, use the rest of the node API"
    )
)]
mod node;

pub use inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWord, WithInlineKey};
pub use map::BBTreeMap;
