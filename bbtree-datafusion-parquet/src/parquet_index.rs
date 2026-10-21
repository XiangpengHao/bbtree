//! Indexes inside Parquet files, as pq-vector embeds its own: file maps of
//! rows by the values of one or more columns, after the file's data and
//! listed in its footer ([`crate::ParquetIndexBuilder`] writes them).
//! Readers that know of them look rows up by value; other readers see the
//! file they saw before.
//!
//! - **Keys** are the values of the index's columns ([`crate::index_key`]):
//!   a `u64` for one column of numbers, the bytes for one of text, and the
//!   columns' parts, as bytes, for several. Rows whose first column is null
//!   are left out: no lookup asks for them.
//! - **Values** are a key's rows ([`RowIds`]), in order, each a `u32`: one
//!   entry per distinct key.
//! - **The footer** lists the indexes in its key-value entry
//!   `bbtree.indexes`: each one's schema, its columns and the kinds of
//!   their values, and where its file map lies.

use std::borrow::Cow;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use bbtree::WithInlineKey;
use bbtree_disk::{BBTreeMapFile, DiskQuery, DiskType, Input, VarBytes, VarBytesQuery};
use datafusion::arrow::datatypes::Schema;
use datafusion::parquet::arrow::arrow_reader::RowSelection;
use datafusion::parquet::file::metadata::{FileMetaData, KeyValue, ParquetMetaData};
use serde::{Deserialize, Serialize};

use crate::index_key::{KeyForm, KeyRanges, KeyType};
use crate::scan_predicate::ScanPredicate;

/// The indexes embedded in a Parquet file, as its footer lists them.
#[derive(Debug)]
pub(crate) struct ParquetIndexes {
    path: PathBuf,
    indexes: Vec<ParquetIndex>,
    /// The file's number of rows.
    row_count: u64,
}

impl ParquetIndexes {
    /// The footer's key-value entry that lists a file's indexes.
    const ENTRY_KEY: &str = "bbtree.indexes";
    /// The version of the entry, and of the indexes it lists.
    const VERSION: u32 = 3;

    /// The indexes listed in `metadata`, the footer of the Parquet file at
    /// `path`.
    pub(crate) fn of(path: &Path, metadata: &ParquetMetaData) -> io::Result<Self> {
        let file_metadata = metadata.file_metadata();
        Ok(Self {
            path: path.to_owned(),
            indexes: Self::listed(file_metadata)?,
            row_count: Self::row_count(file_metadata)?,
        })
    }

    /// The number of rows of a file of footer `metadata`.
    pub(crate) fn row_count(metadata: &FileMetaData) -> io::Result<u64> {
        u64::try_from(metadata.num_rows())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a negative number of rows"))
    }

    /// The indexes the footer's entry lists.
    pub(crate) fn listed(metadata: &FileMetaData) -> io::Result<Vec<ParquetIndex>> {
        let mut entries = metadata.key_value_metadata().into_iter().flatten();
        let Some(entry) = entries.find(|entry| entry.key == Self::ENTRY_KEY) else {
            return Ok(Vec::new());
        };
        let list: IndexList = serde_json::from_str(entry.value.as_deref().unwrap_or_default())?;
        if list.version != Self::VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("indexes of version {}, not {}", list.version, Self::VERSION),
            ));
        }
        Ok(list.indexes)
    }

    /// The footer's entry that lists `indexes`.
    pub(crate) fn entry(indexes: &[ParquetIndex]) -> io::Result<KeyValue> {
        let list = IndexList {
            version: Self::VERSION,
            indexes: indexes.to_vec(),
        };
        Ok(KeyValue::new(
            Self::ENTRY_KEY.to_owned(),
            serde_json::to_string(&list)?,
        ))
    }

    /// Whether the file has no index.
    pub(crate) fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }

    /// The rows that the indexes say a scan of `schema` keeps by
    /// `predicate`: those of every index that applies, or `None` if none
    /// does.
    pub(crate) fn select_rows(
        &self,
        predicate: &ScanPredicate,
        schema: &Schema,
    ) -> io::Result<Option<RowSelection>> {
        let mut selected: Option<Vec<u64>> = None;
        for index in &self.indexes {
            let Some(ranges) = predicate.key_ranges(index, schema) else {
                continue;
            };
            let rows = index.rows(&self.path, &ranges)?;
            selected = Some(match selected {
                Some(mut selected) => {
                    selected.retain(|row| rows.binary_search(row).is_ok());
                    selected
                }
                None => rows,
            });
        }
        Ok(selected.map(|rows| self.selection(&rows)))
    }

    /// The selection of `rows`, which ascend, out of the file's.
    fn selection(&self, rows: &[u64]) -> RowSelection {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for &row in rows {
            let row = row as usize;
            match ranges.last_mut() {
                Some(last) if last.end == row => last.end += 1,
                _ => ranges.push(row..row + 1),
            }
        }
        RowSelection::from_consecutive_ranges(ranges.into_iter(), self.row_count as usize)
    }
}

