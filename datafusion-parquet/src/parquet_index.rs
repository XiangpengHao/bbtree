//! Indexes inside Parquet files, as pq-vector embeds its own: the file map of
//! a column's distinct values, each with the rows that hold it, appended
//! after the file's data and named in its footer. Readers that know of it
//! look rows up by value; other readers see the file they saw before.
//!
//! - **Keys** are a value's bytes (`VarBytes`), ordered as the values are
//!   ([`KeyEncoding`]): a number as 8 big-endian bytes, text and byte
//!   strings as they are. Lookups are exact, for every type.
//! - **Values** are the value's rows, in order, each 4 bytes little-endian,
//!   or 8 in a file of 2^32 rows or more: one entry per distinct value, so
//!   a column of few values takes little more than its rows.
//! - **In the file**, the file map lies after the last row group, at a
//!   multiple of its alignment, and before the footer. The footer is
//!   written again, with the same row groups, plus a key-value entry
//!   `bbtree.index.<column>` that says where the file map is, how values are
//!   encoded, and how wide a row is. Nothing else moves: data pages, page
//!   indexes and bloom filters keep their offsets.

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::{Bound, Range};
use std::path::Path;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int64Type, UInt64Type};
use arrow_array::{Array, ArrayRef, BinaryViewArray};
use arrow_schema::DataType;
use bbtree::WithInlineKey;
use bbtree::file::{FileWriter, VarBytes, VarBytesBuf};
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{
    ArrowReaderBuilder, ParquetRecordBatchReaderBuilder, RowSelection,
};
use parquet::file::metadata::{
    FileMetaData, KeyValue, ParquetMetaData, ParquetMetaDataReader, ParquetMetaDataWriter,
};

use bbtree_disk::{MappedFile, OsPages};

/// The file map an index holds: values' bytes, each with its rows.
type VarBytesFile = MappedFile<VarBytes, VarBytes, WithInlineKey>;

/// How an index encodes a column's values as the bytes of its keys, which
/// order as the values do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEncoding {
    /// Signed integers, dates and timestamps: as 64 bits, big-endian, the
    /// sign bit flipped.
    Signed,
    /// Unsigned integers: as 64 bits, big-endian.
    Unsigned,
    /// Floats: as 64 bits, big-endian, flipped so that they order as
    /// numbers, `-0.0` apart from `0.0`.
    Float,
    /// Byte strings and text: their bytes.
    Bytes,
}

impl KeyEncoding {
    /// The encoding of a column of Arrow type `data_type`, and the type
    /// its values are cast to before they are encoded.
    fn of(data_type: &DataType) -> Option<(Self, DataType)> {
        let encoding = match data_type {
            DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::Date32
            | DataType::Date64
            | DataType::Timestamp(..)
            | DataType::Time32(_)
            | DataType::Time64(_) => (Self::Signed, DataType::Int64),
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                (Self::Unsigned, DataType::UInt64)
            }
            DataType::Float16 | DataType::Float32 | DataType::Float64 => {
                (Self::Float, DataType::Float64)
            }
            DataType::Utf8
            | DataType::LargeUtf8
            | DataType::Utf8View
            | DataType::Binary
            | DataType::LargeBinary
            | DataType::BinaryView => (Self::Bytes, DataType::BinaryView),
            DataType::Dictionary(_, values) => return Self::of(values),
            _ => return None,
        };
        Some(encoding)
    }

    /// The name the footer's entry keeps.
    fn name(self) -> &'static str {
        match self {
            Self::Signed => "signed",
            Self::Unsigned => "unsigned",
            Self::Float => "float",
            Self::Bytes => "bytes",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [Self::Signed, Self::Unsigned, Self::Float, Self::Bytes]
            .into_iter()
            .find(|encoding| encoding.name() == name)
    }

    /// Adds to `numbers` each value of `values`, a column of numbers cast
    /// to this encoding's type, that is not null: its encoding, and its
    /// row, counted from `first_row`.
    fn push_numbers(self, values: &dyn Array, first_row: u64, numbers: &mut Vec<(u64, u64)>) {
        let encoded: Box<dyn Iterator<Item = u64>> = match self {
            Self::Signed => Box::new(
                (values.as_primitive::<Int64Type>().values().iter())
                    .map(|&value| ColumnValue::Signed(value).number()),
            ),
            Self::Unsigned => {
                Box::new(values.as_primitive::<UInt64Type>().values().iter().copied())
            }
            Self::Float => Box::new(
                (values.as_primitive::<Float64Type>().values().iter())
                    .map(|&value| ColumnValue::Float(value).number()),
            ),
            Self::Bytes => unreachable!("byte strings are not numbers"),
        };
        let rows = (first_row..).zip(encoded).enumerate();
        numbers.extend(
            rows.filter(|&(index, _)| values.is_valid(index))
                .map(|(_, (row, encoded))| (encoded, row)),
        );
    }
}

