//! `BBTreeMap` against std's `BTreeMap`, one thread at a time, and its
//! answers while threads share it.

use core::ops::{Bound, ControlFlow};
use core::panic::{RefUnwindSafe, UnwindSafe};
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::format;
use std::string::String;
use std::sync::Barrier;
use std::thread;
use std::vec::Vec;

use super::{BBTreeMap, RangeCloned};
use crate::allocator::{Global, InsertError};
use crate::inline_key::{Auto, InlineKeyStrategy, WithInlineKey};
use crate::node_config::Configured;
use crate::tree::node::latch::Latched;
use crate::tree::node::page::TestPages;
use crate::tree::test_support::{HashTagCase, LargePages, SmallInternalPages, TestRng};

/// Inserts `keys` in order, each with its position as its value, into a
/// `BBTreeMap` and a `BTreeMap`, and checks that they agree on every
/// insert's answer, every key's value, keys they lack, and updates; and
/// that the tree is well formed after the inserts.
fn assert_matches_std<K: Ord + Clone + core::fmt::Debug>(keys: &[K], absent: &[K])
where
    BBTreeMap<K, usize>: Default,
{
    let mut map = BBTreeMap::<K, usize>::default();
    let mut std_map = BTreeMap::new();
    for (position, key) in keys.iter().enumerate() {
        assert_eq!(
            map.insert(key.clone(), position).unwrap(),
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
    assert_batches_match_reads(&map, keys);
    assert_batches_match_reads(&map, absent);
    for key in keys.iter().chain(absent) {
        let expected = std_map.get(key).copied();
        assert_eq!(map.get_cloned(key), expected, "looking up {key:?}");
        assert_eq!(map.get_copied(key), expected, "copying {key:?}");
        assert_eq!(map.contains_key(key), expected.is_some(), "{key:?}");
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

/// Looks `queries` up in batches, 8 and 65 at a time, the last batch
/// filled with repeats, and checks that they give what separate reads do,
/// running the closure once per entry found.
fn assert_batches_match_reads<K: Ord + Clone + core::fmt::Debug>(
    map: &BBTreeMap<K, usize>,
    queries: &[K],
) {
    fn check<K: Ord + Clone + core::fmt::Debug, const N: usize>(
        map: &BBTreeMap<K, usize>,
        queries: &[K],
    ) {
        for chunk in queries.chunks(N) {
            let batch: [&K; N] = core::array::from_fn(|lane| &chunk[lane % chunk.len()]);
            let mut calls = 0;
            let entries = map.read_many(batch, |key, &value| {
                calls += 1;
                (key.clone(), value)
            });
            let expected = batch.map(|key| map.read(key, |key, &value| (key.clone(), value)));
            assert_eq!(entries, expected, "looking up {batch:?}");
            assert_eq!(calls, expected.iter().flatten().count());
            assert_eq!(
                map.get_many_cloned(batch),
                batch.map(|key| map.get_cloned(key))
            );
            assert_eq!(
                map.get_many_copied(batch),
                batch.map(|key| map.get_copied(key))
            );
        }
    }
    check::<K, 8>(map, queries);
    // Miri takes more than 64 lanes in the executor's tests (`tree/batch.rs`),
    // as lanes of 65 here would cost it minutes.
    if !cfg!(miri) {
        check::<K, 65>(map, queries);
    }
}

/// `count` random numbers below `universe`, some repeated.
fn random_numbers(rng: &TestRng, count: usize, universe: u64) -> Vec<u64> {
    (0..count).map(|_| rng.below_u64(universe)).collect()
}

/// A shared map crosses `catch_unwind` as std's map does: unwind safe, by
/// value and by reference, with keys and values that are.
#[test]
fn unwind_safe_as_std_map() {
    fn assert_unwind_safe<T: UnwindSafe + RefUnwindSafe>() {}
    assert_unwind_safe::<BBTreeMap<u64, u64>>();
    assert_unwind_safe::<BBTreeMap<String, u64, WithInlineKey>>();
}

#[test]
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
fn strings_match_std() {
    let rng = TestRng::new(92);
    let path = |number: u64| format!("/usr/share/{}/{:x}", number % 7, number);
    let keys: Vec<String> = random_numbers(&rng, 8_000, 50_000)
        .into_iter()
        .map(path)
        .collect();
    let mut absent: Vec<String> = random_numbers(&rng, 500, 50_000)
        .into_iter()
        .map(path)
        .collect();
    // Keys that lack what every key shares, before and after them all.
    absent.extend(["", "/usr", "/usr/shared", "a", "zz"].map(String::from));
    assert_matches_std(&keys, &absent);
}

/// A map of the case's keys, each to its index, as `u128`s: with 16-byte
/// values, latched `String` leaves have room for hash tags (D36).
impl From<&HashTagCase> for BBTreeMap<String, u128> {
    fn from(case: &HashTagCase) -> Self {
        let mut map = Self::new();
        for (index, key) in case.keys().iter().enumerate() {
            map.insert(key.clone(), index as u128).unwrap();
        }
        map.check_invariants(case.keys().len());
        map
    }
}

/// Every lookup of the shared hash-tag cases answers as std's map does,
/// the unlatched copies among them, which an exact tag may settle alone
/// (`node/optimistic.rs`).
#[test]
fn hash_tag_cases_match_std() {
    if cfg!(target_pointer_width = "64") {
        assert!(
            HashTagCase::leaves_keep_tags::<String, u128, Latched<Auto>>(),
            "the cases test hash tags only in leaves that keep them"
        );
    }
    for case in HashTagCase::all() {
        let map = BBTreeMap::from(&case);
        for probe in case.probes() {
            let probe = probe.as_str();
            let expected = case.value_of(probe).map(|index| index as u128);
            assert_eq!(map.get_copied(probe), expected, "{probe:.20}");
            assert_eq!(map.get_cloned(probe), expected);
            assert_eq!(map.contains_key(probe), expected.is_some());
            assert_eq!(map.get_many_copied([probe, probe]), [expected; 2]);
            assert_eq!(
                map.read(probe, |key, _| key.len()),
                expected.map(|_| probe.len())
            );
        }
    }
}

#[test]
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

/// Inserts and removes `keys`, each removed after the next `lag` were
/// inserted, the removals in another order, against a `BTreeMap`; checks
/// every answer, every key's value, and the tree, every `check_every`
/// operations; then removes the rest, and inserts all again.
fn assert_removals_match_std<K, S>(keys: &[K], lag: usize, check_every: usize)
where
    K: Ord + Clone + core::fmt::Debug,
    S: InlineKeyStrategy<K> + Default,
{
    let mut map = BBTreeMap::<K, usize, S>::with_strategy(S::default());
    let mut std_map = BTreeMap::new();
    let rng = TestRng::new(97);
    let mut order: Vec<usize> = (0..keys.len()).collect();
    rng.shuffle(&mut order);
    for (step, key) in keys.iter().enumerate() {
        assert_eq!(
            map.insert(key.clone(), step).unwrap(),
            std_map.insert(key.clone(), step)
        );
        if step >= lag {
            let gone = &keys[order[step - lag]];
            assert_eq!(
                map.remove_entry(gone),
                std_map.remove_entry(gone),
                "removing {gone:?}"
            );
            assert_eq!(map.get_cloned(gone), None);
        }
        if step % check_every == 0 {
            map.check_invariants(std_map.len());
        }
    }
    map.check_invariants(std_map.len());
    for (key, &value) in &std_map {
        assert_eq!(map.get_cloned(key), Some(value), "looking up {key:?}");
    }
    let rest: Vec<K> = std_map.keys().cloned().collect();
    for key in &rest {
        assert_eq!(map.remove(key), std_map.remove(key));
    }
    map.check_invariants(0);
    for (step, key) in keys.iter().enumerate() {
        assert_eq!(
            map.insert(key.clone(), step).unwrap(),
            std_map.insert(key.clone(), step)
        );
    }
    map.check_invariants(std_map.len());
}

#[test]
fn integer_removals_match_std() {
    let rng = TestRng::new(98);
    let keys = random_numbers(&rng, 30_000, 1 << 40);
    assert_removals_match_std::<_, Auto>(&keys, 10_000, 997);
    let ascending: Vec<u64> = (0..20_000).collect();
    assert_removals_match_std::<_, Auto>(&ascending, 15_000, 991);
}

#[test]
fn string_and_array_removals_match_std() {
    let rng = TestRng::new(99);
    let path = |number: u64| format!("/usr/share/{}/{:x}", number % 7, number);
    let keys: Vec<String> = random_numbers(&rng, 10_000, 1 << 30)
        .into_iter()
        .map(path)
        .collect();
    assert_removals_match_std::<_, Auto>(&keys, 4_000, 499);
    let array = |number: u64| {
        let mut key = [0_u8; 32];
        key[24..].copy_from_slice(&number.to_be_bytes());
        key
    };
    let keys: Vec<[u8; 32]> = random_numbers(&rng, 10_000, 1 << 30)
        .into_iter()
        .map(array)
        .collect();
    assert_removals_match_std::<_, Auto>(&keys, 4_000, 499);
}

/// A map whose node config (D59) sets other page sizes matches std through
/// removals that merge nodes onto the free lists, and inserts that take
/// their pages back.
#[test]
fn configured_page_sizes_match_std() {
    let rng = TestRng::new(102);
    let keys = random_numbers(&rng, 30_000, 1 << 40);
    assert_removals_match_std::<_, Configured<Auto, SmallInternalPages>>(&keys, 10_000, 997);
    assert_removals_match_std::<_, Configured<Auto, LargePages>>(&keys, 10_000, 997);
}

/// Pages that removals empty serve later splits: filling the map a second
/// time takes no more pages than the first.
#[test]
fn emptied_pages_serve_new_nodes() {
    let pages = TestPages::<u64, u64>::new();
    let rng = TestRng::new(100);
    let keys = random_numbers(&rng, if cfg!(miri) { 400 } else { 20_000 }, 1 << 40);
    {
        let map = BBTreeMap::<u64, u64, Auto, &TestPages<u64, u64>>::new_in(&pages);
        for &key in &keys {
            map.insert(key, key).unwrap();
        }
        let filled = pages.in_use_count();
        for &key in &keys {
            map.remove(&key);
        }
        assert_eq!(pages.in_use_count(), filled, "pages stay with the map");
        for &key in &keys {
            map.insert(key, key).unwrap();
        }
        assert!(
            pages.in_use_count() <= filled + 2,
            "{} > {filled}",
            pages.in_use_count()
        );
    }
    pages.assert_all_released();
}

/// An insert the allocator has no page for, the root page of a new tree
/// or any page a split takes, changes nothing and gives the entry back;
/// given the pages, it goes in. The pages a refused split took serve its
/// next try, and every page goes back to the allocator when the map drops.
#[test]
fn refused_pages_leave_the_map_unchanged() {
    type Key = [u8; 32];
    let key = |number: u64| {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&number.to_be_bytes());
        key
    };
    let pages = TestPages::<Key, u64>::new();
    let rng = TestRng::new(113);
    {
        let mut map = BBTreeMap::<Key, u64, Auto, &TestPages<Key, u64>>::new_in(&pages);
        let mut std_map = BTreeMap::new();
        // Until a full internal root splits: a page for the leaf's right
        // half, one for the root's, and one for the root node, which moves
        // out of the root page as the tree grows. Internal nodes hold more
        // edges where pointers are narrower.
        let mut most_pages = 0;
        while most_pages < 3 {
            assert!(std_map.len() < 100_000, "no internal root split");
            let number = rng.below_u64(1 << 40);
            let in_use = pages.in_use_count();
            let mut entry = (key(number), number);
            // A try that a budget of `budget` pages refuses leaves the pages
            // it took with the map, for the next try.
            for budget in 0.. {
                pages.set_budget(Some(budget));
                match map.insert(entry.0, entry.1) {
                    Ok(replaced) => {
                        assert_eq!(replaced, std_map.insert(entry.0, entry.1));
                        break;
                    }
                    Err(refused) => {
                        entry = refused.into_entry();
                        assert_eq!(entry, (key(number), number));
                        assert_eq!(map.get_copied(&entry.0), None);
                        map.check_invariants(std_map.len());
                    }
                }
            }
            most_pages = most_pages.max(pages.in_use_count() - in_use);
        }
        for (key, &value) in &std_map {
            assert_eq!(map.get_copied(key), Some(value));
        }
    }
    pages.assert_all_released();
}

/// A refused `get_or_insert` or `update_or_insert` runs nothing; a batch
/// inserts the entries it has pages for, and gives the others back. A root
/// leaf's split, given one of the two pages it takes, keeps that one for
/// its next try.
#[test]
fn refused_pages_fail_their_own_entries() {
    let pages = TestPages::<u64, u64>::new();
    {
        let map = BBTreeMap::<u64, u64, Auto, &TestPages<u64, u64>>::new_in(&pages);
        pages.set_budget(Some(0));
        let refused = map.get_or_insert(0, 0, |_, _| panic!("no entry to run on"));
        assert_eq!(refused.map_err(InsertError::into_entry), Err((0, 0)));
        let refused = map.update_or_insert(0, 0, |_, _, _| panic!("no entry to run on"));
        assert_eq!(refused.map_err(InsertError::into_entry), Err((0, 0)));
        assert!(map.is_empty());
        pages.set_budget(None);
        map.insert(0, 0).unwrap();
        // Fills the root leaf.
        pages.set_budget(Some(0));
        let mut full_at = 1;
        while map.insert(full_at, full_at).is_ok() {
            full_at += 1;
        }
        pages.set_budget(Some(1));
        let [replaced, refused] = map.insert_many([(0, 10), (full_at, full_at)]);
        assert_eq!(replaced.unwrap(), Some(0));
        assert_eq!(
            refused.map_err(InsertError::into_entry),
            Err((full_at, full_at))
        );
        let in_use = pages.in_use_count();
        pages.set_budget(None);
        assert_eq!(map.insert(full_at, full_at).unwrap(), None);
        assert_eq!(pages.in_use_count(), in_use + 1);
        for number in 1..=full_at {
            assert_eq!(map.get_copied(&number), Some(number));
        }
        assert_eq!(map.get_copied(&0), Some(10));
    }
    pages.assert_all_released();
}

/// Keys that share leading bytes give the tree a shared prefix (D50),
/// which keys without them shrink, ascending ones appended to the last leaf
/// (D45); removing every key, and inserting keys of another range, starts
/// it anew.
#[test]
fn what_keys_share_grows_shrinks_and_starts_anew() {
    let mut map = BBTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    let high = |number: u64| (0xAB << 40) + number * 7;
    for number in 0..3_000 {
        assert_eq!(
            map.insert(high(number), number).unwrap(),
            std_map.insert(high(number), number)
        );
    }
    assert!(
        map.shared_prefix_len() >= 3,
        "dense keys share their high bytes"
    );
    map.check_invariants(std_map.len());
    let rng = TestRng::new(101);
    for number in random_numbers(&rng, 2_000, 1 << 20) {
        assert_eq!(
            map.insert(number, number).unwrap(),
            std_map.insert(number, number)
        );
    }
    assert_eq!(map.shared_prefix_len(), 2, "both ranges start 0x0000");
    map.check_invariants(std_map.len());
    for (key, value) in &std_map {
        assert_eq!(map.get_cloned(key), Some(*value));
    }
    let keys: Vec<u64> = std_map.keys().copied().collect();
    for key in &keys {
        assert_eq!(map.remove(key), std_map.remove(key));
    }
    map.check_invariants(0);
    for number in 0..2_000 {
        let key = (0xCD << 40) + number;
        assert_eq!(
            map.insert(key, number).unwrap(),
            std_map.insert(key, number)
        );
    }
    assert!(map.shared_prefix_len() >= 3, "an emptied tree starts anew");
    map.check_invariants(std_map.len());

    let mut strings = BBTreeMap::<String, usize>::new();
    for number in 0..3_000 {
        strings
            .insert(format!("/usr/share/doc/{number:08}"), number)
            .unwrap();
    }
    assert!(strings.shared_prefix_len() >= "/usr/share/doc/0000".len());
    strings.insert(String::from("/etc"), 0).unwrap();
    assert_eq!(strings.shared_prefix_len(), 1);
    strings.check_invariants(3_001);
}

#[test]
fn a_small_map_matches_std() {
    let rng = TestRng::new(94);
    let (count, absent) = if cfg!(miri) { (60, 10) } else { (300, 50) };
    let keys = random_numbers(&rng, count, 1_000);
    let absent = random_numbers(&rng, absent, 1_000);
    assert_matches_std(&keys, &absent);
    let empty = BBTreeMap::<u64, u64>::new();
    assert_eq!(empty.get_cloned(&1), None);
    assert_eq!(empty.get_copied(&1), None);
    assert!(!empty.contains_key(&1));
    assert_eq!(empty.get_many_copied([&1, &2]), [None, None]);
    assert_eq!(empty.get_many_cloned([&1, &2]), [None, None]);
    assert_eq!(empty.get_many_cloned::<u64, 0>([]), []);
}

/// Threads insert their own keys while others read: every key a writer
/// has inserted is there for every reader from then on, and at the end
/// every key holds its writer's value, in a well-formed tree.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_find_what_other_threads_inserted() {
    let (writers, per_writer) = (4, 20_000);
    let map = BBTreeMap::<u64, u64>::new();
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
                    assert_eq!(map.insert(key, key ^ writer as u64).unwrap(), None);
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
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_racing_on_the_same_keys_agree() {
    let (threads, key_count) = (4, 10_000);
    let map = BBTreeMap::<String, (usize, u64)>::new();
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
                    map.insert(key.clone(), (thread, 0)).unwrap();
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

/// Threads remove their keys while others insert theirs and readers read
/// both: a key a writer has inserted is there until its remover takes it,
/// once; at the end the tree is well formed and holds what stayed.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_remove_while_others_insert() {
    let (threads, per_thread) = (4, 20_000);
    let map = BBTreeMap::<u64, u64>::new();
    let key = |thread: usize, index: usize| (index * threads + thread) as u64 * 0x9E37_79B9;
    // Half the keys are there before, for the removers.
    for thread in 0..threads {
        for index in (0..per_thread).step_by(2) {
            map.insert(key(thread, index), 1).unwrap();
        }
    }
    let barrier = Barrier::new(2 * threads);
    thread::scope(|scope| {
        for thread in 0..threads {
            let (map, barrier) = (&map, &barrier);
            // The odd keys go in.
            scope.spawn(move || {
                barrier.wait();
                for index in (1..per_thread).step_by(2) {
                    assert_eq!(map.insert(key(thread, index), 2).unwrap(), None);
                    assert_eq!(map.get_cloned(&key(thread, index)), Some(2));
                }
            });
            // The even keys go out, each found once.
            scope.spawn(move || {
                barrier.wait();
                let rng = TestRng::new(200 + thread as u64);
                let mut evens: Vec<usize> = (0..per_thread).step_by(2).collect();
                rng.shuffle(&mut evens);
                for index in evens {
                    assert_eq!(map.remove(&key(thread, index)), Some(1));
                    assert_eq!(map.get_cloned(&key(thread, index)), None);
                }
            });
        }
    });
    let mut map = map;
    map.check_invariants(threads * per_thread / 2);
    for thread in 0..threads {
        for index in 0..per_thread {
            let expected = (index % 2 == 1).then_some(2);
            assert_eq!(map.get_cloned(&key(thread, index)), expected);
        }
    }
}

/// Threads race to remove the same keys, and to put some back: each removal
/// that returns a value took one that was there, so the values taken add up
/// to what went in and didn't stay.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_racing_to_remove_take_each_entry_once() {
    let (threads, key_count) = (4, 20_000_u64);
    let map = BBTreeMap::<String, u64>::new();
    let name = |key: u64| format!("item/{key:06}");
    for key in 0..key_count {
        map.insert(name(key), 1).unwrap();
    }
    let barrier = Barrier::new(threads);
    let taken: Vec<u64> = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|thread| {
                let (map, barrier) = (&map, &barrier);
                scope.spawn(move || {
                    let rng = TestRng::new(300 + thread as u64);
                    let mut order: Vec<u64> = (0..key_count).collect();
                    rng.shuffle(&mut order);
                    barrier.wait();
                    let mut taken = 0;
                    for key in order {
                        taken += map.remove(&name(key)).unwrap_or(0);
                        // Every fourth key, this thread puts one back.
                        if key % 4 == thread as u64 {
                            map.insert(name(key), 1).unwrap();
                        }
                    }
                    taken
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    });
    let mut map = map;
    let left = (0..key_count)
        .filter(|&key| map.contains_key(&name(key)))
        .count() as u64;
    // Every key went in twice: before, and once put back by its thread.
    assert_eq!(taken.iter().sum::<u64>() + left, 2 * key_count);
    map.check_invariants(left as usize);
}

/// Threads each own a set of keys, interleaved with the others', and run
/// random inserts, removals, updates and lookups on them against a model of
/// their own: every answer must match it, while the others' operations
/// split and merge the same nodes and replace separators above them.
///
/// Races that need a narrow schedule escape it: it passed with a removal
/// that left the nodes below a replaced separator unchanged, which a
/// 12-thread release run with 400,000 operations a thread caught at once.
/// Those need controlled schedules (Shuttle, doc 09).
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_with_their_own_keys_match_their_models() {
    assert_threads_match_their_models(4, 100_000, 2_000, 400);
}

/// [`threads_with_their_own_keys_match_their_models`] with about a leaf's
/// worth of keys: the root leaf splits, and the tree lowers back to one
/// leaf in the root page, over and over, under the threads' descents.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_raise_and_lower_the_root_around_one_leaf() {
    assert_threads_match_their_models(4, 50_000, 16, 410);
}

/// `threads` threads, each with `keys_per_thread` keys of its own, running
/// `operations` random operations against their models, from `seed`.
fn assert_threads_match_their_models(
    threads: usize,
    operations: usize,
    keys_per_thread: u64,
    seed: u64,
) {
    let map = BBTreeMap::<u64, u64>::new();
    let barrier = Barrier::new(threads);
    thread::scope(|scope| {
        for thread in 0..threads {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(seed + thread as u64);
                let mut model = BTreeMap::new();
                barrier.wait();
                for step in 0..operations {
                    let number = rng.below(keys_per_thread as usize) as u64;
                    let key = (number * threads as u64 + thread as u64)
                        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    // Grow, churn, then shrink, so that merges come in runs.
                    let inserts = [70, 45, 15][step * 3 / operations];
                    let roll = rng.below(100);
                    if roll < inserts {
                        assert_eq!(
                            map.insert(key, step as u64).unwrap(),
                            model.insert(key, step as u64)
                        );
                    } else if roll < inserts + 30 {
                        assert_eq!(map.remove(&key), model.remove(&key));
                    } else if roll < inserts + 35 {
                        let expected = model.get_mut(&key).map(|value| {
                            *value += 1;
                            *value
                        });
                        let updated = map.update(&key, |_, value| {
                            *value += 1;
                            *value
                        });
                        assert_eq!(updated, expected);
                    } else {
                        assert_eq!(map.get_cloned(&key), model.get(&key).copied());
                    }
                }
                for (key, value) in &model {
                    assert_eq!(map.get_cloned(key), Some(*value));
                }
            });
        }
    });
}

