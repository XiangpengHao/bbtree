//! File maps (`design/15-index-file.md`, D58): a tree's pages one after another in one
//! byte string, which a reader uses where it lies, mapped from a file or
//! read into memory, without building anything.
//!
//! - **Pages** are copied byte for byte ([`NodePtr::copy_page_to`]), in
//!   level order: the root page, then each level of internal nodes below
//!   it, left to right, then the leaves, in key order. Each page starts at
//!   a multiple of its alignment, and the file map at a multiple of the
//!   largest, so that every page is aligned where the file map is.
//! - **Edges are offsets.** An internal node's edge holds its child's
//!   offset from the node's page, tagged with the child's kind as a pointer
//!   to it is (D52). Following one adds it to the node's address
//!   (`Internal::edge_unchecked`): a reader needs no base address, and
//!   writes nothing to the bytes. Children lie after their parents, in the
//!   order a breadth-first walk meets them, so every offset is positive.
//! - **The header**, at offset 0, holds where the root is, the number of
//!   entries, and a fingerprint of the layout: every size, offset and
//!   choice the node code reads pages with, the key and value types' sizes,
//!   the pointer width and the byte order. A file map of another layout is
//!   refused, not misread.
//!
//! A tree in a file map has strategy [`InFile`]: the map's own, with offsets
//! for edges. Its pages have the unsync tree's layout, its reads run the
//! unsync tree's code, and nothing changes it.

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::marker::PhantomData;
use core::ptr::NonNull;

use super::node::Node;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::node_plan::NodePlan;
use super::node::page::{NodeHeader, NodePtr, PageType};
use super::node::root_page::RootPage;
use super::node_stack::MAX_DEPTH;
use super::root::Tree;
use super::shared_prefix::SharedPrefix;
use crate::file::{FileError, LeafBytes, Storable};
use crate::inline_key::InlineKeyStrategy;
use crate::inline_key::sealed::{InlineKeySource, Sealed};
use crate::node_config::NodeConfig;

/// The strategy of a tree in a file map: `S`'s inline keys, and edges that
/// are offsets from their node's page. Nothing public names it: an
/// `FileMap` with strategy `S` reads its tree with this one.
pub(crate) struct InFile<S>(PhantomData<fn() -> S>);

impl<K, S: InlineKeyStrategy<K>> Sealed<K> for InFile<S> {
    const SOURCE: InlineKeySource<K> = S::SOURCE;
    type Config = S::Config;
    const IN_FILE: bool = true;
}

impl<K, S: InlineKeyStrategy<K>> InlineKeyStrategy<K> for InFile<S> {
    type LeafAnnex = S::LeafAnnex;
}

/// The pages of a file map of a tree with keys `K`, values `V` and strategy
/// `S`: those of the unsync tree, whose layout a tree in a file map keeps.
pub(crate) struct FileFormat<K, V, S> {
    _types: PhantomData<fn() -> (K, V)>,
    _strategy: PhantomData<fn() -> S>,
}

impl<K: Storable, V: Storable, S: InlineKeyStrategy<K>> FileFormat<K, V, S> {
    fn root_page() -> Layout {
        RootPage::<K, V, S>::page_layout()
    }

    fn internal_page() -> Layout {
        Internal::<K, V, S>::page_layout()
    }

    fn leaf_page() -> Layout {
        Leaf::<K, V, S>::page_layout()
    }

    /// What a file map's start must be aligned to, so that each page is: the
    /// largest page alignment.
    pub(crate) fn alignment() -> usize {
        Self::root_page()
            .align()
            .max(Self::internal_page().align())
            .max(Self::leaf_page().align())
    }

