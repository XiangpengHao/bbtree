//! File maps through their API: a reader finds what the map held, in every
//! way it can look, for keys with and without inline keys, of every size
//! of tree; a writer refuses entries out of order; and a reader refuses
//! bytes that are not a file map of its layout.

use core::cmp::Ordering;
use core::fmt::Debug;
use core::ops::Bound;
use std::vec::Vec;

use super::{FileBuf, FileError, FileMap, FileWriter, NotAscending, PlainBytes};
use crate::inline_key::{InlineKey, InlineWord, WithInlineKey};
use crate::tree::test_support::TestRng;
use crate::unsync::BBTreeMap;

/// A plain key the tests make from a number, ordered like the numbers.
trait NumberedKey: PlainBytes + Ord + Debug {
    fn of(number: usize) -> Self;
}

impl NumberedKey for u64 {
    fn of(number: usize) -> Self {
        number as u64
    }
}

/// Negatives and positives, around 0.
impl NumberedKey for i64 {
    fn of(number: usize) -> Self {
        number as i64 - 1_000_000
    }
}

/// Keys that share their first eight bytes.
impl NumberedKey for u128 {
    fn of(number: usize) -> Self {
        (7 << 64) | number as u128
    }
}

/// A byte array with inline keys under `Auto`, whose keys differ only in
/// their last bytes.
impl NumberedKey for [u8; 32] {
    fn of(number: usize) -> Self {
        let mut key = [b'k'; 32];
        key[24..].copy_from_slice(&(number as u64).to_be_bytes());
        key
    }
}

/// A key without inline keys under `Auto`: internal nodes keep keys.
impl NumberedKey for u32 {
    fn of(number: usize) -> Self {
        number as u32
    }
}

/// A key of the tests' own, which has inline keys with `WithInlineKey`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Id([u8; 12]);

// SAFETY: an array of bytes, with nothing else.
unsafe impl PlainBytes for Id {}

impl InlineKey for Id {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.0.as_slice().inline_key_at(offset)
    }

    fn hash_tag(&self) -> u8 {
        self.0.as_slice().hash_tag()
    }

    fn cmp_from(&self, other: &Self, equal_len: usize) -> Ordering {
        self.0.as_slice().cmp_from(other.0.as_slice(), equal_len)
    }

    fn cmp_prefix(&self, prefix: &[u8]) -> Ordering {
        self.0.as_slice().cmp_prefix(prefix)
    }
}

impl NumberedKey for Id {
    fn of(number: usize) -> Self {
        let mut key = *b"user:0000000";
        key[4..].copy_from_slice(&(number as u64).to_be_bytes());
        Self(key)
    }
}

/// Builds a map of `count` keys, even numbers of a space twice as large,
/// inserted in random order so that its nodes are not full, and checks
/// that its file map, read back, holds what it holds: every lookup, hit or
/// miss, both iteration orders, ranges, and both ends.
fn round_trip<K: NumberedKey, S: crate::InlineKeyStrategy<K>>(
    map: BBTreeMap<K, u64, S>,
    count: usize,
    seed: u64,
) where
    for<'i> FileMap<'i, K, u64, S>: Debug,
{
    let rng = TestRng::new(seed);
    let space = 2 * count + 2;
    let mut map = map;
    let mut numbers = rng.distinct_numbers(count, count + 1);
    rng.shuffle(&mut numbers);
    for number in numbers {
        map.insert(K::of(2 * number), number as u64);
    }

    let writer = FileWriter::from(&map);
    assert_eq!(writer.len(), count);
    let buffer = writer.to_buf();
    assert_eq!(buffer.len(), writer.byte_len());
    let mut written = Vec::new();
    writer
        .write(|bytes| {
            written.extend_from_slice(bytes);
            Ok::<(), ()>(())
        })
        .unwrap();
    assert_eq!(written, &buffer[..], "writing and buffering agree");

    let file_map = FileMap::<K, u64, S>::open(&buffer).unwrap();
    assert_eq!(file_map.len(), map.len());
    assert_eq!(file_map.is_empty(), map.is_empty());
    for number in 0..space {
        let key = K::of(number);
        assert_eq!(
            file_map.get_key_value(&key),
            map.get_key_value(&key),
            "{key:?}"
        );
        assert_eq!(file_map.contains_key(&key), map.contains_key(&key));
    }
    // Every key in batches, also through lanes at any size: `usize::MAX`
    // passes the batch threshold.
    let keys: Vec<K> = (0..space).map(K::of).collect();
    for chunk in keys.chunks(8) {
        let batch: [&K; 8] = core::array::from_fn(|lane| &chunk[lane % chunk.len()]);
        let expected = batch.map(|key| map.get(key));
        assert_eq!(file_map.get_many(batch), expected);
        assert_eq!(file_map.tree.find_many(batch, usize::MAX), expected);
    }
    assert!(file_map.iter().eq(map.iter()));
    assert!(file_map.iter().rev().eq(map.iter().rev()));
    assert_eq!(file_map.iter().len(), map.len());
    assert!(file_map.keys().eq(map.keys()));
    assert!(file_map.values().eq(map.values()));
    assert_eq!(file_map.first_key_value(), map.first_key_value());
    assert_eq!(file_map.last_key_value(), map.last_key_value());
    for _ in 0..if cfg!(miri) { 8 } else { 40 } {
        let (start, end) = rng.range_bounds(space, K::of);
        let bounds = (start.as_ref(), end.as_ref());
        assert!(file_map.range(bounds).eq(map.range(bounds)));
        assert!(file_map.range(bounds).rev().eq(map.range(bounds).rev()));
    }
    let unbounded: (Bound<&K>, Bound<&K>) = (Bound::Unbounded, Bound::Unbounded);
    assert!(file_map.range(unbounded).eq(map.iter()));

    // SAFETY: the buffer holds what the writer wrote.
    let unchecked = unsafe { FileMap::<K, u64, S>::open_unchecked(&buffer) }.unwrap();
    assert!(unchecked.iter().eq(map.iter()));
    assert_eq!(
        std::format!("{unchecked:?}"),
        std::format!("{:?}", map),
        "a reader prints as the map"
    );
}

