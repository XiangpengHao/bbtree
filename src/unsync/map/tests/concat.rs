//! Joining trees (`tree/join.rs`), through [`Tree::concat`], which puts
//! back together the pieces `extract_if` cuts: on trees of every pair of
//! leaf depths up to 2, with roots and borders of every fill, so that every way
//! a join goes comes up. The shorter root is absorbed by the taller tree's
//! border node, or hangs beside it, splitting the border up to a new root,
//! and then, if short of its minimum, takes keys over from its neighbour, or
//! gives them; on both sides, between leaves and between internal nodes.
//! Each result is checked whole: its structure, its prefixes, and its
//! entries in order.

#![forbid(unsafe_code)]

use core::fmt::Debug;
use std::collections::BTreeSet;
use std::string::String;
use std::vec::Vec;

use crate::allocator::Global;
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::root::Tree;
use crate::tree::test_support::{FromNumber, TestRng};
use crate::unsync::map::BBTreeMap;

/// A map of the keys made from `numbers`, which ascend, each with its number
/// as its value, in one of four shapes: built whole, into full nodes but
/// along the right border; the lower or upper side of a cut at a random
/// number, whose border along the cut holds its minimum; or inserted one by
/// one in random order, its nodes split in halves.
fn random_shape<K: Ord + FromNumber>(rng: &TestRng, numbers: &[usize]) -> BBTreeMap<K, usize> {
    let entries = || {
        numbers
            .iter()
            .map(|&number| (K::from_number(number), number))
    };
    match rng.below(4) {
        0 => entries().collect(),
        shape @ (1 | 2) => {
            let mut map: BBTreeMap<K, usize> = entries().collect();
            let Some(&last) = numbers.last() else {
                return map;
            };
            let upper = map.split_off(&K::from_number(rng.below(last + 2)));
            if shape == 1 { map } else { upper }
        }
        _ => {
            let mut shuffled: Vec<(K, usize)> = entries().collect();
            rng.shuffle(&mut shuffled);
            let mut map = BBTreeMap::new();
            map.extend(shuffled);
            map
        }
    }
}

/// How many entries a piece gets: none, a few, or up to `max`, spread over
/// the orders of magnitude, so that leaves at every depth up to 2 come up.
fn random_len(rng: &TestRng, max: usize) -> usize {
    match rng.below(4) {
        0 => rng.below(3),
        _ => {
            let magnitude = 1 << rng.below(max.ilog2() as usize + 1);
            rng.below(magnitude + 1)
        }
    }
}

/// Joins two random pieces, the left one's numbers all below the right
/// one's, `trials` times, checking each result against the numbers; and,
/// with the sizes of a native run, that every pair of leaf depths up to 2
/// came up.
fn concat_random_pieces<K>(seed: u64, trials: usize, max_len: usize)
where
    K: Ord + Clone + Debug + FromNumber,
    Auto: InlineKeyStrategy<K>,
{
    let rng = TestRng::new(seed);
    let mut leaf_depths = BTreeSet::new();
    for _ in 0..trials {
        let left_numbers: Vec<usize> = (0..random_len(&rng, max_len)).map(|i| 2 * i).collect();
        let right_start = 2 * left_numbers.len() + 2;
        let right_numbers: Vec<usize> = (0..random_len(&rng, max_len))
            .map(|i| right_start + 2 * i)
            .collect();
        let mut left = random_shape::<K>(&rng, &left_numbers);
        let mut right = random_shape::<K>(&rng, &right_numbers);
        let expected: Vec<usize> = left.values().chain(right.values()).copied().collect();
        leaf_depths.insert((left.leaf_depth(), right.leaf_depth()));
        let joined = BBTreeMap {
            tree: Tree::concat(left.tree.take(), right.tree.take(), &Global),
            length: expected.len(),
            alloc: Global,
        };
        (left.length, right.length) = (0, 0);
        joined.check_invariants();
        assert!(joined.values().copied().eq(expected.iter().copied()));
        let keys = expected.iter().map(|&number| K::from_number(number));
        assert!(joined.keys().cloned().eq(keys));
    }
    if !cfg!(miri) {
        for pair in (0..=2).flat_map(|left| (0..=2).map(move |right| (left, right))) {
            assert!(
                leaf_depths.contains(&pair),
                "no join of leaf depths {pair:?}"
            );
        }
    }
}

/// String keys, with prefixes along their fences, and shared prefixes
/// (D50), which every join shrinks to what both trees share. The fuzz
/// targets join trees of every key type through `extract_if`.
#[test]
fn concat_joins_string_trees_of_every_shape() {
    let (trials, max_len) = if cfg!(miri) { (30, 200) } else { (300, 20_000) };
    concat_random_pieces::<String>(32, trials, max_len);
}
