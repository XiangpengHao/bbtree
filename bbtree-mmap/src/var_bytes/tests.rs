//! Keys and values of any length, kept in heap pages after their leaves: a
//! reader finds what the entries held, by every way it looks, whichever of
//! keys and values are bytes; records stay in their pages; and damaged
//! spans are refused, or read in bounds.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ops::Bound;
use std::panic::{AssertUnwindSafe, catch_unwind};

use bbtree::file::{FileBuf, FileError, FileMap, FileWriter, PlainBytes, Storable};
use bbtree::{InlineKey, InlineWord, WithInlineKey};

use super::{ByteArena, VarBytes, VarBytesQuery};
use crate::disk_type::DiskStored;
use crate::disk_type::storage::DiskStorage;
use crate::heap_pages::HeapPages;

/// A deterministic xorshift generator.
struct Rng(Cell<u64>);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(Cell::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1))
    }

    fn below(&self, bound: usize) -> usize {
        let mut state = self.0.get();
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.0.set(state);
        (state % bound as u64) as usize
    }
}

/// A random byte string of at most `max_len` bytes, over an alphabet with
/// a zero byte: strings share prefixes, end within each other's inline
/// keys, and differ past them.
fn random_bytes(rng: &Rng, max_len: usize) -> Vec<u8> {
    let len = rng.below(max_len + 1);
    (0..len)
        .map(|_| [0, 1, b'a', b'b', 0xff][rng.below(5)])
        .collect()
}

/// `count` distinct random keys, each with a random value.
fn random_entries(rng: &Rng, count: usize) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut entries = BTreeMap::new();
    while entries.len() < count {
        entries.insert(random_bytes(rng, 24), random_bytes(rng, 40));
    }
    entries
}

/// Random bounds in order over random byte strings, as std's ranges take
/// them.
fn random_bounds(rng: &Rng) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
    let (mut low, mut high) = (random_bytes(rng, 8), random_bytes(rng, 8));
    if low > high {
        std::mem::swap(&mut low, &mut high);
    }
    let bound = |bytes: Vec<u8>| match rng.below(3) {
        0 => Bound::Unbounded,
        1 => Bound::Included(bytes),
        _ => Bound::Excluded(bytes),
    };
    match (bound(low), bound(high)) {
        (Bound::Excluded(start), Bound::Excluded(end)) if start == end => {
            (Bound::Included(start), Bound::Excluded(end))
        }
        bounds => bounds,
    }
}

/// The file map of `entries`, written as a `BBTreeMapFile`'s writer writes
/// it: keys and values with bytes in heap pages after their leaves.
fn file_map_of<'e, K, V>(
    entries: impl IntoIterator<Item = (K::Input<'e>, V::Input<'e>)>,
) -> Result<FileBuf, bbtree::file::NotAscending>
where
    K: DiskStored<InWriter: Ord>,
    V: DiskStored,
    WithInlineKey: bbtree::InlineKeyStrategy<K> + bbtree::InlineKeyStrategy<K::InWriter>,
{
    let mut arena = ByteArena::default();
    let entries = (entries.into_iter()).map(|(key, value)| {
        (
            K::in_writer(key, &mut arena),
            V::in_writer(value, &mut arena),
        )
    });
    let writer = FileWriter::<K, V, WithInlineKey, HeapPages>::from_ascending(entries)?;
    let buffer = writer.to_buf();
    let mut written = Vec::new();
    writer
        .write(|bytes| {
            written.extend_from_slice(bytes);
            Ok::<(), ()>(())
        })
        .unwrap();
    assert_eq!(written, &buffer[..], "writing and buffering agree");
    Ok(buffer)
}

type VarBytesMap<'i> = FileMap<'i, VarBytes, VarBytes, WithInlineKey>;

