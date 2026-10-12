//! Read-only views of a map's nodes, for the visualizer (`vis/`), behind the
//! `inspect` feature, which promises no stability.
//!
//! The views read each node through the same handles the map's own code
//! uses, and say what the node holds as bbtree stores it: its keys, inline
//! keys, length tags and prefixes, and where each slot array of its page
//! lies, from the layout the page was allocated with. Nothing here computes
//! what the node code computes, so whatever draws a view draws the tree as it
//! is (`doc/10-visualization.md`).

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt;
use core::mem::size_of;
use core::ops::Range;

use super::handle::{ForceResult, Immut, Internal, Leaf, LeafOrInternal, NodeRef};
use super::page::{InlineKeyNodeHeader, NodeHeader, NodePtr, PageKind};
use crate::inline_key::{InlineKeyStrategy, InlineWord};

/// A map's tree, read in place: see [`crate::BBTreeMap::inspect`].
pub struct TreeView<'a, K, V, S> {
    root: Option<NodeRef<Immut<'a>, K, V, S, LeafOrInternal>>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> TreeView<'a, K, V, S> {
    pub(crate) fn new(root: Option<NodeRef<Immut<'a>, K, V, S, LeafOrInternal>>) -> Self {
        Self { root }
    }

    /// The root node, or `None` if the map has never held an entry. A map
    /// whose entries were all removed may keep an empty root leaf.
    pub fn root(&self) -> Option<NodeView<'a, K, V, S>> {
        self.root.map(|node| NodeView { node })
    }
}

/// One node of a map's tree, read in place.
pub struct NodeView<'a, K, V, S> {
    node: NodeRef<Immut<'a>, K, V, S, LeafOrInternal>,
}

impl<K, V, S> Clone for NodeView<'_, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for NodeView<'_, K, V, S> {}

/// `$body`, with `$node` bound to `$view`'s handle of its own kind, a leaf's
/// or an internal node's.
macro_rules! with_node {
    ($view:expr, |$node:ident| $body:expr) => {
        match $view.node.force() {
            ForceResult::Leaf($node) => $body,
            ForceResult::Internal($node) => $body,
        }
    };
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> NodeView<'a, K, V, S> {
    /// What tells this node apart from the others of its map, while it lives:
    /// its page's address.
    pub fn id(&self) -> NodeId {
        NodeId(self.node.node_ptr().addr())
    }

    /// How far above the leaves the node is: 0 for a leaf.
    pub fn height(&self) -> usize {
        self.node.height()
    }

    pub fn is_leaf(&self) -> bool {
        self.height() == 0
    }

    /// The number of keys.
    pub fn len(&self) -> usize {
        self.node.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The most keys a node of this kind holds.
    pub fn capacity(&self) -> usize {
        with_node!(self, |node| Self::capacity_of(&node))
    }

    /// The fewest keys the node holds between removals, unless it is the
    /// root (D22).
    pub fn min_len(&self) -> usize {
        with_node!(self, |node| node.min_len())
    }

    /// How many leading bytes the node's fences share, and so every key in
    /// it, which its inline keys skip (05-fence-key.md); 0 for a node
    /// without inline keys.
    pub fn prefix_len(&self) -> usize {
        self.node.prefix_len()
    }

    /// The longer prefix the node's own keys share, which its searches skip
    /// instead, if it keeps one (D33).
    pub fn local_prefix_len(&self) -> Option<usize> {
        with_node!(self, |node| node.local_prefix_len())
    }

    /// Key `index`.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn key(&self, index: usize) -> &'a K {
        with_node!(self, |node| node.keys().get(index)).expect("key index out of bounds")
    }

    /// How many bytes of each key's window the node's inline keys keep, or
    /// `None` if the node has no inline keys (D18).
    pub fn inline_key_width(&self) -> Option<usize> {
        with_node!(self, |node| Self::inline_width_of(&node))
    }

    /// Inline key `index`: the bytes the node keeps of key `index`'s
    /// window, from its first byte, zeros after them; `None` if the node has
    /// no inline keys.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn inline_key(&self, index: usize) -> Option<InlineWord> {
        with_node!(self, |node| node
            .inline_keys()
            .map(|inline_keys| inline_keys.inline_key(index)))
    }

    /// Key `index`'s length tag, if the node keeps them (D36).
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn length_tag(&self, index: usize) -> Option<u16> {
        assert!(index < self.len(), "key index out of bounds");
        with_node!(self, |node| node.length_tags().map(|tags| tags[index]))
    }

    /// The leaf's head, the value of the separator just left of it (D17):
    /// `None` in the leftmost leaf, and in an internal node.
    pub fn head(&self) -> Option<&'a V> {
        match self.node.force() {
            ForceResult::Leaf(leaf) => leaf.head().map(|head| head.into_value()),
            ForceResult::Internal(_) => None,
        }
    }

    /// Value `index` of a leaf; `None` in an internal node, whose keys keep
    /// their values in the leaves right of them, as heads (D17).
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn value(&self, index: usize) -> Option<&'a V> {
        match self.node.force() {
            ForceResult::Leaf(leaf) => Some(leaf.kv_at(index).into_key_value().1),
            ForceResult::Internal(_) => None,
        }
    }

    /// The child at edge `index` of an internal node, left of key `index`;
    /// `None` in a leaf.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    pub fn child(&self, index: usize) -> Option<NodeView<'a, K, V, S>> {
        match self.node.force() {
            ForceResult::Leaf(_) => None,
            ForceResult::Internal(internal) => Some(NodeView {
                node: internal.edge_at(index).descend(),
            }),
        }
    }

    /// The node's page: its size, and where each of its slot arrays lies,
    /// with how many of each array's slots it uses.
    pub fn page_map(&self) -> PageMap {
        match self.node.force() {
            ForceResult::Leaf(leaf) => PageMap::of_leaf::<K, V, S>(&leaf),
            ForceResult::Internal(internal) => PageMap::of_internal::<K, V, S>(&internal),
        }
    }

    fn capacity_of<Kind: PageKind>(_: &NodeRef<Immut<'a>, K, V, S, Kind>) -> usize {
        NodePtr::<K, V, S>::capacity::<Kind>()
    }

    fn inline_width_of<Kind: PageKind>(_: &NodeRef<Immut<'a>, K, V, S, Kind>) -> Option<usize> {
        NodePtr::<K, V, S>::inline_width::<Kind>().map(|width| width.bytes())
    }
}

