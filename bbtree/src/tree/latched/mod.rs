//! A tree that threads share (`design/09-concurrency.md`, D54): the unsync
//! tree, with a latch word in each node (`node/latch.rs`), behind an atomic
//! pointer to the root page.
//!
//! It is the unsync tree in every way but how threads share it. It has the
//! same nodes, the same shared prefix (D50) and prefixes fit to their
//! fences, and the same splits, merges and moves between siblings. Its root
//! grows and shrinks in its page, and appends go straight to the last leaf
//! (D45). What it adds is how threads share it:
//!
//! - **Descent** (`descent.rs`) is optimistic through the internal nodes.
//!   Each is read in place while writers may change it, and checked by its
//!   version (optimistic lock coupling, `node/optimistic.rs`). The leaf is
//!   latched, shared or exclusive; a lookup reads it in place too
//!   (`lookup.rs`, D57).
//! - **Operations** run the caller's closure on an entry while its leaf is
//!   latched, and let go before returning: no reference or latch outlives
//!   the call.
//! - **Changes to more than a leaf** are a split (`insert.rs`), a removal
//!   that rebalances (`remove.rs`) and a shrinking of the shared prefix
//!   (`shared_prefix.rs`). Each takes, without waiting, every node the
//!   unsync tree's operation will change or read a separator of
//!   (`held.rs`). It starts over if another thread holds one, or changed it
//!   since the descent. Then it runs the unsync tree's operation itself, on
//!   the tree as it is, along a node stack from the topmost node it holds
//!   ([`LatchedTree::change_held`]). This works because the unsync tree's
//!   operations go no higher than the nodes they change and the nearest
//!   separators above them, their fences. A reader that read a node
//!   meanwhile fails its version check.
//! - **Pages are type-stable** (`pages.rs`). A node a merge empties goes on
//!   the tree's free list for its kind. It comes back only as a node of that
//!   kind, with the same latch word, whose version only grows. So a pointer
//!   a racing reader loads always points to a node of the kind it says, and
//!   the reader's version check fails if the node was freed since. The root
//!   page alone changes kind, so a reader checks the root's version before
//!   it follows an edge read from it. Pages go back to the allocator when
//!   the map is dropped.
//! - **The last leaf** is where an append goes (`append.rs`), by a pointer
//!   that only a thread holding that leaf changes.
//!
//! A leaf's head key, which internal nodes route by (D49), changes only
//! while the node holding its separator is held. So a thread holding that
//! node reads it where it is (`held.rs`). A reader that holds nothing reads
//! it with its leaf latched shared (`node/optimistic.rs`).

mod append;
mod batch;
mod descent;
mod first_copied;
mod held;
mod insert;
mod lookup;
mod owner;
mod pages;
mod remove;
mod scan;
mod shared_prefix;

pub(crate) use self::insert::{ReadOrInsert, UpdateOrInsert, WithLeaf};
pub(crate) use self::scan::{LeafStamp, ScanResume};

use core::alloc::Layout;
use core::borrow::Borrow;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

use self::descent::Exclusive;
use self::pages::{FreeLists, NodePages};
use super::interleaving::Interleaving;
use super::node::Node;
use super::node::inline_keys::SearchKey;
use super::node::internal::Internal;
use super::node::latch::Latched;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, NodePtr, PageType};
use super::node::root_page::RootPage;
use super::root::Tree;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// A tree that threads share, by an atomic pointer to its root page, tagged
/// with the root's kind; null until the first insert makes one. Its nodes'
/// strategy is `Latched<S>`.
///
/// It owns its nodes, keys and values, as a [`Tree`] does, and the pages on
/// its free lists, and has no `Drop`: [`Self::destroy`] takes it apart.
pub(crate) struct LatchedTree<K, V, S> {
    /// Read by every operation, written when the root changes kind.
    root: AtomicPtr<u8>,
    /// The tree's last leaf, tagged, where an append goes (`append.rs`):
    /// null until the first insert makes the tree. Written when the last
    /// leaf splits.
    rightmost_leaf: CachePadded<AtomicPtr<u8>>,
    /// Pages of nodes a merge emptied, for new nodes of their kind: written
    /// by merges, and splits that take a page.
    free: CachePadded<FreeLists>,
    /// Owns keys and values that threads share: see the `Send` and `Sync`
    /// impls.
    _entries: PhantomData<*const (K, V)>,
    _strategy: PhantomData<fn() -> S>,
}

/// A value on cache lines of its own: writing it takes no line from threads
/// that read what sits beside it. Two lines, which CPUs fetch in pairs.
#[repr(align(128))]
struct CachePadded<T>(T);

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