/// The footer's entry: the file's indexes.
#[derive(Serialize, Deserialize)]
struct IndexList {
    version: u32,
    indexes: Vec<ParquetIndex>,
}

/// An index embedded in a Parquet file: its schema, the columns it keys
/// rows by, and where its file map lies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ParquetIndex {
    schema: Vec<IndexColumn>,
    /// Where the file map lies in the file.
    file_range: Range<u64>,
}

impl ParquetIndex {
    pub(crate) fn new(schema: Vec<IndexColumn>, file_range: Range<u64>) -> Self {
        Self { schema, file_range }
    }

    /// The names of the columns the index keys rows by, in order.
    pub(crate) fn columns(&self) -> impl Iterator<Item = &str> {
        self.schema.iter().map(IndexColumn::name)
    }

    /// The columns the index keys rows by, in order, and the kinds of their
    /// values.
    pub(crate) fn schema(&self) -> &[IndexColumn] {
        &self.schema
    }

    /// The type of the index's keys.
    pub(crate) fn key_form(&self) -> KeyForm {
        KeyForm::of(self.schema.iter().map(IndexColumn::key_type))
    }

    /// The rows of the file at `path` whose keys lie in `ranges`, in order.
    fn rows(&self, path: &Path, ranges: &KeyRanges) -> io::Result<Vec<u64>> {
        let mut rows = Vec::new();
        // SAFETY, of each `open`: the scans trust the indexes they read, as
        // `with_parquet_index` says: the file holds what the builder wrote,
        // and nothing changes it while it is read.
        match ranges {
            KeyRanges::Numbers(ranges) => {
                let file_map = BBTreeMapFile::<u64, RowIds>::mmap(path);
                let index = unsafe { file_map.range(self.file_range.clone()).open()? };
                for &range in ranges {
                    for (_, ids) in index.range::<u64, _>(range) {
                        rows.extend(ids.iter().map(u64::from));
                    }
                }
            }
            KeyRanges::Bytes(ranges) => {
                let file_map = BBTreeMapFile::<Vec<u8>, RowIds>::mmap(path);
                let index = unsafe { file_map.range(self.file_range.clone()).open()? };
                for (start, end) in ranges {
                    let range = (
                        start.as_ref().map(Vec::as_slice),
                        end.as_ref().map(Vec::as_slice),
                    );
                    for (_, ids) in index.range::<[u8], _>(range) {
                        rows.extend(ids.iter().map(u64::from));
                    }
                }
            }
        }
        // Ranges may overlap, as `a < 5 OR a < 7` does.
        rows.sort_unstable();
        rows.dedup();
        Ok(rows)
    }
}

/// A key's rows, as an index keeps them: their numbers in the file, in
/// order, each 4 bytes little-endian, kept as bytes, as the file keeps them.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowIds {
    bytes: Vec<u8>,
}

impl RowIds {
    pub(crate) fn push(&mut self, row: u32) {
        self.bytes.extend_from_slice(&row.to_le_bytes());
    }

    /// The number of rows.
    pub(crate) fn len(&self) -> usize {
        self.bytes.len() / 4
    }
}

impl DiskQuery for RowIds {
    type Stored = VarBytes;
    type Query<'a> = VarBytesQuery<'a>;

    fn query(&self) -> VarBytesQuery<'_> {
        VarBytesQuery::new(&self.bytes)
    }

    fn input(&self) -> Input<'_, VarBytes> {
        Cow::Borrowed(&self.bytes)
    }
}

/// Kept in the heap pages after its leaf, as `Vec<u8>` is.
impl DiskType for RowIds {
    type Strategy = WithInlineKey;
    type View<'a> = RowIdsView<'a>;

    fn view(stored: &VarBytes) -> RowIdsView<'_> {
        RowIdsView {
            bytes: stored.bytes(),
        }
    }
}

/// A key's rows, read where the index lies: its bytes, which needn't be
/// aligned for `u32`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RowIdsView<'a> {
    bytes: &'a [u8],
}

impl RowIdsView<'_> {
    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> {
        (self.bytes.as_chunks::<4>().0.iter()).map(|&row| u32::from_le_bytes(row))
    }
}