/// A value to look rows up by: of the kind the column's index encodes
/// ([`KeyEncoding`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColumnValue<'a> {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Bytes(&'a [u8]),
}

impl<'a> ColumnValue<'a> {
    /// A number's encoding: 64 bits that order as the numbers do.
    fn number(self) -> u64 {
        match self {
            Self::Signed(value) => value.cast_unsigned() ^ (1 << 63),
            Self::Unsigned(value) => value,
            Self::Float(value) => {
                let bits = value.to_bits();
                if bits >> 63 == 1 {
                    !bits
                } else {
                    bits | (1 << 63)
                }
            }
            Self::Bytes(_) => unreachable!("a byte string is not a number"),
        }
    }

    /// The bytes of the value's key, if it is of the kind `encoding`
    /// encodes.
    fn key_bytes(self, encoding: KeyEncoding) -> io::Result<Cow<'a, [u8]>> {
        match (self, encoding) {
            (Self::Bytes(bytes), KeyEncoding::Bytes) => Ok(Cow::Borrowed(bytes)),
            (Self::Signed(_), KeyEncoding::Signed)
            | (Self::Unsigned(_), KeyEncoding::Unsigned)
            | (Self::Float(_), KeyEncoding::Float) => {
                Ok(Cow::Owned(self.number().to_be_bytes().to_vec()))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{self:?} is not a value of an index of {encoding:?} keys"),
            )),
        }
    }
}

/// A column's values, read to be indexed: numbers, encoded, each with its
/// row; or byte strings, in the batches they were read in, each value by
/// its batch and its index there.
enum ColumnValues {
    Numbers(Vec<(u64, u64)>),
    Bytes {
        batches: Vec<BinaryViewArray>,
        /// The first row of each batch.
        first_rows: Vec<u64>,
        /// Each value that is not null: its batch, and its index there.
        values: Vec<(u32, u32)>,
    },
}

impl ColumnValues {
    /// Sorts the values by their keys' bytes, and within a value by row.
    fn sort(&mut self) {
        match self {
            Self::Numbers(numbers) => numbers.sort_unstable(),
            Self::Bytes {
                batches, values, ..
            } => {
                // Stable: values read in row order keep it among equals.
                values.sort_by(|&(a_batch, a_index), &(b_batch, b_index)| {
                    let a = batches[a_batch as usize].value(a_index as usize);
                    a.cmp(batches[b_batch as usize].value(b_index as usize))
                });
            }
        }
    }

    /// The sorted values' entries for an index: each distinct value's key
    /// bytes, and its rows, `row_bytes` wide, little-endian.
    fn entries(&self, row_bytes: usize) -> impl Iterator<Item = (Cow<'_, [u8]>, Cow<'_, [u8]>)> {
        let len = match self {
            Self::Numbers(numbers) => numbers.len(),
            Self::Bytes { values, .. } => values.len(),
        };
        let mut position = 0;
        std::iter::from_fn(move || {
            if position == len {
                return None;
            }
            let (key, row) = self.key_and_row(position);
            let mut rows = Vec::new();
            rows.extend_from_slice(&row.to_le_bytes()[..row_bytes]);
            position += 1;
            while position < len {
                let (next, row) = self.key_and_row(position);
                if next != key {
                    break;
                }
                rows.extend_from_slice(&row.to_le_bytes()[..row_bytes]);
                position += 1;
            }
            Some((key.to_bytes(), Cow::Owned(rows)))
        })
    }

