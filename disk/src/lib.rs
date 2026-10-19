//! bbtree on disk (`doc/15-index-file.md`): a map compacted into a file map
//! ([`bbtree::file`]), written to a file, and read back by mapping the file
//! into memory, where lookups use it as it lies, at a map's speed.
//!
//! - [`IndexFile`] is a map from keys to values of any length in a file,
//!   read and written as the caller's types, through [`ToBytes`] and
//!   [`FromBytes`].
//! - [`MappedFile`] writes a file map to a file, and maps or reads one back,
//!   whole or from a range of a larger file, in [`OsPages`] of either size.

mod byte_encoding;
mod index_file;
mod mapped_file;

pub use byte_encoding::{FromBytes, ToBytes};
pub use index_file::IndexFile;
pub use mapped_file::{MappedFile, OsPages};
