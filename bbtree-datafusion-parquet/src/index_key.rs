//! Index keys: what an index keys a row by, the values of its columns, and
//! ranges of them.
//!
//! - **An index of one column** keys rows by the column's value, in a key
//!   type of the file map's own ([`KeyForm`]): a number as a `u64` that
//!   orders as the numbers do, text and byte strings as their bytes.
//! - **An index of several columns** keys rows by bytes: the columns are
//!   chosen at run time, and no Rust type holds any mix of them. Each
//!   column's part is a tag byte, `0` for null and `1` for a value, then the
//!   value: a number as its `u64`, big-endian; a byte string with each `0`
//!   byte written `0 0xFF`, and `0 0` after it. No part's bytes start
//!   another's, so keys order as their parts do, left to right, and the keys
//!   whose first columns hold given values, and whose next one lies in a
//!   range, are a range of keys. Nulls of later columns stay, ordered first.
//! - **Rows whose first column is null** aren't keyed: a comparison or an
//!   `IN` list is never true of null, so no lookup asks for them.

use std::borrow::Cow;
use std::ops::Bound;

use datafusion::arrow::array::{
    Array, ArrayRef, AsArray, BinaryViewArray, Float64Array, Int64Array, UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, Float64Type, Int64Type, TimeUnit, UInt64Type};
use datafusion::common::ScalarValue;
use datafusion::logical_expr::Operator;
use serde::{Deserialize, Serialize};

/// The kind of values an index's column holds, which says how they are
/// keyed: columns of one kind key a value alike, whatever their exact
/// types, and a literal is looked up in a column of its own kind alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeyType {
    /// Signed integers.
    Int,
    /// Unsigned integers.
    UInt,
    /// Floats, in IEEE 754's total order, as Arrow compares them: `-0.0`
    /// before `0.0`, NaN after infinity.
    Float,
    /// Text and byte strings, by their bytes.
    Bytes,
    /// Days since the epoch.
    Date32,
    /// Milliseconds since the epoch.
    Date64,
    /// Times of day, counted in a unit.
    Time(TimeUnit),
    /// Instants, counted in a unit since the epoch, in any time zone.
    Timestamp(TimeUnit),
}

impl KeyType {
    /// The kind of the values of a column of Arrow type `data_type`, if an
    /// index can hold them.
    pub(crate) fn of(data_type: &DataType) -> Option<Self> {
        let key_type = match data_type {
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => Self::Int,
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => Self::UInt,
            DataType::Float16 | DataType::Float32 | DataType::Float64 => Self::Float,
            DataType::Utf8
            | DataType::LargeUtf8
            | DataType::Utf8View
            | DataType::Binary
            | DataType::LargeBinary
            | DataType::BinaryView => Self::Bytes,
            DataType::Date32 => Self::Date32,
            DataType::Date64 => Self::Date64,
            DataType::Time32(unit) | DataType::Time64(unit) => Self::Time(*unit),
            DataType::Timestamp(unit, _) => Self::Timestamp(*unit),
            DataType::Dictionary(_, values) => return Self::of(values),
            _ => return None,
        };
        Some(key_type)
    }

    /// The Arrow type a column of this kind is cast to, to be keyed
    /// ([`KeyColumn`]).
    pub(crate) fn cast_type(self) -> DataType {
        match self {
            Self::UInt => DataType::UInt64,
            Self::Float => DataType::Float64,
            Self::Bytes => DataType::BinaryView,
            Self::Int | Self::Date32 | Self::Date64 | Self::Time(_) | Self::Timestamp(_) => {
                DataType::Int64
            }
        }
    }

