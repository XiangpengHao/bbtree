//! B-tree nodes: fixed-size, self-aligned 1 KB pages, with keys and values in
//! leaves and separator keys in internal nodes (`doc/03-decisions.md` D9, D11,
//! D12, D13, D15, D17, D21), behind typed, zero-overhead abstractions over all
//! the `unsafe` code (D14).
//!
//! A leaf is a page of `NodeLayout::<K, V>::LEAF_LAYOUT`, sized by its keys
//! and values, and an internal node a page of `INTERNAL_LAYOUT`, sized by its
//! keys alone (D21). The page says nothing about its kind or its parent: the
//! height tells the kind, and navigation will use path stacks instead of
//! parent pointers.
//!
//! The tree is a B+-tree whose keys are each stored once (D17): values live
//! only in leaves, and a separator key, moved up into an internal node, keeps
//! its value in the leaf right of it, as that leaf's head.
//!
//! - [`page::FreshPage`] is an allocated page that holds nothing, typed by
//!   the kind of node it is for: the only way to create a node, and what a
//!   node turns back into when released.
//! - [`handle::Subtree`] owns a tree; [`handle::NodeRef`] is shared or
//!   exclusive access to one node of it; [`handle::Edge`],
//!   [`handle::Kv`] and [`handle::Head`] are positions in a node, and the
//!   operations hang off them.
//!
//! Only `page.rs`, `slots.rs` and `handle.rs` contain `unsafe` code. Every
//! other module is `#![forbid(unsafe_code)]` and builds on their safe API.

pub(crate) mod handle;
mod internal;
mod layout;
mod leaf;
pub(crate) mod page;
mod separator;
mod slots;
mod teardown;

#[cfg(test)]
pub(crate) mod test_support;