    /// Every size, offset and choice the node code reads the pages with, as
    /// one number: file maps whose fingerprints differ have different layouts.
    fn fingerprint() -> u64 {
        let (leaf, internal) = (
            NodePtr::<K, V, S>::leaf_layout(),
            NodePtr::<K, V, S>::internal_layout(),
        );
        let (leaf_plan, internal_plan) = (
            NodePlan::<K>::of::<S, Leaf<K, V, S>>(),
            NodePlan::<K>::of::<S, Internal<K, V, S>>(),
        );
        let width = |plan: NodePlan<K>| plan.inline_width().map_or(0, |width| width.byte_len());
        let offset = |offset: Option<usize>| offset.unwrap_or(usize::MAX);
        let source = match S::SOURCE {
            InlineKeySource::RecognizedStdTypes => 0,
            InlineKeySource::InlineKeyTrait { .. } => 1,
        };
        let root = Self::root_page();
        let fields = [
            usize::BITS as usize,
            usize::from(cfg!(target_endian = "little")),
            size_of::<K>(),
            align_of::<K>(),
            size_of::<V>(),
            align_of::<V>(),
            K::FORMAT as usize,
            V::FORMAT as usize,
            source,
            width(leaf_plan),
            usize::from(leaf_plan.has_hash_tags()),
            width(internal_plan),
            leaf.page.size(),
            leaf.page.align(),
            leaf.keys.capacity,
            offset(leaf.keys.inline_keys_offset),
            leaf.keys.keys_offset,
            leaf.keys.key_stride,
            leaf.head_offset,
            leaf.value_stride,
            offset(leaf.hash_tags_offset),
            offset(leaf.head_key_offset),
            internal.page.size(),
            internal.page.align(),
            internal.keys.capacity,
            offset(internal.keys.inline_keys_offset),
            internal.keys.keys_offset,
            internal.keys.key_stride,
            internal.edges_offset,
            internal.edge_stride,
            root.size(),
            root.align(),
            RootPage::<K, V, S>::area_offset(),
        ];
        // FNV-1a, over each field's eight little-endian bytes.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in fields
            .iter()
            .flat_map(|&field| (field as u64).to_le_bytes())
        {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Checks every page the tree in `file_bytes` reaches for what a reader relies on
    /// to stay in bounds, given its header, checked already: each page lies
    /// in the file map, aligned for its kind, and holds no more keys than its
    /// kind does, its head flag a `bool`; each edge leads past every page
    /// reached before it, so that the walk visits each page once and ends;
    /// every leaf lies at one depth, at most what a node stack holds; the
    /// shared prefix fits its area; the leaves hold the header's number of
    /// entries; and each slot that points beyond itself passes its type's
    /// check ([`Storable::check`]).
    /// Plain keys and values, inline keys and prefix lengths can hold any
    /// bytes: they steer searches, never past a node (`tree/node`).
    pub(crate) fn check(file_bytes: &[u8], header: FileHeader) -> Result<(), FileError> {
        const LEAF_TAG: usize = 1;
        debug_assert_eq!(NodePtr::<K, V, S>::LEAF_TAG, LEAF_TAG);
        let Some(root) = header.root() else {
            return if header.length == 0 {
                Ok(())
            } else {
                Err(FileError::Corrupt("entries without a tree"))
            };
        };
        let shared_len = file_bytes
            [(root & !LEAF_TAG) + RootPage::<K, V, S>::area_offset() + SharedPrefix::LEN_OFFSET];
        if usize::from(shared_len) > SharedPrefix::MAX_LEN {
            return Err(FileError::Corrupt("a shared prefix longer than its area"));
        }
        let edges = NodePtr::<K, V, S>::internal_layout();
        let mut level = vec![root];
        let mut entries = 0;
        let mut last_reached = root & !LEAF_TAG;
        for depth in 0..=MAX_DEPTH {
            let leaves = level[0] & LEAF_TAG != 0;
            let page = match (depth, leaves) {
                (0, _) => Self::root_page(),
                (_, true) => Self::leaf_page(),
                (_, false) => Self::internal_page(),
            };
            let capacity = if leaves {
                Leaf::<K, V, S>::capacity()
            } else {
                Internal::<K, V, S>::capacity()
            };
            let mut children: Vec<usize> = Vec::new();
            for &tagged in &level {
                let offset = tagged & !LEAF_TAG;
                let bytes = file_bytes
                    .get(offset..offset.saturating_add(page.size()))
                    .filter(|_| offset.is_multiple_of(page.align()))
                    .ok_or(FileError::Corrupt(
                        "a page outside the file map, or misaligned",
                    ))?;
                let len = usize::from(u16::from_ne_bytes([
                    bytes[NodeHeader::LEN_OFFSET],
                    bytes[NodeHeader::LEN_OFFSET + 1],
                ]));
                let has_head = bytes[NodeHeader::HAS_HEAD_OFFSET];
                if len > capacity || has_head > 1 {
                    return Err(FileError::Corrupt("a node header out of range"));
                }
                if leaves {
                    entries += len + usize::from(has_head);
                    Self::check_slots(file_bytes, offset, len, has_head == 1)?;
                    continue;
                }
                for edge in 0..=len {
                    let slot = edges.edges_offset + edge * edges.edge_stride;
                    let mut word = [0; size_of::<usize>()];
                    word.copy_from_slice(&bytes[slot..slot + size_of::<usize>()]);
                    let child = offset
                        .checked_add(usize::from_ne_bytes(word))
                        .filter(|&child| child & !LEAF_TAG > last_reached)
                        .ok_or(FileError::Corrupt("an edge that does not lead forward"))?;
                    if children
                        .first()
                        .is_some_and(|&first| (first ^ child) & LEAF_TAG != 0)
                    {
                        return Err(FileError::Corrupt("children of both kinds at one depth"));
                    }
                    last_reached = child & !LEAF_TAG;
                    children.push(child);
                }
            }
            if leaves {
                return if entries == header.length {
                    Ok(())
                } else {
                    Err(FileError::Corrupt(
                        "leaves holding another number of entries",
                    ))
                };
            }
            level = children;
        }
        Err(FileError::Corrupt("a tree deeper than a node stack"))
    }

    /// Checks each slot of the leaf at `offset` of `file_bytes`, which
    /// holds `len` entries and maybe a head, whose type points beyond it
    /// ([`Storable::check`]).
    fn check_slots(
        file_bytes: &[u8],
        offset: usize,
        len: usize,
        has_head: bool,
    ) -> Result<(), FileError> {
        let layout = NodePtr::<K, V, S>::leaf_layout();
        if K::HAS_BYTES {
            let keys =
                (0..len).map(|index| layout.keys.keys_offset + index * layout.keys.key_stride);
            let head = layout.head_key_offset.filter(|_| has_head);
            for slot in keys.chain(head) {
                K::check(file_bytes, offset + slot)?;
            }
        }
        if V::HAS_BYTES {
            let first = if has_head { 0 } else { 1 };
            for index in first..=len {
                V::check(
                    file_bytes,
                    offset + layout.head_offset + index * layout.value_stride,
                )?;
            }
        }
        Ok(())
    }
}

/// What a file map starts with: where its tree is, and the layout it was
/// written with. Kept in the file map as little-endian words after a magic
/// number and the format's version, [`FileHeader::LEN`] bytes in all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileHeader {
    /// The layout's fingerprint ([`FileFormat::fingerprint`]).
    fingerprint: u64,
    /// The number of entries.
    length: usize,
    /// The root node's offset from the file map's start, tagged with its kind
    /// (D52); 0 without a tree.
    root: usize,
    /// The file map's size in bytes, header included.
    byte_len: usize,
}

impl FileHeader {
    const MAGIC: [u8; 8] = *b"bbtreeIM";