/// A column an index keys rows by, and the kind of its values.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct IndexColumn {
    name: String,
    key_type: KeyType,
}

impl IndexColumn {
    pub(crate) fn new(name: String, key_type: KeyType) -> Self {
        Self { name, key_type }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn key_type(&self) -> KeyType {
        self.key_type
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use datafusion::arrow::datatypes::{DataType, Field, SchemaRef};
    use datafusion::common::ScalarValue;
    use datafusion::logical_expr::Operator;
    use datafusion::physical_expr::PhysicalExpr;
    use datafusion::physical_expr::expressions::{BinaryExpr, Column, Literal};

    use super::*;
    use crate::ParquetIndexBuilder;
    use crate::test_file::TestFile;

    /// The test file of `name`, with indexes of `id` and of `name`.
    fn indexed_file(name: &str) -> (TestFile, ParquetIndexes) {
        let file = TestFile::new(name);
        (ParquetIndexBuilder::source(file.path()))
            .index(&["id"])
            .index(&["name"])
            .build()
            .unwrap();
        let indexes = ParquetIndexes::of(file.path(), &file.metadata()).unwrap();
        (file, indexes)
    }

    /// `column op literal`, of `column` at `position` of the schema.
    fn compare(
        column: &str,
        position: usize,
        op: Operator,
        literal: ScalarValue,
    ) -> Arc<dyn PhysicalExpr> {
        let column = Arc::new(Column::new(column, position));
        Arc::new(BinaryExpr::new(column, op, Arc::new(Literal::new(literal))))
    }

    /// The rows the indexes select by `predicate`, in a scan of `schema`,
    /// or `None` if none applies.
    fn select(
        indexes: &ParquetIndexes,
        predicate: &Arc<dyn PhysicalExpr>,
        schema: &Schema,
    ) -> Option<Vec<u64>> {
        let selection = (indexes.select_rows(&ScanPredicate::new(predicate), schema)).unwrap()?;
        let mut rows = Vec::new();
        let mut row = 0;
        for selector in selection.iter() {
            if !selector.skip {
                rows.extend(row..row + selector.row_count as u64);
            }
            row += selector.row_count as u64;
        }
        assert_eq!(row, TestFile::ROWS as u64, "the selection covers the file");
        Some(rows)
    }

    /// The rows whose `values` are `Some` and pass `keep`.
    fn rows_where<V>(values: Vec<Option<V>>, keep: impl Fn(&V) -> bool) -> Vec<u64> {
        (values.into_iter().enumerate())
            .filter(|(_, value)| value.as_ref().is_some_and(&keep))
            .map(|(row, _)| row as u64)
            .collect()
    }

    #[test]
    fn lookups_find_the_rows_comparisons_keep() {
        let (file, indexes) = indexed_file("index-lookups");
        let schema = file.schema();
        let ops = [
            Operator::Eq,
            Operator::Lt,
            Operator::LtEq,
            Operator::Gt,
            Operator::GtEq,
        ];
        for op in ops {
            for literal in [-51, -50, -3, 0, 17, 49, 50] {
                let predicate = compare("id", 0, op, ScalarValue::Int64(Some(literal)));
                let found = select(&indexes, &predicate, &schema).expect("the index of `id`");
                let expected = rows_where(TestFile::ids(), |&id| match op {
                    Operator::Eq => id == literal,
                    Operator::Lt => id < literal,
                    Operator::LtEq => id <= literal,
                    Operator::Gt => id > literal,
                    _ => id >= literal,
                });
                assert_eq!(found, expected, "id {op} {literal}");
            }
        }
    }

    #[test]
    fn rows_of_every_index_that_applies_intersect() {
        let (file, indexes) = indexed_file("index-intersect");
        let name = ScalarValue::Utf8(Some("user-name-00150".into()));
        let predicate: Arc<dyn PhysicalExpr> = Arc::new(BinaryExpr::new(
            compare("id", 0, Operator::Gt, ScalarValue::Int64(Some(0))),
            Operator::And,
            compare("name", 1, Operator::GtEq, name),
        ));
        let found = select(&indexes, &predicate, &file.schema()).unwrap();
        let names = TestFile::names().into_iter().map(Some).collect();
        let named = rows_where(names, |name| name.as_str() >= "user-name-00150");
        let positive = rows_where(TestFile::ids(), |&id| id > 0);
        let expected: Vec<u64> = named
            .into_iter()
            .filter(|row| positive.contains(row))
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(found, expected);
    }

    #[test]
    fn indexes_apply_to_the_columns_they_resolve_to() {
        let (file, indexes) = indexed_file("index-resolve");
        let schema = file.schema();
        let id_is = |literal| compare("id", 0, Operator::Eq, literal);
        let seventeen = || ScalarValue::Int64(Some(17));
        assert!(select(&indexes, &id_is(seventeen()), &schema).is_some());
        // No index of `flag`.
        let flag = compare("flag", 2, Operator::Eq, ScalarValue::Boolean(Some(true)));
        assert!(select(&indexes, &flag, &schema).is_none());
        // `id` read as another integer type is of the kind the index holds;
        // read as text, it isn't.
        let with_id = |data_type| -> SchemaRef {
            let mut fields: Vec<Field> = schema
                .fields()
                .iter()
                .map(|field| field.as_ref().clone())
                .collect();
            fields[0] = Field::new("id", data_type, true);
            Arc::new(Schema::new(fields))
        };
        let as_int32 = id_is(ScalarValue::Int32(Some(17)));
        let expected = select(&indexes, &id_is(seventeen()), &schema);
        assert_eq!(
            select(&indexes, &as_int32, &with_id(DataType::Int32)),
            expected
        );
        let as_text = id_is(ScalarValue::Utf8(Some("17".into())));
        assert!(select(&indexes, &as_text, &with_id(DataType::Utf8)).is_none());
        // A column of the same name elsewhere in the schema is another one.
        let elsewhere = compare("id", 1, Operator::Eq, seventeen());
        assert!(select(&indexes, &elsewhere, &schema).is_none());
    }

    #[test]
    fn footers_list_indexes_of_their_version_alone() {
        let file = TestFile::new("index-versions");
        let metadata = file.metadata();
        let footer = |entry: Option<&str>| {
            let old = metadata.file_metadata();
            let entries = entry.map(|value| {
                vec![KeyValue::new(
                    ParquetIndexes::ENTRY_KEY.to_owned(),
                    value.to_owned(),
                )]
            });
            FileMetaData::new(
                old.version(),
                old.num_rows(),
                None,
                entries,
                old.schema_descr_ptr(),
                None,
            )
        };
        assert!(ParquetIndexes::listed(&footer(None)).unwrap().is_empty());
        let list = |version| format!(r#"{{"version":{version},"indexes":[]}}"#);
        let empty = list(ParquetIndexes::VERSION);
        assert!(
            ParquetIndexes::listed(&footer(Some(&empty)))
                .unwrap()
                .is_empty()
        );
        let older = list(ParquetIndexes::VERSION - 1);
        let error = ParquetIndexes::listed(&footer(Some(&older))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn indexes_of_several_columns_find_rows_by_values_then_a_range() {
        let file = TestFile::new("index-parts");
        (ParquetIndexBuilder::source(file.path()))
            .index(&["name", "id"])
            .build()
            .unwrap();
        let indexes = ParquetIndexes::of(file.path(), &file.metadata()).unwrap();
        let schema = file.schema();
        let name = "user-name-00150";
        let name_is = || {
            compare(
                "name",
                1,
                Operator::Eq,
                ScalarValue::Utf8(Some(name.into())),
            )
        };
        let rows_where = |keep: &dyn Fn(&str, Option<i64>) -> bool| -> Vec<u64> {
            (TestFile::names().iter().zip(TestFile::ids()).enumerate())
                .filter(|(_, (row_name, id))| keep(row_name, *id))
                .map(|(row, _)| row as u64)
                .collect()
        };
        // A name's keys, nulls of `id` among them.
        assert!(!rows_where(&|row_name, id| row_name == name && id.is_none()).is_empty());
        let found = select(&indexes, &name_is(), &schema).unwrap();
        assert_eq!(found, rows_where(&|row_name, _| row_name == name));
        // A name's, then a range of `id`.
        let positive = compare("id", 0, Operator::Gt, ScalarValue::Int64(Some(0)));
        let both: Arc<dyn PhysicalExpr> =
            Arc::new(BinaryExpr::new(name_is(), Operator::And, positive));
        let found = select(&indexes, &both, &schema).unwrap();
        let expected = rows_where(&|row_name, id| row_name == name && id.is_some_and(|id| id > 0));
        assert!(!expected.is_empty());
        assert_eq!(found, expected);
        // The second column alone: the index doesn't apply.
        let id_is = compare("id", 0, Operator::Eq, ScalarValue::Int64(Some(0)));
        assert!(select(&indexes, &id_is, &schema).is_none());
    }
}
