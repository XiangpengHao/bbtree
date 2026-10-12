//! The shape of a built tree, level by level in key order: what the canvas
//! draws from at every zoom without reading every page on every frame. It
//! caches what bbtree's node views say about each node
//! (`BBTreeMap::inspect`), and computes nothing that bbtree computes.

use std::ops::Range;

use bbtree::InlineKeyStrategy;
use bbtree::inspect::{NodeView, TreeView};

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
    entries: usize,
    /// The longest prefix any node skips, which colors by prefix scale to.
    longest_prefix: usize,
    leaf_page_size: usize,
    internal_page_size: Option<usize>,
}

/// One level's nodes, in key order. The x axis counts leaves: leaf `i`
/// spans `i..i + 1`, and an internal node the leaves below it.
pub(crate) struct Level {
    capacity: usize,
    min_len: usize,
    /// Node `i` covers leaves `first_leaf[i]..first_leaf[i + 1]`.
    first_leaf: Vec<u32>,
    /// Node `i`'s children are nodes `first_child[i]..first_child[i + 1]`
    /// of the level below; empty at the leaves.
    first_child: Vec<u32>,
    /// Each node's parent, an index into the level above; 0 at the root.
    parent: Vec<u32>,
    len: Vec<u16>,
    prefix_len: Vec<u16>,
    /// Running sums: entry `i` sums nodes `0..i`, so that a run of nodes
    /// has its mean in constant time, however many it holds.
    len_sum: Vec<u64>,
    prefix_sum: Vec<u64>,
}

/// One level's numbers, for the tree's summary.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LevelStats {
    pub(crate) level: usize,
    pub(crate) nodes: usize,
    pub(crate) keys: u64,
    pub(crate) capacity: usize,
    pub(crate) mean_fill: f64,
    pub(crate) min_len: usize,
    pub(crate) fewest_keys: usize,
}

