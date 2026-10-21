use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::ControlFlow;
use std::sync::Arc;
use std::thread;

use super::{BBTreeStore, InsertError};
use crate::allocator::{Global, PageKind};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node_config::{Configured, NodeConfig};
use crate::record_ptr::RecordPtr;
use crate::tree::node::page::TestPages;
use crate::tree::test_support::TestRng;

/// The key numbered `n`: how many bytes `n` takes, then those bytes, then
/// filler, from 2 to 35 bytes in all, so that some keys end within their
/// 8-byte inline key and some don't. No two numbers share a key.
fn key(n: usize) -> Vec<u8> {
    let bytes = (n as u64).to_be_bytes();
    let skipped = (bytes.iter()).take_while(|&&byte| byte == 0).count().min(7);
    let mut key = vec![(8 - skipped) as u8];
    key.extend_from_slice(&bytes[skipped..]);
    key.resize(key.len() + n % 32, b'k');
    key
}

/// A value of `len` bytes, telling `n` and `version` apart.
fn value(n: usize, version: usize, len: usize) -> Vec<u8> {
    (0..len).map(|i| (n + version * 7 + i) as u8).collect()
}

#[test]
fn inserts_reads_replaces_and_removes() {
    let mut store = BBTreeStore::new();
    assert_eq!(store.get(b"apple"), None);
    assert_eq!(store.insert(b"apple", b"red"), Ok(false));
    assert_eq!(store.insert(b"banana", b"yellow"), Ok(false));
    assert_eq!(store.get(b"apple").as_deref(), Some(&b"red"[..]));
    // A shorter value goes in place; a longer one in a new record.
    assert_eq!(store.insert(b"apple", b"re"), Ok(true));
    assert_eq!(store.get(b"apple").as_deref(), Some(&b"re"[..]));
    assert_eq!(store.insert(b"apple", b"green and red"), Ok(true));
    assert_eq!(store.get(b"apple").as_deref(), Some(&b"green and red"[..]));
    let mut buf = b"x".to_vec();
    assert_eq!(store.get_into(b"banana", &mut buf), Some(6));
    assert_eq!(buf, b"xyellow");
    assert!(store.contains_key(b"banana"));
    assert!(store.remove(b"banana"));
    assert!(!store.remove(b"banana"));
    assert!(!store.contains_key(b"banana"));
    assert_eq!(store.insert(b"", b""), Ok(false));
    assert_eq!(store.get(b"").as_deref(), Some(&b""[..]));
    store.check_invariants(2);
}

#[test]
fn refuses_keys_and_records_too_large() {
    let store = BBTreeStore::new();
    let max_key = BBTreeStore::<Auto, Global>::MAX_KEY_LEN;
    let max_record = BBTreeStore::<Auto, Global>::MAX_RECORD_SIZE;
    assert_eq!(
        store.insert(&vec![1; max_key + 1], b""),
        Err(InsertError::KeyTooLong)
    );
    let largest_value = max_record - 8 - max_key;
    assert_eq!(
        store.insert(&vec![1; max_key], &vec![2; largest_value]),
        Ok(false)
    );
    assert_eq!(
        store.insert(&vec![1; max_key], &vec![2; largest_value + 1]),
        Err(InsertError::RecordTooLarge)
    );
    assert_eq!(
        store.get(&vec![1; max_key]).map(|v| v.len()),
        Some(largest_value)
    );
}

/// Random inserts, replacements and removals, against std's `BTreeMap`,
/// with values from empty to most of a heap page: leaves split, merge, and
/// compact their heaps.
fn matches_btree_map<S: InlineKeyStrategy<RecordPtr>>(
    mut store: BBTreeStore<S>,
    seed: u64,
    operations: usize,
    universe: usize,
) {
    let rng = TestRng::new(seed);
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let max_value = BBTreeStore::<S>::MAX_RECORD_SIZE - 8 - 40;
    for step in 0..operations {
        let n = rng.below(universe);
        let key = key(n);
        match rng.below(10) {
            0..=5 => {
                let len = match rng.below(8) {
                    0 => rng.below(max_value + 1),
                    1 => 0,
                    _ => rng.below(64),
                };
                let value = value(n, step, len);
                let replaced = store
                    .insert(&key, &value)
                    .expect("the global allocator gives");
                assert_eq!(replaced, model.insert(key, value).is_some());
            }
            6..=8 => assert_eq!(store.remove(&key), model.remove(&key).is_some()),
            _ => assert_eq!(store.get(&key), model.get(&key).cloned()),
        }
        if step % 512 == 0 {
            store.check_invariants(model.len());
        }
    }
    store.check_invariants(model.len());
    for (key, value) in &model {
        assert_eq!(store.get(key).as_ref(), Some(value));
    }
    // Remove all but a few, so that leaves merge down to a root leaf.
    let keys: Vec<Vec<u8>> = model.keys().cloned().collect();
    for key in keys.iter().skip(3) {
        assert!(store.remove(key));
        model.remove(key);
    }
    store.check_invariants(model.len());
}

#[test]
fn matches_a_btree_map() {
    let operations = if cfg!(miri) { 400 } else { 40_000 };
    matches_btree_map(BBTreeStore::new(), 1, operations, 2_000);
    matches_btree_map(BBTreeStore::new(), 2, operations, 200);
}

/// Heap pages larger than leaves, on a free list of their own.
struct LargeHeapPages;

impl NodeConfig for LargeHeapPages {
    const HEAP_PAGE_SIZE: usize = 4096;
}

#[test]
fn matches_a_btree_map_with_larger_heap_pages() {
    let operations = if cfg!(miri) { 400 } else { 20_000 };
    let store = BBTreeStore::with_strategy_in(Configured::<Auto, LargeHeapPages>::new(), Global);
    matches_btree_map(store, 3, operations, 2_000);
}