/// Threads append interleaved ascending keys, which all go to the last
/// leaf, while another inserts keys that share none of their leading bytes,
/// which shrinks what every key shares along both borders, and readers
/// look up what the writers inserted: every key is there once inserted, and
/// at the end the tree is well formed and holds them all.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_append_while_another_shrinks_what_keys_share() {
    let (appenders, per_appender, outsiders) = (4, 20_000_u64, 5_000_u64);
    let mut map = BBTreeMap::<u64, u64>::new();
    let base = 1_u64 << 48;
    let barrier = Barrier::new(appenders as usize + 1);
    thread::scope(|scope| {
        let (map, barrier) = (&map, &barrier);
        for appender in 0..appenders {
            scope.spawn(move || {
                barrier.wait();
                for step in 0..per_appender {
                    let key = base + step * appenders + appender;
                    assert_eq!(map.insert(key, step).unwrap(), None);
                    let earlier = base + (step / 2) * appenders + appender;
                    assert!(map.contains_key(&earlier), "lost {earlier}");
                }
            });
        }
        scope.spawn(move || {
            let rng = TestRng::new(102);
            barrier.wait();
            for number in random_numbers(&rng, outsiders as usize, 1 << 32) {
                map.insert(number, number).unwrap();
                assert_eq!(map.get_cloned(&number), Some(number));
            }
        });
    });
    let rng = TestRng::new(102);
    let mut outside: Vec<u64> = random_numbers(&rng, outsiders as usize, 1 << 32);
    outside.sort_unstable();
    outside.dedup();
    map.check_invariants(outside.len() + (appenders * per_appender) as usize);
    for appender in 0..appenders {
        for step in 0..per_appender {
            assert_eq!(
                map.get_cloned(&(base + step * appenders + appender)),
                Some(step)
            );
        }
    }
}