    /// What a file map's bytes mean, beyond what the fingerprint covers:
    /// where things are in the header, what an edge holds, how inline keys
    /// and prefixes encode keys. Changes with any of them, so that a file map
    /// of another format is refused.
    const FORMAT_VERSION: u32 = 1;

    /// The header's size: room for what it holds, and to spare.
    pub(crate) const LEN: usize = 64;

    /// The number of entries.
    pub(crate) fn length(self) -> usize {
        self.length
    }

    /// The root node's tagged offset, if the file map holds a tree.
    fn root(self) -> Option<usize> {
        (self.root != 0).then_some(self.root)
    }

    fn to_bytes(self) -> [u8; Self::LEN] {
        let mut bytes = [0; Self::LEN];
        bytes[..8].copy_from_slice(&Self::MAGIC);
        bytes[8..12].copy_from_slice(&Self::FORMAT_VERSION.to_le_bytes());
        let words = [
            self.fingerprint,
            self.length as u64,
            self.root as u64,
            self.byte_len as u64,
        ];
        for (index, word) in words.iter().enumerate() {
            bytes[16 + 8 * index..24 + 8 * index].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    /// The header of `file_bytes`, which a file map of a tree with keys `K`,
    /// values `V` and strategy `S` must start with, checked: its magic,
    /// version and fingerprint; that `file_bytes` is as long as it says, and
    /// aligned to [`FileFormat::alignment`]; and that the root page lies
    /// in it, past the header and aligned.
    pub(crate) fn read<K: Storable, V: Storable, S: InlineKeyStrategy<K>>(
        file_bytes: &[u8],
    ) -> Result<Self, FileError> {
        // Keys with bytes beyond their slots, without inline keys, would be
        // kept in internal nodes too, which no bytes follow: no writer
        // writes such a file map, and no reader reads one.
        if K::HAS_BYTES && !NodePtr::<K, V, S>::has_inline_keys() {
            return Err(FileError::LayoutMismatch);
        }
        let bytes = file_bytes.get(..Self::LEN).ok_or(FileError::NotAFileMap)?;
        if bytes[..8] != Self::MAGIC {
            return Err(FileError::NotAFileMap);
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes"));
        if version != Self::FORMAT_VERSION {
            return Err(FileError::UnsupportedVersion(version));
        }
        let word = |index: usize| {
            u64::from_le_bytes(
                bytes[16 + 8 * index..24 + 8 * index]
                    .try_into()
                    .expect("eight bytes"),
            )
        };
        if word(0) != FileFormat::<K, V, S>::fingerprint() {
            return Err(FileError::LayoutMismatch);
        }
        let too_large = FileError::Corrupt("a size past the address space");
        let header = Self {
            fingerprint: word(0),
            length: usize::try_from(word(1)).map_err(|_| too_large)?,
            root: usize::try_from(word(2)).map_err(|_| too_large)?,
            byte_len: usize::try_from(word(3)).map_err(|_| too_large)?,
        };
        if file_bytes.len() < header.byte_len {
            return Err(FileError::Truncated {
                byte_len: header.byte_len,
                available: file_bytes.len(),
            });
        }
        let alignment = FileFormat::<K, V, S>::alignment();
        if !file_bytes.as_ptr().addr().is_multiple_of(alignment) {
            return Err(FileError::Misaligned { alignment });
        }
        if let Some(root) = header.root() {
            let offset = root & !NodePtr::<K, V, S>::LEAF_TAG;
            let page = FileFormat::<K, V, S>::root_page();
            if offset < Self::LEN
                || !offset.is_multiple_of(page.align())
                || offset.saturating_add(page.size()) > header.byte_len
            {
                return Err(FileError::Corrupt("a root page outside the file map"));
            }
        }
        Ok(header)
    }
}

/// A node of the tree a writer builds for a file map of keys `K` and values
/// `V`.
type WriterNode<K, V, S> = Node<<K as Storable>::InWriter, <V as Storable>::InWriter, S>;

/// A tree's pages by level, and where its file map puts each, every leaf
/// followed by what `L` keeps after it: what a file map is written from.
/// `K` and `V` are the file map's key and value types; the tree holds what
/// they are built from, of the same layout.
pub(crate) struct FilePages<'t, K: Storable, V: Storable, S, L> {
    /// The root, then each level below it, left to right; the last is the
    /// leaves, unless the root is a leaf. Empty without a tree.
    levels: Vec<Vec<WriterNode<K, V, S>>>,
    /// Where each page is in the file map, by level.
    offsets: Vec<Vec<usize>>,
    /// The number of entries.
    length: usize,
    byte_len: usize,
    /// The tree the nodes are of, borrowed while they are read.
    _tree: PhantomData<&'t Tree<K::InWriter, V::InWriter, S>>,
    _leaf_bytes: PhantomData<fn() -> L>,
}

impl<'t, K, V, S, L> FilePages<'t, K, V, S, L>
where
    K: Storable,
    V: Storable,
    S: InlineKeyStrategy<K> + InlineKeyStrategy<K::InWriter>,
    L: LeafBytes<K, V>,
{
    /// The file map of `tree`, which holds `length` entries.
    ///
    /// # Panics
    ///
    /// If keys with bytes beyond their slots have no inline keys, which only
    /// `WithInlineKey` gives them: internal nodes would keep keys, which no
    /// bytes follow.
    pub(crate) fn of(tree: &'t Tree<K::InWriter, V::InWriter, S>, length: usize) -> Self {
        assert!(
            NodePtr::<K::InWriter, V::InWriter, S>::leaf_layout()
                == NodePtr::<K, V, S>::leaf_layout()
                && NodePtr::<K::InWriter, V::InWriter, S>::internal_layout()
                    == NodePtr::<K, V, S>::internal_layout(),
            "a writer's tree has its file map's layout"
        );
        assert!(
            !K::HAS_BYTES || NodePtr::<K, V, S>::has_inline_keys(),
            "keys with bytes beyond their slots need inline keys: the WithInlineKey strategy"
        );
        let mut levels = Vec::new();
        if let Some(root_page) = tree.root_page() {
            let mut level = vec![root_page.node()];
            while !level.is_empty() {
                let children = (level.iter())
                    .filter_map(|node| match *node {
                        Node::Internal(internal) => Some(internal),
                        Node::Leaf(_) => None,
                    })
                    .flat_map(|internal| (0..=internal.len()).map(move |edge| internal.child(edge)))
                    .collect();
                levels.push(level);
                level = children;
            }
        }
        let mut offsets = Vec::with_capacity(levels.len());
        let mut position = FileHeader::LEN;
        for (depth, nodes) in levels.iter().enumerate() {
            let page = Self::page_at(depth, levels.len());
            debug_assert!(depth == 0 || page.size().is_multiple_of(page.align()));
            let level_offsets = (nodes.iter())
                .map(|&node| {
                    let offset = position.next_multiple_of(page.align());
                    position = offset + page.size() + Self::leaf_bytes_len(node);
                    offset
                })
                .collect();
            offsets.push(level_offsets);
        }
        Self {
            levels,
            offsets,
            length,
            byte_len: position,
            _tree: PhantomData,
            _leaf_bytes: PhantomData,
        }
    }

    /// The file map's size in bytes.
    pub(crate) fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// The layout of the pages at `depth` in a tree of `depths` levels: the
    /// root page's at the top, the leaves' at the bottom, the internal
    /// nodes' between.
    fn page_at(depth: usize, depths: usize) -> Layout {
        if depth == 0 {
            FileFormat::<K, V, S>::root_page()
        } else if depth + 1 == depths {
            FileFormat::<K, V, S>::leaf_page()
        } else {
            FileFormat::<K, V, S>::internal_page()
        }
    }

    /// Where page `index` of the level at `depth` is in the file map.
    fn offset_of(&self, depth: usize, index: usize) -> usize {
        self.offsets[depth][index]
    }

    /// How many bytes follow `node` in the file map: what `L` keeps after
    /// a leaf.
    fn leaf_bytes_len(node: WriterNode<K, V, S>) -> usize {
        match node {
            Node::Leaf(leaf) => L::len(&WriterLeaf::new(leaf)),
            Node::Internal(_) => 0,
        }
    }

    /// The header the file map starts with.
    fn header(&self) -> FileHeader {
        let root = match self.levels.first().map(|level| level[0]) {
            None => 0,
            Some(Node::Leaf(_)) => self.offset_of(0, 0) | NodePtr::<K, V, S>::LEAF_TAG,
            Some(Node::Internal(_)) => self.offset_of(0, 0),
        };
        FileHeader {
            fingerprint: FileFormat::<K, V, S>::fingerprint(),
            length: self.length,
            root,
            byte_len: self.byte_len,
        }
    }

    /// Hands the file map to `write`, in order, a header, a page, the bytes
    /// after a leaf, or the zeros between them at a time:
    /// [`Self::byte_len`] bytes in all. Stops at the first error `write`
    /// returns, and returns it.
    pub(crate) fn write<E>(&self, write: &mut impl FnMut(&[u8]) -> Result<(), E>) -> Result<(), E> {
        write(&self.header().to_bytes())?;
        let mut position = FileHeader::LEN;
        let largest_page = FileFormat::<K, V, S>::root_page()
            .size()
            .max(FileFormat::<K, V, S>::internal_page().size())
            .max(FileFormat::<K, V, S>::leaf_page().size());
        let mut page = vec![0; largest_page];
        let mut leaf_bytes = Vec::new();
        for (depth, nodes) in self.levels.iter().enumerate() {
            let size = Self::page_at(depth, self.levels.len()).size();
            let mut first_child = 0;
            for (index, &node) in nodes.iter().enumerate() {
                let offset = self.offset_of(depth, index);
                // Fewer zeros than a page: each page's alignment is at most
                // its size.
                page[..offset - position].fill(0);
                write(&page[..offset - position])?;
                // SAFETY: the node's page is allocated with at least the
                // size of its level's pages, the root page's at the top. It
                // came zeroed from its allocator (`Allocator`'s contract),
                // and its keys and values have no padding (`Storable`'s
                // contract). The tree is borrowed: nothing writes it
                // meanwhile.
                unsafe { node.node_ptr().copy_page_to(&mut page, size) };
                leaf_bytes.clear();
                match node {
                    Node::Internal(internal) => {
                        self.offset_edges(&mut page, internal, depth, index, first_child);
                        first_child += internal.len() + 1;
                    }
                    Node::Leaf(leaf) => {
                        Self::write_leaf_bytes(&mut page[..size], &mut leaf_bytes, leaf);
                    }
                }
                write(&page[..size])?;
                write(&leaf_bytes)?;
                position = offset + size + leaf_bytes.len();
            }
        }
        debug_assert_eq!(position, self.byte_len);
        Ok(())
    }

    /// Puts into `leaf_bytes` what `L` keeps after `leaf`, and into `page`,
    /// a copy of the leaf's page, what its slots with bytes hold: those past
    /// the leaf's entries, and a missing head's, zeros, not stale pointers;
    /// the others `L`'s.
    fn write_leaf_bytes(
        page: &mut [u8],
        leaf_bytes: &mut Vec<u8>,
        leaf: Leaf<K::InWriter, V::InWriter, S>,
    ) {
        if !K::HAS_BYTES && !V::HAS_BYTES {
            return;
        }
        let layout = NodePtr::<K::InWriter, V::InWriter, S>::leaf_layout();
        let capacity = Leaf::<K::InWriter, V::InWriter, S>::capacity();
        let mut zero = |slot: usize, size: usize| page[slot..slot + size].fill(0);
        for index in 0..=capacity {
            if K::HAS_BYTES && index < capacity {
                zero(
                    layout.keys.keys_offset + index * layout.keys.key_stride,
                    size_of::<K>(),
                );
            }
            if V::HAS_BYTES {
                zero(
                    layout.head_offset + index * layout.value_stride,
                    size_of::<V>(),
                );
            }
        }
        if K::HAS_BYTES
            && let Some(slot) = layout.head_key_offset
        {
            zero(slot, size_of::<K>());
        }
        let leaf = WriterLeaf::new(leaf);
        leaf_bytes.resize(L::len(&leaf), 0);
        L::write(&leaf, page, leaf_bytes);
    }

    /// Writes into `page`, a copy of `internal`'s page, which is page
    /// `index` at `depth`, each edge's offset in the file map: from the page to
    /// its child's, whose index one level down is `first_child` for edge 0,
    /// tagged as the child's pointer is. Edges past the last are zeros.
    fn offset_edges(
        &self,
        page: &mut [u8],
        internal: Internal<K::InWriter, V::InWriter, S>,
        depth: usize,
        index: usize,
        first_child: usize,
    ) {
        let layout = NodePtr::<K::InWriter, V::InWriter, S>::internal_layout();
        let offset = self.offset_of(depth, index);
        let leaf_children = depth + 2 == self.levels.len();
        let tag = usize::from(leaf_children) * NodePtr::<K, V, S>::LEAF_TAG;
        for edge in 0..=Internal::<K::InWriter, V::InWriter, S>::capacity() {
            let slot = layout.edges_offset + edge * layout.edge_stride;
            let tagged_offset = if edge <= internal.len() {
                (self.offset_of(depth + 1, first_child + edge) - offset) | tag
            } else {
                0
            };
            page[slot..slot + size_of::<usize>()].copy_from_slice(&tagged_offset.to_ne_bytes());
        }
    }
}

/// A leaf of a [`FileWriter`](crate::file::FileWriter)'s tree, as its
/// [`LeafBytes`] sees it: its entries, in key order, the head's first, each
/// with where its key's and value's slots lie in the leaf's page.
pub struct WriterLeaf<'t, K: Storable, V: Storable, S> {
    leaf: Leaf<K::InWriter, V::InWriter, S>,
    _tree: PhantomData<&'t Tree<K::InWriter, V::InWriter, S>>,
}

impl<'t, K, V, S> WriterLeaf<'t, K, V, S>
where
    K: Storable<InWriter: 't>,
    V: Storable<InWriter: 't>,
    S: InlineKeyStrategy<K::InWriter>,
{
    /// `leaf`, of a writer's tree borrowed for `'t`.
    fn new(leaf: Leaf<K::InWriter, V::InWriter, S>) -> Self {
        Self {
            leaf,
            _tree: PhantomData,
        }
    }

    /// The entries, in key order, the head's first.
    pub fn entries(&self) -> impl Iterator<Item = WriterEntry<'t, K, V>> + use<'t, K, V, S> {
        let leaf = self.leaf;
        let layout = NodePtr::<K::InWriter, V::InWriter, S>::leaf_layout();
        // A head keeps its key only with inline keys (D49).
        let head = leaf.has_head().then(|| WriterEntry {
            key: layout.head_key_offset.map(|_| leaf.head_key()),
            key_slot: layout.head_key_offset,
            value: leaf.head_value(),
            value_slot: layout.head_offset,
        });
        let entries = (0..leaf.len()).map(move |index| {
            let (key, value) = leaf.key_value(index);
            WriterEntry {
                key: Some(key),
                key_slot: Some(layout.keys.keys_offset + index * layout.keys.key_stride),
                value,
                value_slot: layout.values_offset() + index * layout.value_stride,
            }
        });
        head.into_iter().chain(entries)
    }

    /// The size of the heap pages of the map's node config
    /// (`NodeConfig::HEAP_PAGE_SIZE`), for bytes that follow the leaf.
    pub fn heap_page_size(&self) -> usize {
        const {
            let size = <<S as Sealed<K::InWriter>>::Config as NodeConfig>::HEAP_PAGE_SIZE;
            assert!(
                size >= 1024,
                "a NodeConfig's HEAP_PAGE_SIZE is at least 1024"
            );
            size
        }
    }
}

/// An entry of a [`WriterLeaf`]: what the writer's tree holds for its key
/// and value, and where their slots lie in the leaf's page.
pub struct WriterEntry<'t, K: Storable, V: Storable> {
    /// `None` for a head, in a leaf without inline keys, which keeps no key
    /// for it (D49); so `key_slot`.
    key: Option<&'t K::InWriter>,
    key_slot: Option<usize>,
    value: &'t V::InWriter,
    value_slot: usize,
}

impl<'t, K: Storable, V: Storable> WriterEntry<'t, K, V> {
    /// What the writer's tree holds for the key: `None` for a head whose
    /// leaf keeps no key for it, without inline keys.
    pub fn key(&self) -> Option<&'t K::InWriter> {
        self.key
    }