    /// The value at `position` of the sorted values, and its row.
    fn key_and_row(&self, position: usize) -> (ValueKey<'_>, u64) {
        match self {
            Self::Numbers(numbers) => {
                let (number, row) = numbers[position];
                (ValueKey::Number(number), row)
            }
            Self::Bytes {
                batches,
                first_rows,
                values,
            } => {
                let (batch, index) = values[position];
                let row = first_rows[batch as usize] + u64::from(index);
                (
                    ValueKey::Bytes(batches[batch as usize].value(index as usize)),
                    row,
                )
            }
        }
    }
}

/// A column's value, as the index keys it: a number's encoding, or a byte
/// string borrowed from the batch it was read in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKey<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

impl<'a> ValueKey<'a> {
    /// The key's bytes: a number's, big-endian.
    fn to_bytes(self) -> Cow<'a, [u8]> {
        match self {
            Self::Number(number) => Cow::Owned(number.to_be_bytes().to_vec()),
            Self::Bytes(bytes) => Cow::Borrowed(bytes),
        }
    }
}

/// The index of a Parquet file's column, embedded in the file: rows by
/// value.
///
/// # Examples
///
/// ```no_run
/// use bbtree_datafusion_parquet::{ColumnValue, ParquetIndex};
/// use bbtree_disk::OsPages;
///
/// ParquetIndex::embed("hits.parquet", "UserID")?;
/// // SAFETY: nothing changes the file while it is mapped.
/// let index = unsafe { ParquetIndex::mmap("hits.parquet", "UserID", OsPages::Small)? };
/// let rows = index.rows_with(ColumnValue::Signed(435090932899640449))?;
/// let file = std::fs::File::open("hits.parquet")?;
/// let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
/// let reader = index.access(&rows).apply(builder).build()?;
/// # use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct ParquetIndex {
    file: VarBytesFile,
    encoding: KeyEncoding,
    /// How many bytes each row takes in a value's rows.
    row_bytes: usize,
    /// The first row of each of the file's row groups, and after them the
    /// file's number of rows.
    row_group_starts: Vec<u64>,
}

impl ParquetIndex {
    /// The footer's key-value entry for `column`'s index.
    fn entry_key(column: &str) -> String {
        format!("bbtree.index.{column}")
    }