/// Every size from one node to several levels, around node boundaries.
const COUNTS: [usize; 9] = [0, 1, 2, 11, 12, 13, 100, 1000, 20_000];

#[test]
fn u64_keys_round_trip() {
    for (seed, count) in COUNTS.into_iter().enumerate() {
        round_trip::<u64, _>(BBTreeMap::new(), count, seed as u64);
    }
}

#[test]
fn i64_keys_round_trip() {
    for (seed, count) in COUNTS.into_iter().enumerate() {
        round_trip::<i64, _>(BBTreeMap::new(), count, seed as u64);
    }
}

#[test]
fn u128_keys_round_trip() {
    for (seed, count) in COUNTS.into_iter().enumerate() {
        round_trip::<u128, _>(BBTreeMap::new(), count, seed as u64);
    }
}

#[test]
fn byte_array_keys_round_trip() {
    for (seed, count) in COUNTS.into_iter().enumerate() {
        round_trip::<[u8; 32], _>(BBTreeMap::new(), count, seed as u64);
    }
}

#[test]
fn keys_without_inline_keys_round_trip() {
    for (seed, count) in COUNTS.into_iter().enumerate() {
        round_trip::<u32, _>(BBTreeMap::new(), count, seed as u64);
    }
}

#[test]
fn keys_with_their_own_inline_keys_round_trip() {
    for (seed, count) in COUNTS.into_iter().enumerate() {
        round_trip::<Id, _>(BBTreeMap::with_strategy(WithInlineKey), count, seed as u64);
    }
}

/// Keys alone, with values that take no room, as an index of rows by
/// value keeps them: keys side by side in the leaves.
#[test]
fn keys_without_values_round_trip() {
    for (seed, count) in COUNTS.into_iter().enumerate() {
        let rng = TestRng::new(seed as u64);
        let mut numbers: Vec<u128> = (0..count as u128).map(|number| number << 64 | 3).collect();
        rng.shuffle(&mut numbers);
        let mut map = BBTreeMap::new();
        for &key in &numbers {
            map.insert(key, ());
        }
        let file_bytes = FileWriter::from(&map).to_buf();
        let read = FileMap::<u128, ()>::open(&file_bytes).unwrap();
        assert!(read.iter().eq(map.iter()));
        for number in 0..count as u128 + 1 {
            for key in [number << 64 | 3, number << 64] {
                assert_eq!(read.contains_key(&key), map.contains_key(&key));
            }
        }
    }
}

/// A small tree, of a few levels of large keys, for Miri: it reaches
/// every unsafe step of writing and reading a file map.
#[test]
fn a_small_file_map_round_trips() {
    let most = if cfg!(miri) { 30 } else { 120 };
    for (seed, count) in [0, 1, 13, most].into_iter().enumerate() {
        round_trip::<[u8; 32], _>(BBTreeMap::new(), count, seed as u64);
    }
}

