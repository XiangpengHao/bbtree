//! What every key of a latched tree shares (D50), as bbtree keeps it
//! (`tree/shared_prefix.rs`, `tree/prefix.rs`): every descent checks a key
//! against it once, at the root (`descent.rs`), and an insert whose key
//! lacks it shrinks it first, with the root and every node along the
//! tree's borders held, whose prefixes move back with it.
//!
//! A node on a border stays on it, whatever other threads change: a split
//! keeps the part on the border's side there, and a merge keeps the node on
//! that side. So an operation that holds a border node holds what every key
//! shares still, which a shrinking needs that node for.

use super::super::interleaving::Interleaving;
use super::super::node::Node;
use super::super::node::latch::Latched;
use super::super::node_stack::{FirstEdge, LastEdge, NodeEnd};
use super::LatchedTree;
use super::held::HeldNodes;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Shrinks what every key of the tree shares to what `key` shares too,
    /// with bbtree's insert ([`Tree::shrink_shared_prefix_for`]), once it
    /// holds the root and the nodes along both borders, taken without
    /// waiting, trying until no other thread holds one; and returns `key`.
    /// `alloc` is the map's.
    ///
    /// [`Tree::shrink_shared_prefix_for`]: super::super::root::Tree::shrink_shared_prefix_for
    #[cold]
    #[inline(never)]
    pub(super) fn shrink_shared_prefix_for(&self, key: K, alloc: &impl Allocator) -> K {
        loop {
            let mut held_nodes = HeldNodes::new();
            if self.take_borders(&mut held_nodes) {
                return self
                    .change_held(|tree| tree.shrink_shared_prefix_for(key, alloc))
                    .into_key();
            }
            drop(held_nodes);
            Interleaving::wait(1);
        }
    }

    /// Takes the root and every node along the tree's left and right
    /// borders, down to the leaves: `false` if another thread holds one.
    fn take_borders(&self, held_nodes: &mut HeldNodes<K, V, S>) -> bool {
        let root = self.root_page().expect("a tree to shrink").node();
        if !Self::take_node(root, held_nodes) {
            return false;
        }
        // The root page may have changed kind before it was taken.
        let still_root = self.root_page().is_some_and(|root_page| {
            root_page.node().node_ptr().tagged() == root.node_ptr().tagged()
        });
        still_root
            && Self::take_border::<FirstEdge>(root, held_nodes)
            && Self::take_border::<LastEdge>(root, held_nodes)
    }

    /// Takes the nodes below `top`, which the caller holds, down its `End`
    /// edges: each where the one above it, held, keeps it.
    fn take_border<End: NodeEnd>(
        top: Node<K, V, Latched<S>>,
        held_nodes: &mut HeldNodes<K, V, S>,
    ) -> bool {
        let mut node = top;
        while let Node::Internal(internal) = node {
            node = internal.child(End::edge(node));
            if !Self::take_node(node, held_nodes) {
                return false;
            }
        }
        true
    }

    /// Takes `node` at the version it is at: `false` if another thread holds
    /// it.
    fn take_node(node: Node<K, V, Latched<S>>, held_nodes: &mut HeldNodes<K, V, S>) -> bool {
        match node {
            Node::Leaf(leaf) => held_nodes.take_leaf(leaf),
            Node::Internal(internal) => held_nodes.take_internal(internal),
        }
    }
}