/// A node's identity: its page's address, unique among the nodes alive at
/// once. A page freed by a merge may come back as a new node.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(usize);

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({:#x})", self.0)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

/// A node's page: its size, and where each of its slot arrays lies, from the
/// layout the page was allocated with (`layout.rs`). Bytes no array covers
/// are padding, or room the layout leaves unused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageMap {
    size: usize,
    arrays: Vec<SlotArray>,
}

impl PageMap {
    /// The page's size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The page's slot arrays, the header first.
    pub fn arrays(&self) -> &[SlotArray] {
        &self.arrays
    }

    fn of_leaf<K, V, S: InlineKeyStrategy<K>>(leaf: &NodeRef<Immut<'_>, K, V, S, Leaf>) -> Self {
        let layout = NodePtr::<K, V, S>::leaf_layout();
        let len = leaf.len();
        let capacity = layout.keys.capacity;
        let mut arrays = Self::header_and_inline_keys::<K, V, S, Leaf>(len);
        if let Some(offset) = layout.length_tags_offset {
            arrays.push(SlotArray::dense(
                SlotContents::LengthTags,
                offset,
                size_of::<u16>(),
                capacity,
                len,
            ));
        }
        arrays.push(SlotArray {
            contents: SlotContents::Keys,
            offset: layout.keys.keys_offset,
            stride: layout.keys.key_stride,
            slot_size: size_of::<K>(),
            capacity,
            used: len,
        });
        arrays.push(SlotArray::dense(
            SlotContents::Head,
            layout.head_offset,
            size_of::<V>(),
            1,
            usize::from(leaf.head().is_some()),
        ));
        arrays.push(SlotArray {
            contents: SlotContents::Values,
            offset: layout.head_offset + layout.value_stride,
            stride: layout.value_stride,
            slot_size: size_of::<V>(),
            capacity,
            used: len,
        });
        Self {
            size: layout.page.size(),
            arrays,
        }
    }

    fn of_internal<K, V, S: InlineKeyStrategy<K>>(
        internal: &NodeRef<Immut<'_>, K, V, S, Internal>,
    ) -> Self {
        let layout = NodePtr::<K, V, S>::internal_layout();
        let len = internal.len();
        let capacity = layout.keys.capacity;
        let mut arrays = Self::header_and_inline_keys::<K, V, S, Internal>(len);
        arrays.push(SlotArray {
            contents: SlotContents::Keys,
            offset: layout.keys.keys_offset,
            stride: layout.keys.key_stride,
            slot_size: size_of::<K>(),
            capacity,
            used: len,
        });
        arrays.push(SlotArray {
            contents: SlotContents::Edges,
            offset: layout.edges_offset,
            stride: layout.edge_stride,
            slot_size: size_of::<NodePtr<K, V, S>>(),
            capacity: capacity + 1,
            used: len + 1,
        });
        Self {
            size: layout.page.size(),
            arrays,
        }
    }

    /// The arrays every page of kind `Kind` starts with: its header, and
    /// its inline keys if it has them, of which a node of `len` keys uses
    /// `len`.
    fn header_and_inline_keys<K, V, S: InlineKeyStrategy<K>, Kind: PageKind>(
        len: usize,
    ) -> Vec<SlotArray> {
        let header_size = if NodePtr::<K, V, S>::has_inline_keys() {
            size_of::<InlineKeyNodeHeader>()
        } else {
            size_of::<NodeHeader>()
        };
        let mut arrays = Vec::from([SlotArray::dense(SlotContents::Header, 0, header_size, 1, 1)]);
        let keys = NodePtr::<K, V, S>::key_area::<Kind>();
        if let (Some(offset), Some(width)) = (
            keys.inline_keys_offset,
            NodePtr::<K, V, S>::inline_width::<Kind>(),
        ) {
            arrays.push(SlotArray::dense(
                SlotContents::InlineKeys,
                offset,
                width.bytes(),
                keys.capacity,
                len,
            ));
        }
        arrays
    }
}

/// One array of a page's slots, all of a size, `stride` bytes apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotArray {
    contents: SlotContents,
    offset: usize,
    stride: usize,
    slot_size: usize,
    capacity: usize,
    used: usize,
}

