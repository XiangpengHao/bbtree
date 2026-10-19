//! Keys and values of any length, kept in the leaves' tails: a reader finds
//! what the entries held, by every way it looks, whichever of keys and
//! values are bytes; queries reuse their buffer; and a file map of var-length keys
//! is only ever read with inline keys.

use core::ops::Bound;
use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use crate::file::{FileError, FileMap, FileWriter, VarBytes, VarBytesBuf};
use crate::inline_key::{Auto, InlineKey, WithInlineKey};
use crate::tree::test_support::TestRng;

/// A random byte string of at most `max_len` bytes, over an alphabet with
/// a zero byte: strings share prefixes, end within each other's inline
/// keys, and differ past them.
fn random_bytes(rng: &TestRng, max_len: usize) -> Vec<u8> {
    let len = rng.below(max_len + 1);
    (0..len)
        .map(|_| [0, 1, b'a', b'b', 0xff][rng.below(5)])
        .collect()
}

/// `count` distinct random keys, each with a random value.
fn random_entries(rng: &TestRng, count: usize) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut entries = BTreeMap::new();
    while entries.len() < count {
        entries.insert(random_bytes(rng, 24), random_bytes(rng, 40));
    }
    entries
}

/// Random bounds in order over random byte strings, as `std`'s ranges
/// take them.
fn random_bounds(rng: &TestRng) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
    let (mut low, mut high) = (random_bytes(rng, 8), random_bytes(rng, 8));
    if low > high {
        core::mem::swap(&mut low, &mut high);
    }
    let bound = |bytes: Vec<u8>| match rng.below(3) {
        0 => Bound::Unbounded,
        1 => Bound::Included(bytes),
        _ => Bound::Excluded(bytes),
    };
    let (start, end) = (bound(low), bound(high));
    match (start, end) {
        (Bound::Excluded(start), Bound::Excluded(end)) if start == end => {
            (Bound::Included(start), Bound::Excluded(end))
        }
        bounds => bounds,
    }
}

