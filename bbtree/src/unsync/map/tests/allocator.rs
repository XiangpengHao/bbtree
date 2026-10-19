//! The allocator a map is made with: every page the map uses comes from it
//! and goes back to it, through every operation that allocates or frees one,
//! and a copy or a split-off part takes a clone of it. The test allocator
//! panics on a page from elsewhere, one given back twice or with another
//! layout, and counts those never given back.

#![forbid(unsafe_code)]

use std::vec::Vec;

use crate::inline_key::Auto;
use crate::tree::node::page::TestPages;
use crate::tree::test_support::TestRng;
use crate::unsync::map::{BBTreeMap, Entry};

/// A 64-byte key: 14 entries per node of either kind, so that few entries
/// make a tall tree, which Miri can afford.
type Key = [u64; 8];

fn key(number: u64) -> Key {
    [number; 8]
}

/// A map on pages from `pages`.
type MapIn<'p> = BBTreeMap<Key, u64, Auto, &'p TestPages<Key, u64>>;

/// A map of `numbers`' keys, inserted in that order, on pages from `pages`.
fn map_in<'p>(pages: &'p TestPages<Key, u64>, numbers: &[u64]) -> MapIn<'p> {
    let mut map = BBTreeMap::new_in(pages);
    for &number in numbers {
        assert_eq!(map.insert(key(number), number), None);
    }
    map
}

#[test]
fn every_page_comes_from_and_goes_back_to_the_map_allocator() {
    let count = if cfg!(miri) { 200 } else { 2000 };
    let mut numbers: Vec<u64> = (0..count).collect();
    TestRng::new(21).shuffle(&mut numbers);
    let pages = TestPages::<Key, u64>::new();
    {
        let mut map = map_in(&pages, &numbers);
        assert!(map.leaf_depth() >= 2);
        for number in &numbers[..numbers.len() / 3] {
            assert_eq!(map.remove(&key(*number)), Some(*number));
        }
        map.pop_first();
        map.pop_last();
        let Entry::Vacant(vacant) = map.entry(key(count)) else {
            panic!("a key past the others is vacant");
        };
        vacant.insert(count);
        map.last_entry().expect("the map has entries").remove();
        let mut upper = map.split_off(&key(count / 2));
        let copy = upper.clone();
        map.append(&mut upper);
        let extracted: Vec<(Key, u64)> = map.extract_if(.., |key, _| key[0] % 2 == 1).collect();
        assert!(!extracted.is_empty());
        map.retain(|key, _| key[0] % 3 != 0);
        map.check_invariants();
        let mut rest = copy.into_iter();
        rest.next();
        rest.next_back();
        drop(rest);
        assert!(pages.in_use_count() > 0);
        map.clear();
        map.insert(key(1), 1);
    }
    pages.assert_all_released();
}

/// `append` of maps with different allocators moves entries, not pages:
/// each map's pages go back to its own allocator. Into an empty map, the
/// maps swap, allocators too.
#[test]
fn append_keeps_each_page_with_its_own_allocator() {
    let count = if cfg!(miri) { 100 } else { 1000 };
    let (evens, odds): (Vec<u64>, Vec<u64>) = (0..count).partition(|number| number % 2 == 0);
    let (ours, theirs) = (TestPages::<Key, u64>::new(), TestPages::<Key, u64>::new());
    {
        let mut target = map_in(&ours, &evens);
        let mut other = map_in(&theirs, &odds);
        target.append(&mut other);
        assert_eq!(target.len(), usize::try_from(count).unwrap());
        target.check_invariants();
        other.insert(key(count), count);
        let mut empty = BBTreeMap::new_in(&theirs);
        empty.append(&mut target);
        assert!(target.is_empty());
        target.insert(key(count), count);
    }
    ours.assert_all_released();
    theirs.assert_all_released();
}