/// Scans of random ranges, bounded and not, of integer and string keys,
/// visit what std's `range` does, in order, and stop where the closure
/// breaks.
#[test]
fn scans_match_std() {
    let rng = TestRng::new(103);
    let map = BBTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    for number in random_numbers(&rng, 20_000, 1 << 20) {
        map.insert(number, number * 3).unwrap();
        std_map.insert(number, number * 3);
    }
    let collect = |range: (Bound<u64>, Bound<u64>), reverse: bool| {
        let mut visited = Vec::new();
        let visit = |key: &u64, value: &u64| {
            visited.push((*key, *value));
            ControlFlow::Continue(())
        };
        let none: Option<()> = if reverse {
            map.scan_rev(range, visit)
        } else {
            map.scan(range, visit)
        };
        assert_eq!(none, None);
        visited
    };
    for _ in 0..300 {
        let (a, b) = (rng.below_u64(1 << 20), rng.below_u64(1 << 20));
        let (low, high) = (a.min(b), a.max(b));
        let bound = |key: u64, kind: usize| match kind {
            0 => Bound::Included(key),
            1 if low != high => Bound::Excluded(key),
            1 => Bound::Included(key),
            _ => Bound::Unbounded,
        };
        let range = (bound(low, rng.below(3)), bound(high, rng.below(3)));
        let expected: Vec<(u64, u64)> = std_map.range(range).map(|(&k, &v)| (k, v)).collect();
        assert_eq!(collect(range, false), expected, "{range:?}");
        let reversed: Vec<(u64, u64)> = expected.into_iter().rev().collect();
        assert_eq!(collect(range, true), reversed, "{range:?} in reverse");
    }
    // Breaking stops the scan, with what the closure broke with.
    let mut seen = 0;
    let tenth = map.scan(.., |key, _| {
        seen += 1;
        if seen == 10 {
            ControlFlow::Break(*key)
        } else {
            ControlFlow::Continue(())
        }
    });
    assert_eq!(tenth, std_map.keys().nth(9).copied());
    let mut seen = 0;
    let tenth_last = map.scan_rev(.., |key, _| {
        seen += 1;
        if seen == 10 {
            ControlFlow::Break(*key)
        } else {
            ControlFlow::Continue(())
        }
    });
    assert_eq!(tenth_last, std_map.keys().nth_back(9).copied());

    let strings = BBTreeMap::<String, usize>::new();
    let mut std_strings = BTreeMap::new();
    for (index, number) in random_numbers(&rng, 5_000, 1 << 30).into_iter().enumerate() {
        let key = format!("/srv/{}/{number:08x}", number % 3);
        strings.insert(key.clone(), index).unwrap();
        std_strings.insert(key, index);
    }
    for start in ["", "/srv/1/", "/srv/1/8", "/srv/2/f", "/zzz"] {
        let mut visited = Vec::new();
        strings.scan::<str, ()>((Bound::Included(start), Bound::Unbounded), |key, &value| {
            visited.push((key.clone(), value));
            ControlFlow::Continue(())
        });
        let expected: Vec<(String, usize)> = (std_strings
            .range::<str, _>((Bound::Included(start), Bound::Unbounded)))
        .map(|(k, &v)| (k.clone(), v))
        .collect();
        assert_eq!(visited, expected, "from {start:?}");
        let mut visited = Vec::new();
        strings.scan_rev::<str, ()>((Bound::Unbounded, Bound::Included(start)), |key, &value| {
            visited.push((key.clone(), value));
            ControlFlow::Continue(())
        });
        let expected: Vec<(String, usize)> = (std_strings
            .range::<str, _>((Bound::Unbounded, Bound::Included(start))))
        .rev()
        .map(|(k, &v)| (k.clone(), v))
        .collect();
        assert_eq!(visited, expected, "down from {start:?}");
    }
}

