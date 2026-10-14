//! The B+-tree under the map and the set: a root, internal nodes and leaves
//! (`doc/03-decisions.md` D51, D52), on fixed-size, self-aligned node pages,
//! with keys and values in leaves, and separator keys in internal nodes, or
//! with inline keys, in leaves too (`doc/03-decisions.md` D9, D11, D12, D13,
//! D15, D17, D21, D29, D49).
//!
//! The tree stores each key once (D17): values live only in leaves, and a
//! separator keeps its value in the leaf right of it, as that leaf's head.
//! Its key moves up into an internal node; with inline keys, it stays in the
//! head beside its value, and the internal node keeps only its inline key
//! (D49). Nodes have no parent pointers: whatever goes back up keeps a path
//! from the root instead (D11).
//!
//! - [`node`] is the two kinds of node, each its own type: [`node::leaf`]
//!   and [`node::internal`], with every operation on one node, and what
//!   they are both made of: pages, their layouts, slot arrays, and the
//!   inline keys they search (D18).
//! - [`root`] is the root and the tree that owns it ([`root::Tree`]): the root
//!   page, with the root node and what the tree keeps for it in place of a
//!   parent, its fences, as the bytes every key shares (D50, D52,
//!   `node/root_page.rs`, `shared_prefix.rs`), and the last leaf an append
//!   went to (D45). Every operation on a whole tree starts there.
//! - The rest of this module is the tree's operations, each in its own
//!   feature's module. Paths from the root and the iterators' walks along
//!   them (`path.rs`, `walk.rs`), and searches that need no path
//!   (`search.rs`). Changes along a path: `insert.rs`, `remove.rs`, `cut.rs`,
//!   which cuts a tree in two, and `join.rs`, which joins two. Whole trees at
//!   once: `build.rs`, from entries in order, `clone.rs` and `teardown.rs`.
//!   The prefixes nodes strip, which their fences decide (`prefix.rs`). And
//!   checks and views of a tree: `invariants.rs`, `inspect.rs`, and
//!   `test_support.rs` for tests.
//!
//! A node handle is a page pointer, which carries no access of its own; the
//! code that holds one keeps the contracts `tree/node/mod.rs` lists. The
//! types keep safety only where the code is stable: the borrow types of
//! paths and walks, which hand out each entry once (`path.rs`, `walk.rs`),
//! and the slot arrays and page allocation under the nodes. `unsafe` code
//! is in the node modules that touch pages or erase types (`node/mod.rs`,
//! `page.rs`, `slots.rs`, `leaf.rs`, `internal.rs`, `root_page.rs`,
//! `siblings.rs`, `inline_key_search.rs`, `vector_count.rs`,
//! `type_identity.rs`), and in `root.rs`, `path.rs`, `search.rs` and
//! `walk.rs`, where a contract the type system doesn't check is relied on.
//! Every other module is `#![forbid(unsafe_code)]`. What only the tree
//! uses, such as making a node handle from a pointer, is
//! `pub(in crate::tree)`; the map and the set see the rest.

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
mod prefix;
mod remove;
pub(crate) mod root;
mod search;
mod shared_prefix;
mod teardown;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod walk;
