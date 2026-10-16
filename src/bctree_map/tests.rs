//! `BCTreeMap` against std's `BTreeMap`, one thread at a time, and its
//! answers while threads share it.

use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::sync::Barrier;
use std::thread;
use std::vec::Vec;

use super::BCTreeMap;
use crate::tree::test_support::TestRng;

/// Inserts `keys` in order, each with its position as its value, into a
/// `BCTreeMap` and a `BTreeMap`, and checks that they agree on every
/// insert's answer, every key's value, keys they lack, and updates; and
/// that the tree is well formed, every prefix exactly what its fences
/// share, after the inserts.
fn assert_matches_std<K: Ord + Clone + core::fmt::Debug>(keys: &[K], absent: &[K])
where
    BCTreeMap<K, usize>: Default,
{
    let mut map = BCTreeMap::<K, usize>::default();
    let mut std_map = BTreeMap::new();
    for (position, key) in keys.iter().enumerate() {
        assert_eq!(
            map.insert(key.clone(), position),
            std_map.insert(key.clone(), position),
            "inserting {key:?}"
        );
    }
    map.check_invariants(std_map.len());
    for (key, &value) in &std_map {
        assert_eq!(
            map.read(key, |stored, &value| (stored.clone(), value)),
            Some((key.clone(), value))
        );
    }
    for key in absent {
        assert_eq!(
            map.get_cloned(key),
            std_map.get(key).copied(),
            "looking up {key:?}"
        );
    }
    for key in std_map.keys() {
        assert_eq!(
            map.update(key, |_, value| core::mem::replace(value, *value + 1)),
            std_map.get(key).copied()
        );
    }
    for (key, &value) in &std_map {
        assert_eq!(map.get_cloned(key), Some(value + 1));
    }
}

/// `count` random numbers below `universe`, some repeated.
fn random_numbers(rng: &TestRng, count: usize, universe: usize) -> Vec<u64> {
    (0..count).map(|_| rng.below(universe) as u64).collect()
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn integers_match_std() {
    let rng = TestRng::new(91);
    let keys = random_numbers(&rng, 20_000, 1 << 40);
    let absent = random_numbers(&rng, 2_000, 1 << 40);
    assert_matches_std(&keys, &absent);
    let ascending: Vec<u64> = (0..5_000).collect();
    let descending: Vec<u64> = (0..5_000).rev().collect();
    assert_matches_std(&ascending, &[5_000, u64::MAX]);
    assert_matches_std(&descending, &[5_000, u64::MAX]);
    // Dense keys share their top bytes, which no border node skips: every
    // search ties there.
    let dense: Vec<u64> = random_numbers(&rng, 5_000, 6_000);
    assert_matches_std(&dense, &[6_001]);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn strings_match_std() {
    let rng = TestRng::new(92);
    let path = |number: u64| format!("/usr/share/{}/{:x}", number % 7, number);
    let keys: Vec<String> = random_numbers(&rng, 8_000, 50_000)
        .into_iter()
        .map(path)
        .collect();
    let absent: Vec<String> = random_numbers(&rng, 500, 50_000)
        .into_iter()
        .map(path)
        .collect();
    assert_matches_std(&keys, &absent);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn byte_arrays_match_std() {
    let rng = TestRng::new(93);
    let array = |number: u64| {
        let mut key = [0_u8; 32];
        key[..8].copy_from_slice(&(number % 3).to_be_bytes());
        key[24..].copy_from_slice(&number.to_be_bytes());
        key
    };
    let keys: Vec<[u8; 32]> = random_numbers(&rng, 8_000, 1 << 30)
        .into_iter()
        .map(array)
        .collect();
    let absent: Vec<[u8; 32]> = random_numbers(&rng, 500, 1 << 30)
        .into_iter()
        .map(array)
        .collect();
    assert_matches_std(&keys, &absent);
}

#[test]
fn a_small_map_matches_std() {
    let rng = TestRng::new(94);
    let keys = random_numbers(&rng, 300, 1_000);
    let absent = random_numbers(&rng, 50, 1_000);
    assert_matches_std(&keys, &absent);
    assert_eq!(BCTreeMap::<u64, u64>::new().get_cloned(&1), None);
}

/// Threads insert their own keys while others read: every key a writer
/// has inserted is there for every reader from then on, and at the end
/// every key holds its writer's value, in a well-formed tree.
#[test]
#[cfg_attr(
    miri,
    ignore = "optimistic lock coupling reads nodes writers are changing, which Miri reports as data races"
)]
fn threads_find_what_other_threads_inserted() {
    let (writers, per_writer) = (4, 20_000);
    let map = BCTreeMap::<u64, u64>::new();
    let rng = TestRng::new(95);
    let keys: Vec<Vec<u64>> = (0..writers)
        .map(|writer| {
            let mut keys: Vec<u64> = (0..per_writer)
                .map(|index| (index * writers + writer) as u64 * 0x9E37_79B9)
                .collect();
            rng.shuffle(&mut keys);
            keys
        })
        .collect();
    let barrier = Barrier::new(2 * writers);
    thread::scope(|scope| {
        for (writer, keys) in keys.iter().enumerate() {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                barrier.wait();
                for &key in keys {
                    assert_eq!(map.insert(key, key ^ writer as u64), None);
                }
            });
            scope.spawn(move || {
                barrier.wait();
                // Reads the writer's keys as it inserts them: once found,
                // a key stays.
                let mut found = 0;
                while found < keys.len() {
                    match map.get_cloned(&keys[found]) {
                        Some(value) => {
                            assert_eq!(value, keys[found] ^ writer as u64);
                            if found > 0 {
                                let earlier = keys[found / 2];
                                assert_eq!(map.get_cloned(&earlier), Some(earlier ^ writer as u64));
                            }
                            found += 1;
                        }
                        None => thread::yield_now(),
                    }
                }
            });
        }
    });
    let mut map = map;
    map.check_invariants(writers * per_writer);
    for (writer, keys) in keys.iter().enumerate() {
        for &key in keys {
            assert_eq!(map.get_cloned(&key), Some(key ^ writer as u64));
        }
    }
}

/// Threads insert the same keys, in different orders, and count with
/// `update`: each key ends with one writer's value and every count.
#[test]
#[cfg_attr(
    miri,
    ignore = "optimistic lock coupling reads nodes writers are changing, which Miri reports as data races"
)]
fn threads_racing_on_the_same_keys_agree() {
    let (threads, key_count) = (4, 10_000);
    let map = BCTreeMap::<String, (usize, u64)>::new();
    let keys: Vec<String> = (0..key_count)
        .map(|index| format!("key/{index:08}"))
        .collect();
    let barrier = Barrier::new(threads);
    thread::scope(|scope| {
        for thread in 0..threads {
            let (map, barrier, keys) = (&map, &barrier, &keys);
            scope.spawn(move || {
                let rng = TestRng::new(96 + thread as u64);
                let mut order: Vec<usize> = (0..keys.len()).collect();
                rng.shuffle(&mut order);
                barrier.wait();
                for &index in &order {
                    let key = &keys[index];
                    map.insert(key.clone(), (thread, 0));
                    map.update(key, |_, (_, count)| *count += 1)
                        .expect("an inserted key stays");
                }
            });
        }
    });
    let mut map = map;
    map.check_invariants(key_count);
    for key in &keys {
        let (writer, count) = map.get_cloned(key).expect("every key was inserted");
        assert!(writer < threads);
        // Each thread counts once after its own insert; a later insert by
        // another thread resets the count.
        assert!((1..=threads as u64).contains(&count), "{key}: {count}");
    }
}