/// Copied firsts match std's first entry of a range, for every kind of
/// bound, bounds on keys the map holds included (ties, heads, separators),
/// and work on a map whose root is a leaf, an empty map, and byte-array
/// keys.
#[test]
fn copied_firsts_match_std() {
    let rng = TestRng::new(113);
    let map = BBTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    let numbers = random_numbers(&rng, 20_000, 1 << 20);
    for &number in &numbers {
        map.insert(number, number * 3).unwrap();
        std_map.insert(number, number * 3);
    }
    let first_in =
        |range: (Bound<u64>, Bound<u64>)| std_map.range(range).next().map(|(&k, &v)| (k, v));
    for _ in 0..3_000 {
        // Half the bounds are keys the map holds.
        let pick = || {
            if rng.below(2) == 0 {
                numbers[rng.below(numbers.len())]
            } else {
                rng.below_u64(1 << 20)
            }
        };
        let (a, b) = (pick(), pick());
        let (low, high) = (a.min(b), a.max(b));
        let bound = |key: u64, kind: usize| match kind {
            0 => Bound::Included(key),
            1 if low != high => Bound::Excluded(key),
            1 => Bound::Included(key),
            _ => Bound::Unbounded,
        };
        let range = (bound(low, rng.below(3)), bound(high, rng.below(3)));
        assert_eq!(map.first_copied(range), first_in(range), "{range:?}");
    }
    assert_eq!(
        map.first_copied(..),
        first_in((Bound::Unbounded, Bound::Unbounded))
    );
    assert_eq!(map.first_copied((1_u64 << 20)..), None);

    let small = BBTreeMap::<u64, u64>::new();
    assert_eq!(small.first_copied(..), None);
    for key in [5, 1, 3] {
        small.insert(key, key).unwrap();
    }
    assert_eq!(small.first_copied(..), Some((1, 1)));
    assert_eq!(small.first_copied(2..), Some((3, 3)));
    assert_eq!(small.first_copied(3..), Some((3, 3)));
    assert_eq!(
        small.first_copied((Bound::Excluded(3), Bound::Unbounded)),
        Some((5, 5))
    );
    assert_eq!(small.first_copied(6..), None);
    assert_eq!(small.first_copied(..=0), None);
    assert_eq!(small.first_copied(2..3), None);

    let arrays = BBTreeMap::<[u8; 16], u32>::new();
    let mut std_arrays = BTreeMap::new();
    for (index, number) in random_numbers(&rng, 5_000, 1 << 30).into_iter().enumerate() {
        let key = (u128::from(number) << 64 | u128::from(number % 7)).to_be_bytes();
        arrays.insert(key, index as u32).unwrap();
        std_arrays.insert(key, index as u32);
    }
    for _ in 0..500 {
        let start = (u128::from(rng.below_u64(1 << 30)) << 64).to_be_bytes();
        assert_eq!(
            arrays.first_copied(start..),
            std_arrays.range(start..).next().map(|(&k, &v)| (k, v))
        );
    }
    for (&key, &value) in std_arrays.iter().take(300) {
        assert_eq!(arrays.first_copied(key..), Some((key, value)));
        let after = (Bound::Excluded(key), Bound::Unbounded);
        assert_eq!(
            arrays.first_copied(after),
            std_arrays.range(after).next().map(|(&k, &v)| (k, v))
        );
    }
}

