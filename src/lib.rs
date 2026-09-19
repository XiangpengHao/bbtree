//! bbtree: the blessed B-tree.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

#[cfg(test)]
extern crate std;

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the tree layer, not yet written")
)]
mod node;
