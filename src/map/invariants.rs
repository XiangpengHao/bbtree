//! Test-only check of the structural invariants of a [`BBTreeMap`], through
//! the node API.

#![forbid(unsafe_code)]

use std::vec::Vec;

use crate::node::handle::{ForceResult, Immut, LeafOrInternal, NodeRef};

use super::BBTreeMap;

impl<K: Ord, V> BBTreeMap<K, V> {
    /// Panics unless the map is a well-formed B+-tree of `len()` entries
    /// (D17):
    ///
    /// - in order, keys strictly increase;
    /// - every key lies within the separator bounds passed down from its
    ///   parent;
    /// - all leaves are at the same depth;
    /// - every node below the root holds at least as many keys as a split
    ///   leaves in a half (nothing removes entries yet);
    /// - the root of a non-empty map holds a key, and so does an internal
    ///   root;
    /// - every leaf but the leftmost has a head, the value of the separator
    ///   just left of it, and the leftmost has none: a tree of `L` leaves
    ///   has `L - 1` separators and as many heads;
    /// - `len()` is the number of entries, each counted once: leaf entries
    ///   and separators, whose values are the heads.
    pub(super) fn check_invariants(&self) {
        let Some(root) = &self.root else {
            assert_eq!(self.length, 0, "a map without a root has entries");
            return;
        };
        let root = root.borrow();
        if self.length > 0 || root.height() > 0 {
            assert!(root.len() > 0, "the root of a non-empty map has no keys");
        }
        let mut walk = InOrderWalk {
            previous_key: None,
            entries: 0,
            leaves: 0,
            separators: 0,
            leaf_depth: root.height(),
        };
        walk.check_subtree(root, KeyBounds::UNBOUNDED, 0);
        assert_eq!(
            walk.separators + 1,
            walk.leaves,
            "L leaves need L - 1 separators"
        );
        assert_eq!(walk.entries, self.length, "len() is not the entry count");
    }

    /// The height of the root above the leaves: 0 for a map that is one leaf
    /// or nothing.
    pub(super) fn height(&self) -> usize {
        self.root.as_ref().map_or(0, |root| root.height())
    }

    /// The keys that live in internal nodes, in order: each one's value is a
    /// leaf's head (D17).
    pub(super) fn separator_keys(&self) -> Vec<&K> {
        let mut keys = Vec::new();
        if let Some(root) = &self.root {
            Self::collect_separator_keys(root.borrow(), &mut keys);
        }
        keys
    }

    fn collect_separator_keys<'a>(
        node: NodeRef<Immut<'a>, K, V, LeafOrInternal>,
        keys: &mut Vec<&'a K>,
    ) {
        if let ForceResult::Internal(internal) = node.force() {
            for index in 0..=internal.len() {
                Self::collect_separator_keys(internal.edge_at(index).descend(), keys);
                if index < internal.len() {
                    keys.push(internal.kv_at(index).into_key());
                }
            }
        }
    }
}

/// The open interval of keys a subtree may hold: strictly between the
/// separators left and right of the edge leading to it.
#[derive(Clone, Copy)]
struct KeyBounds<'a, K> {
    lower: Option<&'a K>,
    upper: Option<&'a K>,
}

impl<'a, K: Ord> KeyBounds<'a, K> {
    const UNBOUNDED: Self = Self {
        lower: None,
        upper: None,
    };

    fn assert_contains(&self, key: &K) {
        if let Some(lower) = self.lower {
            assert!(lower < key, "a key is not above its left separator");
        }
        if let Some(upper) = self.upper {
            assert!(key < upper, "a key is not below its right separator");
        }
    }

    /// The bounds of the child at edge `index` of a node with these bounds
    /// and these keys.
    fn of_child(&self, keys: &'a [K], index: usize) -> Self {
        Self {
            lower: index
                .checked_sub(1)
                .map_or(self.lower, |left| Some(&keys[left])),
            upper: keys.get(index).or(self.upper),
        }
    }
}

/// A walk over every entry in key order, checking each node on the way.
struct InOrderWalk<'a, K> {
    /// The key of the last entry visited.
    previous_key: Option<&'a K>,
    /// The number of entries visited: leaf entries and separators.
    entries: usize,
    /// The number of leaves visited.
    leaves: usize,
    /// The number of separators visited.
    separators: usize,
    /// The depth every leaf must be at: the root's height.
    leaf_depth: usize,
}

impl<'a, K: Ord> InOrderWalk<'a, K> {
    fn check_subtree<V>(
        &mut self,
        node: NodeRef<Immut<'a>, K, V, LeafOrInternal>,
        bounds: KeyBounds<'a, K>,
        depth: usize,
    ) {
        let keys = node.keys();
        if depth > 0 {
            assert!(
                keys.len() >= node.min_len_after_split(),
                "a node below the root has {} keys, fewer than a split leaves",
                keys.len()
            );
        }
        for key in keys {
            bounds.assert_contains(key);
        }
        match node.force() {
            ForceResult::Leaf(leaf) => {
                assert_eq!(depth, self.leaf_depth, "leaves at different depths");
                let is_leftmost = self.leaves == 0;
                assert_eq!(
                    leaf.head().is_some(),
                    !is_leftmost,
                    "every leaf but the leftmost has a head, and only those"
                );
                self.leaves += 1;
                keys.iter().for_each(|key| self.visit(key));
            }
            ForceResult::Internal(internal) => {
                for index in 0..=keys.len() {
                    let child = internal.edge_at(index).descend();
                    self.check_subtree(child, bounds.of_child(keys, index), depth + 1);
                    if let Some(key) = keys.get(index) {
                        self.separators += 1;
                        self.visit(key);
                    }
                }
            }
        }
    }

    fn visit(&mut self, key: &'a K) {
        if let Some(previous_key) = self.previous_key {
            assert!(previous_key < key, "keys do not strictly increase in order");
        }
        self.previous_key = Some(key);
        self.entries += 1;
    }
}
