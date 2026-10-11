//! B-tree nodes: fixed-size, self-aligned 1 KB pages, with keys and values in
//! leaves and separator keys in internal nodes (`doc/03-decisions.md` D9, D11,
//! D12, D13, D15, D17, D21, D29), behind typed, zero-overhead abstractions
//! over all the `unsafe` code (D14).
//!
//! A leaf is a page of a `NodeLayout::<K, V>` leaf layout, sized by its keys
//! and values, and an internal node a page of an internal layout, sized by its
//! keys alone (D21). The page says nothing about its kind or its parent: the
//! height tells the kind, and navigation will use path stacks instead of
//! parent pointers.
//!
//! Every node type also carries the map's inline-key strategy `S` (D18). If
//! it gives the keys inline keys, every node stores them in a dense array
//! before its keys, and searches run over that array first
//! (`inline_keys.rs`); each key then shares an entry with its value, or with
//! the edge right of it, so that the key a search ends at brings them along
//! (D29). Otherwise keys and values, or edges, are arrays of their own, and
//! nothing else changes, at no cost.
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
//! Only `page.rs`, `slots.rs`, `handle.rs`, `path.rs`, `walk.rs` and
//! `vector_count.rs` contain `unsafe` code: `path.rs` to keep nodes as
//! pointers along a path and make handles from them, `walk.rs`, the
//! iterators' walks, only to hand out values mutably, each once, and
//! `vector_count.rs` for the AVX-512 intrinsics of the inline-key count. Every other module is `#![forbid(unsafe_code)]`
//! and builds on their safe API.

mod border;
pub(crate) mod build;
mod clone;
pub(crate) mod handle;
pub(crate) mod inline_keys;
mod internal;
#[cfg(any(test, fuzzing))]
mod invariants;
mod join;
mod layout;
mod leaf;
mod length_tags;
mod local_prefix;
mod node_plan;
pub(crate) mod page;
pub(crate) mod path;
pub(crate) mod prefix;
mod search;
mod separator;
mod slots;
mod teardown;
mod vector_count;
pub(crate) mod walk;

#[cfg(test)]
pub(crate) mod test_support;
