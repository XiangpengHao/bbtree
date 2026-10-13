//! A map built in the browser, with its tree's shape, behind one face for
//! every key type, so the panels draw any of them alike.

use std::ops::Deref;
use std::rc::Rc;

use bbtree::inspect::{NodeView, PageMap};
use bbtree::{Auto, BBTreeMap, InlineKey};

use crate::fences::{Fence, KeyBytes, NodeFences};
use crate::node_inspector::{KeyRow, NodeDetail};
use crate::trace::{Trace, TraceStep};
use crate::tree_shape::{NodeAt, TreeShape};
use crate::workload::KeyKind;

/// How the panels read and show one type of key.
pub(crate) trait VisKey: Ord + InlineKey + 'static {
    /// The key as text, as the lookup bar takes it back.
    fn text(&self) -> String;

    /// The key the text names, if it names one.
    fn parse(text: &str) -> Option<Self>
    where
        Self: Sized;

    /// The key as text, split where its first `prefix_len` bytes end, if
    /// the key is its bytes, as byte strings and arrays are: those bytes are
    /// shared by every key of the node, which skips them. Other keys come
    /// back whole, in the second part.
    fn text_split_at(&self, prefix_len: usize) -> (String, String);

    /// How many bytes the key's encoding has, past which its windows read
    /// zeros.
    fn encoded_len(&self) -> usize;

    /// The key's bytes as bbtree orders and windows it: its `InlineKey`
    /// encoding, read a window at a time, as bbtree reads it.
    fn encoding(&self) -> Vec<u8> {
        let len = self.encoded_len();
        let mut bytes: Vec<u8> = (0..len)
            .step_by(8)
            .flat_map(|offset| self.inline_key_at(offset).to_be_bytes())
            .collect();
        bytes.truncate(len);
        bytes
    }

    /// The key as the fence strip draws it.
    fn key_bytes(&self) -> KeyBytes {
        KeyBytes::new(self.text(), self.encoding())
    }
}

impl VisKey for u64 {
    fn text(&self) -> String {
        self.to_string()
    }

    fn parse(text: &str) -> Option<Self> {
        text.trim().replace(['_', ',', ' '], "").parse().ok()
    }

    fn text_split_at(&self, _: usize) -> (String, String) {
        (String::new(), self.text())
    }

    fn encoded_len(&self) -> usize {
        size_of::<u64>()
    }
}

impl VisKey for String {
    fn text(&self) -> String {
        self.clone()
    }

    fn parse(text: &str) -> Option<Self> {
        Some(text.to_string())
    }

    fn text_split_at(&self, prefix_len: usize) -> (String, String) {
        let bytes = self.as_bytes();
        let split = prefix_len.min(bytes.len());
        (
            String::from_utf8_lossy(&bytes[..split]).into_owned(),
            String::from_utf8_lossy(&bytes[split..]).into_owned(),
        )
    }

    fn encoded_len(&self) -> usize {
        self.len()
    }
}

impl VisKey for [u8; 16] {
    fn text(&self) -> String {
        self.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn parse(text: &str) -> Option<Self> {
        let digits: String = text
            .trim()
            .trim_start_matches("0x")
            .chars()
            .filter(|digit| !digit.is_whitespace() && *digit != '_')
            .collect();
        if digits.len() != 32 || !digits.is_ascii() {
            return None;
        }
        let mut id = [0; 16];
        for (index, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&digits[2 * index..2 * index + 2], 16).ok()?;
        }
        Some(id)
    }

    fn text_split_at(&self, prefix_len: usize) -> (String, String) {
        let text = self.text();
        let split = 2 * prefix_len.min(self.len());
        (text[..split].to_string(), text[split..].to_string())
    }

    fn encoded_len(&self) -> usize {
        self.len()
    }
}

/// A built map, whatever its key type.
pub(crate) trait InspectedTree {
    fn shape(&self) -> &TreeShape;

    fn key_kind(&self) -> KeyKind;

    /// The lookup of the key `query` names, as the map takes it; or why
    /// `query` names no key of this map's type.
    fn trace(&self, query: &str) -> Result<Trace, String>;