/// Writes the file map of `count` random entries of byte keys and values,
/// and checks that a reader finds them: every key, and absent ones,
/// batches, iteration both ways, ranges, both ends, checked and unchecked.
fn round_trip(count: usize, seed: u64) {
    let rng = Rng::new(seed);
    let entries = random_entries(&rng, count);
    let buffer = file_map_of::<VarBytes, VarBytes>(
        (entries.iter()).map(|(key, value)| (key.as_slice().into(), value.as_slice().into())),
    )
    .unwrap();
    let file_map = VarBytesMap::open(&buffer).unwrap();
    assert_eq!(file_map.len(), count);
    for (key, value) in &entries {
        let found = (file_map.get_key_value(&VarBytesQuery::new(key)))
            .map(|(key, value)| (key.bytes(), value.bytes()));
        assert_eq!(found, Some((key.as_slice(), value.as_slice())), "{key:?}");
    }
    for _ in 0..2 * count + 10 {
        let key = random_bytes(&rng, 26);
        let found = file_map.get(&VarBytesQuery::new(&key)).map(VarBytes::bytes);
        assert_eq!(found, entries.get(&key).map(Vec::as_slice), "{key:?}");
    }
    let probes: Vec<Vec<u8>> = (entries.keys())
        .flat_map(|key| [key.clone(), random_bytes(&rng, 26)])
        .collect();
    for chunk in probes.chunks(8) {
        let queries: [VarBytesQuery<'_>; 8] =
            std::array::from_fn(|lane| VarBytesQuery::new(&chunk[lane % chunk.len()]));
        let batch = queries.each_ref().map(|query| &**query);
        let expected = batch.map(|key| entries.get(key.bytes()).map(Vec::as_slice));
        let found = (file_map.get_many(batch)).map(|value| value.map(VarBytes::bytes));
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
        let (start_query, end_query) = (
            start.as_ref().map(|bytes| VarBytesQuery::new(bytes)),
            end.as_ref().map(|bytes| VarBytesQuery::new(bytes)),
        );
        let file_map_bounds = (
            start_query.as_ref().map(|query| &**query),
            end_query.as_ref().map(|query| &**query),
        );
        let std_bounds = (
            start.as_ref().map(Vec::as_slice),
            end.as_ref().map(Vec::as_slice),
        );
        let found: Vec<_> = (file_map.range::<VarBytes, _>(file_map_bounds))
            .map(as_bytes)
            .collect();
        let wanted: Vec<_> = entries.range::<[u8], _>(std_bounds).map(expected).collect();
        assert_eq!(found, wanted, "{start:?}..{end:?}");
    }
    // SAFETY: the buffer holds what the writer wrote.
    let unchecked = unsafe { VarBytesMap::open_unchecked(&buffer) }.unwrap();
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
/// reaches every unsafe step of keeping bytes in heap pages and reading
/// them.
#[test]
fn a_small_var_bytes_file_map_round_trips() {
    let counts: &[usize] = if cfg!(miri) {
        &[0, 1, 40]
    } else {
        &[0, 1, 40, 300]
    };
    for (seed, &count) in counts.iter().enumerate() {
        round_trip(count, seed as u64 + 100);
    }
}

/// Byte keys with plain values, and plain keys with byte values.
#[test]
fn var_bytes_and_plain_slots_mix() {
    let rng = Rng::new(7);
    let entries = random_entries(&rng, 3000);
    let buffer = file_map_of::<VarBytes, u64>(
        (entries.iter()).map(|(key, value)| (key.as_slice().into(), value.len() as u64)),
    )
    .unwrap();
    let file_map = FileMap::<VarBytes, u64, WithInlineKey>::open(&buffer).unwrap();
    let found = (file_map.iter()).map(|(key, &len)| (key.bytes().to_vec(), len));
    assert!(found.eq((entries.iter()).map(|(key, value)| (key.clone(), value.len() as u64))));

    let mut arena = ByteArena::default();
    let entries = (0..3000_u64).map(|key| {
        let value = vec![key as u8; key as usize % 50];
        (key * 3, VarBytes::in_writer(value.into(), &mut arena))
    });
    let buffer = FileWriter::<u64, VarBytes, bbtree::Auto, HeapPages>::from_ascending(entries)
        .unwrap()
        .to_buf();
    let file_map = FileMap::<u64, VarBytes>::open(&buffer).unwrap();
    for key in 0..9000_u64 {
        let expected = (key % 3 == 0).then(|| vec![(key / 3) as u8; (key / 3) as usize % 50]);
        assert_eq!(
            file_map.get(&key).map(|value| value.bytes().to_vec()),
            expected
        );
    }
}

/// Values of hundreds of KiB, larger than a heap page: each starts a page,
/// and runs on over the next; every smaller record stays in one page.
#[test]
fn records_stay_in_their_pages_and_large_ones_start_one() {
    let rng = Rng::new(9);
    let count: u32 = if cfg!(miri) { 60 } else { 400 };
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..count)
        .map(|key| {
            let len = match rng.below(4) {
                0 => rng.below(if cfg!(miri) { 2000 } else { 300_000 }),
                _ => rng.below(600),
            };
            (
                key.to_be_bytes().to_vec(),
                (0..len).map(|byte| byte as u8).collect(),
            )
        })
        .collect();
    let buffer = file_map_of::<VarBytes, VarBytes>(
        (entries.iter()).map(|(key, value)| (key.as_slice().into(), value.as_slice().into())),
    )
    .unwrap();
    let file_map = VarBytesMap::open(&buffer).unwrap();
    // The default node config's heap pages: the leaves' size.
    let page = 1024;
    for (key, value) in &entries {
        let (found_key, found) = file_map.get_key_value(&VarBytesQuery::new(key)).unwrap();
        assert_eq!(found.bytes(), value.as_slice());
        // A record is the key's bytes, then the value's.
        let start = found_key.bytes().as_ptr() as usize - buffer.as_ptr() as usize;
        let size = key.len() + value.len();
        if size > page {
            assert_eq!(start % page, 0, "a large record starts a page");
        } else {
            assert_eq!(
                start / page,
                (start + size - 1) / page,
                "a record stays in its page"
            );
        }
    }
}

#[test]
fn var_bytes_writers_refuse_entries_out_of_order() {
    let entries: [(&[u8], &[u8]); 3] = [(b"b", b""), (b"c", b""), (b"bb", b"")];
    let refused =
        file_map_of::<VarBytes, VarBytes>(entries.map(|(key, value)| (key.into(), value.into())));
    assert_eq!(refused.err().map(|error| error.index()), Some(2));
}

#[test]
fn queries_hold_any_bytes() {
    for bytes in [&b""[..], b"x", b"exactly8", b"nine byte", &[0xff; 100]] {
        let query = VarBytesQuery::new(bytes);
        assert_eq!(
            (query.bytes(), query.len(), query.is_empty()),
            (bytes, bytes.len(), bytes.is_empty())
        );
    }
    assert!(
        (*VarBytesQuery::new(b"ab"))
            .cmp(&VarBytesQuery::new(b"b"))
            .is_lt()
    );
}

/// A span refuses bytes past the file map's end, and a query's mark.
#[test]
fn spans_whose_bytes_lie_outside_are_refused() {
    let mut file_bytes = vec![0; 64];
    let mut check = |len: u32, offset: u32| {
        file_bytes[16..24].copy_from_slice(&VarBytes::encode(len, offset));
        VarBytes::check(&file_bytes, 16)
    };
    assert_eq!(check(8, 40), Ok(()));
    assert_eq!(check(0, 48), Ok(()));
    let outside = Err(FileError::Corrupt(
        "a key or value's bytes outside the file map",
    ));
    for (len, offset) in [(9, 40), (1, u32::MAX), (VarBytes::QUERY, 8)] {
        assert_eq!(check(len, offset), outside, "{len} at {offset}");
    }
}

/// A plain key of a span's size and alignment, with a span's inline keys:
/// a file map of it differs from one of var-length keys in what its slots
/// hold alone.
#[repr(C, align(8))]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Bytes8([u8; 8]);

// SAFETY: eight bytes, aligned to 8, with no padding.
unsafe impl PlainBytes for Bytes8 {}

impl InlineKey for Bytes8 {
    const HAS_HASH_TAG: bool = true;
    const HAS_CMP_FROM: bool = true;

    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.0.as_slice().inline_key_at(offset)
    }

    fn hash_tag(&self) -> u8 {
        self.0.as_slice().hash_tag()
    }

    fn cmp_from(&self, other: &Self, equal_len: usize) -> std::cmp::Ordering {
        self.0.as_slice().cmp_from(other.0.as_slice(), equal_len)
    }

    fn cmp_prefix(&self, prefix: &[u8]) -> std::cmp::Ordering {
        self.0.as_slice().cmp_prefix(prefix)
    }
}

