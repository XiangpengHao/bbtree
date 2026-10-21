//! bbtree-store (`design/16-bbtree-store.md`): an ordered store of byte
//! strings that threads share, whose every byte lies in pages from its
//! allocator: bbtree's shared map, whose leaves keep their keys and values
//! in heap pages of their own ([`bbtree::store`]). An allocator that refuses
//! pages past a budget bounds all the memory a store takes; past it, the
//! store refuses inserts ([`InsertError::Alloc`]), unchanged.
//!
//! ```
//! use bbtree_store::{BBTreeStore, InsertError};
//!
//! let store = BBTreeStore::new();
//! store.insert(b"apple", b"red")?;
//! assert_eq!(store.get(b"apple").as_deref(), Some(&b"red"[..]));
//! # Ok::<(), InsertError>(())
//! ```

/// The bbtree this crate builds on.
pub use bbtree;
pub use bbtree::store::{BBTreeStore, InsertError};
pub use bbtree::{Auto, Configured, NodeConfig};