    /// The value of `literal`, if it is a value of this kind.
    pub(crate) fn literal(self, literal: &ScalarValue) -> Option<KeyPart<'_>> {
        use ScalarValue as S;
        let part = match (self, literal) {
            (_, S::Dictionary(_, values)) => return self.literal(values),
            (Self::Int, S::Int8(Some(value))) => KeyPart::signed(i64::from(*value)),
            (Self::Int, S::Int16(Some(value))) => KeyPart::signed(i64::from(*value)),
            (Self::Int, S::Int32(Some(value))) => KeyPart::signed(i64::from(*value)),
            (Self::Int, S::Int64(Some(value))) => KeyPart::signed(*value),
            (Self::UInt, S::UInt8(Some(value))) => KeyPart::Number(u64::from(*value)),
            (Self::UInt, S::UInt16(Some(value))) => KeyPart::Number(u64::from(*value)),
            (Self::UInt, S::UInt32(Some(value))) => KeyPart::Number(u64::from(*value)),
            (Self::UInt, S::UInt64(Some(value))) => KeyPart::Number(*value),
            (Self::Float, S::Float16(Some(value))) => KeyPart::float(f64::from(*value)),
            (Self::Float, S::Float32(Some(value))) => KeyPart::float(f64::from(*value)),
            (Self::Float, S::Float64(Some(value))) => KeyPart::float(*value),
            (
                Self::Bytes,
                S::Utf8(Some(text)) | S::LargeUtf8(Some(text)) | S::Utf8View(Some(text)),
            ) => KeyPart::Bytes(Cow::Borrowed(text.as_bytes())),
            (
                Self::Bytes,
                S::Binary(Some(bytes)) | S::LargeBinary(Some(bytes)) | S::BinaryView(Some(bytes)),
            ) => KeyPart::Bytes(Cow::Borrowed(bytes)),
            (Self::Date32, S::Date32(Some(days))) => KeyPart::signed(i64::from(*days)),
            (Self::Date64, S::Date64(Some(millis))) => KeyPart::signed(*millis),
            (Self::Time(TimeUnit::Second), S::Time32Second(Some(time)))
            | (Self::Time(TimeUnit::Millisecond), S::Time32Millisecond(Some(time))) => {
                KeyPart::signed(i64::from(*time))
            }
            (Self::Time(TimeUnit::Microsecond), S::Time64Microsecond(Some(time)))
            | (Self::Time(TimeUnit::Nanosecond), S::Time64Nanosecond(Some(time)))
            | (Self::Timestamp(TimeUnit::Second), S::TimestampSecond(Some(time), _))
            | (Self::Timestamp(TimeUnit::Millisecond), S::TimestampMillisecond(Some(time), _))
            | (Self::Timestamp(TimeUnit::Microsecond), S::TimestampMicrosecond(Some(time), _))
            | (Self::Timestamp(TimeUnit::Nanosecond), S::TimestampNanosecond(Some(time), _)) => {
                KeyPart::signed(*time)
            }
            _ => return None,
        };
        Some(part)
    }
}

/// The type of an index's keys, which its columns' kinds decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyForm {
    /// One column of numbers: `u64` keys.
    Number,
    /// One column of text or byte strings: their bytes.
    Bytes,
    /// Several columns: their parts, as bytes.
    Parts,
}

impl KeyForm {
    /// The form of an index of columns of `key_types`, in order.
    pub(crate) fn of(mut key_types: impl ExactSizeIterator<Item = KeyType>) -> Self {
        match (key_types.len(), key_types.next()) {
            (1, Some(KeyType::Bytes)) => Self::Bytes,
            (1, _) => Self::Number,
            _ => Self::Parts,
        }
    }
}

/// A column's value, as an index orders it: a number as 64 bits that order
/// as the numbers do, or bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KeyPart<'a> {
    Number(u64),
    Bytes(Cow<'a, [u8]>),
}

