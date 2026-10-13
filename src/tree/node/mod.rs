//! One node of the tree: what its page holds, and how it is read and
//! changed, with nothing that follows an edge to another node, which is the
//! tree's (`tree/mod.rs`).
//!
//! A leaf is a page of a `NodeLayout::<K, V>` leaf layout, sized by its keys
//! and values, and an internal node a page of an internal layout, sized by its
//! keys alone (D21). The page says nothing about its kind or its parent: the
//! height tells the kind, and navigation uses paths instead of parent
//! pointers.
//!
//! Every node type also carries the map's inline-key strategy `S` (D18). If
//! it gives the keys inline keys, every node stores them in a dense array
//! before its keys, and searches run over that array first
//! (`inline_keys.rs`); in a leaf, each key then shares an entry with its
//! value, so that the key a search ends at brings it along (D29), and the
//! head shares one with its separator's key. Internal nodes then keep no
//! keys, only inline keys and edges: a separator's key is a head below,
//! read on a tie (D49). Otherwise keys and values, or edges, are arrays of
//! their own, and nothing else changes, at no cost.
//!
//! - [`page::FreshPage`] is an allocated page that holds nothing, typed by
//!   the kind of node it is for: the only way to create a node, and what a
//!   node turns back into when released. `layout.rs` lays out its bytes, as
//!   the node's plan says (`node_plan.rs`), and `slots.rs` moves the keys,
//!   values and edges in it.
//! - [`handle::Subtree`] owns a tree; [`handle::NodeRef`] is shared or
//!   exclusive access to one node of it; [`handle::Edge`],
//!   [`handle::Kv`] and [`handle::Head`] are positions in a node, and the
//!   operations hang off them.
//! - `shared_prefix.rs` is what every key of a tree shares, the prefix of
//!   the nodes along its borders, which a search checks once (D50).
//! - `inline_keys.rs` is what a node keeps beside its keys to search them;
//!   `vector_count.rs` counts over a node's inline keys in explicit vectors,
//!   and `length_tags.rs` filters byte-string keys by their lengths (D36).

pub(crate) mod handle;
pub(crate) mod inline_keys;
pub(super) mod layout;
mod length_tags;
pub(super) mod node_plan;
pub(crate) mod page;
pub(crate) mod shared_prefix;
mod slots;
#[cfg(test)]
mod tests;
mod vector_count;
