//! Test-only check of the structural invariants of a [`BBTreeMap`], through
//! the node API.

#![forbid(unsafe_code)]

use std::vec::Vec;

use crate::inline_key::InlineKeyStrategy;
use crate::node::handle::{ForceResult, Immut, LeafOrInternal, NodeRef};
use crate::node::page::PageKind;

use super::BBTreeMap;

impl<K: Ord, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Panics unless the map is a well-formed B+-tree of `len()` entries
    /// (D17):
    ///
    /// - in order, keys strictly increase;
    /// - every key lies within the separator bounds passed down from its
    ///   parent;
    /// - all leaves are at the same depth;
    /// - every node below the root holds at least its `min_len` keys, which
    ///   splits and removals keep (D22);
    /// - the root of a non-empty map holds a key, and so does an internal
    ///   root;
    /// - every leaf but the leftmost has a head, the value of the separator
    ///   just left of it, and the leftmost has none: a tree of `L` leaves
    ///   has `L - 1` separators and as many heads;
    /// - `len()` is the number of entries, each counted once: leaf entries
    ///   and separators, whose values are the heads;
    /// - if the strategy gives the keys inline keys, every node holds its
    ///   keys' windows at its prefix length, in order (D18), which is what
    ///   its fences share, 0 along the tree's borders (05-fence-key.md);
    ///   otherwise none, and prefix 0.
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

    /// Every node's prefix length (05-fence-key.md), from the root down, each
    /// node before its children.
    pub(super) fn node_prefix_lens(&self) -> Vec<usize> {
        let mut prefix_lens = Vec::new();
        if let Some(root) = &self.root {
            Self::collect_prefix_lens(root.borrow(), &mut prefix_lens);
        }
        prefix_lens
    }

    fn collect_prefix_lens(
        node: NodeRef<Immut<'_>, K, V, S, LeafOrInternal>,
        prefix_lens: &mut Vec<usize>,
    ) {
        prefix_lens.push(node.prefix_len());
        if let ForceResult::Internal(internal) = node.force() {
            for index in 0..=internal.len() {
                Self::collect_prefix_lens(internal.edge_at(index).descend(), prefix_lens);
            }
        }
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
        node: NodeRef<Immut<'a>, K, V, S, LeafOrInternal>,
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
    fn check_subtree<V, S: InlineKeyStrategy<K>>(
        &mut self,
        node: NodeRef<Immut<'a>, K, V, S, LeafOrInternal>,
        bounds: KeyBounds<'a, K>,
        depth: usize,
    ) {
        match node.force() {
            ForceResult::Leaf(leaf) => {
                let keys = Self::check_node(leaf, &bounds, depth);
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
                let keys = Self::check_node(internal, &bounds, depth);
                for index in 0..=keys.len() {
                    let child = internal.edge_at(index).descend();
                    let child_bounds = bounds.of_child(keys, index);
                    self.check_subtree(child, child_bounds, depth + 1);
                    if let Some(key) = keys.get(index) {
                        self.separators += 1;
                        self.visit(key);
                    }
                }
            }
        }
    }

    /// Checks what every node satisfies, whatever its kind: its inline keys
    /// and prefix length, at least its minimum of keys below the root, and
    /// every key within its bounds. Returns its keys.
    fn check_node<V, S: InlineKeyStrategy<K>, Kind: PageKind>(
        node: NodeRef<Immut<'a>, K, V, S, Kind>,
        bounds: &KeyBounds<'a, K>,
        depth: usize,
    ) -> &'a [K] {
        node.assert_inline_keys_match();
        node.assert_prefix_fits_fences(bounds.lower, bounds.upper);
        let keys = node.keys();
        if depth > 0 {
            assert!(
                !node.is_underfull(),
                "a node below the root has {} keys, fewer than its minimum of {}",
                keys.len(),
                node.min_len()
            );
        }
        for key in keys {
            bounds.assert_contains(key);
        }
        keys
    }

    fn visit(&mut self, key: &'a K) {
        if let Some(previous_key) = self.previous_key {
            assert!(previous_key < key, "keys do not strictly increase in order");
        }
        self.previous_key = Some(key);
        self.entries += 1;
    }
}
