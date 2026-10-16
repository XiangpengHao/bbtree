//! Checks of what a well-formed tree satisfies, through the node API, in
//! tests and in fuzzing builds (`--cfg fuzzing`, which `cargo fuzz` sets),
//! where the fuzz targets (`fuzz/`) call them through the map's
//! `check_invariants`: the whole tree's shape, order and counts
//! ([`Tree::check_invariants`]), and on every node, that its inline
//! keys and length tags match its keys, and its prefix length is what its
//! fences share, or along the borders, what every key shares (D50).
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use super::node::inline_keys::KeyInliner;
use super::node::internal::Internal;
use super::node::page::NodePtr;
use super::node::{Child, Node};
use super::root::Tree;
use super::shared_prefix::SharedPrefix;

use crate::inline_key::{InlineKeyStrategy, InlineWidth, InlineWord};

impl<K: Ord, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Checks the keys and prefixes node by node, not just the ordered
    /// entries: batched inserts must make the same splits as individual ones.
    pub(crate) fn check_same_nodes(&self, other: &Self) {
        match (self.root_page(), other.root_page()) {
            (Some(left), Some(right)) => {
                assert_eq!(
                    left.shared_prefix().as_bytes(),
                    right.shared_prefix().as_bytes()
                );
                left.node().check_same_nodes(right.node());
            }
            (None, None) => {}
            _ => panic!("only one tree has a root"),
        }
    }

    /// Panics unless this is a well-formed B+-tree of `length` entries
    /// (D17):
    ///
    /// - in order, keys strictly increase;
    /// - every key lies within the separator bounds passed down from its
    ///   parent;
    /// - all leaves are at the same depth, the kinds of the nodes as the
    ///   tags of the edges to them tell (`tree/node/page.rs`);
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
    /// Without a tree, `length` is 0.
    ///
    /// Calls `Ord` and the strategy's inline-key encoding: a tree whose keys
    /// break their contracts fails it.
    pub(crate) fn check_invariants(&self, length: usize) {
        let Some(root_page) = self.root_page() else {
            assert_eq!(length, 0, "a map without a tree has entries");
            return;
        };
        let root = root_page.node();
        if length > 0 || matches!(root, Node::Internal(_)) {
            assert!(
                root.len() > 0,
                "a root that is internal or holds the entries of a non-empty map has no keys"
            );
        }
        let mut walk = InOrderWalk {
            shared_prefix: root_page.shared_prefix(),
            previous_key: None,
            entries: 0,
            leaves: 0,
            separators: 0,
            leaf_depth: None,
        };
        walk.check_nodes_under(root, KeyBounds::UNBOUNDED, 0);
        assert_eq!(
            walk.separators + 1,
            walk.leaves,
            "L leaves need L - 1 separators"
        );
        assert_eq!(walk.entries, length, "len() is not the entry count");
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Panics unless no node's prefix length is shorter than its parent's
    /// (D44), and every node along the tree's left and right borders, which
    /// lacks a fence, has the tree's shared prefix (D50): a search for a key
    /// that shares it but sorts past the first or last key goes down a
    /// border, and reads its windows there. Calls neither `Ord` nor any
    /// encoding: it holds even for keys that panic or break their
    /// contracts, whose maps give wrong answers.
    pub(crate) fn check_prefix_lens(&self) {
        let Some(root_page) = self.root_page() else {
            return;
        };
        let shared_len = root_page.shared_prefix().len();
        Self::check_prefix_lens_in(root_page.node(), shared_len, shared_len, true, true);
    }

    fn check_prefix_lens_in(
        node: Node<K, V, S>,
        shared_len: usize,
        parent_prefix_len: usize,
        on_left_border: bool,
        on_right_border: bool,
    ) {
        let prefix_len = node.prefix_len();
        assert!(
            prefix_len >= parent_prefix_len,
            "child prefix {prefix_len} < parent {parent_prefix_len}"
        );
        assert!(
            prefix_len == shared_len || !(on_left_border || on_right_border),
            "prefix {prefix_len} on the tree's border, where every key shares {shared_len}"
        );
        if let Node::Internal(internal) = node {
            let len = internal.len();
            for index in 0..=len {
                Self::check_prefix_lens_in(
                    internal.child(index),
                    shared_len,
                    prefix_len,
                    on_left_border && index == 0,
                    on_right_border && index == len,
                );
            }
        }
    }
}

#[cfg(test)]
impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Every node's prefix length (05-fence-key.md), from the root down, each
    /// node before its children.
    pub(crate) fn node_prefix_lens(&self) -> Vec<usize> {
        let mut prefix_lens = Vec::new();
        Self::collect_prefix_lens(self.root(), &mut prefix_lens);
        prefix_lens
    }

    fn collect_prefix_lens(node: Node<K, V, S>, prefix_lens: &mut Vec<usize>) {
        prefix_lens.push(node.prefix_len());
        if let Node::Internal(internal) = node {
            for index in 0..=internal.len() {
                Self::collect_prefix_lens(internal.child(index), prefix_lens);
            }
        }
    }

    /// The separator keys, in order: each one's value is a leaf's head
    /// (D17), and where separator keys are heads, so is the key (D49).
    pub(crate) fn separator_keys(&self) -> Vec<&K> {
        let mut keys = Vec::new();
        Self::collect_separator_keys(self.root(), &mut keys);
        keys
    }

    fn collect_separator_keys(node: Node<K, V, S>, keys: &mut Vec<&K>) {
        if let Node::Internal(internal) = node {
            for index in 0..=internal.len() {
                Self::collect_separator_keys(internal.child(index), keys);
                if index < internal.len() {
                    keys.push(internal.separator_key(index));
                }
            }
        }
    }
}