impl KeyPart<'_> {
    const NULL_TAG: u8 = 0;
    const VALUE_TAG: u8 = 1;

    /// A signed number: its sign bit flipped.
    fn signed(value: i64) -> Self {
        Self::Number(value.cast_unsigned() ^ (1 << 63))
    }

    /// A float: its bits, flipped so that they order in IEEE 754's total
    /// order.
    fn float(value: f64) -> Self {
        let bits = value.to_bits();
        Self::Number(if bits >> 63 == 1 {
            !bits
        } else {
            bits | (1 << 63)
        })
    }

    /// The number, of a column of numbers.
    pub(crate) fn number(&self) -> u64 {
        match self {
            Self::Number(number) => *number,
            Self::Bytes(_) => unreachable!("a column of numbers holds numbers"),
        }
    }

    /// The bytes, of a column of text or byte strings.
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Number(_) => unreachable!("a column of byte strings holds byte strings"),
        }
    }

    /// Appends the value's part to a key of several columns: its tag, and
    /// its bytes.
    pub(crate) fn write(&self, key: &mut Vec<u8>) {
        key.push(Self::VALUE_TAG);
        match self {
            Self::Number(number) => key.extend_from_slice(&number.to_be_bytes()),
            Self::Bytes(bytes) => {
                for chunk in bytes.split_inclusive(|&byte| byte == 0) {
                    key.extend_from_slice(chunk);
                    if chunk.last() == Some(&0) {
                        key.push(0xFF);
                    }
                }
                key.extend_from_slice(&[0, 0]);
            }
        }
    }

    /// Appends a null's part to a key of several columns.
    pub(crate) fn write_null(key: &mut Vec<u8>) {
        key.push(Self::NULL_TAG);
    }

    /// The value's part of a key of several columns.
    pub(crate) fn encoded(&self) -> Vec<u8> {
        let mut part = Vec::new();
        self.write(&mut part);
        part
    }
}