    /// Indexes `column` of the Parquet file at `path`, and embeds the index
    /// in the file: its file map after the last row group, and the footer
    /// written again after it, with an entry that names it. Returns the
    /// index's size in bytes.
    ///
    /// The file is changed in place: if writing fails, it is put back as it
    /// was, but a crash meanwhile may leave it without a footer.
    pub fn embed(path: impl AsRef<Path>, column: &str) -> io::Result<u64> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let footer = ParquetFooter::read(&mut file)?;
        if footer.entry(&Self::entry_key(column)).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("column {column} has an index already"),
            ));
        }
        let row_count = footer.metadata.file_metadata().num_rows();
        let row_bytes = if u32::try_from(row_count).is_ok() {
            4
        } else {
            8
        };
        let (mut values, encoding) = Self::column_values(&file, column)?;
        values.sort();
        let writer = FileWriter::<VarBytes, VarBytes, WithInlineKey>::from_ascending(
            values.entries(row_bytes),
        )
        .expect("sorted distinct values ascend");
        drop(values);

        file.set_len(footer.start)?;
        let written = MappedFile::append(&mut file, &writer).and_then(|range| {
            let entry = format!(
                "{}..{} {} {row_bytes}",
                range.start,
                range.end,
                encoding.name()
            );
            footer.write_with(&mut file, KeyValue::new(Self::entry_key(column), entry))?;
            Ok(range.end - range.start)
        });
        if written.is_err() {
            file.set_len(footer.start)?;
            file.seek(SeekFrom::End(0))?;
            file.write_all(&footer.bytes)?;
        }
        written
    }

    /// The values of `column`, unsorted, and their encoding.
    fn column_values(file: &File, column: &str) -> io::Result<(ColumnValues, KeyEncoding)> {
        let builder = ParquetRecordBatchReaderBuilder::try_new(file.try_clone()?)?;
        let field = builder
            .schema()
            .field_with_name(column)
            .map_err(io::Error::other)?;
        let (encoding, cast_to) = KeyEncoding::of(field.data_type()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("no index for column {column} of type {}", field.data_type()),
            )
        })?;
        let row_count = builder.metadata().file_metadata().num_rows();
        let projection = ProjectionMask::columns(builder.parquet_schema(), [column]);
        let reader = builder
            .with_projection(projection)
            .with_batch_size(64 * 1024)
            .build()?;
        let mut values = match encoding {
            KeyEncoding::Bytes => ColumnValues::Bytes {
                batches: Vec::new(),
                first_rows: Vec::new(),
                values: Vec::new(),
            },
            _ => ColumnValues::Numbers(Vec::with_capacity(usize::try_from(row_count).unwrap_or(0))),
        };
        let mut first_row = 0;
        for batch in reader {
            let batch = batch.map_err(io::Error::other)?;
            let column: ArrayRef =
                arrow_cast::cast(batch.column(0), &cast_to).map_err(io::Error::other)?;
            match &mut values {
                ColumnValues::Numbers(numbers) => {
                    encoding.push_numbers(&column, first_row, numbers);
                }
                ColumnValues::Bytes {
                    batches,
                    first_rows,
                    values,
                } => {
                    let index = u32::try_from(batches.len()).map_err(io::Error::other)?;
                    let valid = (0..column.len()).filter(|&row| column.is_valid(row));
                    values.extend(valid.map(|row| (index, row as u32)));
                    batches.push(column.as_binary_view().clone());
                    first_rows.push(first_row);
                }
            }
            first_row += batch.num_rows() as u64;
        }
        Ok((values, encoding))
    }

    /// The index of `column` embedded in the Parquet file at `path`,
    /// mapped into memory in `pages` ([`MappedFile::map`]): opening it reads
    /// the footer and the index's first page; lookups read the rest as they
    /// reach it.
    ///
    /// # Safety
    ///
    /// The file's index is what [`Self::embed`] wrote, and nothing changes
    /// or truncates the file while it is mapped.
    pub unsafe fn mmap(path: impl AsRef<Path>, column: &str, pages: OsPages) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let (range, entry) = Self::locate(&mut file, column)?;
        // SAFETY: the caller's.
        let mapped = unsafe { MappedFile::mmap(&file, range, pages)? };
        Ok(entry.index(mapped))
    }

    /// The index of `column` embedded in the Parquet file at `path`, read
    /// into memory and checked whole ([`MappedFile::read`]).
    pub fn read(path: impl AsRef<Path>, column: &str) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let (range, entry) = Self::locate(&mut file, column)?;
        let read = MappedFile::read(&mut file, range)?;
        Ok(entry.index(read))
    }

    /// Where `column`'s index lies in `file`, and what else the footer
    /// says of it.
    fn locate(file: &mut File, column: &str) -> io::Result<(Range<u64>, IndexMetadata)> {
        let footer = ParquetFooter::read(file)?;
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        let entry = footer.entry(&Self::entry_key(column)).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no index of {column}"))
        })?;
        let mut fields = entry.split(' ');
        let (range, encoding, row_bytes) = (fields.next(), fields.next(), fields.next());
        let (start, end) = range
            .and_then(|range| range.split_once(".."))
            .ok_or_else(|| invalid("an index entry without a range"))?;
        let parse = |number: &str| {
            number
                .parse::<u64>()
                .map_err(|_| invalid("an index entry's number"))
        };
        let encoding = encoding
            .and_then(KeyEncoding::from_name)
            .ok_or_else(|| invalid("an index entry without an encoding"))?;
        let row_bytes = row_bytes
            .map(parse)
            .transpose()?
            .filter(|&bytes| bytes == 4 || bytes == 8)
            .ok_or_else(|| invalid("an index entry without a row width"))?;
        let mut row_group_starts = vec![0];
        for row_group in footer.metadata.row_groups() {
            let rows = u64::try_from(row_group.num_rows())
                .map_err(|_| invalid("a negative number of rows"))?;
            row_group_starts.push(row_group_starts.last().unwrap() + rows);
        }
        let entry = IndexMetadata {
            encoding,
            row_bytes: row_bytes as usize,
            row_group_starts,
        };
        Ok((parse(start)?..parse(end)?, entry))
    }

    /// How the index encodes values.
    pub fn encoding(&self) -> KeyEncoding {
        self.encoding
    }

    /// The number of distinct values indexed, nulls aside.
    pub fn len(&self) -> usize {
        self.file.map().len()
    }

    /// Whether no row has a value.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The rows whose value is `value`, in order.
    pub fn rows_with(&self, value: ColumnValue<'_>) -> io::Result<Vec<u64>> {
        let query = VarBytesBuf::new(&value.key_bytes(self.encoding)?);
        let rows = self.file.map().get(&*query);
        Ok(rows.map_or_else(Vec::new, |rows| self.rows_of(rows).collect()))
    }

    /// The rows whose value lies within `start` and `end`, in order.
    pub fn rows_between(
        &self,
        start: Bound<ColumnValue<'_>>,
        end: Bound<ColumnValue<'_>>,
    ) -> io::Result<Vec<u64>> {
        let query = |bound: Bound<ColumnValue<'_>>| -> io::Result<Bound<VarBytesBuf>> {
            Ok(match bound {
                Bound::Included(value) => {
                    Bound::Included(VarBytesBuf::new(&value.key_bytes(self.encoding)?))
                }
                Bound::Excluded(value) => {
                    Bound::Excluded(VarBytesBuf::new(&value.key_bytes(self.encoding)?))
                }
                Bound::Unbounded => Bound::Unbounded,
            })
        };
        let (start, end) = (query(start)?, query(end)?);
        let (start, end) = (
            start.as_ref().map(|key| &**key),
            end.as_ref().map(|key| &**key),
        );
        let empty = match (start, end) {
            (Bound::Included(start), Bound::Included(end)) => start > end,
            (Bound::Included(start) | Bound::Excluded(start), Bound::Excluded(end))
            | (Bound::Excluded(start), Bound::Included(end)) => start >= end,
            _ => false,
        };
        if empty {
            return Ok(Vec::new());
        }
        let mut rows: Vec<u64> = (self.file.map().range::<VarBytes, _>((start, end)))
            .flat_map(|(_, rows)| self.rows_of(rows))
            .collect();
        rows.sort_unstable();
        Ok(rows)
    }

    /// The rows a value's entry holds.
    fn rows_of<'a>(&self, rows: &'a VarBytes) -> impl Iterator<Item = u64> + 'a {
        let row_bytes = self.row_bytes;
        rows.bytes().chunks_exact(row_bytes).map(move |row| {
            let mut bytes = [0; 8];
            bytes[..row_bytes].copy_from_slice(row);
            u64::from_le_bytes(bytes)
        })
    }

    /// What a Parquet reader reads to read `rows` alone, which must be in
    /// order: the row groups that hold them, and a selection of them within
    /// those ([`RowAccess::apply`]).
    pub fn access(&self, rows: &[u64]) -> RowAccess {
        let mut row_groups: Vec<usize> = Vec::new();
        let mut ranges: Vec<Range<usize>> = Vec::new();
        // Rows of the row groups taken before the current one: where its
        // rows start in the selection.
        let mut selected_before = 0;
        for &row in rows {
            let row_group = self.row_group_starts.partition_point(|&start| start <= row) - 1;
            if row_groups.last() != Some(&row_group) {
                if let Some(&last) = row_groups.last() {
                    selected_before += self.row_group_len(last);
                }
                row_groups.push(row_group);
            }
            let position = (selected_before + row - self.row_group_starts[row_group]) as usize;
            match ranges.last_mut() {
                Some(last) if last.end == position => last.end += 1,
                _ => ranges.push(position..position + 1),
            }
        }
        let selected = selected_before
            + row_groups
                .last()
                .map_or(0, |&last| self.row_group_len(last));
        RowAccess {
            selection: RowSelection::from_consecutive_ranges(ranges.into_iter(), selected as usize),
            row_groups,
        }
    }

    /// The number of rows of row group `row_group`.
    fn row_group_len(&self, row_group: usize) -> u64 {
        self.row_group_starts[row_group + 1] - self.row_group_starts[row_group]
    }
}

