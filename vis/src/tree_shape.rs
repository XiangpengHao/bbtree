//! The shape of a built tree, level by level in key order: where each node
//! is, its parent and children, and its key count and prefix, so the panels
//! can name nodes, walk to neighbors and draw gauges without a page read.
//! It caches what bbtree's node views say about each node
//! (`unsync::BBTreeMap::inspect`), and computes nothing that bbtree computes.

use std::ops::Range;

use bbtree::InlineKeyStrategy;
use bbtree::inspect::{NodeView, TreeView};

use crate::count_text::Group;

/// A node's place in the tree: its level, 0 for the leaves, and its index
/// among that level's nodes, in key order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NodeAt {
    level: usize,
    index: usize,
}

impl NodeAt {
    pub(crate) fn new(level: usize, index: usize) -> Self {
        Self { level, index }
    }

    pub(crate) fn level(&self) -> usize {
        self.level
    }

    pub(crate) fn index(&self) -> usize {
        self.index
    }
}

/// Every level of a tree, the leaves first.
pub(crate) struct TreeShape {
    levels: Vec<Level>,
    leaf_page_size: usize,
    internal_page_size: usize,
    /// The root's page: a node's room and the root area (D52).
    root_page_size: usize,
}

/// One level's nodes, in key order.
pub(crate) struct Level {
    capacity: usize,
    /// Node `i`'s children are nodes `first_child[i]..first_child[i + 1]`
    /// of the level below; empty at the leaves.
    first_child: Vec<u32>,
    /// Each node's parent, an index into the level above; 0 at the root.
    parent: Vec<u32>,
    len: Vec<u16>,
    prefix_len: Vec<u16>,
}

impl TreeShape {
    /// Reads every node of `tree` once, top down a level at a time: the
    /// children of node `i` follow those of node `i - 1`, so each level comes
    /// out in key order.
    pub(crate) fn of<K, V, S: InlineKeyStrategy<K>>(tree: &TreeView<'_, K, V, S>) -> Self {
        let mut shape = Self {
            levels: Vec::new(),
            leaf_page_size: 0,
            internal_page_size: 0,
            root_page_size: 0,
        };
        let Some(root) = tree.root() else {
            return shape;
        };
        let mut top_down: Vec<Vec<NodeView<'_, K, V, S>>> = vec![vec![root]];
        let mut first_children: Vec<Vec<u32>> = Vec::new();
        while let Some(above) = top_down.last().filter(|nodes| !nodes[0].is_leaf()) {
            let mut below = Vec::new();
            let mut first_child = Vec::with_capacity(above.len() + 1);
            for node in above {
                first_child.push(below.len() as u32);
                below.extend((0..=node.len()).filter_map(|edge| node.child(edge)));
            }
            first_child.push(below.len() as u32);
            first_children.push(first_child);
            top_down.push(below);
        }
        shape.leaf_page_size = top_down
            .last()
            .map_or(0, |leaves| leaves[0].page_map().size());
        shape.root_page_size = root.page_map().size();
        // An internal node below the root has a page of its own size.
        shape.internal_page_size = top_down
            .iter()
            .skip(1)
            .find(|nodes| !nodes[0].is_leaf())
            .map_or(shape.root_page_size, |nodes| nodes[0].page_map().size());
        for (depth, nodes) in top_down.iter().enumerate().rev() {
            let level = Level::new(
                nodes,
                first_children.get(depth).cloned().unwrap_or_default(),
            );
            if let Some(below) = shape.levels.last_mut() {
                below.parent = level.parents_of_children(below.node_count());
            }
            shape.levels.push(level);
        }
        shape
    }

    /// How many levels the tree has, the leaves' and the root's included.
    pub(crate) fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// The root's level, the last; `None` for a tree without nodes.
    fn root_level(&self) -> Option<usize> {
        self.levels.len().checked_sub(1)
    }

    pub(crate) fn level(&self, level: usize) -> &Level {
        &self.levels[level]
    }

    pub(crate) fn leaf_count(&self) -> usize {
        self.levels.first().map_or(0, Level::node_count)
    }

    /// The bytes all the tree's pages take.
    pub(crate) fn page_bytes(&self) -> usize {
        (0..self.levels.len())
            .map(|level| self.level_bytes(level))
            .sum()
    }

    /// The bytes the pages of one level take.
    pub(crate) fn level_bytes(&self, level: usize) -> usize {
        if Some(level) == self.root_level() {
            self.root_page_size
        } else if level == 0 {
            self.leaf_count() * self.leaf_page_size
        } else {
            self.levels[level].node_count() * self.internal_page_size
        }
    }

    /// The root, if the tree has nodes.
    pub(crate) fn root(&self) -> Option<NodeAt> {
        self.root_level().map(|level| NodeAt::new(level, 0))
    }

    /// The child at edge `edge` of the internal node at `at`.
    pub(crate) fn child(&self, at: NodeAt, edge: usize) -> NodeAt {
        let children = self.levels[at.level].children(at.index);
        assert!(edge < children.len(), "edge out of bounds");
        NodeAt::new(at.level - 1, children.start + edge)
    }

    /// The node `delta` places right of `at` on its level, if there is one.
    pub(crate) fn neighbor(&self, at: NodeAt, delta: isize) -> Option<NodeAt> {
        let index = at.index.checked_add_signed(delta)?;
        (index < self.levels[at.level].node_count()).then(|| NodeAt::new(at.level, index))
    }

