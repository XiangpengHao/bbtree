//! Maps in files: what a map held, read back as the caller's types, mapped
//! or read, each type kept as fits it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::ops::Bound;
use std::path::PathBuf;

use bbtree::file::PlainBytes;
use bbtree::{InlineKey, InlineWidth, InlineWord, WithInlineKey};
use bbtree_disk::{BBTreeMapFile, plain_disk_type};

/// A file of its own for a test, removed when dropped.
struct TestFile(PathBuf);

impl TestFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("bbtree-disk-{name}-{}", std::process::id())))
    }

    fn len(&self) -> u64 {
        std::fs::metadata(&self.0).unwrap().len()
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn text_maps_round_trip() {
    let file = TestFile::new("text");
    let map: BTreeMap<String, String> = (0..5000)
        .map(|number: u32| (format!("key-{number:x}"), "v".repeat(number as usize % 37)))
        .collect();
    BBTreeMapFile::<String, String>::create(&file.0, &map).unwrap();
    // SAFETY: nothing changes the file while it is mapped.
    let mapped = unsafe { BBTreeMapFile::<String, String>::mmap(&file.0).open() }.unwrap();
    let read = BBTreeMapFile::<String, String>::read(&file.0).unwrap();
    for disk in [&mapped, &read] {
        assert_eq!(disk.len(), map.len());
        for (key, value) in &map {
            assert_eq!(disk.get(key.as_str()), Some(value.as_str()));
            assert_eq!(disk.get(key), Some(value.as_str()));
        }
        assert_eq!(disk.get("key-"), None);
        assert_eq!(disk.get("key-fffff"), None);
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        for chunk in keys.chunks(4) {
            let batch: [&str; 6] = std::array::from_fn(|lane| match lane {
                4 => "key-",
                5 => "key-fffff",
                _ => chunk[lane % chunk.len()],
            });
            let expected = batch.map(|key| map.get(key).map(String::as_str));
            assert_eq!(disk.get_many(batch), expected);
        }
        assert!(
            disk.iter().eq(map
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())))
        );
        let bounds = (Bound::Included("key-10"), Bound::Excluded("key-2"));
        let range = disk.range::<str, _>(bounds);
        let expected = map.range::<str, _>(bounds);
        assert!(range.eq(expected.map(|(key, value)| (key.as_str(), value.as_str()))));
    }
}

/// Integers stay in their slots: a `u64` map's file takes little more than
/// its 16 bytes an entry, where var-length bytes in heap pages take 49.
#[test]
fn fixed_size_types_stay_in_their_slots() {
    let file = TestFile::new("plain");
    let map: BTreeMap<u64, u64> = (0..100_000).map(|key| (key * 2, !key)).collect();
    BBTreeMapFile::<u64, u64>::create(&file.0, &map).unwrap();
    let bytes_per_entry = file.len() as f64 / map.len() as f64;
    assert!(bytes_per_entry < 19.0, "{bytes_per_entry} bytes an entry");
    let disk = BBTreeMapFile::<u64, u64>::read(&file.0).unwrap();
    for key in 0..1000 {
        assert_eq!(disk.get(&key), map.get(&key));
    }
    assert_eq!(disk.get_many([&4, &5, &4]), [Some(&!2), None, Some(&!2)]);
    assert!(disk.range(10..20).eq(map.range(10..20)));
}

#[test]
fn text_keys_with_plain_values_round_trip() {
    let file = TestFile::new("text-plain");
    let map: BTreeMap<String, u64> = (0..3000_u64).map(|n| (format!("user/{n}"), n)).collect();
    BBTreeMapFile::<String, u64>::create(&file.0, &map).unwrap();
    let disk = BBTreeMapFile::<String, u64>::read(&file.0).unwrap();
    assert!(
        disk.iter()
            .eq(map.iter().map(|(key, value)| (key.as_str(), value)))
    );
    assert_eq!(disk.get("user/42"), Some(&42));
    assert_eq!(disk.get_key_value("user/7"), Some(("user/7", &7)));
}

/// A plain type of the caller's, kept in its slot: one searched whole, and
/// one with inline keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
struct Version(u32);

// SAFETY: one `u32`: every bit pattern is a `Version`, with no padding and
// nothing to point to.
unsafe impl PlainBytes for Version {}

plain_disk_type!(Version);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
struct UserId(u64);

// SAFETY: one `u64`: every bit pattern is a `UserId`, with no padding and
// nothing to point to.
unsafe impl PlainBytes for UserId {}

impl InlineKey for UserId {
    const WIDTH: InlineWidth = u64::WIDTH;

    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.0.inline_key_at(offset)
    }
}

plain_disk_type!(UserId, WithInlineKey);

