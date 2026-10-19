# bbtree-datafusion-parquet

bbtree-disk as an index of Parquet files: the index of a column embedded in
the file, after the last row group and named in the footer, as pq-vector
embeds its own. Other readers read the file as before; this one finds a
value's rows, exactly, for numbers and text alike, in microseconds, and the
row groups to read them from. `design/15-index-file.md` has the design and the
measurements.

```rust
use bbtree_datafusion_parquet::{ColumnValue, ParquetIndex};
use bbtree_disk::OsPages;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

ParquetIndex::embed("hits.parquet", "UserID")?;
// SAFETY: nothing changes the file while it is mapped.
let index = unsafe { ParquetIndex::mmap("hits.parquet", "UserID", OsPages::Small)? };
let rows = index.rows_with(ColumnValue::Signed(435090932899640449))?;
let builder = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open("hits.parquet")?)?;
let batches = index.access(&rows).apply(builder).build()?;
```

The example `parquet_lookups` indexes a column of a Parquet file and times
lookups against scanning it. Integration with DataFusion's query planning,
which would turn a predicate into these lookups, is not built yet.