/// A column of values to key rows by, cast to its kind's type
/// ([`KeyType::cast_type`]).
pub(crate) enum KeyColumn<'a> {
    Signed(&'a Int64Array),
    Unsigned(&'a UInt64Array),
    Float(&'a Float64Array),
    Bytes(&'a BinaryViewArray),
}

impl<'a> KeyColumn<'a> {
    /// `values`, cast to `key_type`'s type.
    pub(crate) fn new(key_type: KeyType, values: &'a ArrayRef) -> Self {
        match key_type {
            KeyType::UInt => Self::Unsigned(values.as_primitive::<UInt64Type>()),
            KeyType::Float => Self::Float(values.as_primitive::<Float64Type>()),
            KeyType::Bytes => Self::Bytes(values.as_binary_view()),
            KeyType::Int
            | KeyType::Date32
            | KeyType::Date64
            | KeyType::Time(_)
            | KeyType::Timestamp(_) => Self::Signed(values.as_primitive::<Int64Type>()),
        }
    }

    /// The value at `row`, unless it is null.
    pub(crate) fn part(&self, row: usize) -> Option<KeyPart<'a>> {
        match self {
            Self::Signed(values) => {
                (values.is_valid(row)).then(|| KeyPart::signed(values.value(row)))
            }
            Self::Unsigned(values) => {
                (values.is_valid(row)).then(|| KeyPart::Number(values.value(row)))
            }
            Self::Float(values) => {
                (values.is_valid(row)).then(|| KeyPart::float(values.value(row)))
            }
            Self::Bytes(values) => {
                (values.is_valid(row)).then(|| KeyPart::Bytes(Cow::Borrowed(values.value(row))))
            }
        }
    }
}

/// A range of a column's values, from its start bound to its end bound.
type ValueRange<'a> = (Bound<KeyPart<'a>>, Bound<KeyPart<'a>>);

/// The values a column's value lies among: ranges of them, which may
/// overlap; no values, if no range.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ValueRanges<'a> {
    ranges: Vec<ValueRange<'a>>,
}

impl<'a> ValueRanges<'a> {
    /// No values.
    pub(crate) fn none() -> Self {
        Self { ranges: Vec::new() }
    }

    /// The values that compare to `value` as `op` says: one of `=`, `<`,
    /// `<=`, `>` and `>=`.
    pub(crate) fn compared(op: Operator, value: KeyPart<'a>) -> Option<Self> {
        use Bound::{Excluded, Included, Unbounded};
        let range = match op {
            Operator::Eq => (Included(value.clone()), Included(value)),
            Operator::Lt => (Unbounded, Excluded(value)),
            Operator::LtEq => (Unbounded, Included(value)),
            Operator::Gt => (Excluded(value), Unbounded),
            Operator::GtEq => (Included(value), Unbounded),
            _ => return None,
        };
        Some(Self {
            ranges: vec![range],
        })
    }

    /// The values among either's.
    pub(crate) fn union(mut self, other: Self) -> Self {
        self.ranges.extend(other.ranges);
        self
    }

    /// The values among both's.
    pub(crate) fn intersect(&self, other: &Self) -> Self {
        let ranges = (self.ranges.iter())
            .flat_map(|a| (other.ranges.iter()).map(move |b| Self::intersect_ranges(a, b)))
            .filter(|range| !Self::is_empty(range))
            .collect();
        Self { ranges }
    }

    fn intersect_ranges(a: &ValueRange<'a>, b: &ValueRange<'a>) -> ValueRange<'a> {
        use Bound::{Excluded, Included, Unbounded};
        let start = match (&a.0, &b.0) {
            (Unbounded, start) | (start, Unbounded) => start.clone(),
            (Included(x), Included(y)) => Included(x.max(y).clone()),
            (Excluded(x), Excluded(y)) => Excluded(x.max(y).clone()),
            (Included(included), Excluded(excluded)) | (Excluded(excluded), Included(included)) => {
                if excluded >= included {
                    Excluded(excluded.clone())
                } else {
                    Included(included.clone())
                }
            }
        };
        let end = match (&a.1, &b.1) {
            (Unbounded, end) | (end, Unbounded) => end.clone(),
            (Included(x), Included(y)) => Included(x.min(y).clone()),
            (Excluded(x), Excluded(y)) => Excluded(x.min(y).clone()),
            (Included(included), Excluded(excluded)) | (Excluded(excluded), Included(included)) => {
                if excluded <= included {
                    Excluded(excluded.clone())
                } else {
                    Included(included.clone())
                }
            }
        };
        (start, end)
    }

    fn is_empty(range: &ValueRange<'_>) -> bool {
        use Bound::{Excluded, Included};
        match range {
            (Included(start), Included(end)) => start > end,
            (Included(start) | Excluded(start), Excluded(end))
            | (Excluded(start), Included(end)) => start >= end,
            _ => false,
        }
    }

    /// The values, if each range holds one.
    pub(crate) fn values(&self) -> Option<Vec<&KeyPart<'a>>> {
        (self.ranges.iter())
            .map(|range| match range {
                (Bound::Included(start), Bound::Included(end)) if start == end => Some(start),
                _ => None,
            })
            .collect()
    }
}

/// A range of `u64` keys, from its start bound to its end bound.
type NumberRange = (Bound<u64>, Bound<u64>);

/// A range of byte-string keys, from its start bound to its end bound.
type BytesRange = (Bound<Vec<u8>>, Bound<Vec<u8>>);

/// Ranges of an index's keys, in the type of its keys ([`KeyForm`]).
#[derive(Debug, PartialEq)]
pub(crate) enum KeyRanges {
    Numbers(Vec<NumberRange>),
    Bytes(Vec<BytesRange>),
}

