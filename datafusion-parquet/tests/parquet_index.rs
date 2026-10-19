//! Indexes embedded in Parquet files: lookups find the rows a scan finds,
//! and the file reads as it did before.

use std::fs::File;
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::Int64Type;
use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use bbtree_datafusion_parquet::{ColumnValue, KeyEncoding, ParquetIndex};
use bbtree_disk::OsPages;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::metadata::{PageIndexPolicy, ParquetMetaDataReader};
use parquet::file::properties::WriterProperties;

/// A Parquet file of its own for a test, removed when dropped.
struct TestFile(PathBuf);

impl TestFile {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "bbtree-datafusion-parquet-{name}-{}.parquet",
            std::process::id()
        ));
        Self(path)
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The values of the test file's columns, row by row: `id`, with repeats
/// and nulls, and `name`, strings sharing long prefixes.
fn columns(rows: usize) -> (Vec<Option<i64>>, Vec<String>) {
    let ids = (0..rows)
        .map(|row| (row % 7 != 3).then(|| (row as i64 * 7919) % 1000 - 500))
        .collect();
    let names = (0..rows)
        .map(|row| format!("user-name-{:05}", (row * 31) % 2000))
        .collect();
    (ids, names)
}

/// Writes the test file, in row groups of 3000 rows and small pages, with
/// page indexes.
fn write_file(file: &TestFile, rows: usize) {
    let (ids, names) = columns(rows);
    let batch = RecordBatch::try_from_iter([
        ("id", Arc::new(Int64Array::from(ids)) as ArrayRef),
        ("name", Arc::new(StringArray::from(names)) as ArrayRef),
    ])
    .unwrap();
    let properties = WriterProperties::builder()
        .set_max_row_group_row_count(Some(3000))
        .set_data_page_row_count_limit(500)
        .build();
    let mut writer = ArrowWriter::try_new(
        File::create(&file.0).unwrap(),
        batch.schema(),
        Some(properties),
    )
    .unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

/// The file's page indexes, which its footer points to.
fn page_indexes(file: &TestFile) -> String {
    let metadata = ParquetMetaDataReader::new()
        .with_page_index_policy(PageIndexPolicy::Required)
        .parse_and_finish(&File::open(&file.0).unwrap())
        .unwrap();
    let ranges: Vec<_> = (metadata.row_groups().iter())
        .flat_map(|row_group| row_group.columns())
        .map(|column| (column.column_index_range(), column.offset_index_range()))
        .collect();
    assert!(
        ranges
            .iter()
            .all(|(column, offset)| column.is_some() && offset.is_some())
    );
    format!(
        "{ranges:?} {:?}",
        metadata.page_index().map(|index| format!("{index:?}"))
    )
}

/// Every row's `id` and `name`, read by a plain Parquet reader.
fn read_all(file: &TestFile) -> Vec<RecordBatch> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&file.0).unwrap())
        .unwrap()
        .build()
        .unwrap();
    reader.map(Result::unwrap).collect()
}