/// Copied firsts, while other threads insert and remove: the entry found
/// is at or after the bound, never past the first key that is present
/// throughout, and its value is its key's.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn copied_firsts_hold_while_others_change_the_map() {
    let (writers, seekers, steady) = (3, 3, 30_000_u64);
    let map = BBTreeMap::<u64, u64>::new();
    // Even keys stay for the whole test; odd ones come and go.
    for number in 0..steady {
        map.insert(number * 2, number).unwrap();
    }
    let barrier = Barrier::new(writers + seekers);
    thread::scope(|scope| {
        for writer in 0..writers {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(900 + writer as u64);
                barrier.wait();
                for _ in 0..100_000 {
                    let key = rng.below_u64(steady) * 2 + 1;
                    if rng.below(2) == 0 {
                        map.insert(key, key).unwrap();
                    } else {
                        map.remove(&key);
                    }
                }
            });
        }
        for seeker in 0..seekers {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(1000 + seeker as u64);
                barrier.wait();
                for _ in 0..200_000 {
                    let start = rng.below_u64(steady * 2);
                    let first_even = start.div_ceil(2) * 2;
                    let found = map.first_copied(start..);
                    match found {
                        Some((key, value)) => {
                            assert!(key >= start && key <= first_even, "{found:?} from {start}");
                            let expected = if key % 2 == 0 { key / 2 } else { key };
                            assert_eq!(value, expected, "the value of {key}");
                        }
                        None => assert!(first_even >= steady * 2, "nothing from {start}"),
                    }
                }
            });
        }
    });
}

/// Scans, in both orders, while other threads insert and remove: every key
/// present for the whole scan is visited exactly once, in the scan's order.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn scans_visit_every_steady_key_once_while_others_change_the_map() {
    let (writers, scanners, steady) = (3, 3, 30_000_u64);
    let map = BBTreeMap::<u64, u64>::new();
    // Even keys stay for the whole test; odd ones come and go.
    for number in 0..steady {
        map.insert(number * 2, number).unwrap();
    }
    let barrier = Barrier::new(writers + scanners);
    thread::scope(|scope| {
        for writer in 0..writers {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(500 + writer as u64);
                barrier.wait();
                for _ in 0..100_000 {
                    let key = rng.below_u64(steady) * 2 + 1;
                    if rng.below(2) == 0 {
                        map.insert(key, key).unwrap();
                    } else {
                        map.remove(&key);
                    }
                }
            });
        }
        for scanner in 0..scanners {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(600 + scanner as u64);
                barrier.wait();
                for round in 0..40 {
                    let start = rng.below_u64(steady * 2);
                    let (mut previous, mut evens) = (None, 0);
                    if round % 2 == 0 {
                        map.scan(start.., |&key, _| {
                            assert!(previous < Some(key), "{previous:?} then {key}");
                            previous = Some(key);
                            evens += u64::from(key % 2 == 0);
                            ControlFlow::<()>::Continue(())
                        });
                        assert_eq!(evens, steady - start.div_ceil(2), "from {start}");
                    } else {
                        map.scan_rev(..start, |&key, _| {
                            assert!(
                                previous.is_none_or(|previous| previous > key),
                                "{previous:?} then {key}"
                            );
                            previous = Some(key);
                            evens += u64::from(key % 2 == 0);
                            ControlFlow::<()>::Continue(())
                        });
                        assert_eq!(evens, start.div_ceil(2), "down from {start}");
                    }
                }
            });
        }
    });
}

/// Batches of lookups while other threads insert and remove: every key
/// present throughout is found, with its value, and a key that comes and
/// goes is found with its own value or not at all. With a map of a few
/// steady keys, the writers raise the root from a leaf and lower it again
/// under the lanes.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn batches_find_steady_keys_while_others_change_the_map() {
    for steady in [30_000_u64, 8] {
        let (writers, readers) = (3, 3);
        let map = BBTreeMap::<u64, u64>::new();
        // Even keys stay for the whole test; odd ones come and go.
        for number in 0..steady {
            map.insert(number * 2, number).unwrap();
        }
        let changing = steady.max(300);
        let barrier = Barrier::new(writers + readers);
        thread::scope(|scope| {
            for writer in 0..writers {
                let (map, barrier) = (&map, &barrier);
                scope.spawn(move || {
                    let rng = TestRng::new(700 + writer as u64);
                    barrier.wait();
                    for _ in 0..100_000 {
                        let key = rng.below_u64(changing) * 2 + 1;
                        if rng.below(2) == 0 {
                            map.insert(key, key).unwrap();
                        } else {
                            map.remove(&key);
                        }
                    }
                });
            }
            for reader in 0..readers {
                let (map, barrier) = (&map, &barrier);
                scope.spawn(move || {
                    let rng = TestRng::new(800 + reader as u64);
                    barrier.wait();
                    for _ in 0..20_000 {
                        let keys: [u64; 16] = core::array::from_fn(|lane| {
                            if lane % 2 == 0 {
                                rng.below_u64(steady) * 2
                            } else {
                                rng.below_u64(changing) * 2 + 1
                            }
                        });
                        let values = map.get_many_cloned(keys.each_ref());
                        for (key, value) in keys.into_iter().zip(values) {
                            if key % 2 == 0 {
                                assert_eq!(value, Some(key / 2), "steady key {key}");
                            } else {
                                assert!(value.is_none_or(|value| value == key), "{key}");
                            }
                        }
                    }
                });
            }
        });
        let mut map = map;
        let odd = (0..changing)
            .filter(|number| map.contains_key(&(number * 2 + 1)))
            .count();
        map.check_invariants(steady as usize + odd);
    }
}