    /// How many leading bytes every key of the map shares, as the map
    /// keeps them (D50): the prefix of the nodes along the tree's borders.
    fn shared_prefix_len(&self) -> usize;

    /// Key `index` of the node at `at`, as text, split after the bytes the
    /// node's search skips, if the key is its bytes.
    fn key_text_split(&self, at: NodeAt, index: usize) -> (String, String);

    /// The layout of the node at `at`'s page.
    fn page_map(&self, at: NodeAt) -> PageMap;

    /// Everything the inspector shows of the node at `at`, read from the
    /// node itself.
    fn detail(&self, at: NodeAt) -> NodeDetail;

    /// The first key stored under the node at `at`: in its leftmost leaf.
    fn first_key_under(&self, at: NodeAt) -> Option<String>;

    /// A key of the map, picked by `seed`.
    fn random_key(&self, seed: u64) -> Option<String>;
}

/// A map with the shape of its tree, read once when built.
pub(crate) struct InspectedMap<K> {
    map: BBTreeMap<K, u64>,
    shape: TreeShape,
    kind: KeyKind,
}

impl<K: VisKey> InspectedMap<K> {
    pub(crate) fn new(map: BBTreeMap<K, u64>, kind: KeyKind) -> Self {
        let shape = TreeShape::of(&map.inspect());
        Self { map, shape, kind }
    }

