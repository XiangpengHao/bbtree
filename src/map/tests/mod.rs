//! The map's tests, through its API and against std's `BTreeMap`: the
//! harness that feeds both the same operations (`differential.rs`), and on
//! it, inline keys, prefixes and length tags; drops and panics; inserts that
//! replay their search; iterator folds; joined trees; and type properties.

#![forbid(unsafe_code)]

mod concat;
mod differential;
mod drops;
mod inline_key;
mod insert;
mod iterator;
mod length_tag;
mod prefix;
mod type_properties;

use std::vec::Vec;

use crate::inline_key::InlineKeyStrategy;
use crate::map::BBTreeMap;

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// The height of the root above the leaves: 0 for a map that is one leaf
    /// or nothing.
    fn height(&self) -> usize {
        self.root.page().map_or(0, |root| root.height())
    }

    /// Every node's prefix length (05-fence-key.md), from the root down, each
    /// node before its children.
    fn node_prefix_lens(&self) -> Vec<usize> {
        if self.root.has_tree() {
            self.root.node_prefix_lens()
        } else {
            Vec::new()
        }
    }

    /// The keys that live in internal nodes, in order: each one's value is a
    /// leaf's head (D17).
    fn separator_keys(&self) -> Vec<&K> {
        if self.root.has_tree() {
            self.root.separator_keys()
        } else {
            Vec::new()
        }
    }
}
