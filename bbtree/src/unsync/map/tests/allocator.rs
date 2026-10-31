//! The allocator a map is made with: every page the map uses comes from it
//! and goes back to it, through every operation that allocates or frees one,
//! and a copy or a split-off part takes a clone of it. The test allocator
//! panics on a page from elsewhere, one given back twice or with another
//! layout, and counts those never given back.

#![forbid(unsafe_code)]

use std::vec::Vec;

use crate::inline_key::Auto;
use crate::node_config::{Configured, NodeConfig};
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

/// `try_insert` takes the pages a split needs before the map changes: an
/// allocator that refuses one leaves the map holding the entries it held,
/// takes back every page taken for it, and the entry comes back. Inserts
/// refused at random agree with std's map's, once allowed.
#[test]
fn try_insert_refused_leaves_the_map_as_it_was() {
    let count = if cfg!(miri) { 300 } else { 3000 };
    let mut numbers: Vec<u64> = (0..count).collect();
    TestRng::new(22).shuffle(&mut numbers);
    let pages = TestPages::<Key, u64>::new();
    {
        let mut map: MapIn<'_> = BBTreeMap::new_in(&pages);
        let mut model = std::collections::BTreeMap::new();
        let mut refused = 0;
        for (index, &number) in numbers.iter().enumerate() {
            // A budget too small for some splits, none for the first insert.
            pages.set_budget(Some(index % 3));
            let in_use = pages.in_use_count();
            match map.try_insert(key(number), number) {
                Ok(old) => assert_eq!(old, model.insert(key(number), number)),
                Err(error) => {
                    refused += 1;
                    assert_eq!(error.into_entry(), (key(number), number));
                    assert_eq!(map.len(), model.len());
                    assert_eq!(map.get(&key(number)), None);
                    assert_eq!(pages.in_use_count(), in_use);
                    pages.set_budget(None);
                    assert_eq!(map.try_insert(key(number), number).ok(), Some(None));
                    model.insert(key(number), number);
                }
            }
        }
        pages.set_budget(None);
        assert!(refused > 0, "some splits were refused");
        assert_eq!(
            map.try_insert(key(numbers[0]), 0).ok(),
            Some(Some(numbers[0]))
        );
        model.insert(key(numbers[0]), 0);
        map.check_invariants();
        assert!(map.iter().eq(model.iter()));
    }
    pages.assert_all_released();
}

/// `from_sorted_in` builds on the allocator given the map that inserting
/// the same entries builds, keeping the last of equal keys.
#[test]
fn from_sorted_in_builds_what_inserts_build() {
    let count = if cfg!(miri) { 300 } else { 3000 };
    let numbers: Vec<u64> = (0..count).collect();
    let pages = TestPages::<Key, u64>::new();
    {
        let sorted = numbers
            .iter()
            .flat_map(|&number| [(key(number), 0), (key(number), number)]);
        let built: MapIn<'_> = BBTreeMap::from_sorted_in(sorted, Auto, &pages);
        built.check_invariants();
        let inserted = map_in(&pages, &numbers);
        assert_eq!(built.len(), inserted.len());
        assert!(built.iter().eq(inserted.iter()));
    }
    pages.assert_all_released();
}

#[test]
#[should_panic(expected = "entries not sorted by key")]
fn from_sorted_in_refuses_entries_out_of_order() {
    let pages = TestPages::<Key, u64>::new();
    let _: MapIn<'_> = BBTreeMap::from_sorted_in([(key(2), 2), (key(1), 1)], Auto, &pages);
}

/// Nodes of a 4 KiB frame's size, as a kernel's frame pool hands out.
struct FrameSized;

impl NodeConfig for FrameSized {
    const LEAF_PAGE_SIZE: usize = 4096;
    const INTERNAL_PAGE_SIZE: usize = 4096;
}

/// A map whose nodes are a frame's size asks its allocator for frames
/// alone: its root page is a page of the larger kind, a leaf's.
#[test]
fn a_map_of_frame_sized_nodes_asks_for_frames_alone() {
    let pages = TestPages::<u64, Key, Configured<Auto, FrameSized>>::new();
    {
        let mut map: BBTreeMap<u64, Key, Configured<Auto, FrameSized>, _> =
            BBTreeMap::with_strategy_in(Configured::new(), &pages);
        // Until internal nodes lie below the root, as many as the pointer
        // width makes it: pages of every kind.
        let mut number = 0;
        while map.is_empty() || map.leaf_depth() < 2 {
            assert_eq!(map.insert(number, key(number)), None);
            number += 1;
        }
    }
    let layouts = pages.kind_layouts();
    assert_eq!(layouts.len(), 2, "both kinds of page were asked for");
    for (kind, layout) in layouts {
        assert_eq!(layout.size(), 4096, "{kind:?} pages are frames");
    }
    pages.assert_all_released();
}