#[test]
fn plain_types_of_the_callers_round_trip() {
    let file = TestFile::new("users");
    let users: BTreeMap<UserId, Version> = (0..20_000_u64)
        .map(|id| (UserId(id * 3), Version(id as u32 % 7)))
        .collect();
    BBTreeMapFile::<UserId, Version>::create(&file.0, &users).unwrap();
    let disk = BBTreeMapFile::<UserId, Version>::read(&file.0).unwrap();
    for id in 0..60_000 {
        assert_eq!(disk.get(&UserId(id)), users.get(&UserId(id)));
    }
    assert!(disk.iter().eq(users.iter()));

    let versions: BTreeMap<Version, u64> =
        (0..5000).map(|v| (Version(v * 2), u64::from(v))).collect();
    BBTreeMapFile::<Version, u64>::create(&file.0, &versions).unwrap();
    let disk = BBTreeMapFile::<Version, u64>::read(&file.0).unwrap();
    for v in 0..10_000 {
        assert_eq!(disk.get(&Version(v)), versions.get(&Version(v)));
    }
}

/// A map within a file of other data, after it and before more of it, as
/// a Parquet file holds an index before its footer.
#[test]
fn maps_embed_in_other_files() {
    let path = TestFile::new("embedded");
    let mut file = File::options()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path.0)
        .unwrap();
    file.write_all(b"other data, of no particular length")
        .unwrap();
    let map: BTreeMap<u64, String> = (0..2000).map(|key| (key, format!("value {key}"))).collect();
    let offset = file.metadata().unwrap().len();
    let range = BBTreeMapFile::<u64, String>::write_at(&mut file, offset, &map).unwrap();
    assert!(range.start >= offset);
    assert_eq!(file.metadata().unwrap().len(), range.end);
    file.write_all(b"a footer after it").unwrap();

    // SAFETY: nothing changes the file while it is mapped.
    let mapped = unsafe {
        BBTreeMapFile::<u64, String>::mmap(&path.0)
            .range(range.clone())
            .open()
    }
    .unwrap();
    let read = BBTreeMapFile::<u64, String>::read_range(&mut file, range).unwrap();
    for disk in [&mapped, &read] {
        assert!(
            disk.iter()
                .eq(map.iter().map(|(key, value)| (key, value.as_str())))
        );
    }
}

/// A shared map, by its owner, and while threads may still change it.
#[test]
fn shared_maps_round_trip() {
    let file = TestFile::new("shared");
    let mut map = bbtree::BBTreeMap::<u64, String>::new();
    for key in 0..1000_u64 {
        map.insert(key * 3, format!("value-{key}")).unwrap();
    }
    let entries: Vec<(u64, String)> = map.range_cloned(..).collect();
    let expected = entries.iter().map(|(key, value)| (key, value.as_str()));
    BBTreeMapFile::<u64, String>::create(&file.0, &mut map).unwrap();
    let owned = BBTreeMapFile::<u64, String>::read(&file.0).unwrap();
    assert!(owned.iter().eq(expected.clone()));
    BBTreeMapFile::<u64, String>::create(&file.0, map.range_cloned(..)).unwrap();
    let cloned = BBTreeMapFile::<u64, String>::read(&file.0).unwrap();
    assert!(cloned.iter().eq(expected));
}

#[test]
fn integer_keys_order_as_numbers() {
    let file = TestFile::new("integers");
    let map: BTreeMap<i64, Vec<u8>> = (-300..300)
        .map(|key: i64| (key * 7, key.to_le_bytes().to_vec()))
        .collect();
    BBTreeMapFile::<i64, Vec<u8>>::create(&file.0, &map).unwrap();
    let disk = BBTreeMapFile::<i64, Vec<u8>>::read(&file.0).unwrap();
    assert!(disk.iter().map(|(key, _)| key).eq(map.keys()));
    assert_eq!(disk.get(&-7), Some(&(-1_i64).to_le_bytes()[..]));
    assert_eq!(disk.get(&-8), None);
    let below_zero: Vec<i64> = disk.range(..0).map(|(&key, _)| key).collect();
    assert_eq!(below_zero.len(), 300);
    assert!(below_zero.iter().all(|&key| key < 0));
}

#[test]
fn entries_out_of_order_are_refused() {
    let file = TestFile::new("unordered");
    let entries = [(&2_u32, &1_u32), (&1, &1)];
    let error = BBTreeMapFile::<u32, u32>::create(&file.0, entries).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn an_empty_map_round_trips() {
    let file = TestFile::new("empty");
    BBTreeMapFile::<u64, u64>::create(&file.0, &BTreeMap::<u64, u64>::new()).unwrap();
    let disk = BBTreeMapFile::<u64, u64>::read(&file.0).unwrap();
    assert!(disk.is_empty());
    assert_eq!(disk.get(&1), None);
    assert_eq!(disk.iter().count(), 0);
}