/// Copies of values, single and batched, while other threads insert,
/// remove and update: a key present throughout is found, and every copy is
/// of its own key's value, never another entry's or a half-written one.
/// Keys are made from numbers, and each value carries its key's number in
/// its high half, while writers change its low half in place. Integers
/// compare as copies; short strings end within their inline keys, which
/// with their lengths settle them unlatched (D36); long ones don't.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn copies_are_of_their_own_entries_while_others_change_the_map() {
    assert_copies_are_of_their_own_entries(|number| number);
    assert_copies_are_of_their_own_entries(|number| format!("{number:x}"));
    assert_copies_are_of_their_own_entries(|number| format!("{number:x}, past its inline key"));
}

fn assert_copies_are_of_their_own_entries<K: Ord + Send + Sync>(key_of: impl Fn(u64) -> K + Sync)
where
    BBTreeMap<K, u64>: Default,
{
    let tagged = |number: u64, count: u64| number << 32 | count;
    for steady in [30_000_u64, 8] {
        let (writers, readers) = (3, 3);
        let map = BBTreeMap::<K, u64>::default();
        // Even numbers stay for the whole test; odd ones come and go.
        for number in (0..steady).map(|number| number * 2) {
            map.insert(key_of(number), tagged(number, 0)).unwrap();
        }
        let changing = steady.max(300);
        let barrier = Barrier::new(writers + readers);
        thread::scope(|scope| {
            for writer in 0..writers {
                let (map, barrier, key_of) = (&map, &barrier, &key_of);
                scope.spawn(move || {
                    let rng = TestRng::new(900 + writer as u64);
                    barrier.wait();
                    for round in 0..50_000 {
                        let number = rng.below_u64(changing) * 2 + 1;
                        match rng.below(3) {
                            0 => drop(map.insert(key_of(number), tagged(number, round)).unwrap()),
                            1 => drop(map.remove(&key_of(number))),
                            _ => {
                                let number = rng.below_u64(steady) * 2;
                                map.update(&key_of(number), |_, value| {
                                    *value = tagged(number, round);
                                });
                            }
                        }
                    }
                });
            }
            for reader in 0..readers {
                let (map, barrier, key_of) = (&map, &barrier, &key_of);
                scope.spawn(move || {
                    let rng = TestRng::new(1000 + reader as u64);
                    barrier.wait();
                    let number_at = |lane: usize| {
                        if lane.is_multiple_of(2) {
                            rng.below_u64(steady) * 2
                        } else {
                            rng.below_u64(changing) * 2 + 1
                        }
                    };
                    let check = |number: u64, value: Option<u64>| {
                        if number.is_multiple_of(2) {
                            assert!(value.is_some(), "steady number {number}");
                        }
                        if let Some(value) = value {
                            assert_eq!(value >> 32, number, "a copy of another entry's value");
                        }
                    };
                    for _ in 0..10_000 {
                        let numbers: [u64; 16] = core::array::from_fn(number_at);
                        let keys = numbers.map(&key_of);
                        for (number, value) in numbers
                            .into_iter()
                            .zip(map.get_many_copied(keys.each_ref()))
                        {
                            check(number, value);
                        }
                        let number = number_at(rng.below(2));
                        check(number, map.get_copied(&key_of(number)));
                        assert!(!number.is_multiple_of(2) || map.contains_key(&key_of(number)));
                    }
                });
            }
        });
        let mut map = map;
        let odd = (0..changing)
            .filter(|number| map.contains_key(&key_of(number * 2 + 1)))
            .count();
        map.check_invariants(steady as usize + odd);
    }
}

/// Batched inserts and `update_or_insert`s, in turns, answer as std's
/// inserts and entries one at a time, across splits, repeated keys within a
/// batch, ascending runs that append, and keys that shrink what every key
/// shares.
#[test]
fn batched_inserts_match_std() {
    fn check<K: Ord + Clone + core::fmt::Debug, const N: usize>(keys: &[K])
    where
        BBTreeMap<K, usize>: Default,
    {
        let mut map = BBTreeMap::<K, usize>::default();
        let mut std_map = BTreeMap::new();
        for (batch, chunk) in keys.chunks(N).enumerate() {
            // Each batch ends with its first key again.
            let entries: [(K, usize); N] = core::array::from_fn(|lane| {
                let at = if lane == N - 1 { 0 } else { lane % chunk.len() };
                (chunk[at].clone(), batch * N + lane)
            });
            if batch % 2 == 0 {
                let expected = entries
                    .clone()
                    .map(|(key, value)| std_map.insert(key, value));
                assert_eq!(
                    map.insert_many(entries).map(Result::unwrap),
                    expected,
                    "batch {batch}"
                );
                continue;
            }
            // Adds each value to the stored one.
            let expected = entries
                .clone()
                .map(|(key, value)| match std_map.entry(key) {
                    Entry::Occupied(mut entry) => {
                        *entry.get_mut() += value;
                        Some((entry.key().clone(), *entry.get()))
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(value);
                        None
                    }
                });
            let ours = map.update_or_insert_many(entries, |key, sum, value| {
                *sum += value;
                (key.clone(), *sum)
            });
            assert_eq!(ours.map(Result::unwrap), expected, "batch {batch}");
        }
        map.check_invariants(std_map.len());
        for (key, &value) in &std_map {
            assert_eq!(map.get_copied(key), Some(value));
        }
    }
    let rng = TestRng::new(211);
    let numbers = random_numbers(&rng, 60_000, 1 << 20);
    check::<u64, 16>(&numbers);
    check::<u64, 3>(&numbers);
    let ascending: Vec<u64> = (0..40_000).collect();
    check::<u64, 16>(&ascending);
    let strings: Vec<String> = numbers.iter().map(|number| format!("{number:x}")).collect();
    check::<String, 16>(&strings);
    // Short keys first share their first bytes; then keys that lack them.
    let shrinking: Vec<String> = (0..30_000_u64)
        .map(|number| format!("shared/{number}"))
        .chain((0..30_000_u64).map(|number| format!("{number}")))
        .collect();
    check::<String, 16>(&shrinking);
}

/// Threads insert their own keys in batches while others look them up:
/// each key a writer has inserted is there from then on, and at the end
/// every key holds its writer's value, in a well-formed tree.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_insert_batches_while_others_look_up() {
    let (writers, readers, per_writer) = (4, 2, 40_000_u64);
    let map = BBTreeMap::<u64, u64>::new();
    // A tree deep enough for lanes from the start.
    for number in 0..20_000 {
        map.insert((number * 1_000_003 % (1 << 40)) | (1 << 41), number)
            .unwrap();
    }
    let barrier = Barrier::new(writers + readers);
    thread::scope(|scope| {
        for writer in 0..writers {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(1100 + writer as u64);
                let mut keys: Vec<u64> = (0..per_writer)
                    .map(|number| number * writers as u64 + writer as u64)
                    .collect();
                rng.shuffle(&mut keys);
                barrier.wait();
                for chunk in keys.as_chunks::<16>().0 {
                    assert_eq!(
                        map.insert_many(chunk.map(|key| (key, key * 3)))
                            .map(Result::unwrap),
                        [None; 16]
                    );
                    for &key in chunk {
                        assert_eq!(map.get_copied(&key), Some(key * 3), "{key} after its batch");
                    }
                }
            });
        }
        for reader in 0..readers {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(1200 + reader as u64);
                barrier.wait();
                for _ in 0..200_000 {
                    let key = rng.below_u64(per_writer * writers as u64);
                    assert!(
                        map.get_copied(&key).is_none_or(|value| value == key * 3),
                        "{key}"
                    );
                }
            });
        }
    });
    let mut map = map;
    map.check_invariants(20_000 + (per_writer * writers as u64) as usize);
    for key in 0..per_writer * writers as u64 {
        assert_eq!(map.get_copied(&key), Some(key * 3));
    }
}

