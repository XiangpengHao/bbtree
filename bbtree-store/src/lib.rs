//! bbtree-store (`design/16-bbtree-store.md`): an ordered store of byte
//! strings that threads share, whose every byte lies in pages from its
//! allocator. It is bbtree's shared map, whose keys point to their records,
//! key and value, and whose leaves keep their records in chains of heap
//! pages of their own ([`Allocator::allocate_heap`], a leaf annex,
//! [`bbtree::LeafAnnex`]). An allocator that refuses pages past a budget
//! bounds all the memory a store takes; past it, the store refuses inserts
//! ([`InsertError::Alloc`]), and holds what it held.
//!
//! ```
//! use bbtree_store::{BBTreeStore, InsertError};
//!
//! let store = BBTreeStore::new();
//! store.insert(b"apple", b"red")?;
//! assert_eq!(store.get(b"apple").as_deref(), Some(&b"red"[..]));
//! # Ok::<(), InsertError>(())
//! ```
//!
//! [`Allocator::allocate_heap`]: bbtree::Allocator::allocate_heap

/// The bbtree this crate builds on.
pub use bbtree;

mod heap_page;
mod leaf_heap;
mod range;
mod record;
mod store;

pub use range::RangeCopies;
pub use store::{BBTreeStore, DefaultConfig, InsertError};