impl KeyRanges {
    /// The keys of an index of one column, of form `form`, whose values
    /// lie in `ranges`.
    pub(crate) fn of_column(form: KeyForm, ranges: &ValueRanges<'_>) -> Self {
        let ranges = ranges.ranges.iter();
        match form {
            KeyForm::Number => {
                let number = |bound: &Bound<KeyPart<'_>>| bound.as_ref().map(KeyPart::number);
                Self::Numbers(
                    ranges
                        .map(|(start, end)| (number(start), number(end)))
                        .collect(),
                )
            }
            KeyForm::Bytes | KeyForm::Parts => {
                let bytes =
                    |bound: &Bound<KeyPart<'_>>| bound.as_ref().map(|part| part.bytes().to_vec());
                Self::Bytes(
                    ranges
                        .map(|(start, end)| (bytes(start), bytes(end)))
                        .collect(),
                )
            }
        }
    }

    /// The keys of an index of several columns that start with one of
    /// `prefixes`, the parts of its first columns, and whose next column's
    /// value lies in `next`, or anywhere, if `None`.
    pub(crate) fn of_parts(prefixes: &[Vec<u8>], next: Option<&ValueRanges<'_>>) -> Self {
        use Bound::{Excluded, Included, Unbounded};
        let Some(next) = next else {
            let starting_with =
                |prefix: &Vec<u8>| (Included(prefix.clone()), Excluded(Self::after(prefix)));
            return Self::Bytes(prefixes.iter().map(starting_with).collect());
        };
        let mut ranges = Vec::with_capacity(prefixes.len() * next.ranges.len());
        for prefix in prefixes {
            let key = |part: &[u8]| [prefix, part].concat();
            for (start, end) in &next.ranges {
                // A key lies in the range exactly when its part does: no
                // part's bytes start another's. Nulls, tagged 0, lie before
                // every value.
                let start = match start {
                    Included(value) => Included(key(&value.encoded())),
                    Excluded(value) => Included(key(&Self::after(&value.encoded()))),
                    Unbounded => Included(key(&[KeyPart::VALUE_TAG])),
                };
                let end = match end {
                    Included(value) => Excluded(key(&Self::after(&value.encoded()))),
                    Excluded(value) => Excluded(key(&value.encoded())),
                    Unbounded => Excluded(key(&[KeyPart::VALUE_TAG + 1])),
                };
                ranges.push((start, end));
            }
        }
        Self::Bytes(ranges)
    }

    /// The least byte string after every one that starts with `prefix`,
    /// which holds a byte below `0xFF`, as every part's tag is.
    fn after(prefix: &[u8]) -> Vec<u8> {
        let last = prefix.iter().rposition(|&byte| byte < 0xFF).unwrap();
        let mut after = prefix[..=last].to_vec();
        after[last] += 1;
        after
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeBounds;

    use super::*;

    fn text(bytes: &[u8]) -> KeyPart<'_> {
        KeyPart::Bytes(Cow::Borrowed(bytes))
    }

