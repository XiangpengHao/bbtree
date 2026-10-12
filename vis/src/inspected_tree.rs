//! A map built in the browser, with its tree's shape, behind one face for
//! every key type, so the panels draw any of them alike.

use std::ops::Deref;
use std::rc::Rc;

use bbtree::BBTreeMap;
use bbtree::inspect::{NodeView, PageMap};

use crate::node_detail::NodeDetail;
use crate::tree_shape::{NodeAt, TreeShape};
use crate::workload::KeyKind;

/// How the panels show one type of key.
pub(crate) trait VisKey: Ord + 'static {
    /// The key as text, split where the first `prefix_len` bytes end, if
    /// the key is its bytes, as byte strings and arrays are: those bytes are
    /// shared by every key of the node, which skips them. Other keys come
    /// back whole, in the second part.
    fn text_split_at(&self, prefix_len: usize) -> (String, String);
}

impl VisKey for u64 {
    fn text_split_at(&self, _: usize) -> (String, String) {
        (String::new(), self.to_string())
    }
}

impl VisKey for String {
    fn text_split_at(&self, prefix_len: usize) -> (String, String) {
        let bytes = self.as_bytes();
        let split = prefix_len.min(bytes.len());
        (
            String::from_utf8_lossy(&bytes[..split]).into_owned(),
            String::from_utf8_lossy(&bytes[split..]).into_owned(),
        )
    }
}

impl VisKey for [u8; 16] {
    fn text_split_at(&self, prefix_len: usize) -> (String, String) {
        let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let split = prefix_len.min(self.len());
        (hex(&self[..split]), hex(&self[split..]))
    }
}

/// A built map, whatever its key type.
pub(crate) trait InspectedTree {
    fn shape(&self) -> &TreeShape;

    fn key_kind(&self) -> KeyKind;

    /// Everything the detail panel shows of the node at `at`, read from the
    /// node itself.
    fn node_detail(&self, at: NodeAt) -> NodeDetail;

    /// The node's first and last keys, as text.
    fn key_range(&self, at: NodeAt) -> Option<(String, String)>;

    /// Where the node's slot arrays lie in its page.
    fn page_map(&self, at: NodeAt) -> PageMap;
}

/// A map with the shape of its tree, read once when built.
pub(crate) struct InspectedMap<K> {
    map: BBTreeMap<K, u64>,
    shape: TreeShape,
    kind: KeyKind,
}

impl<K: VisKey> InspectedMap<K> {
    pub(crate) fn new(map: BBTreeMap<K, u64>, kind: KeyKind) -> Self {
        let shape = TreeShape::of(&map.inspect(), map.len());
        Self { map, shape, kind }
    }

    /// The node at `at`, reached from the root along the shape's path.
    fn node(&self, at: NodeAt) -> NodeView<'_, K, u64, bbtree::Auto> {
        let tree = self.map.inspect();
        let mut node = tree.root().expect("a node is in a tree with a root");
        for edge in self.shape.path_to(at) {
            node = node
                .child(edge)
                .expect("a path leads through internal nodes");
        }
        node
    }
}

impl<K: VisKey> InspectedTree for InspectedMap<K> {
    fn shape(&self) -> &TreeShape {
        &self.shape
    }

    fn key_kind(&self) -> KeyKind {
        self.kind
    }

    fn node_detail(&self, at: NodeAt) -> NodeDetail {
        NodeDetail::read(self.node(at))
    }

    fn key_range(&self, at: NodeAt) -> Option<(String, String)> {
        let node = self.node(at);
        let last = node.len().checked_sub(1)?;
        let text = |index| {
            let (shared, rest) = node.key(index).text_split_at(0);
            shared + &rest
        };
        Some((text(0), text(last)))
    }

    fn page_map(&self, at: NodeAt) -> PageMap {
        self.node(at).page_map()
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