/// Writes the file map of `count` random entries of byte keys and values,
/// and checks that a reader finds them: every key, and absent ones,
/// iteration both ways, ranges, both ends, checked and unchecked.
fn round_trip(count: usize, seed: u64) {
    let rng = TestRng::new(seed);
    let entries = random_entries(&rng, count);
    let writer = FileWriter::<VarBytes, VarBytes, WithInlineKey>::from_ascending(
        (entries.iter()).map(|(key, value)| (key.as_slice().into(), value.as_slice().into())),
    )
    .unwrap();
    assert_eq!(writer.len(), count);
    let buffer = writer.to_buf();
    let mut written = Vec::new();
    writer
        .write(|bytes| {
            written.extend_from_slice(bytes);
            Ok::<(), ()>(())
        })
        .unwrap();
    assert_eq!(written, &buffer[..], "writing and buffering agree");
    drop(writer);

    let file_map = FileMap::<VarBytes, VarBytes, WithInlineKey>::open(&buffer).unwrap();
    assert_eq!(file_map.len(), count);
    let mut query = VarBytesBuf::new(b"");
    for (key, value) in &entries {
        query.set(key);
        let found = file_map
            .get_key_value(&*query)
            .map(|(k, v)| (k.bytes(), v.bytes()));
        assert_eq!(found, Some((key.as_slice(), value.as_slice())), "{key:?}");
    }
    for _ in 0..2 * count + 10 {
        let key = random_bytes(&rng, 26);
        query.set(&key);
        let found = file_map.get(&*query).map(VarBytes::bytes);
        assert_eq!(found, entries.get(&key).map(Vec::as_slice), "{key:?}");
    }
    // Every key and as many random ones in batches, also through lanes at
    // any size: `usize::MAX` passes the batch threshold.
    let probes: Vec<Vec<u8>> = (entries.keys())
        .flat_map(|key| [key.clone(), random_bytes(&rng, 26)])
        .collect();
    let mut queries: [VarBytesBuf; 8] = core::array::from_fn(|_| VarBytesBuf::empty());
    for chunk in probes.chunks(8) {
        for (lane, query) in queries.iter_mut().enumerate() {
            query.set(&chunk[lane % chunk.len()]);
        }
        let batch = queries.each_ref().map(|query| &**query);
        let expected = batch.map(|key| entries.get(key.bytes()).map(Vec::as_slice));
        let found = file_map
            .get_many(batch)
            .map(|value| value.map(VarBytes::bytes));
        assert_eq!(found, expected);
        let found =
            (file_map.tree.find_many(batch, usize::MAX)).map(|value| value.map(VarBytes::bytes));
        assert_eq!(found, expected);
    }
    let as_bytes =
        |(key, value): (&VarBytes, &VarBytes)| (key.bytes().to_vec(), value.bytes().to_vec());
    let expected = |(key, value): (&Vec<u8>, &Vec<u8>)| (key.clone(), value.clone());
    assert!(
        file_map
            .iter()
            .map(as_bytes)
            .eq(entries.iter().map(expected))
    );
    assert!((file_map.iter().rev().map(as_bytes)).eq(entries.iter().rev().map(expected)));
    assert_eq!(
        file_map.first_key_value().map(as_bytes),
        entries.first_key_value().map(expected)
    );
    assert_eq!(
        file_map.last_key_value().map(as_bytes),
        entries.last_key_value().map(expected)
    );
    for _ in 0..40 {
        let (start, end) = random_bounds(&rng);
        let to_query = |bound: &Bound<Vec<u8>>| bound.as_ref().map(|bytes| VarBytesBuf::new(bytes));
        let (start_query, end_query) = (to_query(&start), to_query(&end));
        let file_map_bounds = (
            start_query.as_ref().map(|query| &**query),
            end_query.as_ref().map(|query| &**query),
        );
        let std_bounds = (
            start.as_ref().map(Vec::as_slice),
            end.as_ref().map(Vec::as_slice),
        );
        let found: Vec<_> = file_map
            .range::<VarBytes, _>(file_map_bounds)
            .map(as_bytes)
            .collect();
        let wanted: Vec<_> = entries.range::<[u8], _>(std_bounds).map(expected).collect();
        assert_eq!(found, wanted, "{start:?}..{end:?}");
    }

    // SAFETY: the buffer holds what the writer wrote.
    let unchecked =
        unsafe { FileMap::<VarBytes, VarBytes, WithInlineKey>::open_unchecked(&buffer) }.unwrap();
    assert!(
        unchecked
            .iter()
            .map(as_bytes)
            .eq(entries.iter().map(expected))
    );
}

#[test]
fn byte_keys_and_values_round_trip() {
    for (seed, count) in [0, 1, 2, 12, 13, 100, 1000, 20_000].into_iter().enumerate() {
        round_trip(count, seed as u64);
    }
}

/// A small tree of byte keys and values, of a few levels, for Miri: it
/// reaches every unsafe step of keeping bytes in the tail and reading
/// them.
#[test]
fn a_small_var_bytes_file_map_round_trips() {
    for (seed, count) in [0, 1, 40, 300].into_iter().enumerate() {
        round_trip(count, seed as u64 + 100);
    }
}

/// Byte keys with plain values, and plain keys with byte values.
#[test]
fn var_bytes_and_plain_slots_mix() {
    let rng = TestRng::new(7);
    let entries = random_entries(&rng, 3000);
    let writer = FileWriter::<VarBytes, u64, WithInlineKey>::from_ascending(
        (entries.iter()).map(|(key, value)| (key.as_slice().into(), value.len() as u64)),
    )
    .unwrap();
    let buffer = writer.to_buf();
    let file_map = FileMap::<VarBytes, u64, WithInlineKey>::open(&buffer).unwrap();
    let found = file_map
        .iter()
        .map(|(key, &len)| (key.bytes().to_vec(), len));
    assert!(
        found.eq(entries
            .iter()
            .map(|(key, value)| (key.clone(), value.len() as u64)))
    );

    let writer = FileWriter::<u64, VarBytes>::from_ascending(
        (0..3000_u64).map(|key| (key * 3, vec![key as u8; key as usize % 50].into())),
    )
    .unwrap();
    let buffer = writer.to_buf();
    let file_map = FileMap::<u64, VarBytes>::open(&buffer).unwrap();
    for key in 0..9000_u64 {
        let expected = (key % 3 == 0).then(|| vec![(key / 3) as u8; (key / 3) as usize % 50]);
        assert_eq!(
            file_map.get(&key).map(|value| value.bytes().to_vec()),
            expected
        );
    }
}