    /// Where the key's slot lies in the leaf's page: `None` for a head
    /// whose leaf keeps no key for it, without inline keys.
    pub fn key_slot(&self) -> Option<usize> {
        self.key_slot
    }

    /// What the writer's tree holds for the value.
    pub fn value(&self) -> &'t V::InWriter {
        self.value
    }

    /// Where the value's slot lies in the leaf's page.
    pub fn value_slot(&self) -> usize {
        self.value_slot
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, InFile<S>> {
    /// The tree in `file_bytes`, whose header, read and checked, is `header`.
    ///
    /// # Safety
    ///
    /// `file_bytes` holds a file map of a tree with keys `K`, values `V` and
    /// strategy `S`, as [`FilePages`] writes it, or one that passed
    /// [`FileFormat::check`]; it stays unchanged for as long as the tree is
    /// used. Its keys and values are valid whatever their bytes.
    pub(crate) unsafe fn in_file(file_bytes: &[u8], header: FileHeader) -> Self {
        let Some(root) = header.root() else {
            return Self::EMPTY;
        };
        let start = NonNull::from(file_bytes).cast::<u8>();
        // SAFETY: the root node lies in the file map at its tagged offset from
        // its start (caller).
        let root = unsafe { NodePtr::at_offset(start, root) };
        Self::of_root_page(RootPage::of_node(Node::at(root)))
    }
}

#[cfg(test)]
mod tests {
    use core::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;
    use std::vec::Vec;

    use super::{FileFormat, InFile};
    use crate::file::{FileBuf, FileError, FileMap, FileWriter, Storable};
    use crate::inline_key::{Auto, InlineKeyStrategy, WithInlineKey};
    use crate::tree::node::internal::Internal;
    use crate::tree::node::leaf::Leaf;
    use crate::tree::node::page::{NodeHeader, NodePtr};
    use crate::tree::test_support::TestRng;

    /// A file map of `count` entries of `u64` keys and values, `key → !key`.
    fn file_map_of(count: u64) -> FileBuf {
        FileWriter::<u64, u64>::from_ascending((0..count).map(|key| (key, !key)))
            .unwrap()
            .to_buf()
    }

    /// The root's offset in `file_bytes`, and whether it is a leaf.
    fn root_of(file_bytes: &[u8]) -> (usize, bool) {
        let root = u64::from_le_bytes(file_bytes[32..40].try_into().unwrap()) as usize;
        (root & !1, root & 1 != 0)
    }

    /// Where edge `edge` of the internal node at `offset` is.
    fn edge_slot(offset: usize, edge: usize) -> usize {
        let layout = NodePtr::<u64, u64, Auto>::internal_layout();
        offset + layout.edges_offset + edge * layout.edge_stride
    }

    /// A copy of `file_bytes`, aligned, for damaging.
    fn copy_of(file_bytes: &[u8]) -> FileBuf {
        let mut copy = FileBuf::zeroed(file_bytes.len(), 4096);
        copy.copy_from_slice(file_bytes);
        copy
    }

    fn corruption(file_bytes: &[u8]) -> Option<&'static str> {
        match FileMap::<u64, u64>::open(file_bytes) {
            Err(FileError::Corrupt(what)) => Some(what),
            Ok(_) => None,
            Err(other) => panic!("a file map of the right layout refused as {other:?}"),
        }
    }

    /// A tree in a file map reads its pages with the layout they were
    /// written with: the unsync tree's, for each strategy.
    #[test]
    fn trees_in_file_maps_have_the_unsync_layout() {
        fn same_layout<K: Storable, V: Storable, S: InlineKeyStrategy<K>>() {
            assert_eq!(
                FileFormat::<K, V, S>::fingerprint(),
                FileFormat::<K, V, InFile<S>>::fingerprint()
            );
            assert_eq!(
                FileFormat::<K, V, S>::alignment(),
                FileFormat::<K, V, InFile<S>>::alignment()
            );
        }
        same_layout::<u64, u64, Auto>();
        same_layout::<u128, [u8; 3], Auto>();
        same_layout::<[u8; 32], u64, Auto>();
        same_layout::<u32, u32, Auto>();
        same_layout::<u64, u64, WithInlineKey>();
        assert_ne!(
            FileFormat::<u64, u64, Auto>::fingerprint(),
            FileFormat::<i64, u64, Auto>::fingerprint() ^ 1,
        );
        assert_ne!(
            FileFormat::<[u8; 32], u64, Auto>::fingerprint(),
            FileFormat::<[u8; 32], u64, WithInlineKey>::fingerprint()
        );
    }

    /// Children follow their parents, level by level, each page at a
    /// multiple of its alignment, the leaves last: a breadth-first walk
    /// reads the file map front to back.
    #[test]
    fn pages_lie_in_level_order() {
        let file_bytes = file_map_of(5000);
        let (root, is_leaf) = root_of(&file_bytes);
        assert!(!is_leaf);
        assert!(root.is_multiple_of(FileMap::<u64, u64>::alignment()));
        let leaf_size = FileFormat::<u64, u64, Auto>::leaf_page().size();
        let mut level = Vec::from([root]);
        let mut depth = 0;
        loop {
            let mut children = Vec::new();
            for &offset in &level {
                let len = u16::from_ne_bytes([file_bytes[offset], file_bytes[offset + 1]]) as usize;
                for edge in 0..=len {
                    let slot = edge_slot(offset, edge);
                    let tagged = usize::from_ne_bytes(
                        file_bytes[slot..slot + size_of::<usize>()]
                            .try_into()
                            .unwrap(),
                    );
                    children.push(offset + tagged);
                }
            }
            assert!(children.windows(2).all(|pair| pair[0] < pair[1]));
            depth += 1;
            if children[0] & 1 != 0 {
                let leaves: Vec<usize> = children.iter().map(|child| child & !1).collect();
                assert_eq!(*leaves.last().unwrap() + leaf_size, file_bytes.len());
                assert!(leaves.windows(2).all(|pair| pair[1] - pair[0] == leaf_size));
                break;
            }
            level = children;
        }
        assert_eq!(depth, 2, "5000 entries take a root and two levels below");
    }

    #[test]
    fn a_node_holding_more_than_it_fits_is_refused() {
        let file_bytes = file_map_of(5000);
        let mut damaged = copy_of(&file_bytes);
        let last_leaf = file_bytes.len() - FileFormat::<u64, u64, Auto>::leaf_page().size();
        let too_many = Leaf::<u64, u64, Auto>::capacity() as u16 + 1;
        damaged[last_leaf + NodeHeader::LEN_OFFSET..][..2].copy_from_slice(&too_many.to_ne_bytes());
        assert_eq!(corruption(&damaged), Some("a node header out of range"));

        let mut damaged = copy_of(&file_bytes);
        let (root, _) = root_of(&file_bytes);
        let too_many = Internal::<u64, u64, Auto>::capacity() as u16 + 1;
        damaged[root..][..2].copy_from_slice(&too_many.to_ne_bytes());
        assert_eq!(corruption(&damaged), Some("a node header out of range"));
    }

    #[test]
    fn a_head_flag_that_is_no_bool_is_refused() {
        let file_bytes = file_map_of(5000);
        let mut damaged = copy_of(&file_bytes);
        let last_leaf = file_bytes.len() - FileFormat::<u64, u64, Auto>::leaf_page().size();
        damaged[last_leaf + NodeHeader::HAS_HEAD_OFFSET] = 2;
        assert_eq!(corruption(&damaged), Some("a node header out of range"));
    }

    #[test]
    fn edges_that_lead_back_outside_or_twice_are_refused() {
        let file_bytes = file_map_of(5000);
        let read_edge = |node: usize, edge: usize| {
            let slot = edge_slot(node, edge);
            usize::from_ne_bytes(
                file_bytes[slot..slot + size_of::<usize>()]
                    .try_into()
                    .unwrap(),
            )
        };
        let last_edge =
            |node: usize| u16::from_ne_bytes([file_bytes[node], file_bytes[node + 1]]) as usize;
        let damage = |node: usize, edge: usize, tagged: usize| {
            let mut damaged = copy_of(&file_bytes);
            let slot = edge_slot(node, edge);
            damaged[slot..slot + size_of::<usize>()].copy_from_slice(&tagged.to_ne_bytes());
            corruption(&damaged)
        };
        let (root, _) = root_of(&file_bytes);
        assert_eq!(
            damage(root, 0, 0),
            Some("an edge that does not lead forward")
        );
        assert_eq!(
            damage(root, 1, read_edge(root, 0)),
            Some("an edge that does not lead forward")
        );
        assert_eq!(
            damage(root, 0, usize::MAX - 1),
            Some("an edge that does not lead forward")
        );
        assert_eq!(
            damage(root, 0, read_edge(root, 0) | 1),
            Some("children of both kinds at one depth")
        );
        // The edge to the last leaf, which no page is reached after.
        let last_parent = root + read_edge(root, last_edge(root));
        let to_last_leaf = last_edge(last_parent);
        let past_the_end = file_bytes.len() - last_parent;
        assert_eq!(
            damage(last_parent, to_last_leaf, past_the_end | 1),
            Some("a page outside the file map, or misaligned")
        );
        assert_eq!(
            damage(
                last_parent,
                to_last_leaf,
                read_edge(last_parent, to_last_leaf) + 8
            ),
            Some("a page outside the file map, or misaligned")
        );
    }

    #[test]
    fn a_wrong_number_of_entries_is_refused() {
        let file_bytes = file_map_of(5000);
        let mut damaged = copy_of(&file_bytes);
        damaged[24..32].copy_from_slice(&4999_u64.to_le_bytes());
        assert_eq!(
            corruption(&damaged),
            Some("leaves holding another number of entries")
        );
        let empty = file_map_of(0);
        let mut damaged = copy_of(&empty);
        damaged[24..32].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(corruption(&damaged), Some("entries without a tree"));
    }

    /// Bytes damaged at random past the header: each file map is refused, or
    /// read in bounds, its lookups and walks giving answers, maybe wrong,
    /// or panicking, never undefined behavior.
    #[test]
    #[cfg_attr(panic = "abort", ignore = "needs unwinding")]
    fn damaged_file_maps_are_refused_or_read_in_bounds() {
        let rng = TestRng::new(23);
        let count = if cfg!(miri) { 300 } else { 3000 };
        let file_bytes = file_map_of(count);
        let mut damaged = copy_of(&file_bytes);
        let (mut refused, mut read) = (0, 0);
        for _ in 0..if cfg!(miri) { 10 } else { 400 } {
            damaged.copy_from_slice(&file_bytes);
            for _ in 0..1 + rng.below(3) {
                let at = 64 + rng.below(file_bytes.len() - 64);
                damaged[at] = rng.below(256) as u8;
            }
            let Ok(map) = FileMap::<u64, u64>::open(&damaged) else {
                refused += 1;
                continue;
            };
            read += 1;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let found = (0..count + 100)
                    .step_by(7)
                    .filter(|key| map.get(key).is_some())
                    .count();
                let forward = map.iter().fold(0, |entries, _| entries + 1);
                let backward = map.iter().rev().fold(0, |entries, _| entries + 1);
                let ranged = map.range(10..count - 10).fold(0, |entries, _| entries + 1);
                (found, forward, backward, ranged)
            }));
        }
        // Miri, which damages 10 file maps, checks that reading them stays
        // in bounds, so it needs some read; which of so few it refuses
        // depends on the layout.
        if cfg!(miri) {
            assert!(read > 0, "{refused} refused, {read} read");
        } else {
            assert!(refused > 0 && read > 0, "{refused} refused, {read} read");
        }
    }
}
