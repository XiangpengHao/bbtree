//! The B+-tree under the map and the set: fixed-size, self-aligned node
//! pages, with keys and values in leaves, and separator keys in internal
//! nodes, or with inline keys, in leaves too (`doc/03-decisions.md` D9, D11,
//! D12, D13, D15, D17, D21, D29, D49), behind typed, zero-overhead
//! abstractions over all the `unsafe` code (D14).
//!
//! The tree stores each key once (D17): values live only in leaves, and a
//! separator keeps its value in the leaf right of it, as that leaf's head.
//! Its key moves up into an internal node; with inline keys, it stays in the
//! head beside its value, and the internal node keeps only its inline key
//! (D49). Nodes have no parent pointers: whatever goes back up keeps a path
//! from the root instead (D11).
//!
//! - [`node`] is one node, and nothing that follows an edge: its page and
//!   the byte layout of each kind, the typed handles to it and the positions
//!   in it, on which every operation hangs, and the inline keys it searches
//!   (D18).
//! - The rest of this module is the tree, built on those handles. Paths from
//!   the root and the iterators' walks along them (`path.rs`, `walk.rs`), and
//!   searches that need no path (`search.rs`). Changes along a path:
//!   `insert.rs`, `remove.rs`, `cut.rs`, which cuts a tree in two, and
//!   `join.rs`, which joins two. Whole trees at once: `build.rs`, from
//!   entries in order, `clone.rs` and `teardown.rs`. The prefixes nodes
//!   strip, which their fences decide (`prefix.rs`). And checks and views of
//!   a tree: `invariants.rs`, `inspect.rs`, and `test_support.rs` for tests.
//!
//! Only `tree/node/page.rs`, `tree/node/slots.rs`, `tree/node/handle.rs`,
//! `tree/node/vector_count.rs`, `path.rs` and `walk.rs` contain `unsafe`
//! code: `path.rs` to keep nodes as pointers along a path and make handles
//! from them, `walk.rs`, the iterators' walks, only to hand out values
//! mutably, each once, and `vector_count.rs` for the AVX-512 intrinsics of
//! the inline-key count. Every other module is `#![forbid(unsafe_code)]` and
//! builds on their safe API. What of a node's API only the tree may use,
//! such as making a handle from a pointer, is `pub(in crate::tree)`.

pub(crate) mod build;
mod clone;
mod cut;
mod insert;
#[cfg(any(test, feature = "inspect"))]
pub(crate) mod inspect;
#[cfg(any(test, fuzzing))]
mod invariants;
pub(crate) mod join;
pub(crate) mod node;
pub(crate) mod path;
pub(crate) mod prefix;
mod remove;
mod search;
mod teardown;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod walk;