#[test]
fn plain_keys_of_a_spans_layout_read_no_var_bytes_file_map() {
    let buffer = file_map_of::<VarBytes, u64>([(b"key".as_slice().into(), 1)]).unwrap();
    let read = FileMap::<VarBytes, u64, WithInlineKey>::open(&buffer).unwrap();
    assert_eq!(read.get(&VarBytesQuery::new(b"key")), Some(&1));
    assert_eq!(
        FileMap::<Bytes8, u64, WithInlineKey>::open(&buffer).err(),
        Some(FileError::LayoutMismatch)
    );
}

/// Damaged at random, a file map of var-length keys and values is refused,
/// or read in bounds, its spans' bytes included.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn damaged_var_bytes_file_maps_are_refused_or_read_in_bounds() {
    let rng = Rng::new(29);
    let count = if cfg!(miri) { 100 } else { 3000 };
    let mut keys: Vec<Vec<u8>> = (0..count)
        .map(|number| number.to_string().into_bytes())
        .collect();
    keys.sort();
    let entries =
        (keys.iter()).map(|key| (key.as_slice().into(), key.repeat(key.len() % 5).into()));
    let file_bytes = file_map_of::<VarBytes, VarBytes>(entries).unwrap();
    let mut damaged = FileBuf::zeroed(file_bytes.len(), VarBytesMap::alignment());
    let (mut refused, mut read) = (0, 0);
    for _ in 0..if cfg!(miri) { 6 } else { 400 } {
        damaged.copy_from_slice(&file_bytes);
        for _ in 0..1 + rng.below(3) {
            let at = 64 + rng.below(file_bytes.len() - 64);
            damaged[at] = rng.below(256) as u8;
        }
        let Ok(map) = VarBytesMap::open(&damaged) else {
            refused += 1;
            continue;
        };
        read += 1;
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut found = 0;
            for number in (0..count + 100).step_by(7) {
                let key = number.to_string();
                found += map
                    .get(&VarBytesQuery::new(key.as_bytes()))
                    .map_or(0, VarBytes::len);
            }
            let bytes = |sum: usize, (key, value): (&VarBytes, &VarBytes)| {
                sum + key.bytes().len() + value.bytes().len()
            };
            (
                found,
                map.iter().fold(0, bytes),
                map.iter().rev().fold(0, bytes),
            )
        }));
    }
    // Miri, which damages 6 file maps, checks that reading them stays in
    // bounds, so it needs some read; which of so few it refuses depends on
    // the layout.
    if cfg!(miri) {
        assert!(read > 0, "{refused} refused, {read} read");
    } else {
        assert!(refused > 0 && read > 0, "{refused} refused, {read} read");
    }
}