    /// The node at `at`, reached from the root along the shape's path.
    fn node(&self, at: NodeAt) -> NodeView<'_, K, u64, Auto> {
        let tree = self.map.inspect();
        let mut node = tree.root().expect("a node is in a tree with a root");
        for edge in self.shape.path_to(at) {
            node = node
                .child(edge)
                .expect("a path leads through internal nodes");
        }
        node
    }

    /// The fences of the node at `at`: the keys just left and just right of
    /// the way down to it, in the nearest ancestors that have one (bbtree's
    /// `PathMut::bounds`, `doc/05-fence-key.md`); with its own first and
    /// last keys.
    fn fences(&self, at: NodeAt) -> NodeFences {
        let tree = self.map.inspect();
        let (Some(mut node), Some(mut node_at)) = (tree.root(), self.shape.root()) else {
            return NodeFences::default();
        };
        let mut way_down = Vec::new();
        for edge in self.shape.path_to(at) {
            way_down.push((node, node_at, edge));
            node = node
                .child(edge)
                .expect("a path leads through internal nodes");
            node_at = self.shape.child(node_at, edge);
        }
        let fence = |ancestor: NodeView<'_, K, u64, Auto>, owner: NodeAt, slot: usize| {
            Fence::new(ancestor.key(slot).key_bytes(), owner, slot)
        };
        let (mut low, mut high) = (None, None);
        for &(ancestor, owner, edge) in way_down.iter().rev() {
            if low.is_none() && edge > 0 {
                low = Some(fence(ancestor, owner, edge - 1));
            }
            if high.is_none() && edge < ancestor.len() {
                high = Some(fence(ancestor, owner, edge));
            }
            if low.is_some() && high.is_some() {
                break;
            }
        }
        let first = (!node.is_empty()).then(|| node.key(0).key_bytes());
        let last = (!node.is_empty()).then(|| node.key(node.len() - 1).key_bytes());
        NodeFences::new(low, high, first, last)
    }

    /// An inline key as the node keeps it: its first `width` bytes, in
    /// hex, a byte at a time.
    fn inline_key_hex(word: u64, width: usize) -> String {
        word.to_be_bytes()[..width]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl<K: VisKey> InspectedTree for InspectedMap<K> {
    fn shape(&self) -> &TreeShape {
        &self.shape
    }

    fn key_kind(&self) -> KeyKind {
        self.kind
    }

    fn trace(&self, query: &str) -> Result<Trace, String> {
        let key = K::parse(query).ok_or_else(|| self.kind.parse_error(query))?;
        let view = self.map.inspect();
        let mut steps = Vec::new();
        if let Some(mut at) = self.shape.root() {
            for step in view.trace(&key) {
                let width = step.node().inline_key_width();
                let inline_key = Option::zip(step.inline_key(), width)
                    .map(|(word, width)| Self::inline_key_hex(word, width));
                let edge = step.edge_taken();
                steps.push(TraceStep::new(
                    at,
                    step.outcome(),
                    inline_key,
                    step.tied(),
                    edge,
                ));
                if let Some(edge) = edge {
                    at = self.shape.child(at, edge);
                }
            }
        }
        let placed = view.placed_by_shared_prefix(&key);
        Ok(Trace::new(key.key_bytes(), steps, placed))
    }

    fn shared_prefix_len(&self) -> usize {
        self.map.inspect().shared_prefix().len()
    }

    fn key_text_split(&self, at: NodeAt, index: usize) -> (String, String) {
        let node = self.node(at);
        node.key(index).text_split_at(node.prefix_len())
    }

    fn page_map(&self, at: NodeAt) -> PageMap {
        self.node(at).page_map()
    }

    fn detail(&self, at: NodeAt) -> NodeDetail {
        let node = self.node(at);
        let skipped = node.prefix_len();
        let width = node.inline_key_width();
        let keys = (0..node.len())
            .map(|index| {
                let (shared, rest) = node.key(index).text_split_at(skipped);
                KeyRow {
                    shared,
                    rest,
                    inline_key: Option::zip(node.inline_key(index), width)
                        .map(|(word, width)| Self::inline_key_hex(word, width)),
                    length_tag: node.length_tag(index),
                    value: node.value(index).map(u64::to_string),
                }
            })
            .collect();
        let child_lens = (0..=node.len())
            .filter_map(|edge| node.child(edge))
            .map(|child| child.len())
            .collect();
        NodeDetail::read(
            at,
            &node,
            keys,
            child_lens,
            node.head().map(u64::to_string),
            self.fences(at),
        )
    }

    fn first_key_under(&self, at: NodeAt) -> Option<String> {
        let mut node = self.node(at);
        while let Some(child) = node.child(0) {
            node = child;
        }
        (!node.is_empty()).then(|| node.key(0).text())
    }

    fn random_key(&self, seed: u64) -> Option<String> {
        let leaves = self.shape.leaf_count();
        if leaves == 0 {
            return None;
        }
        let node = self.node(NodeAt::new(0, (seed % leaves as u64) as usize));
        let index = ((seed >> 32) % node.len().max(1) as u64) as usize;
        (!node.is_empty()).then(|| node.key(index).text())
    }
}

/// A built tree as the panels hold it: shared, and equal only to itself,
/// so a new build redraws everything and nothing else does.
#[derive(Clone)]
pub(crate) struct LoadedTree(Rc<dyn InspectedTree>);

impl LoadedTree {
    pub(crate) fn new(tree: impl InspectedTree + 'static) -> Self {
        Self(Rc::new(tree))
    }
}

impl PartialEq for LoadedTree {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Deref for LoadedTree {
    type Target = dyn InspectedTree;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_back_from_their_text() {
        assert_eq!(u64::parse(" 1,234_567 "), Some(1_234_567));
        assert_eq!(u64::parse("x"), None);
        let id = [0xab; 16];
        assert_eq!(<[u8; 16]>::parse(&id.text()), Some(id));
        assert_eq!(<[u8; 16]>::parse("abcd"), None);
        assert_eq!(String::parse("a/b").unwrap().text(), "a/b");
    }

    #[test]
    fn traces_end_where_the_shape_has_the_node() {
        let map: BBTreeMap<u64, u64> = (0..50_000).map(|key| (key * 3 % 50_000, key)).collect();
        let tree = InspectedMap::new(map, KeyKind::Integers);
        for query in ["31337", "0", "49999"] {
            let trace = tree.trace(query).unwrap();
            assert_eq!(trace.steps().last().unwrap().at().level(), 0);
            // In a leaf, or as a separator above the leaf that holds its value.
            let step = trace
                .steps()
                .iter()
                .find(|step| step.found().is_some())
                .unwrap();
            let (shared, rest) = tree.key_text_split(step.at(), step.found().unwrap());
            assert_eq!(shared + &rest, query);
        }
        assert!(tree.trace("not a number").is_err());
    }
}