/// What a Parquet file's footer says of an index, but where its file map is:
/// how it encodes values, how wide a row is, and where each row group
/// starts, the file's number of rows last.
struct IndexMetadata {
    encoding: KeyEncoding,
    row_bytes: usize,
    row_group_starts: Vec<u64>,
}

impl IndexMetadata {
    /// The index this entry describes, whose file map is in `file`.
    fn index(self, file: VarBytesFile) -> ParquetIndex {
        ParquetIndex {
            file,
            encoding: self.encoding,
            row_bytes: self.row_bytes,
            row_group_starts: self.row_group_starts,
        }
    }
}

/// Rows for a Parquet reader to read, as [`ParquetIndex::access`] found
/// them: the row groups that hold them, and which of those row groups'
/// rows they are.
#[derive(Clone, Debug)]
pub struct RowAccess {
    row_groups: Vec<usize>,
    selection: RowSelection,
}

impl RowAccess {
    /// The row groups that hold the rows, in order.
    pub fn row_groups(&self) -> &[usize] {
        &self.row_groups
    }

    /// The rows, out of those of [`Self::row_groups`] one after another.
    pub fn selection(&self) -> &RowSelection {
        &self.selection
    }

    /// `builder`, reading these rows alone: their row groups, and within
    /// them, the rows.
    pub fn apply<T>(self, builder: ArrowReaderBuilder<T>) -> ArrowReaderBuilder<T> {
        builder
            .with_row_groups(self.row_groups)
            .with_row_selection(self.selection)
    }
}

