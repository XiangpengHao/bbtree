//! Index files of keys and values of any length: what a map held, read back
//! as the caller's types, mapped or read.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::ops::Bound;
use std::path::PathBuf;

use bbtree_disk::{IndexFile, OsPages};

/// A file of its own for a test, removed when dropped.
struct TestFile(PathBuf);

impl TestFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("bbtree-disk-{name}-{}", std::process::id())))
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
    IndexFile::create(&file.0, &map).unwrap();
    // SAFETY: nothing changes the file while it is mapped.
    let mapped = unsafe { IndexFile::<String, String>::mmap(&file.0, OsPages::Small) }.unwrap();
    let read = IndexFile::<String, String>::read(&file.0).unwrap();
    for index in [&mapped, &read] {
        assert_eq!(index.len(), map.len());
        for (key, value) in &map {
            assert_eq!(index.get(key.as_str()), Some(value.as_str()));
        }
        assert_eq!(index.get("key-"), None);
        assert_eq!(index.get("key-fffff"), None);
        assert!(
            index.iter().eq(map
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())))
        );
        let bounds = (Bound::Included("key-10"), Bound::Excluded("key-2"));
        let range = index.range::<str, _>(bounds);
        let expected = map.range::<str, _>(bounds);
        assert!(range.eq(expected.map(|(key, value)| (key.as_str(), value.as_str()))));
    }
}

#[test]
fn integer_keys_order_as_numbers() {
    let file = TestFile::new("integers");
    let map: BTreeMap<i64, Vec<u8>> = (-300..300)
        .map(|key: i64| (key * 7, key.to_le_bytes().to_vec()))
        .collect();
    IndexFile::create(&file.0, &map).unwrap();
    let index = IndexFile::<i64, Vec<u8>>::read(&file.0).unwrap();
    assert!(index.iter().map(|(key, _)| key).eq(map.keys().copied()));
    assert_eq!(index.get(&-7), Some(&(-1_i64).to_le_bytes()[..]));
    assert_eq!(index.get(&-8), None);
    let below_zero: Vec<i64> = index.range(..0).map(|(key, _)| key).collect();
    assert_eq!(below_zero.len(), 300);
    assert!(below_zero.iter().all(|&key| key < 0));
}

#[test]
fn entries_out_of_order_are_refused() {
    let file = TestFile::new("unordered");
    let entries = [(&2_u32, &1_u32), (&1, &1)];
    let error = IndexFile::create(&file.0, entries).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn an_empty_map_round_trips() {
    let file = TestFile::new("empty");
    IndexFile::<u64, u64>::create(&file.0, &BTreeMap::new()).unwrap();
    let index = IndexFile::<u64, u64>::read(&file.0).unwrap();
    assert!(index.is_empty());
    assert_eq!(index.get(&1), None);
    assert_eq!(index.iter().count(), 0);
}
