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

/// Integers get prefixes through their big-endian bytes: keys below 2^24
/// share their first 5 bytes, and nodes off the borders skip them.
#[test]
fn integer_keys_get_prefixes() {
    let rng = TestRng::new(84);
    let mut map: BBTreeMap<u64, usize, WithInlineKey> = BBTreeMap::with_strategy(WithInlineKey);
    for value in 0..key_count() {
        map.insert(rng.below(1 << 24) as u64, value);
    }
    map.check_invariants();
    let prefix_lens = map.node_prefix_lens();
    let short = prefix_lens.iter().filter(|&&len| len < 5).count();
    let border_nodes = 2 * (map.height() + 1);
    assert!(
        short <= border_nodes,
        "{short} of {} nodes skip fewer than 5 bytes",
        prefix_lens.len()
    );
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

/// A local prefix is not a fence: short and differently prefixed queries
/// must still choose the correct edge. Separator replacement, cuts and
/// rebuilding must preserve this distinction, including embedded zeros.
#[test]
fn local_prefix_queries_and_mutations_match_std() {
    use std::collections::BTreeMap;
    use std::ops::Bound::{Included, Unbounded};

    let prefix = "path\0prefix/".repeat(8);
    let entries = (0..2048).map(|index| (format!("{prefix}{index:06}"), index));
    let mut expected: BTreeMap<_, _> = entries.clone().collect();
    let mut actual: BBTreeMap<_, _> = entries.collect();
    let mut queries = (0..=prefix.len())
        .map(|len| String::from(&prefix[..len]))
        .collect::<Vec<_>>();
    for index in [
        0,
        7,
        8,
        15,
        16,
        31,
        32,
        33,
        47,
        48,
        63,
        64,
        65,
        prefix.len() - 1,
    ] {
        for byte in *b"!~" {
            let mut query = prefix.as_bytes().to_vec();
            query[index] = byte;
            queries.push(String::from_utf8(query).unwrap());
        }
    }
    for query in &queries {
        assert_eq!(actual.get(query.as_str()), expected.get(query.as_str()));
        let range = (Included(query.as_str()), Unbounded);
        assert_eq!(
            actual.range::<str, _>(range).take(4).collect::<Vec<_>>(),
            expected.range::<str, _>(range).take(4).collect::<Vec<_>>()
        );
    }
    let separators = actual
        .separator_keys()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for key in separators.iter().step_by(3) {
        assert_eq!(actual.remove(key.as_str()), expected.remove(key.as_str()));
        actual.check_invariants();
    }
    for (value, key) in queries.into_iter().enumerate() {
        assert_eq!(
            actual.insert(key.clone(), value),
            expected.insert(key, value)
        );
        actual.check_invariants();
    }
    let cut = format!("{prefix}001000");
    let mut actual_upper = actual.split_off(cut.as_str());
    let mut expected_upper = expected.split_off(cut.as_str());
    actual.check_invariants();
    actual_upper.check_invariants();
    actual.append(&mut actual_upper);
    expected.append(&mut expected_upper);
    actual = actual.clone();
    actual.check_invariants();
    assert!(actual.iter().eq(expected.iter()));

    // Distinct lengths can have identical zero-padded encodings. A local
    // prefix is bounded by actual bytes, even when the fence prefix is not.
    let mut zeros = BBTreeMap::new();
    for length in 0..128 {
        zeros.insert(format!("a{}", "\0".repeat(length)), length);
    }
    zeros.check_invariants();
    for length in 0..128 {
        assert_eq!(
            zeros.get(format!("a{}", "\0".repeat(length)).as_str()),
            Some(&length)
        );
    }
}

/// A seven-byte shared prefix cannot activate the eight-byte local guard.
/// Its narrow parent must still export bytes four through seven when a
/// separator moves down or fills the window of a leaf whose fence shrinks.
#[test]
fn narrow_parent_windows_survive_demotion_and_fence_changes() {
    use std::collections::BTreeMap;

    let mut actual = BBTreeMap::new();
    let mut expected = BTreeMap::new();
    let mut keys = (0..2048)
        .map(|index| format!("abc/def{:04x}/tail\0", (index * 40503) % 65536))
        .collect::<Vec<_>>();
    TestRng::new(93).shuffle(&mut keys);
    for (index, key) in keys.iter().enumerate() {
        actual.insert(key.clone(), index);
        expected.insert(key.clone(), index);
    }
    actual.check_invariants();
    let separators = actual
        .separator_keys()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for key in separators.iter().step_by(2) {
        assert_eq!(actual.remove(key), expected.remove(key));
        actual.check_invariants();
    }
    for key in keys.iter().step_by(3) {
        assert_eq!(actual.remove(key), expected.remove(key));
    }
    actual.check_invariants();
    let mut upper = actual.split_off("abc/def8000");
    let mut expected_upper = expected.split_off("abc/def8000");
    actual.check_invariants();
    upper.check_invariants();
    actual.append(&mut upper);
    expected.append(&mut expected_upper);
    let copy = actual.clone();
    copy.check_invariants();
    assert!(copy.iter().eq(expected.iter()));
    for key in &keys {
        assert_eq!(copy.get(key.as_str()), expected.get(key.as_str()));
    }
}
