//! Per-node prefix stripping (`doc/05-fence-key.md`) at the map level: that
//! nodes get prefixes however a tree is made, and keep them fitting their
//! fences through every change. The invariant checker verifies that each
//! prefix is what its node's fences share; these tests make sure it has long
//! prefixes to verify, and fences that move far.

#![forbid(unsafe_code)]

use std::format;
use std::string::String;
use std::vec::Vec;

use super::BBTreeMap;
use crate::inline_key::WithInlineKey;
use crate::node::test_support::TestRng;

/// The bytes every key of [`shared_prefix_key`] starts with.
const SHARED: &str = "shared/prefix/of/many/keys/";

/// A key that starts with [`SHARED`], then eight digits.
fn shared_prefix_key(number: usize) -> String {
    format!("{SHARED}{number:08}")
}

/// How many keys a test map holds: enough for a tree three levels tall.
fn key_count() -> usize {
    if cfg!(miri) { 300 } else { 20_000 }
}

/// Panics unless the map is valid and every node off its two borders,
/// where prefixes are 0, skips at least the bytes all its keys share.
fn assert_prefixes_cover_shared_bytes<S: crate::InlineKeyStrategy<String>>(
    map: &BBTreeMap<String, usize, S>,
) {
    map.check_invariants();
    let prefix_lens = map.node_prefix_lens();
    let short = prefix_lens
        .iter()
        .filter(|&&len| len < SHARED.len())
        .count();
    let border_nodes = 2 * (map.height() + 1);
    assert!(
        short <= border_nodes,
        "{short} of {} nodes skip fewer than the {} shared bytes",
        prefix_lens.len(),
        SHARED.len()
    );
}

#[test]
fn random_inserts_give_nodes_prefixes() {
    let rng = TestRng::new(81);
    let mut map = BBTreeMap::new();
    for value in 0..key_count() {
        map.insert(shared_prefix_key(rng.below(1 << 30)), value);
    }
    assert!(map.height() >= 2 || cfg!(miri));
    assert_prefixes_cover_shared_bytes(&map);
}

/// Ascending inserts split the rightmost node every time, and leave its
/// left half behind for good: it gets its prefix as it splits off.
#[test]
fn ascending_inserts_give_nodes_prefixes() {
    let mut map = BBTreeMap::new();
    for number in 0..key_count() {
        map.insert(shared_prefix_key(number), number);
    }
    assert_prefixes_cover_shared_bytes(&map);
    let mut descending = BBTreeMap::new();
    for number in (0..key_count()).rev() {
        descending.insert(shared_prefix_key(number), number);
    }
    assert_prefixes_cover_shared_bytes(&descending);
}

/// A built tree's nodes get their prefixes as they close; a copy keeps
/// them.
#[test]
fn built_and_cloned_trees_have_prefixes() {
    let map: BBTreeMap<String, usize> = (0..key_count())
        .map(|number| (shared_prefix_key(number), number))
        .collect();
    assert_prefixes_cover_shared_bytes(&map);
    let copy = map.clone();
    copy.check_invariants();
    assert_eq!(copy.node_prefix_lens(), map.node_prefix_lens());
}

/// Keys with their own `InlineKey` impl get prefixes through it.
#[test]
fn opted_in_keys_get_prefixes() {
    let rng = TestRng::new(82);
    let mut map: BBTreeMap<String, usize, WithInlineKey> = BBTreeMap::with_strategy(WithInlineKey);
    for value in 0..key_count() {
        map.insert(shared_prefix_key(rng.below(1 << 30)), value);
    }
    assert_prefixes_cover_shared_bytes(&map);
}

/// Removals refit prefixes where fences move, and cuts clear them along
/// the cut: the trees stay valid, and lookups right, throughout.
#[test]
fn removals_and_cuts_keep_prefixes_valid() {
    // Distinct numbers in a scattered order: an odd multiplier is a
    // bijection modulo a power of two.
    let numbers: Vec<usize> = (0..key_count())
        .map(|index| index.wrapping_mul(0x9E37_79B9) % (1 << 30))
        .collect();
    let mut map: BBTreeMap<String, usize> = numbers
        .iter()
        .map(|&number| (shared_prefix_key(number), number))
        .collect();
    for (step, &number) in numbers.iter().enumerate().step_by(3) {
        assert_eq!(map.remove(&shared_prefix_key(number)), Some(number));
        if step % 999 == 0 {
            map.check_invariants();
        }
    }
    map.check_invariants();
    for (step, &number) in numbers.iter().enumerate() {
        let expected = (step % 3 != 0).then_some(number);
        assert_eq!(map.get(&shared_prefix_key(number)).copied(), expected);
    }
    let cut = shared_prefix_key(1 << 29);
    let upper = map.split_off(&cut);
    map.check_invariants();
    upper.check_invariants();
    assert!(map.keys().all(|key| *key < cut));
    assert!(upper.keys().all(|key| *key >= cut));
}

/// A key in one of several clusters, each with a long prefix of its own:
/// a fence that moves from one cluster into the next changes what a node's
/// fences share by many bytes.
fn clustered_key(cluster: usize, number: usize) -> String {
    format!("{cluster:03}/{SHARED}{number:06}")
}

/// A cut fixes its borders by moving entries over from the nodes beside
/// them, whose fences move, across clusters too: both trees stay valid, in
/// a built tree, with full nodes, and in one made by random inserts.
#[test]
fn cuts_refit_the_nodes_beside_their_borders() {
    let rng = TestRng::new(83);
    let mut keys = Vec::new();
    for cluster in 0..if cfg!(miri) { 4 } else { 40 } {
        for number in 0..1 + rng.below(if cfg!(miri) { 40 } else { 300 }) {
            keys.push(clustered_key(cluster, number));
        }
    }
    let built: BBTreeMap<String, usize> = keys.iter().cloned().zip(0..).collect();
    let mut inserted = BBTreeMap::new();
    for _ in 0..keys.len() {
        let index = rng.below(keys.len());
        inserted.insert(keys[index].clone(), index);
    }
    for map in [&built, &inserted] {
        map.check_invariants();
        for _ in 0..if cfg!(miri) { 4 } else { 200 } {
            let cut = &keys[rng.below(keys.len())];
            let mut lower = map.clone();
            let upper = lower.split_off(cut);
            lower.check_invariants();
            upper.check_invariants();
        }
    }
}