/// What the map's owner does through `&mut` and by value matches std's map:
/// `get_mut`, iterating with values to change from either end, moving the
/// entries out, `clear`, `extend` and `collect`, `is_empty` and `Debug`.
#[test]
fn owners_match_std() {
    let rng = TestRng::new(223);
    let numbers = random_numbers(&rng, 50_000, 1 << 20);
    let mut map: BBTreeMap<u64, u64> = numbers.iter().map(|&number| (number, number)).collect();
    let mut std_map: BTreeMap<u64, u64> = numbers.iter().map(|&number| (number, number)).collect();
    for number in numbers.iter().step_by(7) {
        *map.get_mut(number).expect("a key the map holds") += 1;
        *std_map.get_mut(number).expect("a key the map holds") += 1;
    }
    assert_eq!(map.get_mut(&(1 << 21)), None);
    for ((key, value), (std_key, std_value)) in map.iter_mut().zip(std_map.iter_mut()) {
        assert_eq!((key, *value), (std_key, *std_value));
        *value *= 3;
        *std_value *= 3;
    }
    let mut ends = map.iter_mut();
    for _ in 0..1_000 {
        let (back, _) = ends.next_back().expect("entries left");
        let (front, _) = ends.next().expect("entries left");
        assert!(front < back);
    }
    drop(ends);
    map.check_invariants(std_map.len());
    assert!(map.into_iter().eq(std_map.clone()));

    let mut map: BBTreeMap<String, usize> = std_map
        .keys()
        .map(|number| (format!("{number:x}"), 0))
        .collect();
    map.extend((0..1_000_usize).map(|number| (format!("{number:x}"), number)));
    let mut expected: BTreeMap<String, usize> = std_map
        .keys()
        .map(|number| (format!("{number:x}"), 0))
        .collect();
    expected.extend((0..1_000_usize).map(|number| (format!("{number:x}"), number)));
    assert!(!map.is_empty());
    let half: Vec<_> = map.into_iter().take(expected.len() / 2).collect();
    assert!(
        half.iter().eq(expected
            .iter()
            .take(expected.len() / 2)
            .map(|(k, &v)| (k.clone(), v))
            .collect::<Vec<_>>()
            .iter())
    );

    let mut map = BBTreeMap::from([(2_u64, "b"), (1, "a")]);
    assert_eq!(format!("{map:?}"), r#"{1: "a", 2: "b"}"#);
    map.clear();
    assert!(map.is_empty());
    assert_eq!(format!("{map:?}"), "{}");
    map.insert(3, "c").unwrap();
    assert_eq!(map.get_copied(&3), Some("c"));
    map.check_invariants(1);
}

/// `get_or_insert` leaves an entry the map holds as it is, and runs its
/// closure on the stored entry, as std's `entry().or_insert` does, across
/// splits.
#[test]
fn get_or_insert_matches_std() {
    let rng = TestRng::new(107);
    let mut map = BBTreeMap::<String, usize>::new();
    let mut std_map = BTreeMap::new();
    for (position, number) in random_numbers(&rng, 30_000, 1 << 14)
        .into_iter()
        .enumerate()
    {
        let key = format!("{number:05}");
        let stored = map
            .get_or_insert(key.clone(), position, |stored, &value| {
                (stored.clone(), value)
            })
            .unwrap();
        let expected = *std_map.entry(key.clone()).or_insert(position);
        assert_eq!(stored, (key, expected));
    }
    map.check_invariants(std_map.len());
    for (key, &value) in &std_map {
        assert_eq!(map.get_cloned(key), Some(value));
    }
}

/// `update_or_insert` changes an entry the map holds, with the value it was
/// given, or inserts one, as std's `entry` does, across splits.
#[test]
fn update_or_insert_matches_std() {
    let rng = TestRng::new(109);
    let mut map = BBTreeMap::<String, usize>::new();
    let mut std_map = BTreeMap::new();
    for (position, number) in random_numbers(&rng, 30_000, 1 << 14)
        .into_iter()
        .enumerate()
    {
        let key = format!("{number:05}");
        let ours = map
            .update_or_insert(key.clone(), position, |stored, sum, position| {
                *sum += position;
                (stored.clone(), *sum)
            })
            .unwrap();
        let theirs = match std_map.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                *entry.get_mut() += position;
                Some((key, *entry.get()))
            }
            Entry::Vacant(entry) => {
                entry.insert(position);
                None
            }
        };
        assert_eq!(ours, theirs);
    }
    map.check_invariants(std_map.len());
    for (key, &value) in &std_map {
        assert_eq!(map.get_cloned(key), Some(value));
    }
}

/// Threads that count the same keys with `update_or_insert`, half of them
/// in batches, miss none of each other's counts: of each key, one thread
/// inserts it, and the others see each count after that once.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_counting_the_same_keys_miss_no_count() {
    let (threads, key_count) = (4, 20_000);
    let map = BBTreeMap::<u64, usize>::new();
    let barrier = Barrier::new(threads);
    let seen: Vec<Vec<usize>> = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|thread| {
                let (map, barrier) = (&map, &barrier);
                scope.spawn(move || {
                    let rng = TestRng::new(110 + thread as u64);
                    let mut order: Vec<u64> = (0..key_count).collect();
                    rng.shuffle(&mut order);
                    let add_one = |_: &u64, count: &mut usize, one| {
                        *count += one;
                        *count
                    };
                    let mut seen = std::vec![0; key_count as usize];
                    barrier.wait();
                    for chunk in order.as_chunks::<16>().0 {
                        let counts = if thread % 2 == 0 {
                            chunk.map(|key| map.update_or_insert(key, 1, add_one))
                        } else {
                            map.update_or_insert_many(chunk.map(|key| (key, 1)), add_one)
                        };
                        for (&key, count) in chunk.iter().zip(counts) {
                            seen[key as usize] = count.unwrap().unwrap_or(1);
                        }
                    }
                    seen
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    });
    let mut map = map;
    map.check_invariants(key_count as usize);
    for key in 0..key_count {
        assert_eq!(map.get_copied(&key), Some(threads), "key {key}");
        let mut counts: Vec<usize> = seen.iter().map(|seen| seen[key as usize]).collect();
        counts.sort_unstable();
        assert_eq!(counts, (1..=threads).collect::<Vec<_>>(), "key {key}");
    }
}

/// Threads racing to `get_or_insert` the same keys all see the first
/// thread's entry, the one the map keeps.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_racing_to_get_or_insert_agree() {
    let (threads, key_count) = (4, 20_000);
    let map = BBTreeMap::<u64, usize>::new();
    let barrier = Barrier::new(threads);
    let seen: Vec<Vec<usize>> = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|thread| {
                let (map, barrier) = (&map, &barrier);
                scope.spawn(move || {
                    let rng = TestRng::new(108 + thread as u64);
                    let mut order: Vec<u64> = (0..key_count).collect();
                    rng.shuffle(&mut order);
                    let mut seen = std::vec![usize::MAX; key_count as usize];
                    barrier.wait();
                    for key in order {
                        seen[key as usize] =
                            map.get_or_insert(key, thread, |_, &value| value).unwrap();
                    }
                    seen
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    });
    let mut map = map;
    map.check_invariants(key_count as usize);
    for key in 0..key_count {
        let stored = map.get_cloned(&key).expect("every key was inserted");
        for thread_seen in &seen {
            assert_eq!(thread_seen[key as usize], stored, "key {key}");
        }
    }
}