#[test]
fn embedded_indexes_find_the_rows_a_scan_finds() {
    let rows = 10_000;
    let file = TestFile::new("lookups");
    write_file(&file, rows);
    let (before, page_indexes_before) = (read_all(&file), page_indexes(&file));
    assert!(ParquetIndex::embed(&file.0, "id").unwrap() > 0);
    assert!(ParquetIndex::embed(&file.0, "name").unwrap() > 0);
    assert_eq!(read_all(&file), before, "the file reads as it did");
    assert_eq!(page_indexes(&file), page_indexes_before);
    assert_eq!(
        ParquetIndex::embed(&file.0, "id").unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );

    let (ids, names) = columns(rows);
    // SAFETY: nothing changes the file while it is mapped.
    let mapped = unsafe { ParquetIndex::mmap(&file.0, "id", OsPages::Small) }.unwrap();
    // SAFETY: as for the other.
    let huge = unsafe { ParquetIndex::mmap(&file.0, "id", OsPages::Huge) }.unwrap();
    let read = ParquetIndex::read(&file.0, "id").unwrap();
    assert_eq!(mapped.encoding(), KeyEncoding::Signed);
    let distinct: std::collections::BTreeSet<i64> = ids.iter().flatten().copied().collect();
    assert_eq!(mapped.len(), distinct.len(), "an entry per distinct value");
    for value in -510..510 {
        let expected: Vec<u64> = (0..rows as u64)
            .filter(|&row| ids[row as usize] == Some(value))
            .collect();
        assert_eq!(
            mapped.rows_with(ColumnValue::Signed(value)).unwrap(),
            expected
        );
        assert_eq!(
            read.rows_with(ColumnValue::Signed(value)).unwrap(),
            expected
        );
        assert_eq!(
            huge.rows_with(ColumnValue::Signed(value)).unwrap(),
            expected
        );
    }
    for (start, end) in [(-500, -400), (-3, 3), (499, 600), (-1000, 1000)] {
        let expected: Vec<u64> = (0..rows as u64)
            .filter(|&row| ids[row as usize].is_some_and(|id| start <= id && id < end))
            .collect();
        let found = mapped
            .rows_between(
                Bound::Included(ColumnValue::Signed(start)),
                Bound::Excluded(ColumnValue::Signed(end)),
            )
            .unwrap();
        assert_eq!(found, expected, "{start}..{end}");
    }
    assert!(mapped.rows_with(ColumnValue::Unsigned(3)).is_err());

    let names_index = ParquetIndex::read(&file.0, "name").unwrap();
    assert_eq!(names_index.encoding(), KeyEncoding::Bytes);
    for wanted in [
        "user-name-01234",
        "user-name-00000",
        "user-name-01999",
        "user-name",
        "",
    ] {
        let expected: Vec<u64> = (0..rows as u64)
            .filter(|&row| names[row as usize] == wanted)
            .collect();
        let found = names_index
            .rows_with(ColumnValue::Bytes(wanted.as_bytes()))
            .unwrap();
        assert_eq!(found, expected, "{wanted:?}");
    }
    let (start, end) = ("user-name-00100", "user-name-00200");
    let expected: Vec<u64> = (0..rows as u64)
        .filter(|&row| (start..end).contains(&names[row as usize].as_str()))
        .collect();
    let found = names_index
        .rows_between(
            Bound::Included(ColumnValue::Bytes(start.as_bytes())),
            Bound::Excluded(ColumnValue::Bytes(end.as_bytes())),
        )
        .unwrap();
    assert_eq!(found, expected);
    let reversed = names_index
        .rows_between(
            Bound::Included(ColumnValue::Bytes(end.as_bytes())),
            Bound::Excluded(ColumnValue::Bytes(start.as_bytes())),
        )
        .unwrap();
    assert!(
        reversed.is_empty(),
        "a range whose start is past its end holds nothing"
    );
}

#[test]
fn a_selection_reads_the_rows_found() {
    let rows = 10_000;
    let file = TestFile::new("selection");
    write_file(&file, rows);
    ParquetIndex::embed(&file.0, "id").unwrap();
    let index = ParquetIndex::read(&file.0, "id").unwrap();
    let found = index.rows_with(ColumnValue::Signed(123)).unwrap();
    assert!(!found.is_empty());
    let access = index.access(&found);
    let mut row_groups: Vec<usize> = found.iter().map(|&row| row as usize / 3000).collect();
    row_groups.dedup();
    assert_eq!(access.row_groups(), row_groups, "row groups of 3000 rows");
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(&file.0).unwrap()).unwrap();
    let reader = access.apply(builder).build().unwrap();
    let selected: Vec<i64> = reader
        .flat_map(|batch| {
            let batch = batch.unwrap();
            batch
                .column(0)
                .as_primitive::<Int64Type>()
                .values()
                .to_vec()
        })
        .collect();
    assert_eq!(selected, vec![123; found.len()]);

    // Rows of the last row groups alone, and none.
    let late = index
        .rows_between(Bound::Included(ColumnValue::Signed(0)), Bound::Unbounded)
        .unwrap();
    let late: Vec<u64> = late.into_iter().filter(|&row| row >= 7000).collect();
    let access = index.access(&late);
    assert_eq!(access.row_groups(), [2, 3]);
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(&file.0).unwrap()).unwrap();
    let read: usize = (access.apply(builder).build().unwrap())
        .map(|batch| batch.unwrap().num_rows())
        .sum();
    assert_eq!(read, late.len());
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(&file.0).unwrap()).unwrap();
    assert_eq!(index.access(&[]).apply(builder).build().unwrap().count(), 0);
}

#[test]
fn files_without_an_index_say_so() {
    let file = TestFile::new("absent");
    write_file(&file, 100);
    let error = ParquetIndex::read(&file.0, "id").err().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