impl TreeShape {
    /// Reads every node of `tree` once, top down a level at a time: the
    /// children of node `i` follow those of node `i - 1`, so each level comes
    /// out in key order. `entries` is the map's length.
    pub(crate) fn of<K, V, S: InlineKeyStrategy<K>>(
        tree: &TreeView<'_, K, V, S>,
        entries: usize,
    ) -> Self {
        let Some(root) = tree.root() else {
            return Self {
                levels: Vec::new(),
                entries,
                longest_prefix: 0,
                leaf_page_size: 0,
                internal_page_size: None,
            };
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
        let leaf_page_size = top_down
            .last()
            .map_or(0, |leaves| leaves[0].page_map().size());
        let internal_page_size = (top_down.len() > 1).then(|| root.page_map().size());

        let mut levels: Vec<Level> = Vec::with_capacity(top_down.len());
        for (depth, nodes) in top_down.iter().enumerate().rev() {
            let first_child = first_children.get(depth).cloned().unwrap_or_default();
            let first_leaf = match levels.last() {
                None => (0..=nodes.len() as u32).collect(),
                Some(below) => first_child
                    .iter()
                    .map(|&child| below.first_leaf[child as usize])
                    .collect(),
            };
            let level = Level::new(nodes, first_leaf, first_child);
            if let Some(below) = levels.last_mut() {
                below.parent = level.parents_of_children(below.node_count());
            }
            levels.push(level);
        }
        let longest_prefix = levels.iter().map(Level::longest_prefix).max().unwrap_or(0);
        Self {
            levels,
            entries,
            longest_prefix,
            leaf_page_size,
            internal_page_size,
        }
    }

    pub(crate) fn longest_prefix(&self) -> usize {
        self.longest_prefix
    }

    /// Levels above the leaves; `None` for a tree without nodes.
    pub(crate) fn height(&self) -> Option<usize> {
        self.levels.len().checked_sub(1)
    }

    pub(crate) fn level(&self, level: usize) -> &Level {
        &self.levels[level]
    }

    pub(crate) fn entries(&self) -> usize {
        self.entries
    }

    pub(crate) fn leaf_count(&self) -> usize {
        self.levels.first().map_or(0, Level::node_count)
    }

    pub(crate) fn node_count(&self) -> usize {
        self.levels.iter().map(Level::node_count).sum()
    }

    /// The bytes all the tree's pages take.
    pub(crate) fn page_bytes(&self) -> usize {
        let internal_nodes = self.node_count() - self.leaf_count();
        self.leaf_count() * self.leaf_page_size
            + internal_nodes * self.internal_page_size.unwrap_or(0)
    }

    pub(crate) fn leaf_page_size(&self) -> usize {
        self.leaf_page_size
    }

    pub(crate) fn internal_page_size(&self) -> Option<usize> {
        self.internal_page_size
    }

    /// The node of `level` over leaf position `x`, if there is one.
    pub(crate) fn node_over(&self, level: usize, x: f64) -> Option<NodeAt> {
        let nodes = self.levels.get(level)?.nodes_across(x..x);
        (!nodes.is_empty()).then(|| NodeAt::new(level, nodes.start))
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

    /// Each level's numbers, the root's first.
    pub(crate) fn level_stats(&self) -> Vec<LevelStats> {
        (0..self.levels.len())
            .rev()
            .map(|level| self.levels[level].stats(level))
            .collect()
    }
}

impl Level {
    fn new<K, V, S: InlineKeyStrategy<K>>(
        nodes: &[NodeView<'_, K, V, S>],
        first_leaf: Vec<u32>,
        first_child: Vec<u32>,
    ) -> Self {
        let len: Vec<u16> = nodes.iter().map(|node| node.len() as u16).collect();
        let prefix_len: Vec<u16> = nodes
            .iter()
            .map(|node| node.local_prefix_len().unwrap_or(node.prefix_len()) as u16)
            .collect();
        Self {
            capacity: nodes[0].capacity(),
            min_len: nodes[0].min_len(),
            first_leaf,
            first_child,
            parent: vec![0; nodes.len()],
            len_sum: Self::running_sum(&len),
            prefix_sum: Self::running_sum(&prefix_len),
            len,
            prefix_len,
        }
    }

    fn running_sum(values: &[u16]) -> Vec<u64> {
        let mut sum = 0;
        std::iter::once(0)
            .chain(values.iter().map(|&value| {
                sum += u64::from(value);
                sum
            }))
            .collect()
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

    /// The prefix the node's searches skip: its local prefix if it keeps
    /// one, else the one its fences share.
    pub(crate) fn prefix_len(&self, node: usize) -> usize {
        usize::from(self.prefix_len[node])
    }

    /// The leaf positions node `node` covers.
    pub(crate) fn span(&self, node: usize) -> Range<f64> {
        f64::from(self.first_leaf[node])..f64::from(self.first_leaf[node + 1])
    }

    /// The nodes of the level below that are node `node`'s children.
    pub(crate) fn children(&self, node: usize) -> Range<usize> {
        match self.first_child.get(node + 1) {
            Some(&end) => self.first_child[node] as usize..end as usize,
            None => 0..0,
        }
    }

    pub(crate) fn parent(&self, node: usize) -> usize {
        self.parent[node] as usize
    }

    /// The nodes whose spans overlap `x`, or for an empty `x`, the one
    /// whose span holds it.
    pub(crate) fn nodes_across(&self, x: Range<f64>) -> Range<usize> {
        let count = self.node_count();
        // The first node that ends past where `x` starts.
        let first = self.first_leaf[1..].partition_point(|&end| f64::from(end) <= x.start);
        let end = if x.is_empty() {
            let holds = first < count && f64::from(self.first_leaf[first]) <= x.start;
            first + usize::from(holds)
        } else {
            self.first_leaf[..count].partition_point(|&start| f64::from(start) < x.end)
        };
        first..end.max(first)
    }

    /// How full the nodes `nodes` are, together: their keys over their
    /// room.
    pub(crate) fn mean_fill(&self, nodes: Range<usize>) -> f64 {
        let keys = self.len_sum[nodes.end] - self.len_sum[nodes.start];
        keys as f64 / (nodes.len() * self.capacity).max(1) as f64
    }

    /// The mean prefix length of the nodes `nodes`.
    pub(crate) fn mean_prefix(&self, nodes: Range<usize>) -> f64 {
        let bytes = self.prefix_sum[nodes.end] - self.prefix_sum[nodes.start];
        bytes as f64 / nodes.len().max(1) as f64
    }

    /// The longest prefix any node of the level skips.
    fn longest_prefix(&self) -> usize {
        self.prefix_len.iter().copied().max().map_or(0, usize::from)
    }

    fn stats(&self, level: usize) -> LevelStats {
        LevelStats {
            level,
            nodes: self.node_count(),
            keys: self.len_sum[self.node_count()],
            capacity: self.capacity,
            mean_fill: self.mean_fill(0..self.node_count()),
            min_len: self.min_len,
            fewest_keys: self.len.iter().copied().min().map_or(0, usize::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::BBTreeMap;

    use super::*;

    fn map_of(count: u64) -> BBTreeMap<u64, u64> {
        (0..count).map(|key| (key * 7 % count, key)).collect()
    }

    #[test]
    fn levels_cover_every_node_in_key_order() {
        let map = map_of(100_000);
        let shape = TreeShape::of(&map.inspect(), map.len());
        let height = shape.height().unwrap();
        assert!(height >= 2);
        assert_eq!(shape.level(height).node_count(), 1);
        let keys: u64 = shape.level_stats().iter().map(|stats| stats.keys).sum();
        assert_eq!(keys as usize, map.len());
        for level in 1..=height {
            let nodes = shape.level(level);
            let below = shape.level(level - 1);
            assert_eq!(
                nodes.children(nodes.node_count() - 1).end,
                below.node_count()
            );
            for node in 0..nodes.node_count() {
                let children = nodes.children(node);
                assert_eq!(children.len(), nodes.len(node) + 1);
                assert_eq!(nodes.span(node).start, below.span(children.start).start);
                assert_eq!(nodes.span(node).end, below.span(children.end - 1).end);
                assert!(children.clone().all(|child| below.parent(child) == node));
            }
        }
    }

    #[test]
    fn paths_lead_back_to_the_node() {
        let map = map_of(50_000);
        let tree = map.inspect();
        let shape = TreeShape::of(&tree, map.len());
        let leaves = shape.level(0);
        for index in [0, leaves.node_count() / 3, leaves.node_count() - 1] {
            let at = NodeAt::new(0, index);
            let mut node = tree.root().unwrap();
            for edge in shape.path_to(at) {
                node = node.child(edge).unwrap();
            }
            assert!(node.is_leaf());
            assert_eq!(node.len(), leaves.len(index));
        }
    }

    #[test]
    fn nodes_across_a_span_are_those_overlapping_it() {
        let map = map_of(50_000);
        let shape = TreeShape::of(&map.inspect(), map.len());
        let leaves = shape.level(0);
        let count = leaves.node_count();
        assert_eq!(leaves.nodes_across(0.0..1.0), 0..1);
        assert_eq!(leaves.nodes_across(-5.0..1e9), 0..count);
        assert_eq!(leaves.nodes_across(2.5..2.5), 2..3);
        assert_eq!(leaves.nodes_across(1e9..2e9), count..count);
        assert_eq!(shape.node_over(0, 3.2), Some(NodeAt::new(0, 3)));
        assert_eq!(shape.node_over(0, -1.0), None);
        let root = shape.height().unwrap();
        assert_eq!(
            shape.node_over(root, count as f64 / 2.0),
            Some(NodeAt::new(root, 0))
        );
    }

    #[test]
    fn fill_is_keys_over_room() {
        let map = map_of(20_000);
        let shape = TreeShape::of(&map.inspect(), map.len());
        let leaves = shape.level(0);
        let all = 0..leaves.node_count();
        let keys: usize = all.clone().map(|node| leaves.len(node)).sum();
        let expected = keys as f64 / (leaves.node_count() * leaves.capacity()) as f64;
        assert!((leaves.mean_fill(all) - expected).abs() < 1e-12);
    }
}
