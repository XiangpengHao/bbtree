//! A column of a Parquet file indexed in place, and rows looked up by value
//! with the index, against a scan of the column.
//!
//! `cargo run --release --example parquet_lookups -- <file.parquet> <column>
//! [value...]`: embeds the column's index in the file, unless it has one,
//! then for each value (integers, or text for a text column; by default the
//! values of a few rows of the file) times the lookup, and reading the rows
//! it found, all columns, against reading the column whole and filtering it.

use std::fs::File;
use std::time::Instant;

use arrow_array::cast::AsArray;
use arrow_array::types::Int64Type;
use arrow_array::{Array, BooleanArray, RecordBatch};
use arrow_schema::DataType;
use bbtree_datafusion_parquet::{ColumnValue, KeyEncoding, ParquetIndex};
use bbtree_disk::OsPages;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{
    ArrowPredicateFn, ArrowReaderOptions, ParquetRecordBatchReaderBuilder, RowFilter,
};
use parquet::file::metadata::PageIndexPolicy;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// A value to look up, in the column's type.
enum Wanted {
    Signed(i64),
    Text(String),
}

impl Wanted {
    fn index_value(&self) -> ColumnValue<'_> {
        match self {
            Self::Signed(value) => ColumnValue::Signed(*value),
            Self::Text(text) => ColumnValue::Bytes(text.as_bytes()),
        }
    }

    /// Which of `values`, a column of the file cast to `Int64` or
    /// `BinaryView`,
    /// are this value.
    fn matches(&self, values: &dyn Array) -> BooleanArray {
        match self {
            Self::Signed(wanted) => values
                .as_primitive::<Int64Type>()
                .iter()
                .map(|value| Some(value == Some(*wanted)))
                .collect(),
            Self::Text(wanted) => values
                .as_binary_view()
                .iter()
                .map(|value| Some(value == Some(wanted.as_bytes())))
                .collect(),
        }
    }
}

/// The rows of `column` that are `wanted`, by reading it whole: the scan an
/// index saves.
fn scan(path: &str, column: &str, wanted: &Wanted, cast_to: &DataType) -> usize {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap()).unwrap();
    let projection = ProjectionMask::columns(builder.parquet_schema(), [column]);
    let reader = builder.with_projection(projection).build().unwrap();
    reader
        .map(|batch| {
            let values = arrow_cast::cast(batch.unwrap().column(0), cast_to).unwrap();
            wanted.matches(&values).true_count()
        })
        .sum()
}

/// Reads every column of `rows` of the file, skipping the pages the
/// file's page index says hold none of them, if it has one.
fn read_rows(path: &str, index: &ParquetIndex, rows: &[u64]) -> Vec<RecordBatch> {
    let options = ArrowReaderOptions::new().with_page_index_policy(PageIndexPolicy::Optional);
    let file = File::open(path).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new_with_options(file, options).unwrap();
    let reader = index.access(rows).apply(builder).build().unwrap();
    reader.map(Result::unwrap).collect()
}

/// Reads every column of the rows whose `column` is `wanted`, without an
/// index: the reader decodes `column` whole, and the other columns only
/// where it matched (a row filter, Parquet's late materialization).
fn filter_rows(path: &str, column: &str, wanted: &Wanted, cast_to: &DataType) -> usize {
    let options = ArrowReaderOptions::new().with_page_index_policy(PageIndexPolicy::Optional);
    let file = File::open(path).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new_with_options(file, options).unwrap();
    let projection = ProjectionMask::columns(builder.parquet_schema(), [column]);
    let cast_to = cast_to.clone();
    let wanted = match wanted {
        Wanted::Signed(value) => Wanted::Signed(*value),
        Wanted::Text(text) => Wanted::Text(text.clone()),
    };
    let predicate = ArrowPredicateFn::new(projection, move |batch: RecordBatch| {
        let values = arrow_cast::cast(batch.column(0), &cast_to)?;
        Ok(wanted.matches(&values))
    });
    let reader = builder
        .with_row_filter(RowFilter::new(vec![Box::new(predicate)]))
        .build()
        .unwrap();
    reader.map(|batch| batch.unwrap().num_rows()).sum()
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (path, column) = (arguments[0].as_str(), arguments[1].as_str());

    let started = Instant::now();
    match ParquetIndex::embed(path, column) {
        Ok(bytes) => println!(
            "indexed {column} in {:.2} s: {:.1} MB in a file now of {:.1} MB",
            started.elapsed().as_secs_f64(),
            bytes as f64 / 1e6,
            std::fs::metadata(path).unwrap().len() as f64 / 1e6
        ),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("{error}"),
    }
    let started = Instant::now();
    // SAFETY: nothing changes the file while it is mapped.
    let index = unsafe { ParquetIndex::mmap(path, column, OsPages::Small) }.unwrap();
    println!(
        "opened the index of {} distinct values in {:.1} µs",
        index.len(),
        started.elapsed().as_secs_f64() * 1e6
    );
    let (cast_to, wanted): (DataType, Vec<Wanted>) = match index.encoding() {
        KeyEncoding::Signed => (
            DataType::Int64,
            arguments[2..]
                .iter()
                .map(|value| Wanted::Signed(value.parse().unwrap()))
                .collect(),
        ),
        KeyEncoding::Bytes => (
            DataType::BinaryView,
            arguments[2..]
                .iter()
                .map(|value| Wanted::Text(value.clone()))
                .collect(),
        ),
        other => panic!("the example looks up signed and text columns, not {other:?}"),
    };

    let has_page_index = ParquetRecordBatchReaderBuilder::try_new_with_options(
        File::open(path).unwrap(),
        ArrowReaderOptions::new().with_page_index_policy(PageIndexPolicy::Optional),
    )
    .unwrap()
    .metadata()
    .page_index()
    .is_some();
    println!("the file has a page index: {has_page_index}");
    println!(
        "{:<24} {:>6} {:>12} {:>14} {:>12} {:>14}",
        "value", "rows", "lookup", "read the rows", "scan column", "filter rows"
    );
    for wanted in &wanted {
        let started = Instant::now();
        let rows = index.rows_with(wanted.index_value()).unwrap();
        let lookup = started.elapsed();
        let started = Instant::now();
        let batches = read_rows(path, &index, &rows);
        let read = started.elapsed();
        let read_count: usize = batches.iter().map(RecordBatch::num_rows).sum();
        assert_eq!(read_count, rows.len());
        let started = Instant::now();
        let scanned = scan(path, column, wanted, &cast_to);
        let scan_time = started.elapsed();
        assert!(scanned <= rows.len());
        let started = Instant::now();
        let filtered = filter_rows(path, column, wanted, &cast_to);
        let filter_time = started.elapsed();
        assert_eq!(filtered, scanned);
        let label = match wanted {
            Wanted::Signed(value) => value.to_string(),
            Wanted::Text(text) => text.chars().take(24).collect(),
        };
        println!(
            "{label:<24} {:>6} {:>9.1} µs {:>11.2} ms {:>9.2} ms {:>11.2} ms",
            rows.len(),
            lookup.as_secs_f64() * 1e6,
            read.as_secs_f64() * 1e3,
            scan_time.as_secs_f64() * 1e3,
            filter_time.as_secs_f64() * 1e3
        );
    }
}
