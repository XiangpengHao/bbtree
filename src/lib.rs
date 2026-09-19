//! bbtree: the blessed B-tree.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod map;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "removal and iteration, not yet written, use the rest of the node API"
    )
)]
mod node;

pub use map::BBTreeMap;
