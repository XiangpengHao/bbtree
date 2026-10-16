//! A tree that threads share (`doc/09-concurrency.md`): bbtree's tree, with
//! a latch word in each node (`node/latch.rs`), behind an atomic pointer to
//! the root page.
//!
//! It is bbtree's tree in every way but how threads share it: the same
//! nodes, the same shared prefix (D50) and prefixes fit to their fences,
//! the same splits, merges and moves between siblings, the same root that
//! grows and shrinks in its page, and an append to the last leaf (D45). Its
//! own are the ways threads share it:
//!
//! - **Descent** (`descent.rs`): optimistic through the internal nodes, each
//!   read in place while writers may change it and checked by its version
//!   (optimistic lock coupling, `node/optimistic.rs`); the leaf latched,
//!   shared or exclusive.
//! - **Operations** run the caller's closure on an entry while its leaf is
//!   latched, and let go before returning: no reference or latch outlives
//!   the call.
//! - **Changes to more than a leaf**, a split (`insert.rs`), a removal that
//!   rebalances (`remove.rs`) or a shrinking of the shared prefix
//!   (`shared_prefix.rs`), take every node bbtree's operation will change or
//!   read a separator of, without waiting (`held.rs`), and start over if
//!   another thread holds one or changed it since the descent. Then they
//!   run bbtree's operation itself, on the tree as it is, along a node
//!   stack from the topmost node they hold ([`LatchedTree::change_held`]):
//!   bbtree's operations go no higher than the nodes they change and the
//!   nearest separators above them, their fences. A reader that read a
//!   node meanwhile fails its version check.
//! - **Pages are type-stable** (`pages.rs`): a node a merge empties goes on
//!   the tree's free list for its kind, and comes back only as a node of
//!   that kind, with the same latch word, whose version only grows. So a
//!   pointer a racing reader loads is always to a node of the kind it says,
//!   and its version check fails if the node was freed since. The root page
//!   alone changes kind, and a reader checks a node's version before it
//!   follows an edge it read there. Pages go back to the allocator when the
//!   map is dropped.
//! - **The last leaf** is where an append goes (`append.rs`), by a pointer
//!   that only a thread holding that leaf changes.
//!
//! A leaf's head key, which internal nodes route by (D49), changes only
//! while the node holding its separator is held, so a thread holding that
//! node reads it where it is (`held.rs`). A reader that holds nothing reads
//! it with its leaf latched shared (`node/optimistic.rs`).

mod append;
mod descent;
mod held;
mod insert;
mod pages;
mod remove;
mod shared_prefix;

use core::borrow::Borrow;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

use self::descent::{Exclusive, Shared};
use self::pages::{FreeLists, NodePages};
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
            "a BCTreeMap's keys need inline keys"
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
        self.root.load(Ordering::Acquire) == leaf.node_ptr().tagged().as_ptr()
    }

    /// Makes the tree, unless another thread did: a root page from `alloc`,
    /// an empty leaf, which is the last leaf too.
    #[cold]
    #[inline(never)]
    fn start_tree(&self, alloc: &impl Allocator) {
        let root_page = RootPage::<K, V, Latched<S>>::new(FreshPage::allocate(alloc));
        let Node::Leaf(root) = root_page.node() else {
            unreachable!("a new tree's root is a leaf");
        };
        // Held until the leaf is the last leaf too: a split of it reads
        // whether it is (`insert.rs`).
        let version = root.latch().lock_exclusive();
        let tagged = root.node_ptr().tagged();
        match self.root.compare_exchange(
            ptr::null_mut(),
            tagged.as_ptr(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.rightmost_leaf
                    .store(tagged.as_ptr(), Ordering::Release);
                root.latch().unlock_exclusive(version);
            }
            Err(_) => {
                // SAFETY: the page is a new, empty root page that no other
                // thread ever reached.
                unsafe { root_page.free() }.deallocate(alloc);
            }
        }
    }

    /// Runs `operation`, one of bbtree's own, on the tree under the root
    /// page, of which the caller holds every node the operation changes or
    /// reads a separator of (module documentation); and if it changed the
    /// root's kind, raising a root leaf or lowering the tree to one, points
    /// the tree at the root page as its new kind, while the caller still
    /// holds the root.
    fn change_held<R>(&self, operation: impl FnOnce(&mut Tree<K, V, Latched<S>>) -> R) -> R {
        let root_page = self.root_page().expect("a tree to change");
        let mut tree = Tree::of_root_page(root_page);
        let result = operation(&mut tree);
        let changed = tree
            .root_page()
            .expect("bbtree's operations keep the root page");
        let tagged = changed.node().node_ptr().tagged();
        if tagged != root_page.node().node_ptr().tagged() {
            self.root.store(tagged.as_ptr(), Ordering::Release);
        }
        result
    }

    /// Runs `f` on the entry whose key equals `key`, while its leaf is
    /// latched shared, and returns what `f` returned; `None` without one.
    #[inline]
    pub(crate) fn read<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let key = SearchKey::new(key);
        let (held, at_separator) = self.descend::<Q, Shared, ()>(&key, &mut ())?;
        let leaf = held.leaf();
        let (key, value) = if at_separator {
            (leaf.head_key(), leaf.head_value())
        } else {
            let index = leaf.find(&key, ())?;
            // SAFETY: a search returns the index of a key of the leaf.
            unsafe { leaf.key_value_unchecked(index) }
        };
        let result = f(key, value);
        drop(held);
        Some(result)
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
        let (held, at_separator) = self.descend::<Q, Exclusive, ()>(&key, &mut ())?;
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
    /// ([`Tree::check_invariants`]), once no other thread changes it, whose
    /// appends go to its last leaf.
    #[cfg(test)]
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

    /// How many leading bytes every key of the tree shares (D50).
    #[cfg(test)]
    pub(crate) fn shared_prefix_len(&mut self) -> usize {
        self.root_page()
            .map_or(0, |root_page| root_page.shared_prefix().len())
    }

    /// Takes the tree apart, with `alloc`, the allocator its pages came
    /// from, the pages on its free lists too, once no other thread reaches
    /// it.
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
