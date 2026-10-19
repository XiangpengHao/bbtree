//! bbtree-disk as an index of Parquet files (`doc/15-index-file.md`): the
//! index of a column embedded in the file, as pq-vector embeds its own,
//! which finds a value's rows, and the row groups to read them from.
//!
//! - [`ParquetIndex`] embeds a column's index in a Parquet file, opens it,
//!   and looks rows up by [`ColumnValue`], exactly, for numbers and text.
//! - [`RowAccess`] applies the rows found to an Arrow reader of the file:
//!   the row groups that hold them, and a selection within those.

mod parquet_index;

pub use parquet_index::{ColumnValue, KeyEncoding, ParquetIndex, RowAccess};
