//! `BCTreeMap` against std's `BTreeMap`, one thread at a time, and its
//! answers while threads share it.

use core::ops::{Bound, ControlFlow};
use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::sync::Barrier;
use std::thread;
use std::vec::Vec;

use super::{BCTreeMap, RangeCloned};
use crate::allocator::Global;
use crate::inline_key::Auto;
use crate::tree::node::page::TestPages;
use crate::tree::test_support::TestRng;

/// Inserts `keys` in order, each with its position as its value, into a
/// `BCTreeMap` and a `BTreeMap`, and checks that they agree on every
/// insert's answer, every key's value, keys they lack, and updates; and
/// that the tree is well formed after the inserts.
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
fn random_numbers(rng: &TestRng, count: usize, universe: u64) -> Vec<u64> {
    (0..count).map(|_| rng.below_u64(universe)).collect()
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
    let absent: Vec<String> = random_numbers(&rng, 500, 50_000)
        .into_iter()
        .map(path)
        .collect();
    assert_matches_std(&keys, &absent);
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
fn assert_removals_match_std<K: Ord + Clone + core::fmt::Debug>(
    keys: &[K],
    lag: usize,
    check_every: usize,
) where
    BCTreeMap<K, usize>: Default,
{
    let mut map = BCTreeMap::<K, usize>::default();
    let mut std_map = BTreeMap::new();
    let rng = TestRng::new(97);
    let mut order: Vec<usize> = (0..keys.len()).collect();
    rng.shuffle(&mut order);
    for (step, key) in keys.iter().enumerate() {
        assert_eq!(
            map.insert(key.clone(), step),
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
            map.insert(key.clone(), step),
            std_map.insert(key.clone(), step)
        );
    }
    map.check_invariants(std_map.len());
}

#[test]
fn integer_removals_match_std() {
    let rng = TestRng::new(98);
    let keys = random_numbers(&rng, 30_000, 1 << 40);
    assert_removals_match_std(&keys, 10_000, 997);
    let ascending: Vec<u64> = (0..20_000).collect();
    assert_removals_match_std(&ascending, 15_000, 991);
}

#[test]
fn string_and_array_removals_match_std() {
    let rng = TestRng::new(99);
    let path = |number: u64| format!("/usr/share/{}/{:x}", number % 7, number);
    let keys: Vec<String> = random_numbers(&rng, 10_000, 1 << 30)
        .into_iter()
        .map(path)
        .collect();
    assert_removals_match_std(&keys, 4_000, 499);
    let array = |number: u64| {
        let mut key = [0_u8; 32];
        key[24..].copy_from_slice(&number.to_be_bytes());
        key
    };
    let keys: Vec<[u8; 32]> = random_numbers(&rng, 10_000, 1 << 30)
        .into_iter()
        .map(array)
        .collect();
    assert_removals_match_std(&keys, 4_000, 499);
}

/// Pages that removals empty serve later splits: filling the map a second
/// time takes no more pages than the first.
#[test]
fn emptied_pages_serve_new_nodes() {
    let pages = TestPages::<u64, u64>::new();
    let rng = TestRng::new(100);
    let keys = random_numbers(&rng, if cfg!(miri) { 400 } else { 20_000 }, 1 << 40);
    {
        let map = BCTreeMap::<u64, u64, Auto, &TestPages<u64, u64>>::new_in(&pages);
        for &key in &keys {
            map.insert(key, key);
        }
        let filled = pages.in_use_count();
        for &key in &keys {
            map.remove(&key);
        }
        assert_eq!(pages.in_use_count(), filled, "pages stay with the map");
        for &key in &keys {
            map.insert(key, key);
        }
        assert!(
            pages.in_use_count() <= filled + 2,
            "{} > {filled}",
            pages.in_use_count()
        );
    }
    pages.assert_all_released();
}

/// Keys that share leading bytes give the tree a shared prefix (D50),
/// which keys without them shrink, ascending ones appended to the last leaf
/// (D45); removing every key, and inserting keys of another range, starts
/// it anew.
#[test]
fn what_keys_share_grows_shrinks_and_starts_anew() {
    let mut map = BCTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    let high = |number: u64| (0xAB << 40) + number * 7;
    for number in 0..3_000 {
        assert_eq!(
            map.insert(high(number), number),
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
        assert_eq!(map.insert(number, number), std_map.insert(number, number));
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
        assert_eq!(map.insert(key, number), std_map.insert(key, number));
    }
    assert!(map.shared_prefix_len() >= 3, "an emptied tree starts anew");
    map.check_invariants(std_map.len());

    let mut strings = BCTreeMap::<String, usize>::new();
    for number in 0..3_000 {
        strings.insert(format!("/usr/share/doc/{number:08}"), number);
    }
    assert!(strings.shared_prefix_len() >= "/usr/share/doc/0000".len());
    strings.insert(String::from("/etc"), 0);
    assert_eq!(strings.shared_prefix_len(), 1);
    strings.check_invariants(3_001);
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
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
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
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
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

/// Threads remove their keys while others insert theirs and readers read
/// both: a key a writer has inserted is there until its remover takes it,
/// once; at the end the tree is well formed and holds what stayed.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_remove_while_others_insert() {
    let (threads, per_thread) = (4, 20_000);
    let map = BCTreeMap::<u64, u64>::new();
    let key = |thread: usize, index: usize| (index * threads + thread) as u64 * 0x9E37_79B9;
    // Half the keys are there before, for the removers.
    for thread in 0..threads {
        for index in (0..per_thread).step_by(2) {
            map.insert(key(thread, index), 1);
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
                    assert_eq!(map.insert(key(thread, index), 2), None);
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
    let map = BCTreeMap::<String, u64>::new();
    let name = |key: u64| format!("item/{key:06}");
    for key in 0..key_count {
        map.insert(name(key), 1);
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
                            map.insert(name(key), 1);
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
    let map = BCTreeMap::<u64, u64>::new();
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
                        assert_eq!(map.insert(key, step as u64), model.insert(key, step as u64));
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
    let mut map = BCTreeMap::<u64, u64>::new();
    let base = 1_u64 << 48;
    let barrier = Barrier::new(appenders as usize + 1);
    thread::scope(|scope| {
        let (map, barrier) = (&map, &barrier);
        for appender in 0..appenders {
            scope.spawn(move || {
                barrier.wait();
                for step in 0..per_appender {
                    let key = base + step * appenders + appender;
                    assert_eq!(map.insert(key, step), None);
                    let earlier = base + (step / 2) * appenders + appender;
                    assert!(map.contains_key(&earlier), "lost {earlier}");
                }
            });
        }
        scope.spawn(move || {
            let rng = TestRng::new(102);
            barrier.wait();
            for number in random_numbers(&rng, outsiders as usize, 1 << 32) {
                map.insert(number, number);
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
    let map = BCTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    for number in random_numbers(&rng, 20_000, 1 << 20) {
        map.insert(number, number * 3);
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

    let strings = BCTreeMap::<String, usize>::new();
    let mut std_strings = BTreeMap::new();
    for (index, number) in random_numbers(&rng, 5_000, 1 << 30).into_iter().enumerate() {
        let key = format!("/srv/{}/{number:08x}", number % 3);
        strings.insert(key.clone(), index);
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

/// Scans, in both orders, while other threads insert and remove: every key
/// present for the whole scan is visited exactly once, in the scan's order.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn scans_visit_every_steady_key_once_while_others_change_the_map() {
    let (writers, scanners, steady) = (3, 3, 30_000_u64);
    let map = BCTreeMap::<u64, u64>::new();
    // Even keys stay for the whole test; odd ones come and go.
    for number in 0..steady {
        map.insert(number * 2, number);
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
                        map.insert(key, key);
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

/// `get_or_insert` leaves an entry the map holds as it is, and runs its
/// closure on the stored entry, as std's `entry().or_insert` does, across
/// splits.
#[test]
fn get_or_insert_matches_std() {
    let rng = TestRng::new(107);
    let mut map = BCTreeMap::<String, usize>::new();
    let mut std_map = BTreeMap::new();
    for (position, number) in random_numbers(&rng, 30_000, 1 << 14)
        .into_iter()
        .enumerate()
    {
        let key = format!("{number:05}");
        let stored = map.get_or_insert(key.clone(), position, |stored, &value| {
            (stored.clone(), value)
        });
        let expected = *std_map.entry(key.clone()).or_insert(position);
        assert_eq!(stored, (key, expected));
    }
    map.check_invariants(std_map.len());
    for (key, &value) in &std_map {
        assert_eq!(map.get_cloned(key), Some(value));
    }
}

/// Threads racing to `get_or_insert` the same keys all see the first
/// thread's entry, the one the map keeps.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_racing_to_get_or_insert_agree() {
    let (threads, key_count) = (4, 20_000);
    let map = BCTreeMap::<u64, usize>::new();
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
                        seen[key as usize] = map.get_or_insert(key, thread, |_, &value| value);
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
    let mut map = BCTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    for number in random_numbers(&rng, 30_000, 1 << 20) {
        map.insert(number, number % 7);
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
    let map = BCTreeMap::<u64, u64>::new();
    let mut std_map = BTreeMap::new();
    for number in random_numbers(&rng, 20_000, 1 << 20) {
        map.insert(number, number * 5);
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
    let map = BCTreeMap::<u64, u64>::new();
    for number in 0..10_000 {
        map.insert(number * 4, number);
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
        map.insert(inserted, 1);
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
        map.insert(removed, removed / 4);
    }
}

/// Iterators from `range_cloned`, from both ends, while other threads
/// insert and remove: every key present all along is handed out exactly
/// once, in order.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn cloned_ranges_hand_out_every_steady_key_once_while_others_change_the_map() {
    let (writers, readers, steady) = (3, 3, 30_000_u64);
    let map = BCTreeMap::<u64, u64>::new();
    for number in 0..steady {
        map.insert(number * 2, number);
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
                        map.insert(key, key);
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
        let map = BCTreeMap::<u64, u64>::new();
        let mut std_map = BTreeMap::new();
        for number in random_numbers(&rng, 3_000, 4_000) {
            map.insert(number, number);
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
                    map.insert(key, key);
                    std_map.insert(key, key);
                } else {
                    map.remove(&key);
                    std_map.remove(&key);
                }
            }
        }
    }
}