/// A Parquet file's footer: its metadata, where it starts, and its bytes,
/// to write it again with an entry more, or as it was.
struct ParquetFooter {
    metadata: ParquetMetaData,
    /// Where the footer's metadata starts: the end of the data.
    start: u64,
    /// The footer's bytes, from `start` to the end of the file.
    bytes: Vec<u8>,
}

impl ParquetFooter {
    fn read(file: &mut File) -> io::Result<Self> {
        let metadata = ParquetMetaDataReader::new().parse_and_finish(&*file)?;
        let end = file.seek(SeekFrom::End(0))?;
        let mut tail = [0; 8];
        file.seek(SeekFrom::End(-8))?;
        file.read_exact(&mut tail)?;
        let metadata_len = u64::from(u32::from_le_bytes(tail[..4].try_into().unwrap()));
        let start = end - 8 - metadata_len;
        let mut bytes = vec![0; (end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut bytes)?;
        Ok(Self {
            metadata,
            start,
            bytes,
        })
    }

    /// The value of the footer's key-value entry `key`, if it has one.
    fn entry(&self, key: &str) -> Option<&str> {
        let entries = self.metadata.file_metadata().key_value_metadata()?;
        let entry = entries.iter().find(|entry| entry.key == key)?;
        entry.value.as_deref()
    }

    /// Writes the footer at the end of `file`, with `entry` added to its
    /// key-value entries.
    fn write_with(&self, file: &mut File, entry: KeyValue) -> io::Result<()> {
        let old = self.metadata.file_metadata();
        let mut entries = old.key_value_metadata().cloned().unwrap_or_default();
        entries.push(entry);
        let file_metadata = FileMetaData::new(
            old.version(),
            old.num_rows(),
            old.created_by().map(str::to_owned),
            Some(entries),
            Arc::clone(&old.schema_descr_ptr()),
            old.column_orders().cloned(),
        );
        let metadata = ParquetMetaData::new(file_metadata, self.metadata.row_groups().to_vec());
        file.seek(SeekFrom::End(0))?;
        let mut out = io::BufWriter::new(file);
        ParquetMetaDataWriter::new(&mut out, &metadata).finish()?;
        out.flush()
    }
}
