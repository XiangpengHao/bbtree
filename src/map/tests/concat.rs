//! Joining trees (`tree/join.rs`), through [`Tree::concat`], which puts
//! back together the pieces `extract_if` cuts: on trees of every pair of
//! leaf depths up to 3, with roots and borders of every fill, so that every way
//! a join goes comes up. The shorter root is absorbed by the taller tree's
//! border node, or hangs beside it, splitting the border up to a new root,
//! and then, if short of its minimum, takes keys over from its neighbour, or
//! gives them; on both sides, between leaves and between internal nodes.
//! Each result is checked whole: its structure, its prefixes, and its
//! entries in order.

#![forbid(unsafe_code)]

use core::fmt::Debug;
use std::string::String;
use std::vec::Vec;

use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::map::BBTreeMap;
use crate::tree::root::Tree;
use crate::tree::test_support::{FromNumber, TestRng};

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
/// the orders of magnitude, so that leaves at every depth up to 3 come up.
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
/// one's, `trials` times, checking each result against the numbers.
fn concat_random_pieces<K>(seed: u64, trials: usize, max_len: usize)
where
    K: Ord + Clone + Debug + FromNumber,
    Auto: InlineKeyStrategy<K>,
{
    let rng = TestRng::new(seed);
    for _ in 0..trials {
        let left_numbers: Vec<usize> = (0..random_len(&rng, max_len)).map(|i| 2 * i).collect();
        let right_start = 2 * left_numbers.len() + 2;
        let right_numbers: Vec<usize> = (0..random_len(&rng, max_len))
            .map(|i| right_start + 2 * i)
            .collect();
        let mut left = random_shape::<K>(&rng, &left_numbers);
        let mut right = random_shape::<K>(&rng, &right_numbers);
        let expected: Vec<usize> = left.values().chain(right.values()).copied().collect();
        let joined = BBTreeMap {
            tree: Tree::concat(left.tree.take(), right.tree.take()),
            length: expected.len(),
        };
        (left.length, right.length) = (0, 0);
        joined.check_invariants();
        assert!(joined.values().copied().eq(expected.iter().copied()));
        let keys = expected.iter().map(|&number| K::from_number(number));
        assert!(joined.keys().cloned().eq(keys));
    }
}

#[test]
fn concat_joins_u64_trees_of_every_shape() {
    let (trials, max_len) = if cfg!(miri) {
        (30, 300)
    } else {
        (1000, 20_000)
    };
    concat_random_pieces::<u64>(31, trials, max_len);
}

/// String keys, with prefixes along their fences, and shared prefixes
/// (D50), which every join shrinks to what both trees share.
#[test]
fn concat_joins_string_trees_of_every_shape() {
    let (trials, max_len) = if cfg!(miri) {
        (30, 200)
    } else {
        (1000, 20_000)
    };
    concat_random_pieces::<String>(32, trials, max_len);
}
