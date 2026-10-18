//! The B+-tree under the map and the set: a root, internal nodes and leaves,
//! each kind its own type (D51), on fixed-size, self-aligned pages (D15).
//!
//! - **Leaves** hold the entries, each a key and its value.
//! - **Internal nodes** hold separators and the edges to their children.
//!   Nodes have no parent pointers (D11).
//! - **Heads.** The tree stores each key once (D17). A separator's value
//!   lives in the leftmost leaf right of it, as that leaf's head; the
//!   leftmost leaf of the tree has none. With inline keys (D18), the
//!   separator's key lives in the head too, and the internal node keeps only
//!   its inline key (D49). Without them, the key sits in the internal node.
//! - **The root** has no parent, so the tree keeps its fences beside it, as
//!   the prefix every key shares, in the root page (D50, [`root`]).
//! - **Node stacks** record the way down from the root, for every operation
//!   that goes back up or moves along the tree (D11, `node_stack.rs`).
//! - **Prefixes.** Each node with inline keys skips the bytes its fences
//!   share (`prefix.rs`).
//!
//! [`node`] holds the two kinds of node and every operation on one node.
//! [`root`] holds `Tree`, which owns the root page and the last leaf an
//! append hit (D45); every operation on a whole tree starts there.
//! [`latched`] is the tree that threads share (`BBTreeMap`, D54). Each other
//! module holds one operation or view of a tree.
//!
//! A node handle is a page pointer and carries no access of its own. The code
//! holding one keeps the contracts that `tree/node/mod.rs` lists, and each
//! `// SAFETY:` comment states what else it relies on. Types enforce safety
//! only where the code is stable (D14): the borrow types of node stacks and
//! walks, which hand out each entry once, and the slot arrays and page
//! allocation under the nodes. What only the tree uses, such as making a node
//! handle from a pointer, is `pub(in crate::tree)`.

mod batch;
pub(crate) mod build;
mod clone;
mod cut;
mod insert;
#[cfg(any(test, feature = "inspect"))]
pub(crate) mod inspect;
mod interleaving;
#[cfg(any(test, fuzzing))]
mod invariants;
pub(crate) mod latched;
pub(crate) mod node;
pub(crate) mod node_stack;
mod prefix;
mod remove;
pub(crate) mod root;
mod search;
mod shared_prefix;
mod teardown;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod walk;
