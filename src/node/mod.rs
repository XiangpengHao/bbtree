//! B-tree nodes: fixed-size, self-aligned 1 KB pages holding keys and values
//! (`doc/03-decisions.md` D9, D11, D12, D13, D15), behind typed, zero-overhead
//! abstractions over all the `unsafe` code (D14).
//!
//! A node is a page of `NodeLayout::<K, V>::LAYOUT`. The page says nothing
//! about its kind or its parent: the height tells the kind, and navigation
//! will use path stacks instead of parent pointers.
//!
//! - [`page::FreshPage`] is an allocated page that holds nothing: the only
//!   way to create a node, and what a node turns back into when released.
//! - [`handle::Subtree`] owns a tree; [`handle::NodeRef`] is shared or
//!   exclusive access to one node of it; [`handle::Edge`] and
//!   [`handle::Kv`] are positions in a node, and the operations hang off them.
//!
//! Only `page.rs`, `slots.rs` and `handle.rs` contain `unsafe` code. Every
//! other module is `#![forbid(unsafe_code)]` and builds on their safe API.

pub(crate) mod handle;
mod internal;
mod layout;
mod leaf;
pub(crate) mod page;
mod slots;
mod teardown;

#[cfg(test)]
pub(crate) mod test_support;
