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
use core::borrow::Borrow;
use core::cmp::Ordering;
use core::fmt;
use core::marker::PhantomData;
use core::mem::size_of;
use core::ops::Range;
use core::ptr;

use super::node::inline_key_search::NodeInlineKeys;
use super::node::inline_keys::{InlineTarget, SearchKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{InlineKeyNodeHeader, NodeHeader, NodeKind, NodePtr, PageType};
use super::node::root_page::RootPage;
use super::node::{Child, Node};
use super::root::Tree;
use crate::inline_key::{InlineKeyStrategy, InlineWord};

/// A map's tree, read in place: see [`crate::BBTreeMap::inspect`].
pub struct TreeView<'a, K, V, S> {
    root_page: Option<RootPage<K, V, S>>,
    _borrow: PhantomData<&'a Tree<K, V, S>>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> TreeView<'a, K, V, S> {
    pub(crate) fn new(tree: &'a Tree<K, V, S>) -> Self {
        Self {
            root_page: tree.root_page(),
            _borrow: PhantomData,
        }
    }

    /// The root node, or `None` if the map has never held an entry. A map
    /// whose entries were all removed may keep an empty root leaf.
    pub fn root(&self) -> Option<NodeView<'a, K, V, S>> {
        self.root_page.map(NodeView::of_root_page)
    }

    /// The leading bytes of the encoding of every key of the map, as many
    /// as the map keeps (D50): the prefix of the nodes along the tree's
    /// borders. Empty if the map has never held a key, or its keys have no
    /// inline keys.
    pub fn shared_prefix(&self) -> &'a [u8] {
        self.root_page
            .map_or(&[], |tree| tree.shared_prefix().as_bytes())
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> TreeView<'a, K, V, S> {
    /// The steps the map's exact-key lookup takes for `key`
    /// ([`crate::BBTreeMap::get`]), node by node, through the same
    /// searches: `search` in each internal node, `find` in the
    /// leaf. A key found in an internal node is a separator, whose value is
    /// a head below (D17): the trace then goes on the way the value is read
    /// (`Internal::head_leaf`), down the separator's right edge and first
    /// edges, to the leaf that holds it.
    ///
    /// Empty if the map has no root, or the lookup reads no node: the key
    /// lacks what every key of the map shares ([`Self::placed_by_shared_prefix`]).
    pub fn trace<Q>(&self, key: &Q) -> Vec<TraceStep<'a, K, V, S>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let search = SearchKey::<K, S, Q>::new(key);
        let mut steps = Vec::new();
        let Some(root_page) = self.root_page else {
            return steps;
        };
        if root_page.shared_prefix().place_outside(&search).is_some() {
            return steps;
        }
        let mut view = NodeView::of_root_page(root_page);
        loop {
            match view.node {
                Node::Leaf(leaf) => {
                    let outcome = match leaf.find(&search) {
                        Some(index) => TraceOutcome::Found { index },
                        None => TraceOutcome::Absent {
                            insert_at: match leaf.search(&search) {
                                Ok(index) | Err(index) => index,
                            },
                        },
                    };
                    let (target, inline_keys) = (leaf.inline_target(&search), leaf.inline_keys());
                    steps.push(TraceStep::searched(view, target, inline_keys, outcome));
                    return steps;
                }
                Node::Internal(internal) => {
                    let target = internal.inline_target(&search);
                    let inline_keys = internal.inline_keys();
                    match internal.search(&search) {
                        Err(edge) => {
                            let outcome = TraceOutcome::Descend { edge };
                            steps.push(TraceStep::searched(view, target, inline_keys, outcome));
                            view = view.child_below(internal, edge);
                        }
                        Ok(index) => {
                            let outcome = TraceOutcome::Found { index };
                            steps.push(TraceStep::searched(view, target, inline_keys, outcome));
                            let value: *const V = internal.head_leaf(index).head_value();
                            let mut below = view.child_below(internal, index + 1);
                            loop {
                                match below.node {
                                    Node::Leaf(leaf) => {
                                        debug_assert!(ptr::eq(leaf.head_value(), value));
                                        steps.push(TraceStep::passed(below, TraceOutcome::Head));
                                        return steps;
                                    }
                                    Node::Internal(internal) => {
                                        let outcome = TraceOutcome::ToHead { edge: 0 };
                                        steps.push(TraceStep::passed(below, outcome));
                                        below = below.child_below(internal, 0);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Where `key` sorts against every key of the map, if it lacks what
    /// they all share (D50): before all of them, or after, which a lookup
    /// decides with no node read. `None` if it has those bytes, or the map
    /// keeps none.
    pub fn placed_by_shared_prefix<Q>(&self, key: &Q) -> Option<Ordering>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let search = SearchKey::<K, S, Q>::new(key);
        self.root_page?.shared_prefix().place_outside(&search)
    }
}

/// One node of a lookup's way down ([`TreeView::trace`]): the node, what
/// the lookup did there, and what its search compared.
pub struct TraceStep<'a, K, V, S> {
    node: NodeView<'a, K, V, S>,
    outcome: TraceOutcome,
    inline_key: Option<InlineWord>,
    tied: Option<Range<usize>>,
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> TraceStep<'a, K, V, S> {
    /// A node the lookup searched: with what its search compared the key's
    /// inline key, `target`, against, the node's own (D18).
    fn searched(
        node: NodeView<'a, K, V, S>,
        target: Option<InlineTarget>,
        inline_keys: Option<NodeInlineKeys<'_>>,
        outcome: TraceOutcome,
    ) -> Self {
        let tied = Option::zip(target, inline_keys)
            .map(|(target, inline_keys)| inline_keys.equal_run(target.inline_key()));
        Self {
            node,
            outcome,
            inline_key: target.map(|target| target.inline_key()),
            tied,
        }
    }

    /// A node the lookup went through without searching it.
    fn passed(node: NodeView<'a, K, V, S>, outcome: TraceOutcome) -> Self {
        Self {
            node,
            outcome,
            inline_key: None,
            tied: None,
        }
    }

    pub fn node(&self) -> NodeView<'a, K, V, S> {
        self.node
    }

    pub fn outcome(&self) -> TraceOutcome {
        self.outcome
    }

    /// The searched key's inline key at the node's prefix, as the node
    /// compared it with its own; `None` if the node has no inline keys, or
    /// was not searched.
    pub fn inline_key(&self) -> Option<InlineWord> {
        self.inline_key
    }

    /// The keys whose inline keys equal the searched key's: the only ones
    /// that can equal it, which the search goes on to compare in full.
    pub fn tied(&self) -> Option<Range<usize>> {
        self.tied.clone()
    }

    /// The edge the lookup left the node by, if it went further down.
    pub fn edge_taken(&self) -> Option<usize> {
        match self.outcome {
            TraceOutcome::Descend { edge } | TraceOutcome::ToHead { edge } => Some(edge),
            TraceOutcome::Found { index } if !self.node.is_leaf() => Some(index + 1),
            _ => None,
        }
    }
}

/// What a lookup did at one node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceOutcome {
    /// No key of the node equals the searched key, which lies between keys
    /// `edge - 1` and `edge`: the lookup goes down edge `edge`.
    Descend { edge: usize },
    /// Key `index` equals the searched key. In a leaf its value is beside
    /// it; in an internal node it is a separator, whose value is the head of
    /// the leftmost leaf right of it, which the next steps reach.
    Found { index: usize },
    /// In a leaf, no key equals the searched key; it would go in at slot
    /// `insert_at`.
    Absent { insert_at: usize },
    /// On the way to a separator's value: down edge `edge`.
    ToHead { edge: usize },
    /// The leaf whose head is a separator's value.
    Head,
}

/// One node of a map's tree, read in place.
pub struct NodeView<'a, K, V, S> {
    node: Node<K, V, S>,
    /// How far below the root the node is, which the walk down from the
    /// root counts: a node does not know it.
    depth: usize,
    /// Whether the node is the root, in the root's page.
    is_root: bool,
    _borrow: PhantomData<&'a (K, V)>,
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
        match $view.node {
            Node::Leaf($node) => $body,
            Node::Internal($node) => $body,
        }
    };
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> NodeView<'a, K, V, S> {
    /// The root of the tree of `root`, read for as long as the map is
    /// borrowed for `'a`.
    fn of_root_page(root_page: RootPage<K, V, S>) -> Self {
        Self {
            node: root_page.node(),
            depth: 0,
            is_root: true,
            _borrow: PhantomData,
        }
    }

    /// The child at edge `index` of `internal`, this view's node.
    fn child_below(&self, internal: Internal<K, V, S>, index: usize) -> Self {
        Self {
            node: internal.child(index),
            depth: self.depth + 1,
            is_root: false,
            _borrow: PhantomData,
        }
    }

    /// What tells this node apart from the others of its map, while it lives:
    /// its page's address.
    pub fn id(&self) -> NodeId {
        NodeId(self.node.node_ptr().addr())
    }

    /// How far below the root the node is: 0 for the root. Every leaf lies
    /// as deep as every other.
    pub fn depth(&self) -> usize {
        self.depth
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self.node, Node::Leaf(_))
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
        with_node!(self, |node| Self::capacity_of(node))
    }

    /// The fewest keys the node holds between removals, unless it is the
    /// root (D22).
    pub fn min_len(&self) -> usize {
        with_node!(self, |node| Self::min_len_of(node))
    }

    /// How many leading bytes the node's fences share, and so every key in
    /// it, which its inline keys skip (05-fence-key.md); 0 for a node
    /// without inline keys.
    pub fn prefix_len(&self) -> usize {
        self.node.prefix_len()
    }

    /// Key `index`: of an internal node, separator `index`'s, which in
    /// maps whose keys have inline keys is the head key of the leftmost leaf
    /// right of it (D49).
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn key(&self, index: usize) -> &'a K {
        match self.node {
            Node::Leaf(leaf) => leaf.key(index),
            Node::Internal(internal) => internal.key(index),
        }
    }

    /// How many bytes of each key's window the node's inline keys keep, or
    /// `None` if the node has no inline keys (D18).
    pub fn inline_key_width(&self) -> Option<usize> {
        with_node!(self, |node| Self::inline_width_of(node))
    }

    /// Inline key `index`: the bytes the node keeps of key `index`'s
    /// window, from its first byte, zeros after them; `None` if the node has
    /// no inline keys.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn inline_key(&self, index: usize) -> Option<InlineWord> {
        with_node!(self, |node| Child::inline_keys(node)
            .map(|inline_keys| inline_keys.inline_key(index)))
    }

    /// Key `index`'s length tag, if the node keeps them (D36).
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn length_tag(&self, index: usize) -> Option<u16> {
        assert!(index < self.len(), "key index out of bounds");
        with_node!(self, |node| Child::length_tags(node)
            .map(|tags| tags[index]))
    }

    /// The leaf's head, the value of the separator just left of it (D17):
    /// `None` in the leftmost leaf, and in an internal node.
    pub fn head(&self) -> Option<&'a V> {
        match self.node {
            Node::Leaf(leaf) => leaf.has_head().then(|| leaf.head_value()),
            Node::Internal(_) => None,
        }
    }

    /// The leaf's head's key, the key of the separator just left of it,
    /// where separator keys are heads, in maps whose keys have inline keys
    /// (D49): `None` otherwise, in the leftmost leaf, and in an internal
    /// node.
    pub fn head_key(&self) -> Option<&'a K> {
        match self.node {
            Node::Leaf(leaf) => (leaf.has_head()
                && NodePtr::<K, V, S>::has_separator_keys_in_heads())
            .then(|| leaf.head_key()),
            Node::Internal(_) => None,
        }
    }

    /// Value `index` of a leaf; `None` in an internal node, whose keys keep
    /// their values in the leaves right of them, as heads (D17).
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub fn value(&self, index: usize) -> Option<&'a V> {
        match self.node {
            Node::Leaf(leaf) => Some(leaf.key_value(index).1),
            Node::Internal(_) => None,
        }
    }

    /// The child at edge `index` of an internal node, left of key `index`;
    /// `None` in a leaf.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    pub fn child(&self, index: usize) -> Option<NodeView<'a, K, V, S>> {
        match self.node {
            Node::Leaf(_) => None,
            Node::Internal(internal) => Some(self.child_below(internal, index)),
        }
    }

    /// The node's page: its size, and where each of its slot arrays lies,
    /// with how many of each array's slots it uses; the root's, with the
    /// root area after the node (`tree/node/root_page.rs`).
    pub fn page_map(&self) -> PageMap {
        let page = match self.node {
            Node::Leaf(leaf) => PageMap::of_leaf::<K, V, S>(leaf),
            Node::Internal(internal) => PageMap::of_internal::<K, V, S>(internal),
        };
        if self.is_root {
            page.in_root_page::<K, V, S>()
        } else {
            page
        }
    }

    fn capacity_of<C: Child<K, V, S>>(_: C) -> usize {
        C::capacity()
    }

    fn min_len_of<C: Child<K, V, S>>(_: C) -> usize {
        C::min_len()
    }

    fn inline_width_of<C: Child<K, V, S>>(_: C) -> Option<usize> {
        C::plan().inline_width().map(|width| width.byte_len())
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
/// layout the page was allocated with (`tree/node/layout.rs`). Bytes no array
/// covers are padding, or room the layout leaves unused.
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

    fn of_leaf<K, V, S: InlineKeyStrategy<K>>(leaf: Leaf<K, V, S>) -> Self {
        let layout = NodePtr::<K, V, S>::leaf_layout();
        let len = leaf.len();
        let capacity = layout.keys.capacity;
        let mut arrays = Self::header_and_inline_keys::<K, V, S, Leaf<K, V, S>>(len);
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
        let has_head = usize::from(leaf.has_head());
        if let Some(offset) = layout.head_key_offset {
            arrays.push(SlotArray::dense(
                SlotContents::HeadKey,
                offset,
                size_of::<K>(),
                1,
                has_head,
            ));
        }
        arrays.push(SlotArray::dense(
            SlotContents::Head,
            layout.head_offset,
            size_of::<V>(),
            1,
            has_head,
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

    fn of_internal<K, V, S: InlineKeyStrategy<K>>(internal: Internal<K, V, S>) -> Self {
        let layout = NodePtr::<K, V, S>::internal_layout();
        let len = internal.len();
        let capacity = layout.keys.capacity;
        let mut arrays = Self::header_and_inline_keys::<K, V, S, Internal<K, V, S>>(len);
        // With inline keys, its separator keys are heads below (D49).
        if layout.keeps_keys() {
            arrays.push(SlotArray {
                contents: SlotContents::Keys,
                offset: layout.keys.keys_offset,
                stride: layout.keys.key_stride,
                slot_size: size_of::<K>(),
                capacity,
                used: len,
            });
        }
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

    /// The same node's map in the root's page: its size, and the root area.
    fn in_root_page<K, V, S: InlineKeyStrategy<K>>(mut self) -> Self {
        let area = RootPage::<K, V, S>::area_bytes();
        self.arrays.push(SlotArray::dense(
            SlotContents::RootArea,
            area.start,
            area.len(),
            1,
            1,
        ));
        self.size = RootPage::<K, V, S>::page_layout().size();
        self
    }

    /// The arrays every page of kind `Kind` starts with: its header, and
    /// its inline keys if it has them, of which a node of `len` keys uses
    /// `len`.
    fn header_and_inline_keys<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind>(
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
                width.byte_len(),
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
    /// A leaf's head's key: the key of the separator just left of it, where
    /// separator keys are heads, in maps whose keys have inline keys (D49).
    HeadKey,
    /// A leaf's head: the value of the separator just left of it (D17).
    Head,
    Values,
    /// An internal node's children.
    Edges,
    /// What the tree keeps in the root's page, after the root node: what
    /// every key shares (D50).
    RootArea,
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeSet;
    use alloc::string::String;
    use core::fmt::Debug;
    use std::format;

    use super::*;
    use crate::BBTreeMap;
    use crate::tree::test_support::TestRng;

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
                assert_eq!(child.depth(), node.depth() + 1);
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

    /// Traces every key of `map` and some absent ones, checking each trace
    /// against `get`. Returns how many absent keys the shared prefix placed
    /// alone (D50).
    fn check_traces<K: Ord + Debug, V, S: InlineKeyStrategy<K>>(
        map: &BBTreeMap<K, V, S>,
        absent: &[K],
    ) -> usize {
        let tree = map.inspect();
        let mut placed_by_shared_prefix = 0;
        let check_steps = |steps: &[TraceStep<'_, K, V, S>]| {
            for (depth, step) in steps.iter().enumerate() {
                assert_eq!(step.node().depth(), depth);
            }
        };
        for (key, value) in map.iter() {
            assert_eq!(tree.placed_by_shared_prefix(key), None);
            let steps = tree.trace(key);
            check_steps(&steps);
            let last = steps.last().unwrap();
            let found = match last.outcome() {
                TraceOutcome::Found { index } => {
                    assert!(last.node().is_leaf());
                    assert!(last.tied().is_none_or(|tied| tied.contains(&index)));
                    last.node().value(index).unwrap()
                }
                TraceOutcome::Head => {
                    let separator = &steps[steps
                        .iter()
                        .position(|step| matches!(step.outcome(), TraceOutcome::Found { .. }))
                        .unwrap()];
                    let TraceOutcome::Found { index } = separator.outcome() else {
                        unreachable!()
                    };
                    assert_eq!(separator.node().key(index), key);
                    last.node().head().unwrap()
                }
                outcome => panic!("{key:?} ended at {outcome:?}"),
            };
            assert!(ptr::eq(found, value), "{key:?} traced to another value");
        }
        for key in absent {
            assert!(map.get(key).is_none());
            let steps = tree.trace(key);
            if let Some(order) = tree.placed_by_shared_prefix(key) {
                assert!(steps.is_empty());
                match order {
                    Ordering::Less => assert!(key < map.first_key_value().unwrap().0),
                    _ => assert!(key > map.last_key_value().unwrap().0),
                }
                placed_by_shared_prefix += 1;
                continue;
            }
            check_steps(&steps);
            let last = steps.last().unwrap();
            let TraceOutcome::Absent { insert_at } = last.outcome() else {
                panic!("{key:?} ended at {:?}", last.outcome());
            };
            let leaf = last.node();
            assert!(insert_at == 0 || leaf.key(insert_at - 1) < key);
            assert!(insert_at == leaf.len() || key < leaf.key(insert_at));
        }
        placed_by_shared_prefix
    }

    #[test]
    #[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
    fn traces_follow_lookups() {
        let mut integers = BBTreeMap::new();
        for number in shuffled(20_000, 4) {
            integers.insert(number as u64 * 2, number);
        }
        let absent: Vec<u64> = (0..200).map(|number| number * 2 + 1).collect();
        assert_eq!(check_traces(&integers, &absent), 0);
        // Dense integers share their high bytes, which the map keeps (D50):
        // a key past them all lacks them.
        assert_eq!(tree_shared_len(&integers), 6);
        assert_eq!(check_traces(&integers, &[u64::MAX]), 1);

        let mut strings = BBTreeMap::new();
        for number in shuffled(5_000, 5) {
            strings.insert(format!("https://example.com/{:06}", number * 2), number);
        }
        let absent: Vec<String> = (0..50)
            .map(|number| format!("https://example.com/{:06}", number * 2 + 1))
            .collect();
        assert_eq!(check_traces(&strings, &absent), 0);
        // The greatest is "https://example.com/009998".
        assert_eq!(tree_shared_len(&strings), "https://example.com/00".len());
        for outside in ["https://a", "https://example.com/~"] {
            assert_eq!(check_traces(&strings, &[String::from(outside)]), 1);
        }
    }

    fn tree_shared_len<K, V, S: InlineKeyStrategy<K>>(map: &BBTreeMap<K, V, S>) -> usize {
        map.inspect().shared_prefix().len()
    }

    #[test]
    fn an_empty_map_has_no_root() {
        let map = BBTreeMap::<u64, u64>::new();
        assert!(map.inspect().root().is_none());
        CheckedWalk::of(&map);
    }
}
