//! bbtree-mmap as an index of Parquet files (`design/15-index-file.md`):
//! indexes of one or more columns embedded in a Parquet file, which
//! DataFusion's Parquet scans look their predicates' rows up in.
//!
//! - [`ParquetIndexBuilder`] builds indexes of a file's columns and embeds
//!   them in the file, beside the indexes it has, and returns what it built
//!   ([`ParquetIndexStats`]). Other readers read the file as before.
//! - [`ParquetIndexSessionBuilderExt::with_parquet_index`] has a session's
//!   Parquet scans look rows up in the indexes of the files they read, by
//!   the comparisons and `IN` lists of their predicates, and read those
//!   rows alone.

mod index_key;
mod indexed_scan;
mod parquet_index;
mod parquet_index_builder;
mod scan_predicate;
#[cfg(test)]
mod test_file;

pub use indexed_scan::ParquetIndexSessionBuilderExt;
pub use parquet_index_builder::{ParquetIndexBuilder, ParquetIndexStats};