    /// The node's name in a sentence: "the root", "leaf 1,204", "level 2
    /// node 7".
    pub(crate) fn node_name(&self, at: NodeAt) -> String {
        if Some(at.level) == self.root_level() {
            "the root".to_string()
        } else if at.level == 0 {
            format!("leaf {}", Group(at.index + 1))
        } else {
            format!("level {} node {}", at.level, Group(at.index + 1))
        }
    }

    /// The node's name in a label: "root", "leaf 1,204", "node 7".
    pub(crate) fn short_name(&self, at: NodeAt) -> String {
        if Some(at.level) == self.root_level() {
            "root".to_string()
        } else if at.level == 0 {
            format!("leaf {}", Group(at.index + 1))
        } else {
            format!("node {}", Group(at.index + 1))
        }
    }

    /// The level's name on its own: "root", "leaf", "level 2".
    pub(crate) fn level_name(&self, level: usize) -> String {
        if Some(level) == self.root_level() {
            "root".to_string()
        } else if level == 0 {
            "leaf".to_string()
        } else {
            format!("level {level}")
        }
    }

    /// The child indices that lead from the root down to `at`.
    pub(crate) fn path_to(&self, at: NodeAt) -> Vec<usize> {
        let mut path = Vec::new();
        let mut index = at.index;
        for level in at.level..self.levels.len() - 1 {
            let parent = self.levels[level].parent[index] as usize;
            path.push(index - self.levels[level + 1].first_child[parent] as usize);
            index = parent;
        }
        path.reverse();
        path
    }
}

impl Level {
    fn new<K, V, S: InlineKeyStrategy<K>>(
        nodes: &[NodeView<'_, K, V, S>],
        first_child: Vec<u32>,
    ) -> Self {
        Self {
            capacity: nodes[0].capacity(),
            first_child,
            parent: vec![0; nodes.len()],
            len: nodes.iter().map(|node| node.len() as u16).collect(),
            prefix_len: nodes.iter().map(|node| node.prefix_len() as u16).collect(),
        }
    }

    /// The parent of each of the `children` nodes of the level below.
    fn parents_of_children(&self, children: usize) -> Vec<u32> {
        let mut parent = vec![0; children];
        for node in 0..self.node_count() {
            for child in self.children(node) {
                parent[child] = node as u32;
            }
        }
        parent
    }

    pub(crate) fn node_count(&self) -> usize {
        self.len.len()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn len(&self, node: usize) -> usize {
        usize::from(self.len[node])
    }

    /// The prefix the node's search skips: what its fences share, or on the
    /// tree's border, what every key shares (D50).
    pub(crate) fn prefix_len(&self, node: usize) -> usize {
        usize::from(self.prefix_len[node])
    }

    /// The nodes of the level below that are node `node`'s children.
    pub(crate) fn children(&self, node: usize) -> Range<usize> {
        match self.first_child.get(node + 1) {
            Some(&end) => self.first_child[node] as usize..end as usize,
            None => 0..0,
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::unsync::BBTreeMap;

    use super::*;

    fn map_of(count: u64) -> BBTreeMap<u64, u64> {
        (0..count).map(|key| (key * 7 % count, key)).collect()
    }

    #[test]
    fn levels_hold_every_node_once_under_its_parent() {
        let map = map_of(100_000);
        let shape = TreeShape::of(&map.inspect());
        let root_level = shape.root_level().unwrap();
        assert!(root_level >= 2);
        assert_eq!(shape.level(root_level).node_count(), 1);
        let keys: usize = (0..=root_level)
            .map(|level| {
                let nodes = shape.level(level);
                (0..nodes.node_count())
                    .map(|node| nodes.len(node))
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(keys, map.len());
        for level in 1..=root_level {
            let nodes = shape.level(level);
            let below = shape.level(level - 1);
            assert_eq!(
                nodes.children(nodes.node_count() - 1).end,
                below.node_count()
            );
            for node in 0..nodes.node_count() {
                assert_eq!(nodes.children(node).len(), nodes.len(node) + 1);
                for (edge, child) in nodes.children(node).enumerate() {
                    assert_eq!(
                        shape.child(NodeAt::new(level, node), edge),
                        NodeAt::new(level - 1, child)
                    );
                    assert_eq!(below.parent[child] as usize, node);
                }
            }
        }
    }

    #[test]
    fn paths_lead_back_to_the_node() {
        let map = map_of(50_000);
        let tree = map.inspect();
        let shape = TreeShape::of(&tree);
        let leaves = shape.level(0);
        for index in [0, leaves.node_count() / 3, leaves.node_count() - 1] {
            let mut node = tree.root().unwrap();
            for edge in shape.path_to(NodeAt::new(0, index)) {
                node = node.child(edge).unwrap();
            }
            assert!(node.is_leaf());
            assert_eq!(node.len(), leaves.len(index));
        }
    }

    #[test]
    fn neighbors_stay_on_their_level() {
        let map = map_of(20_000);
        let shape = TreeShape::of(&map.inspect());
        let last = shape.leaf_count() - 1;
        assert_eq!(shape.neighbor(NodeAt::new(0, 0), -1), None);
        assert_eq!(
            shape.neighbor(NodeAt::new(0, 0), 1),
            Some(NodeAt::new(0, 1))
        );
        assert_eq!(shape.neighbor(NodeAt::new(0, last), 1), None);
        assert_eq!(shape.node_name(shape.root().unwrap()), "the root");
        assert_eq!(shape.node_name(NodeAt::new(0, 203)), "leaf 204");
    }
}
