//! Public batch contracts, including chunk boundaries and drop ownership.

use super::*;
use crate::BBTreeSet;
use crate::tree::node::page::TestPages;
use crate::tree::test_support::{DropLedger, TestRng};
use std::vec::Vec;

#[test]
fn empty_single_and_repeated_keys() {
    let mut map = BBTreeMap::<u64, u64>::new();
    assert_eq!(map.insert_many([]), []);
    assert_eq!(map.get_many::<u64, 0>([]), []);
    assert_eq!(map.insert_many([(1, 2)]), [None]);
    assert_eq!(
        map.insert_many([(1, 3), (2, 4), (1, 5)]),
        [Some(2), None, Some(3)]
    );
    assert_eq!(
        map.get_many([&1, &3, &1, &2]),
        [Some(&5), None, Some(&5), Some(&4)]
    );
    let mut set = BBTreeSet::new();
    assert_eq!(
        set.insert_many([1, 2, 1, 3, 2]),
        [true, true, false, true, false]
    );
    assert_eq!(
        set.contains_many([&1, &4, &1, &2]),
        [true, false, true, true]
    );
    assert_eq!(set.insert_many([]), []);
    assert_eq!(set.contains_many::<i32, 0>([]), []);
}

#[test]
fn extend_and_insert_batches_preserve_nodes_across_the_threshold() {
    let pages = TestPages::<u64, u64>::new();
    {
        let mut batch = BBTreeMap::new_in(&pages);
        let mut individual = BBTreeMap::new_in(&pages);
        let count = if cfg!(miri) { 150 } else { 45_000 };
        let mut numbers: Vec<u64> = (0..count).collect();
        TestRng::new(55).shuffle(&mut numbers);
        batch.extend(numbers.iter().map(|&key| (key, key)));
        for &key in &numbers {
            individual.insert(key, key);
        }
        batch.check_invariants();
        batch.check_same_nodes(&individual);
        assert!(batch.iter().eq(individual.iter()));
        for index in 0..16 {
            let entries = core::array::from_fn::<_, 8, _>(|lane| {
                let key = count + (index * 8 + lane) as u64;
                (key, key)
            });
            assert_eq!(
                batch.insert_many(entries),
                entries.map(|(key, value)| individual.insert(key, value))
            );
        }
        batch.check_invariants();
        batch.check_same_nodes(&individual);
        let queries = [count, 0, count + 1, count + 500, count, 42, 1, count - 1];
        assert_eq!(
            batch.get_many(queries.each_ref()),
            queries.map(|key| individual.get(&key))
        );
        // A high first key must not determine how the rest of a mixed
        // iterator is consumed. Each chunk may switch between append and
        // random insertion, and duplicates still retain the original key.
        let mut mixed: Vec<_> = (0..128).map(|index| (count + index + 500, index)).collect();
        TestRng::new(19).shuffle(&mut mixed);
        mixed[0] = (count + 1500, 1);
        mixed[17] = mixed[16];
        batch.extend(mixed.iter().copied());
        for entry in mixed {
            individual.insert(entry.0, entry.1);
        }
        batch.check_invariants();
        batch.check_same_nodes(&individual);
        assert!(batch.iter().eq(individual.iter()));
        batch.extend([(count, 9), (count, 10), (count + 1000, 11)]);
        for (key, value) in [(count, 9), (count, 10), (count + 1000, 11)] {
            individual.insert(key, value);
        }
        batch.check_same_nodes(&individual);
        assert!(batch.iter().eq(individual.iter()));
        let mut calls = 0;
        batch.extend(core::iter::from_fn(|| {
            calls += 1;
            match calls {
                1 => Some((count + 2000, 12)),
                2 => None,
                _ => panic!("extend must stop at the first None"),
            }
        }));
        individual.insert(count + 2000, 12);
        batch.check_same_nodes(&individual);
    }
    pages.assert_all_released();
}

#[test]
fn replaced_and_stored_values_drop_once() {
    let ledger = DropLedger::new();
    {
        let mut map = BBTreeMap::new();
        let entries = core::array::from_fn::<_, 16, _>(|index| (index % 3, ledger.track()));
        let old = map.insert_many(entries);
        assert_eq!(old.iter().filter(|old| old.is_some()).count(), 13);
        drop(old);
        map.check_invariants();
    }
    ledger.assert_all_dropped_once();
}
