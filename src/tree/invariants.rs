//! Checks of what a well-formed tree satisfies, through the node API, in
//! tests and in fuzzing builds (`--cfg fuzzing`, which `cargo fuzz` sets),
//! where the fuzz targets (`fuzz/`) call them through the map's
//! `check_invariants`: the whole tree's shape, order and counts
//! ([`Subtree::check_invariants`]), and on every node, that its inline
//! keys and length tags match its keys, and its prefix length is what its
//! fences share, or along the borders, what every key shares (D50).
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use super::node::handle::{
    ForceResult, Immut, Internal, KeyList, LeafOrInternal, NodeRef, Subtree,
};
use super::node::inline_keys::KeyInliner;
use super::node::node_plan::NodePlan;
use super::node::page::PageKind;
use super::node::shared_prefix::SharedPrefix;

use crate::inline_key::{InlineKeyStrategy, InlineWidth, InlineWord};

impl<K: Ord, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Panics unless this is a well-formed B+-tree of `length` entries
    /// (D17):
    ///
    /// - in order, keys strictly increase;
    /// - every key lies within the separator bounds passed down from its
    ///   parent;
    /// - all leaves are at the same depth;
    /// - every node below the root holds at least its `min_len` keys, which
    ///   splits and removals keep (D22);
    /// - the root of a non-empty tree holds a key, and so does an internal
    ///   root;
    /// - every leaf but the leftmost has a head, the value of the separator
    ///   just left of it, and the leftmost has none: a tree of `L` leaves
    ///   has `L - 1` separators and as many heads; where separator keys are
    ///   heads (D49), a separator's key is that head's, which is what the
    ///   walk reads as the separator's;
    /// - `length` is the number of entries, each counted once: leaf entries
    ///   and separators, whose values are the heads;
    /// - if the strategy gives the keys inline keys, every node holds its
    ///   keys' windows at its prefix length, in order (D18), which is what
    ///   its fences share (05-fence-key.md), and along the tree's borders,
    ///   what every key shares, whose encoding starts with the tree's
    ///   shared prefix (D50); otherwise none, and prefix 0.
    ///
    /// Calls `Ord` and the strategy's inline-key encoding: a tree whose keys
    /// break their contracts fails it.
    pub(crate) fn check_invariants(&self, length: usize) {
        let root = self.borrow();
        if length > 0 || root.height() > 0 {
            assert!(root.len() > 0, "the root of a non-empty map has no keys");
        }
        let mut walk = InOrderWalk {
            shared_prefix: self.shared_prefix(),
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
        assert_eq!(walk.entries, length, "len() is not the entry count");
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Panics unless no node's prefix length is shorter than its parent's
    /// (D44), and every node along the tree's left and right borders, which
    /// lacks a fence, has the tree's shared prefix (D50): a search for a key
    /// that shares it but sorts past the first or last key goes down a
    /// border, and reads its windows there. Calls neither `Ord` nor any
    /// encoding: it holds even for keys that panic or break their
    /// contracts, whose maps give wrong answers.
    pub(crate) fn check_prefix_lens(&self) {
        let shared_len = self.shared_prefix().len();
        Self::check_prefix_lens_in(self.borrow(), shared_len, shared_len, true, true);
    }

    fn check_prefix_lens_in(
        node: NodeRef<Immut<'_>, K, V, S, LeafOrInternal>,
        shared_len: usize,
        parent_prefix: usize,
        on_left_border: bool,
        on_right_border: bool,
    ) {
        let prefix = node.prefix_len();
        assert!(
            prefix >= parent_prefix,
            "child prefix {prefix} < parent {parent_prefix}"
        );
        assert!(
            prefix == shared_len || !(on_left_border || on_right_border),
            "prefix {prefix} on the tree's border, where every key shares {shared_len}"
        );
        if let ForceResult::Internal(internal) = node.force() {
            let len = internal.len();
            for index in 0..=len {
                Self::check_prefix_lens_in(
                    internal.edge_at(index).descend(),
                    shared_len,
                    prefix,
                    on_left_border && index == 0,
                    on_right_border && index == len,
                );
            }
        }
    }
}

#[cfg(test)]
impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Every node's prefix length (05-fence-key.md), from the root down, each
    /// node before its children.
    pub(crate) fn node_prefix_lens(&self) -> Vec<usize> {
        let mut prefix_lens = Vec::new();
        Self::collect_prefix_lens(self.borrow(), &mut prefix_lens);
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

    /// The separator keys, in order: each one's value is a leaf's head
    /// (D17), and where separator keys are heads, so is the key (D49).
    pub(crate) fn separator_keys(&self) -> Vec<&K> {
        let mut keys = Vec::new();
        Self::collect_separator_keys(self.borrow(), &mut keys);
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
    fn of_child(&self, keys: impl KeyList<'a, K>, index: usize) -> Self {
        Self {
            lower: index
                .checked_sub(1)
                .map_or(self.lower, |left| keys.get(left)),
            upper: keys.get(index).or(self.upper),
        }
    }
}

/// A walk over every entry in key order, checking each node on the way.
struct InOrderWalk<'a, K> {
    /// What every key of the tree shares (D50).
    shared_prefix: &'a SharedPrefix,
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
                let keys = self.check_node(leaf, &bounds, depth);
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
                let keys = self.check_node(internal, &bounds, depth);
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
    /// every key within its bounds, sharing what every key shares. Returns
    /// its keys.
    fn check_node<V, S: InlineKeyStrategy<K>, Kind: PageKind>(
        &self,
        node: NodeRef<Immut<'a>, K, V, S, Kind>,
        bounds: &KeyBounds<'a, K>,
        depth: usize,
    ) -> Kind::Keys<'a, K, V, S> {
        node.assert_inline_keys_match();
        node.assert_prefix_fits_fences(bounds.lower, bounds.upper, self.shared_prefix.len());
        let keys = node.keys();
        if depth > 0 {
            assert!(
                !node.is_underfull(),
                "a node below the root has {} keys, fewer than its minimum of {}",
                keys.len(),
                node.min_len()
            );
        }
        for key in keys.iter() {
            bounds.assert_contains(key);
            assert!(
                self.shared_prefix.is_shared_by::<K, S>(key),
                "a key lacks what the tree says every key shares"
            );
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

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeRef<Immut<'_>, K, V, S, Kind> {
    /// Panics unless the node has inline keys exactly if its map's strategy
    /// gives its keys some, and then has the first bytes of its keys'
    /// windows at its prefix length, as many as this kind keeps, in order
    /// (D18, 05-fence-key.md); a node without has prefix 0.
    pub(crate) fn assert_inline_keys_match(self) {
        let plan = Self::plan();
        if let Some(tags) = self.length_tags() {
            assert!(plan.has_length_tags());
            for (index, key) in self.keys().iter().enumerate() {
                assert_eq!(Some(tags[index]), plan.length_tag(key));
            }
        }
        let prefix_len = self.prefix_len();
        if let Some(inliner) = plan.inliner()
            && NodePlan::<K>::of::<S, Internal>().rehydrates_windows()
        {
            // A narrow internal window may next fill a leaf's prefix, so
            // every window a node of this map hands out keeps all the bytes
            // a leaf does.
            for (index, key) in self.keys().iter().enumerate() {
                let window = self.kv_at(index).window();
                assert_eq!(window.prefix_len(), self.prefix_len());
                assert_eq!(
                    window.inline_key(),
                    inliner.inline_key_at(key, self.prefix_len())
                );
            }
        }
        let expected: Option<Vec<InlineWord>> = Option::zip(plan.inliner(), plan.inline_width())
            .map(|(inliner, inline_width)| {
                let dropped_bits = 8 * (InlineWidth::Eight.bytes() - inline_width.bytes()) as u32;
                (self.keys().iter())
                    .map(|key| {
                        inliner.inline_key_at(key, prefix_len) >> dropped_bits << dropped_bits
                    })
                    .collect()
            });
        let actual: Option<Vec<InlineWord>> = self.inline_keys().map(|inline_keys| {
            assert!(
                inline_keys.pads_with_greatest(),
                "an inline-key slot past the length is not the greatest"
            );
            (0..self.len())
                .map(|index| inline_keys.inline_key(index))
                .collect()
        });
        assert_eq!(
            actual, expected,
            "the inline keys are not the keys' windows at prefix length {prefix_len}"
        );
        if expected.is_none() {
            assert_eq!(prefix_len, 0, "a node without inline keys has a prefix");
        }
    }

    /// Panics unless the node's prefix length is what its fences, `lower`
    /// and `upper`, share (05-fence-key.md), or `shared_len`, what every key
    /// shares, if either is missing (D50).
    pub(crate) fn assert_prefix_fits_fences(
        self,
        lower: Option<&K>,
        upper: Option<&K>,
        shared_len: usize,
    ) {
        let shared = KeyInliner::<K>::resolve::<S>().map_or(0, |inliner| {
            inliner.fence_prefix_len(lower, upper, shared_len)
        });
        assert_eq!(
            self.prefix_len(),
            shared,
            "a node's prefix length is not what its fences share"
        );
    }
}