/// Values of hundreds of KiB, whose tails are larger than their leaves.
#[test]
fn large_values_round_trip() {
    let rng = TestRng::new(9);
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..60_u32)
        .map(|key| {
            let len = rng.below(if cfg!(miri) { 2000 } else { 300_000 });
            (
                key.to_be_bytes().to_vec(),
                (0..len).map(|byte| byte as u8).collect(),
            )
        })
        .collect();
    let writer = FileWriter::<VarBytes, VarBytes, WithInlineKey>::from_ascending(
        (entries.iter()).map(|(key, value)| (key.as_slice().into(), value.as_slice().into())),
    )
    .unwrap();
    let buffer = writer.to_buf();
    let file_map = FileMap::<VarBytes, VarBytes, WithInlineKey>::open(&buffer).unwrap();
    for (key, value) in &entries {
        let found = file_map.get(&*VarBytesBuf::new(key)).map(VarBytes::bytes);
        assert_eq!(found, Some(value.as_slice()));
    }
}

#[test]
fn var_bytes_writers_refuse_entries_out_of_order() {
    let entries: [(&[u8], &[u8]); 3] = [(b"b", b""), (b"c", b""), (b"bb", b"")];
    let refused = FileWriter::<VarBytes, VarBytes, WithInlineKey>::from_ascending(
        entries.map(|(key, value)| (key.into(), value.into())),
    );
    assert_eq!(refused.err().map(|error| error.index()), Some(2));
}

#[test]
fn query_buffers_hold_any_bytes() {
    let mut query = VarBytesBuf::new(b"a longer key, past one word");
    assert_eq!(query.bytes(), b"a longer key, past one word");
    for bytes in [&b""[..], b"x", b"exactly8", b"nine byte", &[0xff; 100]] {
        query.set(bytes);
        assert_eq!(
            (query.bytes(), query.len(), query.is_empty()),
            (bytes, bytes.len(), bytes.is_empty())
        );
    }
    assert!(VarBytesBuf::new(b"ab").cmp(&VarBytesBuf::new(b"b")).is_lt());
}

/// Without inline keys, internal nodes would keep keys, which no tail
/// backs: writers refuse to write such a file map, and readers to read one.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn var_bytes_keys_need_inline_keys() {
    let writer = std::panic::catch_unwind(|| {
        FileWriter::<VarBytes, u64, Auto>::from_ascending([(b"key".as_slice().into(), 1)])
            .unwrap()
            .to_buf()
    });
    assert!(writer.is_err());
    let file_bytes =
        FileWriter::<VarBytes, u64, WithInlineKey>::from_ascending([(b"key".as_slice().into(), 1)])
            .unwrap()
            .to_buf();
    assert_eq!(
        FileMap::<VarBytes, u64, Auto>::open(&file_bytes).err(),
        Some(FileError::LayoutMismatch)
    );
    assert_eq!(
        FileMap::<Bytes8, u64, WithInlineKey>::open(&file_bytes).err(),
        Some(FileError::LayoutMismatch),
        "plain keys of a span's size and inline keys are told apart from var-length keys"
    );
}

/// A plain key of a span's size and alignment, with a span's inline
/// keys: a file map of it differs from one of var-length keys in what its slots
/// hold alone.
#[repr(C, align(8))]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Bytes8([u8; 8]);

// SAFETY: eight bytes, aligned to 8, with no padding.
unsafe impl crate::file::PlainBytes for Bytes8 {}

impl InlineKey for Bytes8 {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    fn inline_key_at(&self, offset: usize) -> crate::InlineWord {
        self.0.as_slice().inline_key_at(offset)
    }

    fn hash_tag(&self) -> u8 {
        self.0.as_slice().hash_tag()
    }

    fn cmp_from(&self, other: &Self, equal_len: usize) -> core::cmp::Ordering {
        self.0.as_slice().cmp_from(other.0.as_slice(), equal_len)
    }

    fn cmp_prefix(&self, prefix: &[u8]) -> core::cmp::Ordering {
        self.0.as_slice().cmp_prefix(prefix)
    }
}
