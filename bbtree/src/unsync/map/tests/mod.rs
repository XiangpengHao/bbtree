//! The map's tests, against std's `BTreeMap` or a model, through the map's
//! API and, where the API cannot reach, its tree: the harness that feeds
//! both maps the same operations (`differential.rs`), and on it, inline
//! keys; prefixes and hash tags; drops and panics; inserts that replay
//! their search or append to the remembered rightmost leaf; iterator folds;
//! joined trees; the pages a map takes from its allocator; and type
//! properties.

#![forbid(unsafe_code)]

mod allocator;
mod batch;
mod differential;
mod drops;
mod hash_tag;
mod inline_key;
mod insert;
mod iterator;
mod node_config;
mod prefix;
mod type_properties;

use std::vec::Vec;

use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::unsync::map::BBTreeMap;

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// How far below the root the leaves lie: 0 for a map that is one leaf
    /// or nothing.
    fn leaf_depth(&self) -> usize {
        if self.tree.has_root() {
            self.tree.leaf_depth()
        } else {
            0
        }
    }

    /// Every node's prefix length (05-fence-key.md), from the root down, each
    /// node before its children.
    fn node_prefix_lens(&self) -> Vec<usize> {
        if self.tree.has_root() {
            self.tree.node_prefix_lens()
        } else {
            Vec::new()
        }
    }

    /// The separator keys, in order: each one's value is a leaf's head
    /// (D17), and with inline keys, so is its key (D49).
    fn separator_keys(&self) -> Vec<&K> {
        if self.tree.has_root() {
            self.tree.separator_keys()
        } else {
            Vec::new()
        }
    }
}