impl SlotArray {
    /// An array whose slots follow each other with no room between them.
    fn dense(
        contents: SlotContents,
        offset: usize,
        slot_size: usize,
        capacity: usize,
        used: usize,
    ) -> Self {
        Self {
            contents,
            offset,
            stride: slot_size,
            slot_size,
            capacity,
            used,
        }
    }

    pub fn contents(&self) -> SlotContents {
        self.contents
    }

    /// Where slot 0 starts in the page.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// The bytes from one slot to the next: a slot's size, or a whole
    /// entry's where keys pair with values (D29).
    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn slot_size(&self) -> usize {
        self.slot_size
    }

    /// How many slots the page has room for.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many of them, from slot 0, the node holds something in.
    pub fn used(&self) -> usize {
        self.used
    }

    /// The page's bytes slot `index` takes.
    ///
    /// # Panics
    ///
    /// If `index >= capacity`.
    pub fn slot_bytes(&self, index: usize) -> Range<usize> {
        assert!(index < self.capacity, "slot index out of bounds");
        let start = self.offset + index * self.stride;
        start..start + self.slot_size
    }
}

/// What a page's slot array holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotContents {
    /// The node's length, and with inline keys, its prefix lengths.
    Header,
    /// The first bytes of each key's window past the node's prefix (D18).
    InlineKeys,
    /// Each key's length, of byte-string leaves (D36).
    LengthTags,
    Keys,
    /// A leaf's head: the value of the separator just left of it (D17).
    Head,
    Values,
    /// An internal node's children.
    Edges,
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeSet;
    use alloc::string::String;
    use core::fmt::Debug;
    use std::format;

    use super::*;
    use crate::BBTreeMap;
    use crate::node::test_support::TestRng;

    /// What a walk through every node of a map sees.
    #[derive(Default)]
    struct WalkSummary {
        entries: usize,
        leaves: usize,
        nodes_with_inline_keys: usize,
    }

    /// Walks every node in key order, checking what the views say against
    /// what a well-formed tree holds, and each page map against its page.
    struct CheckedWalk<'a, K> {
        previous: Option<&'a K>,
        ids: BTreeSet<NodeId>,
        summary: WalkSummary,
    }

    impl<'a, K: Ord + Debug> CheckedWalk<'a, K> {
        fn of<V: 'a, S: InlineKeyStrategy<K>>(map: &'a BBTreeMap<K, V, S>) -> WalkSummary {
            let mut walk = Self {
                previous: None,
                ids: BTreeSet::new(),
                summary: WalkSummary::default(),
            };
            let tree = map.inspect();
            match tree.root() {
                Some(root) => walk.visit(root, true),
                None => assert_eq!(map.len(), 0),
            }
            assert_eq!(walk.summary.entries, map.len());
            walk.summary
        }

        fn visit<V: 'a, S: InlineKeyStrategy<K>>(
            &mut self,
            node: NodeView<'a, K, V, S>,
            leftmost: bool,
        ) {
            assert!(self.ids.insert(node.id()), "two nodes share a page");
            assert!(node.len() <= node.capacity());
            self.check_page_map(&node.page_map(), node.len());
            if node.inline_key_width().is_some() && !node.is_empty() {
                assert!(node.inline_key(0).is_some());
                self.summary.nodes_with_inline_keys += 1;
            }
            self.summary.entries += node.len();
            if node.is_leaf() {
                self.summary.leaves += 1;
                assert_eq!(node.head().is_some(), !leftmost);
                assert!(node.child(0).is_none());
                for index in 0..node.len() {
                    assert!(node.value(index).is_some());
                    self.see(node.key(index));
                }
                return;
            }
            assert!(node.head().is_none());
            for index in 0..=node.len() {
                let child = node.child(index).expect("an internal node has children");
                assert_eq!(child.height() + 1, node.height());
                self.visit(child, leftmost && index == 0);
                if index < node.len() {
                    assert!(node.value(index).is_none());
                    self.see(node.key(index));
                }
            }
        }

        fn see(&mut self, key: &'a K) {
            if let Some(previous) = self.previous {
                assert!(previous < key, "{previous:?} is not before {key:?}");
            }
            self.previous = Some(key);
        }

        /// Every slot lies within the page, no two used slots overlap, and
        /// the header comes first.
        fn check_page_map(&self, page: &PageMap, len: usize) {
            let arrays = page.arrays();
            assert_eq!(arrays[0].contents(), SlotContents::Header);
            assert_eq!(arrays[0].offset(), 0);
            let mut taken: Vec<Range<usize>> = Vec::new();
            for array in arrays {
                assert!(array.used() <= array.capacity());
                if array.capacity() > 0 && array.slot_size() > 0 {
                    assert!(array.slot_bytes(array.capacity() - 1).end <= page.size());
                }
                if matches!(
                    array.contents(),
                    SlotContents::Keys | SlotContents::Values | SlotContents::InlineKeys
                ) {
                    assert_eq!(array.used(), len);
                }
                for index in 0..array.used() {
                    let bytes = array.slot_bytes(index);
                    if bytes.is_empty() {
                        continue;
                    }
                    for other in &taken {
                        assert!(
                            bytes.end <= other.start || other.end <= bytes.start,
                            "{:?} slot {index} overlaps another slot",
                            array.contents()
                        );
                    }
                    taken.push(bytes);
                }
            }
        }
    }

    fn shuffled(count: usize, seed: u64) -> Vec<usize> {
        let mut numbers: Vec<usize> = (0..count).collect();
        TestRng::new(seed).shuffle(&mut numbers);
        numbers
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn integer_keys_walk_in_order_with_inline_keys() {
        let mut map = BBTreeMap::new();
        for number in shuffled(20_000, 1) {
            map.insert(number as u64, number as u64 * 2);
        }
        let summary = CheckedWalk::of(&map);
        assert!(summary.leaves > 1);
        assert!(summary.nodes_with_inline_keys > 0);
        let root = map.inspect().root().unwrap();
        let mut leaf = root;
        while let Some(child) = leaf.child(0) {
            leaf = child;
        }
        for index in 0..leaf.len() {
            assert_eq!(*leaf.value(index).unwrap(), *leaf.key(index) * 2);
        }
    }

    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn string_keys_keep_prefixes_and_length_tags() {
        let mut map = BBTreeMap::new();
        for number in shuffled(20_000, 2) {
            map.insert(format!("https://example.com/users/{number:08}"), number);
        }
        CheckedWalk::of(&map);
        // The leftmost leaf right of the root's first key has fences on both
        // sides, which share the keys' common start.
        let mut leaf = map.inspect().root().unwrap().child(1).unwrap();
        while let Some(child) = leaf.child(0) {
            leaf = child;
        }
        assert!(
            leaf.prefix_len() > 0,
            "a leaf between two fences shares a prefix"
        );
        let tag = leaf
            .length_tag(0)
            .expect("byte-string leaves keep length tags");
        assert_eq!(usize::from(tag), leaf.key(0).len());
        let page = leaf.page_map();
        assert!(
            page.arrays()
                .iter()
                .any(|array| array.contents() == SlotContents::LengthTags)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn keys_without_inline_keys_walk_without_them() {
        let mut map = BBTreeMap::new();
        for number in shuffled(5_000, 3) {
            map.insert((number as u32, number as u32), String::new());
        }
        let summary = CheckedWalk::of(&map);
        assert_eq!(summary.nodes_with_inline_keys, 0);
        let root = map.inspect().root().unwrap();
        assert_eq!(root.inline_key_width(), None);
        assert_eq!(root.inline_key(0), None);
        assert_eq!(root.prefix_len(), 0);
    }

    #[test]
    fn an_empty_map_has_no_root() {
        let map = BBTreeMap::<u64, u64>::new();
        assert!(map.inspect().root().is_none());
        CheckedWalk::of(&map);
    }
}