/// `remove_if` removes an entry only if its condition holds of it, from
/// leaves that keep their minimum, and from ones that rebalance.
#[test]
fn remove_if_removes_what_its_condition_holds_of() {
    let rng = TestRng::new(109);
    let mut map = BBTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    for number in random_numbers(&rng, 30_000, 1 << 20) {
        map.insert(number, number % 7).unwrap();
        std_map.insert(number, number % 7);
    }
    let keys: Vec<u64> = std_map.keys().copied().collect();
    for key in keys {
        let removed = map.remove_if(&key, |_, &value| value % 2 == 0);
        let expected = std_map
            .get(&key)
            .filter(|value| *value % 2 == 0)
            .map(|&value| (key, value));
        if expected.is_some() {
            std_map.remove(&key);
        }
        assert_eq!(removed, expected, "key {key}");
    }
    map.check_invariants(std_map.len());
    for (key, &value) in &std_map {
        assert_eq!(map.get_cloned(key), Some(value));
    }
}

/// `range_cloned` hands out what std's `range` does, from both ends, in
/// any interleaving, and stops where the ends meet.
#[test]
fn cloned_ranges_match_std() {
    let rng = TestRng::new(110);
    let map = BBTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    for number in random_numbers(&rng, 20_000, 1 << 20) {
        map.insert(number, number * 5).unwrap();
        std_map.insert(number, number * 5);
    }
    for _ in 0..300 {
        let (a, b) = (rng.below_u64(1 << 20), rng.below_u64(1 << 20));
        let (low, high) = (a.min(b), a.max(b));
        let bound = |key: u64, kind: usize| match kind {
            0 => Bound::Included(key),
            1 if low != high => Bound::Excluded(key),
            1 => Bound::Included(key),
            _ => Bound::Unbounded,
        };
        let range = (bound(low, rng.below(3)), bound(high, rng.below(3)));
        let mut cloned = map.range_cloned(range);
        let mut expected = std_map.range(range);
        // Mostly one end at a time, so that copies get long.
        let back_share = rng.below(4);
        loop {
            let (got, want) = if rng.below(4) < back_share {
                (
                    cloned.next_back(),
                    expected.next_back().map(|(&k, &v)| (k, v)),
                )
            } else {
                (cloned.next(), expected.next().map(|(&k, &v)| (k, v)))
            };
            assert_eq!(got, want, "{range:?}");
            if want.is_none() {
                break;
            }
        }
    }
}

/// An iterator from `range_cloned` hands out a key inserted ahead of it,
/// in a leaf it copied entries of, and no key removed ahead of it.
#[test]
fn cloned_ranges_see_what_changes_ahead_of_them() {
    let map = BBTreeMap::<u64, u64>::new();
    for number in 0..10_000 {
        map.insert(number * 4, number).unwrap();
    }
    for (taken, from_back) in [(1, false), (5, false), (30, false), (5, true), (30, true)] {
        let mut iter = map.range_cloned(..);
        let last = (0..taken)
            .map(|_| {
                if from_back {
                    iter.next_back()
                } else {
                    iter.next()
                }
            })
            .last()
            .flatten()
            .expect("an entry")
            .0;
        let (inserted, removed) = if from_back {
            (last - 2, last - 4)
        } else {
            (last + 2, last + 4)
        };
        map.insert(inserted, 1).unwrap();
        map.remove(&removed);
        let next = |iter: &mut RangeCloned<'_, u64, u64, Auto, Global, u64, _>| {
            if from_back {
                iter.next_back()
            } else {
                iter.next()
            }
        };
        assert_eq!(next(&mut iter).map(|(key, _)| key), Some(inserted));
        let after = next(&mut iter).map(|(key, _)| key);
        let expected = if from_back { last - 8 } else { last + 8 };
        assert_eq!(after, Some(expected), "after taking {taken}");
        map.remove(&inserted);
        map.insert(removed, removed / 4).unwrap();
    }
}

/// Iterators from `range_cloned`, from both ends, while other threads
/// insert and remove: every key present all along is handed out exactly
/// once, in order.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn cloned_ranges_hand_out_every_steady_key_once_while_others_change_the_map() {
    let (writers, readers, steady) = (3, 3, 30_000_u64);
    let map = BBTreeMap::<u64, u64>::new();
    for number in 0..steady {
        map.insert(number * 2, number).unwrap();
    }
    let barrier = Barrier::new(writers + readers);
    thread::scope(|scope| {
        for writer in 0..writers {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(700 + writer as u64);
                barrier.wait();
                for _ in 0..100_000 {
                    let key = rng.below_u64(steady) * 2 + 1;
                    if rng.below(2) == 0 {
                        map.insert(key, key).unwrap();
                    } else {
                        map.remove(&key);
                    }
                }
            });
        }
        for reader in 0..readers {
            let (map, barrier) = (&map, &barrier);
            scope.spawn(move || {
                let rng = TestRng::new(800 + reader as u64);
                barrier.wait();
                for round in 0..20 {
                    let start = rng.below_u64(steady * 2);
                    let keys: Vec<u64> = if round % 2 == 0 {
                        map.range_cloned(start..).map(|(key, _)| key).collect()
                    } else {
                        map.range_cloned(..start)
                            .rev()
                            .map(|(key, _)| key)
                            .collect()
                    };
                    let ordered = keys.windows(2).all(|pair| {
                        if round % 2 == 0 {
                            pair[0] < pair[1]
                        } else {
                            pair[0] > pair[1]
                        }
                    });
                    assert!(ordered, "round {round} from {start}");
                    let evens = keys.iter().filter(|&&key| key % 2 == 0).count() as u64;
                    let expected = if round % 2 == 0 {
                        steady - start.div_ceil(2)
                    } else {
                        start.div_ceil(2)
                    };
                    assert_eq!(evens, expected, "round {round} from {start}");
                }
            });
        }
    });
}

/// Each step of a `range_cloned` iterator, from either end, hands out the
/// successor of the key it handed out last in the map as it is then, with
/// keys inserted and removed around it between steps: copies it still holds,
/// and where its last scan stopped, it uses only while their leaves are
/// unchanged.
#[test]
fn each_cloned_range_step_hands_out_the_current_successor() {
    for seed in 0..8 {
        let rng = TestRng::new(120 + seed);
        let map = BBTreeMap::<u64, u64>::new();
        let mut std_map = BTreeMap::new();
        for number in random_numbers(&rng, 3_000, 4_000) {
            map.insert(number, number).unwrap();
            std_map.insert(number, number);
        }
        let from_back = seed % 2 == 1;
        let mut iter = map.range_cloned(..);
        let mut last: Option<u64> = None;
        loop {
            let expected = match (last, from_back) {
                (None, false) => std_map.keys().next().copied(),
                (None, true) => std_map.keys().next_back().copied(),
                (Some(last), false) => std_map.range(last + 1..).next().map(|(&k, _)| k),
                (Some(last), true) => std_map.range(..last).next_back().map(|(&k, _)| k),
            };
            let got = if from_back {
                iter.next_back()
            } else {
                iter.next()
            };
            assert_eq!(
                got.map(|(key, _)| key),
                expected,
                "seed {seed} after {last:?}"
            );
            let Some(key) = expected else { break };
            last = Some(key);
            for _ in 0..rng.below(4) {
                let key = rng.below_u64(4_000);
                if rng.below(2) == 0 {
                    map.insert(key, key).unwrap();
                    std_map.insert(key, key);
                } else {
                    map.remove(&key);
                    std_map.remove(&key);
                }
            }
        }
    }
}