/// The same entries make the same file map, however the map got them: the
/// file map is compact, and holds no trace of the map's addresses.
#[test]
fn file_maps_depend_on_the_entries_alone() {
    let rng = TestRng::new(5);
    let mut numbers: Vec<u64> = (0..5000).collect();
    let in_order: BBTreeMap<u64, u64> = numbers.iter().map(|&key| (key, !key)).collect();
    rng.shuffle(&mut numbers);
    let mut shuffled = BBTreeMap::new();
    let mut shared = crate::BBTreeMap::new();
    for &key in &numbers {
        shuffled.insert(key, !key);
        shared.insert(key, !key).unwrap();
    }
    let ascending =
        FileWriter::<u64, u64>::from_ascending((0..5000).map(|key| (key, !key))).unwrap();
    let file_bytes = ascending.to_buf();
    assert_eq!(&FileWriter::from(&in_order).to_buf()[..], &file_bytes[..]);
    assert_eq!(&FileWriter::from(&shuffled).to_buf()[..], &file_bytes[..]);
    assert_eq!(&FileWriter::from(&mut shared).to_buf()[..], &file_bytes[..]);
}

/// A compact file map of `u64` keys and values takes little more than their
/// 16 bytes per entry: full leaves, and few internal nodes.
#[test]
fn file_maps_are_compact() {
    let count = 100_000;
    let writer = FileWriter::<u64, u64>::from_ascending((0..count).map(|key| (key, key))).unwrap();
    let bytes_per_entry = writer.byte_len() as f64 / count as f64;
    assert!(bytes_per_entry < 19.0, "{bytes_per_entry} bytes per entry");
}

#[test]
fn writers_refuse_entries_out_of_order() {
    let refused = FileWriter::<u64, u64>::from_ascending([(1, 0), (2, 0), (2, 0), (3, 0)]);
    assert_eq!(refused.err(), Some(NotAscending { index: 2 }));
    let refused = FileWriter::<u64, u64>::from_ascending([(5, 0), (4, 0)]);
    assert_eq!(refused.err().map(|error| error.index()), Some(1));
}

/// A file map of 5000 `u64` entries, which takes three levels.
fn deep_file_map() -> FileBuf {
    FileWriter::<u64, u64>::from_ascending((0..5000).map(|key| (key, key)))
        .unwrap()
        .to_buf()
}

#[test]
fn readers_refuse_what_is_not_a_file_map_of_their_layout() {
    let file_bytes = deep_file_map();
    assert_eq!(
        FileMap::<u128, u64>::open(&file_bytes).err(),
        Some(FileError::LayoutMismatch)
    );
    assert_eq!(
        FileMap::<u64, u32>::open(&file_bytes).err(),
        Some(FileError::LayoutMismatch)
    );
    assert_eq!(
        FileMap::<u64, u64>::open(&file_bytes[..10]).err(),
        Some(FileError::NotAFileMap)
    );
    assert_eq!(
        FileMap::<u64, u64>::open(&file_bytes[..file_bytes.len() - 1]).err(),
        Some(FileError::Truncated {
            byte_len: file_bytes.len(),
            available: file_bytes.len() - 1
        })
    );

    let mut not_a_file_map = FileBuf::zeroed(file_bytes.len(), 4096);
    not_a_file_map.copy_from_slice(&file_bytes);
    not_a_file_map[0] ^= 1;
    assert_eq!(
        FileMap::<u64, u64>::open(&not_a_file_map).err(),
        Some(FileError::NotAFileMap)
    );
    not_a_file_map.copy_from_slice(&file_bytes);
    not_a_file_map[8] = 2;
    assert_eq!(
        FileMap::<u64, u64>::open(&not_a_file_map).err(),
        Some(FileError::UnsupportedVersion(2))
    );

    let alignment = FileMap::<u64, u64>::alignment();
    let mut shifted = FileBuf::zeroed(file_bytes.len() + 8, 4096);
    shifted[8..].copy_from_slice(&file_bytes);
    assert_eq!(
        FileMap::<u64, u64>::open(&shifted[8..]).err(),
        Some(FileError::Misaligned { alignment })
    );
}

#[test]
fn file_bufs_are_aligned_and_zeroed() {
    for (len, alignment) in [(0, 1024), (1, 4096), (5000, 2048)] {
        let buffer = FileBuf::zeroed(len, alignment);
        assert_eq!(buffer.len(), len);
        assert!(buffer.as_ptr().addr().is_multiple_of(alignment));
        assert!(buffer.iter().all(|&byte| byte == 0));
    }
}