#[test]
fn a_spent_budget_refuses_inserts_and_changes_nothing() {
    let pages = TestPages::<RecordPtr, (), Auto>::new();
    pages.set_in_use_limit(Some(64));
    let mut store = BBTreeStore::with_strategy_in(Auto, &pages);
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut refused = 0;
    for n in 0..2_000 {
        let (key, value) = (key(n), value(n, 0, 100));
        match store.insert(&key, &value) {
            Ok(replaced) => assert_eq!(replaced, model.insert(key, value).is_some()),
            Err(InsertError::Alloc(_)) => refused += 1,
            Err(error) => panic!("{error}"),
        }
    }
    assert!(refused > 0, "the budget runs out");
    store.check_invariants(model.len());
    for (key, value) in &model {
        assert_eq!(store.get(key).as_ref(), Some(value));
    }
    // Removing makes room, in the leaves' heaps and on the free lists.
    let removed: Vec<Vec<u8>> = model.keys().step_by(2).cloned().collect();
    for key in &removed {
        assert!(store.remove(key));
        model.remove(key);
    }
    for key in &removed {
        let value = value(0, 1, 100);
        store.insert(key, &value).expect("removals made room");
        model.insert(key.clone(), value);
    }
    store.check_invariants(model.len());
    drop(store);
    pages.assert_all_released();
}

#[test]
fn threads_share_a_store() {
    let threads = if cfg!(miri) { 2 } else { 4 };
    let per_thread = if cfg!(miri) { 50 } else { 5_000 };
    let store = Arc::new(BBTreeStore::new());
    let handles: Vec<_> = (0..threads)
        .map(|thread| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                for i in 0..per_thread {
                    let n = i * threads + thread;
                    let (key, value) = (key(n), value(n, 0, n % 200));
                    store
                        .insert(&key, &value)
                        .expect("the global allocator gives");
                    assert_eq!(store.get(&key), Some(value));
                    if i % 3 == 0 {
                        assert!(store.remove(&key));
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no thread panics");
    }
    let mut store = Arc::into_inner(store).expect("the threads are done");
    let mut left = 0;
    for n in 0..threads * per_thread {
        let present = (n / threads) % 3 != 0;
        assert_eq!(store.get(&key(n)), present.then(|| value(n, 0, n % 200)));
        left += usize::from(present);
    }
    store.check_invariants(left);
}

#[test]
fn ranges_match_a_btree_map() {
    let rng = TestRng::new(4);
    let (count, ranges) = if cfg!(miri) { (300, 20) } else { (3_000, 300) };
    let store = BBTreeStore::new();
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for n in (0..count).filter(|n| n % 3 != 0) {
        let value = value(n, 0, rng.below(80));
        store
            .insert(&key(n), &value)
            .expect("the global allocator gives");
        model.insert(key(n), value);
    }
    for _ in 0..ranges {
        let (low, high) = rng.range_bounds(count, key);
        let bounds = (
            low.as_ref().map(Vec::as_slice),
            high.as_ref().map(Vec::as_slice),
        );
        let expected: Vec<(Vec<u8>, Vec<u8>)> = (model.range::<[u8], _>(bounds))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        let mut scanned = Vec::new();
        store.scan::<[u8], ()>(bounds, |key, value| {
            scanned.push((key.to_vec(), value.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(scanned, expected);

        let mut scanned_back = Vec::new();
        store.scan_rev::<[u8], ()>(bounds, |key, value| {
            scanned_back.push((key.to_vec(), value.to_vec()));
            ControlFlow::Continue(())
        });
        scanned_back.reverse();
        assert_eq!(scanned_back, expected);

        // A scan stops where its closure breaks.
        let stop = rng.below(expected.len() + 1);
        let mut seen = 0;
        let broke = store.scan::<[u8], _>(bounds, |key, _| {
            seen += 1;
            if seen > stop {
                ControlFlow::Break(key.to_vec())
            } else {
                ControlFlow::Continue(())
            }
        });
        assert_eq!(broke, expected.get(stop).map(|(key, _)| key.clone()));

        // Copies from both ends meet without handing an entry out twice.
        let mut copies = store.range_cloned::<[u8], _>(bounds);
        let (mut front, mut back) = (Vec::new(), Vec::new());
        loop {
            let entry = if rng.below(2) == 0 {
                copies.next().map(|entry| front.push(entry))
            } else {
                copies.next_back().map(|entry| back.push(entry))
            };
            if entry.is_none() {
                break;
            }
        }
        back.reverse();
        front.extend(back);
        assert_eq!(front, expected);
    }
}

#[test]
fn heap_pages_go_back_when_emptied_and_node_pages_when_dropped() {
    let pages = TestPages::<RecordPtr, (), Auto>::new();
    let store = BBTreeStore::with_strategy_in(Auto, &pages);
    let count = if cfg!(miri) { 200 } else { 2_000 };
    for n in 0..count {
        store.insert(&key(n), &value(n, 0, 200)).expect("no budget");
    }
    let (heap, leaves) = (
        pages.in_use_of(PageKind::Heap),
        pages.in_use_of(PageKind::Leaf),
    );
    assert!(
        heap > leaves,
        "records of 200 bytes fill more heap pages than leaves"
    );
    for n in 3..count {
        assert!(store.remove(&key(n)));
    }
    // Merges compacted the leaves' heaps, and gave their pages back; the
    // merged leaves' pages wait on the tree's free list.
    assert!(pages.in_use_of(PageKind::Heap) < heap / 10);
    assert_eq!(pages.in_use_of(PageKind::Leaf), leaves);
    assert_eq!(pages.in_use_of(PageKind::Root), 1);
    drop(store);
    pages.assert_all_released();
}
