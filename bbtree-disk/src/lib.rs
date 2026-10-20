//! bbtree on disk (`design/15-index-file.md`): a bbtree map compacted into a
//! file ([`bbtree::file`]), mapped back into memory or read into it, and
//! looked up where it lies, with the map's own code, at a map's speed.
//!
//! - [`BBTreeMapFile`] is the map in a file.
//! - [`DiskType`] is what its keys and values may be, each kept as fits it:
//!   integers and other plain types as they are, in their slots, text and
//!   byte strings in the leaves' tails. [`plain_disk_type!`] makes a plain
//!   type of the caller's one.
//! - [`DiskQuery`] and [`DiskInput`] are what it looks up with and is
//!   written from.

/// The bbtree this crate builds on: what its API names, and what
/// [`plain_disk_type!`] expands to.
pub use bbtree;

mod bbtree_map_file;
mod disk_type;
mod mapped_file;

pub use bbtree_map_file::{BBTreeMapFile, MmapOptions};
pub use disk_type::{DiskInput, DiskQuery, DiskType};
pub use mapped_file::OsPages;