// SAFETY: a tree owns its pages, keys and values, as a `Box` owns its
// contents: sending it sends them.
unsafe impl<K: Send, V: Send, S> Send for LatchedTree<K, V, S> {}

// SAFETY: through `&self`, a thread moves keys and values into the tree,
// which other threads then read, change through `&mut V`, or drop: they
// must be `Send` and `Sync`. Every change to a node through `&self` holds
// its latch, and every read either holds it or is checked by its version
// (`node/latch.rs`, `node/optimistic.rs`).
unsafe impl<K: Send + Sync, V: Send + Sync, S> Sync for LatchedTree<K, V, S> {}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// No tree.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under `S`: a reader that races with
    /// writers reads internal nodes, which must then keep no keys (D49).
    pub(crate) fn new() -> Self {
        assert!(
            NodePtr::<K, V, Latched<S>>::has_inline_keys(),
            "a BBTreeMap's keys need inline keys"
        );
        Self {
            root: AtomicPtr::new(ptr::null_mut()),
            rightmost_leaf: CachePadded(AtomicPtr::new(ptr::null_mut())),
            free: CachePadded(FreeLists::new()),
            _entries: PhantomData,
            _strategy: PhantomData,
        }
    }

    /// The pages one operation allocates and frees, through `alloc`
    /// ([`NodePages`]).
    #[inline(always)]
    fn pages<'t, A: Allocator>(&'t self, alloc: &'t A) -> NodePages<'t, A> {
        NodePages::new(
            &self.free,
            alloc,
            Leaf::<K, V, Latched<S>>::page_layout(),
            Internal::<K, V, Latched<S>>::page_layout(),
        )
    }

    /// The root page, as the tree's pointer tags it; `None` without a tree.
    #[inline(always)]
    fn root_page(&self) -> Option<RootPage<K, V, Latched<S>>> {
        Interleaving::switch_point();
        let tagged = NonNull::new(self.root.load(Ordering::Acquire))?;
        // SAFETY: a non-null root pointer is a root page of this tree's,
        // tagged with its root's kind, stored by `start_tree` or an
        // operation that changed the root's kind (`change_held`).
        Some(RootPage::of_node(Node::at(unsafe {
            NodePtr::from_tagged(tagged)
        })))
    }

    /// Whether `leaf` is the root, which has no minimum (D22).
    #[inline(always)]
    fn is_root(&self, leaf: Leaf<K, V, Latched<S>>) -> bool {
        Interleaving::switch_point();
        self.root.load(Ordering::Acquire) == leaf.node_ptr().tagged().as_ptr()
    }

    /// Makes the tree, unless another thread did: a root page from `alloc`,
    /// zeroed, so that its latch word is unheld and every edge a reader may
    /// read before its check is initialized; an empty leaf, which is the
    /// last leaf too. If `alloc` has no page to give, returns the layout it
    /// was asked for.
    #[cold]
    #[inline(never)]
    fn start_tree(&self, alloc: &impl Allocator) -> Result<(), Layout> {
        let root_page = RootPage::<K, V, Latched<S>>::new(FreshPage::try_allocate(alloc)?);
        let Node::Leaf(root) = root_page.node() else {
            unreachable!("a new tree's root is a leaf");
        };
        // Held until the leaf is the last leaf too: a split of it reads
        // whether it is (`insert.rs`).
        let version = root.latch().lock_exclusive();
        let tagged = root.node_ptr().tagged();
        Interleaving::switch_point();
        match self.root.compare_exchange(
            ptr::null_mut(),
            tagged.as_ptr(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                Interleaving::switch_point();
                self.rightmost_leaf
                    .store(tagged.as_ptr(), Ordering::Release);
                root.latch().unlock_exclusive(version);
            }
            Err(_) => {
                // SAFETY: the page is a new, empty root page that no other
                // thread ever reached, its leaf's annex released once.
                unsafe {
                    root.release_annex(alloc);
                    root_page.free().deallocate(alloc);
                }
            }
        }
        Ok(())
    }

    /// Runs `operation`, one of the unsync tree's own, on the tree under the
    /// root page. The caller holds every node the operation changes or reads
    /// a separator of (module documentation). If the operation changed the
    /// root's kind, by raising a root leaf or lowering the tree to one, this
    /// retags the tree's root pointer for the new kind while the caller
    /// still holds the root.
    fn change_held<R>(&self, operation: impl FnOnce(&mut Tree<K, V, Latched<S>>) -> R) -> R {
        let root_page = self.root_page().expect("a tree to change");
        let mut tree = Tree::of_root_page(root_page);
        let result = operation(&mut tree);
        let changed = tree
            .root_page()
            .expect("bbtree's operations keep the root page");
        let tagged = changed.node().node_ptr().tagged();
        if tagged != root_page.node().node_ptr().tagged() {
            Interleaving::switch_point();
            self.root.store(tagged.as_ptr(), Ordering::Release);
        }
        result
    }

    /// Runs `f` on the entry whose key equals `key`, its value exclusive,
    /// while its leaf is latched exclusively, and returns what `f` returned;
    /// `None` without one.
    #[inline]
    pub(crate) fn update<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &mut V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let key = SearchKey::new(key);
        let (held, at_separator) = self.descend::<Q, Exclusive, ()>(&key, &mut ()).ok()?;
        let leaf = held.leaf();
        let (key, value) = if at_separator {
            (leaf.head_key(), leaf.head_value_mut())
        } else {
            let index = leaf.find(&key, ())?;
            // SAFETY: a search returns the index of a key of the leaf, whose
            // key and value are apart.
            unsafe { (leaf.key_unchecked(index), leaf.value_mut_unchecked(index)) }
        };
        let result = f(key, value);
        drop(held);
        Some(result)
    }

    /// Panics unless the tree is a well-formed B+-tree of `length` entries
    /// ([`Tree::check_invariants`]) whose appends go to its last leaf. The
    /// caller runs it once no other thread changes the tree.
    #[cfg(any(test, fuzzing))]
    pub(crate) fn check_invariants(&mut self, length: usize)
    where
        K: Ord,
    {
        use super::node_stack::LastEdge;

        match self.root_page() {
            Some(root_page) => {
                Tree::of_root_page(root_page).check_invariants(length);
                let last_leaf = root_page.node().border_leaf::<LastEdge>();
                assert_eq!(
                    *self.rightmost_leaf.get_mut(),
                    last_leaf.node_ptr().tagged().as_ptr(),
                    "appends go to a leaf that is not the last"
                );
            }
            None => assert_eq!(length, 0, "a map without a tree has entries"),
        }
    }

    /// How many keys each node holds, level by level from the root, each
    /// level in key order: the shape a test sets up before threads race on
    /// it. Empty without a tree.
    #[cfg(all(test, shuttle))]
    pub(crate) fn node_lengths(&mut self) -> std::vec::Vec<std::vec::Vec<usize>> {
        use std::vec;
        use std::vec::Vec;

        use super::inspect::TreeView;

        let Some(root_page) = self.root_page() else {
            return Vec::new();
        };
        let tree = Tree::of_root_page(root_page);
        let view = TreeView::new(&tree, 0);
        let mut levels = Vec::new();
        let mut level = vec![view.root().expect("a tree has a root")];
        while !level.is_empty() {
            levels.push(level.iter().map(|node| node.len()).collect());
            level = (level.iter())
                .flat_map(|node| (0..=node.len()).filter_map(|edge| node.child(edge)))
                .collect();
        }
        levels
    }

    /// The keys of each leaf, in key order, each leaf's head first.
    #[cfg(all(test, shuttle))]
    pub(crate) fn leaf_keys(&mut self) -> std::vec::Vec<std::vec::Vec<K>>
    where
        K: Clone,
    {
        use std::vec;
        use std::vec::Vec;

        use super::inspect::TreeView;

        let Some(root_page) = self.root_page() else {
            return Vec::new();
        };
        let tree = Tree::of_root_page(root_page);
        let view = TreeView::new(&tree, 0);
        let mut level = vec![view.root().expect("a tree has a root")];
        while !level[0].is_leaf() {
            level = (level.iter())
                .flat_map(|node| (0..=node.len()).filter_map(|edge| node.child(edge)))
                .collect();
        }
        (level.iter())
            .map(|leaf| {
                (leaf.head_key().into_iter())
                    .chain((0..leaf.len()).map(|index| leaf.key(index)))
                    .cloned()
                    .collect()
            })
            .collect()
    }

    /// How many leading bytes every key of the tree shares (D50).
    #[cfg(test)]
    pub(crate) fn shared_prefix_len(&mut self) -> usize {
        self.root_page()
            .map_or(0, |root_page| root_page.shared_prefix().len())
    }

    /// Takes the tree apart, the pages on its free lists too, giving the
    /// pages back to `alloc`, the allocator they came from. The caller runs
    /// it once no other thread reaches the tree.
    pub(crate) fn destroy(&mut self, alloc: &impl Allocator) {
        if let Some(root_page) = self.root_page() {
            *self.root.get_mut() = ptr::null_mut();
            *self.rightmost_leaf.get_mut() = ptr::null_mut();
            Tree::of_root_page(root_page).destroy(alloc);
        }
        self.free.deallocate_all(
            alloc,
            Leaf::<K, V, Latched<S>>::page_layout(),
            Internal::<K, V, Latched<S>>::page_layout(),
        );
    }
}
