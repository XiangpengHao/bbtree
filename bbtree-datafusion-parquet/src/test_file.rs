//! A Parquet file for the unit tests, of known rows, removed when dropped.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, BooleanArray, Int64Array, RecordBatch, StringArray};
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::file::metadata::{ParquetMetaData, ParquetMetaDataReader};
use datafusion::parquet::file::properties::WriterProperties;

/// A Parquet file of [`Self::ROWS`] rows, in row groups of 1000: `id`,
/// integers with repeats and nulls ([`Self::ids`]); `name`, text sharing
/// long prefixes ([`Self::names`]), each `(name, id)` on one or two rows;
/// and `flag`, booleans, which no index can hold.
pub(crate) struct TestFile {
    path: PathBuf,
}

impl TestFile {
    pub(crate) const ROWS: usize = 5000;

    /// The file of test `name`.
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "bbtree-datafusion-parquet-unit-{name}-{}.parquet",
            std::process::id()
        ));
        let batch = RecordBatch::try_from_iter([
            ("id", Arc::new(Int64Array::from(Self::ids())) as ArrayRef),
            (
                "name",
                Arc::new(StringArray::from(Self::names())) as ArrayRef,
            ),
            (
                "flag",
                Arc::new(BooleanArray::from_iter(
                    (0..Self::ROWS).map(|row| Some(row % 2 == 0)),
                )) as ArrayRef,
            ),
        ])
        .unwrap();
        let properties = WriterProperties::builder()
            .set_max_row_group_row_count(Some(1000))
            .build();
        let file = File::create(&path).unwrap();
        let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(properties)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        Self { path }
    }

    /// The `id` of each row: 100 values, and nulls.
    pub(crate) fn ids() -> Vec<Option<i64>> {
        (0..Self::ROWS)
            .map(|row| (row % 7 != 3).then(|| (row as i64 * 7919) % 100 - 50))
            .collect()
    }

    /// The `name` of each row: 330 values, alike in their first 8 bytes,
    /// each on rows of several `id`s, and of nulls.
    pub(crate) fn names() -> Vec<String> {
        (0..Self::ROWS)
            .map(|row| format!("user-name-{:05}", (row * 31) % 330))
            .collect()
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The file's bytes.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap()
    }

    /// Every row, read by a plain Parquet reader.
    pub(crate) fn read_all(&self) -> Vec<RecordBatch> {
        let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&self.path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        reader.map(Result::unwrap).collect()
    }

    /// The file's footer.
    pub(crate) fn metadata(&self) -> ParquetMetaData {
        (ParquetMetaDataReader::new())
            .parse_and_finish(&File::open(&self.path).unwrap())
            .unwrap()
    }

    /// The file's Arrow schema.
    pub(crate) fn schema(&self) -> SchemaRef {
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(File::open(&self.path).unwrap()).unwrap();
        Arc::clone(builder.schema())
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