/// The open interval of keys under a node: strictly between the separators
/// left and right of the edge leading to it.
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

    /// The bounds of the child at edge `index` of `node`, an internal node
    /// with these bounds: the separators beside the edge.
    fn of_child<V, S: InlineKeyStrategy<K>>(&self, node: Internal<K, V, S>, index: usize) -> Self {
        Self {
            lower: index
                .checked_sub(1)
                .map_or(self.lower, |left| Some(node.separator_key(left))),
            upper: (index < node.len())
                .then(|| node.separator_key(index))
                .or(self.upper),
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
    /// The depth the first leaf is at, which every other must be at too.
    leaf_depth: Option<usize>,
}

impl<'a, K: Ord> InOrderWalk<'a, K> {
    fn check_nodes_under<V, S: InlineKeyStrategy<K>>(
        &mut self,
        node: Node<K, V, S>,
        bounds: KeyBounds<'a, K>,
        depth: usize,
    ) {
        match node {
            Node::Leaf(leaf) => {
                self.check_node(leaf, &bounds, depth);
                let first_depth = *self.leaf_depth.get_or_insert(depth);
                assert_eq!(depth, first_depth, "leaves at different depths");
                let is_leftmost = self.leaves == 0;
                assert_eq!(
                    leaf.has_head(),
                    !is_leftmost,
                    "every leaf but the leftmost has a head, and only those"
                );
                self.leaves += 1;
                leaf.keys().iter().for_each(|key| self.visit(key));
            }
            Node::Internal(internal) => {
                self.check_node(internal, &bounds, depth);
                for index in 0..=internal.len() {
                    let child = internal.child(index);
                    let child_bounds = bounds.of_child(internal, index);
                    self.check_nodes_under(child, child_bounds, depth + 1);
                    if index < internal.len() {
                        self.separators += 1;
                        self.visit(internal.separator_key(index));
                    }
                }
            }
        }
    }

    /// Checks what every node satisfies, whatever its kind: its inline keys
    /// and prefix length, at least its minimum of keys below the root, and
    /// every key within its bounds, sharing what every key shares.
    fn check_node<V, S: InlineKeyStrategy<K>, C: Child<K, V, S>>(
        &self,
        node: C,
        bounds: &KeyBounds<'a, K>,
        depth: usize,
    ) {
        node.assert_inline_keys_match();
        node.assert_prefix_fits_fences(bounds.lower, bounds.upper, self.shared_prefix.len());
        if depth > 0 {
            assert!(
                node.len() >= C::min_len(),
                "a node below the root has {} keys, fewer than its minimum of {}",
                node.len(),
                C::min_len()
            );
        }
        for index in 0..node.len() {
            let key = node.key(index);
            bounds.assert_contains(key);
            assert!(
                self.shared_prefix.is_shared_by::<K, S>(key),
                "a key lacks what the tree says every key shares"
            );
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

/// The checks of one node, of either kind.
pub(in crate::tree) trait NodeChecks<K, V, S: InlineKeyStrategy<K>>:
    Child<K, V, S>
{
    /// Panics unless the node has inline keys exactly if its map's strategy
    /// gives its keys some, and then has the first bytes of its keys'
    /// windows at its prefix length, as many as this kind keeps, in order
    /// (D18, 05-fence-key.md); a node without has prefix 0.
    fn assert_inline_keys_match(self) {
        let plan = Self::plan();
        if let Some(tags) = self.length_tags() {
            for (index, &tag) in tags[..self.len()].iter().enumerate() {
                assert_eq!(Some(tag), plan.length_tag(self.key(index)));
            }
        }
        let prefix_len = self.prefix_len();
        let expected: Option<Vec<InlineWord>> = Option::zip(plan.inliner(), plan.inline_width())
            .map(|(inliner, inline_width)| {
                let dropped_bits =
                    8 * (InlineWidth::Eight.byte_len() - inline_width.byte_len()) as u32;
                (0..self.len())
                    .map(|index| {
                        inliner.inline_key_at(self.key(index), prefix_len) >> dropped_bits
                            << dropped_bits
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
    fn assert_prefix_fits_fences(self, lower: Option<&K>, upper: Option<&K>, shared_len: usize) {
        let fence_prefix_len = if NodePtr::<K, V, S>::has_inline_keys() {
            KeyInliner::fence_prefix_len::<S>(lower, upper, shared_len)
        } else {
            0
        };
        assert_eq!(
            self.prefix_len(),
            fence_prefix_len,
            "a node's prefix length is not what its fences share"
        );
    }
}

impl<K, V, S: InlineKeyStrategy<K>, C: Child<K, V, S>> NodeChecks<K, V, S> for C {}

impl<K: Ord, V, S: InlineKeyStrategy<K>> Node<K, V, S> {
    fn check_same_nodes(self, other: Self) {
        assert_eq!(self.prefix_len(), other.prefix_len());
        assert_eq!(self.len(), other.len());
        match (self, other) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                assert_eq!(left.has_head(), right.has_head());
                if left.has_head() && NodePtr::<K, V, S>::has_separator_keys_in_heads() {
                    assert!(left.head_key() == right.head_key());
                }
                for index in 0..left.len() {
                    assert!(left.key(index) == right.key(index));
                }
            }
            (Node::Internal(left), Node::Internal(right)) => {
                for index in 0..left.len() {
                    assert!(left.separator_key(index) == right.separator_key(index));
                }
                for edge in 0..=left.len() {
                    left.child(edge).check_same_nodes(right.child(edge));
                }
            }
            _ => panic!("nodes differ in kind"),
        }
    }
}