    /// The key of several columns of `parts`, `None` a null.
    fn key(parts: &[Option<KeyPart<'_>>]) -> Vec<u8> {
        let mut key = Vec::new();
        for part in parts {
            match part {
                Some(part) => part.write(&mut key),
                None => KeyPart::write_null(&mut key),
            }
        }
        key
    }

    fn holds(range: &BytesRange, key: &[u8]) -> bool {
        let bounds = (
            range.0.as_ref().map(Vec::as_slice),
            range.1.as_ref().map(Vec::as_slice),
        );
        RangeBounds::<[u8]>::contains(&bounds, key)
    }

    #[test]
    fn keys_of_several_columns_order_as_their_values() {
        let numbers = [i64::MIN, -300, -1, 0, 1, 255, 256, i64::MAX].map(KeyPart::signed);
        let floats = [
            f64::NEG_INFINITY,
            -1.5,
            -0.0,
            0.0,
            f64::MIN_POSITIVE,
            2.5,
            f64::INFINITY,
            f64::NAN,
        ]
        .map(KeyPart::float);
        let texts: [&[u8]; 6] = [b"", b"\0", b"\0\0", b"\0\x01", b"a", b"a\0b"];
        let texts = texts.map(text);
        for parts in [&numbers[..], &floats, &texts] {
            assert!(
                parts.windows(2).all(|pair| pair[0] < pair[1]),
                "values order"
            );
            // Keys order by their first column, then their second, a null
            // before any value.
            let mut keys = Vec::new();
            for first in parts {
                keys.push(key(&[Some(first.clone()), None]));
                for second in parts {
                    keys.push(key(&[Some(first.clone()), Some(second.clone())]));
                }
            }
            assert!(keys.windows(2).all(|pair| pair[0] < pair[1]), "{keys:?}");
        }
    }

    #[test]
    fn ranges_of_parts_hold_the_keys_whose_values_they_hold() {
        let seconds = [None, Some(text(b"x")), Some(text(b"x\0"))];
        let mut keys: Vec<(i64, Vec<u8>)> = Vec::new();
        for value in -5..=5 {
            for second in &seconds {
                let parts = [
                    Some(text(b"p")),
                    Some(KeyPart::signed(value)),
                    second.clone(),
                ];
                keys.push((value, key(&parts)));
            }
        }
        // A key of another first value lies in no range.
        let other = key(&[Some(text(b"q")), Some(KeyPart::signed(0)), None]);
        let prefix = text(b"p").encoded();
        let ops = [
            Operator::Eq,
            Operator::Lt,
            Operator::LtEq,
            Operator::Gt,
            Operator::GtEq,
        ];
        for op in ops {
            for literal in [-5, -1, 0, 3, 5] {
                let values = ValueRanges::compared(op, KeyPart::signed(literal)).unwrap();
                let KeyRanges::Bytes(ranges) =
                    KeyRanges::of_parts(std::slice::from_ref(&prefix), Some(&values))
                else {
                    unreachable!("keys of parts are bytes")
                };
                for (value, key) in &keys {
                    let compares = match op {
                        Operator::Eq => value == &literal,
                        Operator::Lt => value < &literal,
                        Operator::LtEq => value <= &literal,
                        Operator::Gt => value > &literal,
                        _ => value >= &literal,
                    };
                    assert_eq!(holds(&ranges[0], key), compares, "{op} {literal} {value}");
                }
                assert!(!holds(&ranges[0], &other));
            }
        }
        let KeyRanges::Bytes(all) = KeyRanges::of_parts(&[prefix], None) else {
            unreachable!("keys of parts are bytes")
        };
        assert!(keys.iter().all(|(_, key)| holds(&all[0], key)));
        assert!(!holds(&all[0], &other));
    }

    #[test]
    fn value_ranges_intersect_and_find_single_values() {
        let compared = |op, value| ValueRanges::compared(op, KeyPart::signed(value)).unwrap();
        let points = compared(Operator::Eq, 1).union(compared(Operator::Eq, 3));
        assert_eq!(points.values().unwrap().len(), 2);
        // `>= 1 AND <= 1` is the value 1.
        let at_least_one = compared(Operator::GtEq, 1);
        let one = at_least_one.intersect(&compared(Operator::LtEq, 1));
        assert_eq!(one, compared(Operator::Eq, 1));
        assert!(at_least_one.values().is_none());
        // `> 1 AND < 1` holds nothing, as `= 1 AND = 3` doesn't.
        let none = compared(Operator::Gt, 1).intersect(&compared(Operator::Lt, 1));
        assert_eq!(none, ValueRanges::none());
        let none = compared(Operator::Eq, 1).intersect(&compared(Operator::Eq, 3));
        assert_eq!(none, ValueRanges::none());
        // A column of numbers keys them as numbers.
        let ranges = KeyRanges::of_column(KeyForm::Number, &compared(Operator::Gt, -1));
        let minus_one = (1 << 63) - 1;
        assert_eq!(
            ranges,
            KeyRanges::Numbers(vec![(Bound::Excluded(minus_one), Bound::Unbounded)])
        );
    }
}
