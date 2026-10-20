//! Per-node prefix stripping (`design/05-fence-key.md`) at the map level: that
//! nodes get prefixes however a tree is made, and keep them fitting their
//! fences, or along the borders the tree's shared prefix (D50), through
//! every change. The invariant checker verifies that each prefix is what
//! its node's fences share, and that every key has the shared prefix; these
//! tests make sure it has long prefixes to verify, fences that move far, and
//! shared prefixes that shrink.

#![forbid(unsafe_code)]

use std::format;
use std::string::String;
use std::vec::Vec;

use crate::inline_key::{Auto, InlineKeyStrategy, WithInlineKey};
use crate::tree::test_support::TestRng;
use crate::unsync::map::BBTreeMap;

/// The bytes every key of [`shared_prefix_key`] starts with.
const SHARED: &str = "shared/prefix/of/many/keys/";

/// A key that starts with [`SHARED`], then eight digits.
pub(super) fn shared_prefix_key(number: usize) -> String {
    format!("{SHARED}{number:08}")
}

/// How many keys a test map holds: enough for a tree three levels tall.
fn key_count() -> usize {
    if cfg!(miri) { 300 } else { 20_000 }
}

/// Panics unless the map is valid and every node, along the borders too
/// (D50), skips at least the bytes all its keys share.
fn assert_prefixes_cover_shared_bytes<S: InlineKeyStrategy<String>>(
    map: &BBTreeMap<String, usize, S>,
) {
    map.check_invariants();
    assert!(map.inspect().shared_prefix().starts_with(SHARED.as_bytes()));
    let prefix_lens = map.node_prefix_lens();
    let short = prefix_lens
        .iter()
        .filter(|&&len| len < SHARED.len())
        .count();
    assert_eq!(
        short,
        0,
        "{short} of {} nodes skip fewer than the {} shared bytes",
        prefix_lens.len(),
        SHARED.len()
    );
}

/// Random inserts into a map with strategy `strategy`, seeded with `seed`.
fn random_inserts_give_nodes_prefixes_with<S: InlineKeyStrategy<String>>(strategy: S, seed: u64) {
    let rng = TestRng::new(seed);
    let mut map = BBTreeMap::with_strategy(strategy);
    for value in 0..key_count() {
        map.insert(shared_prefix_key(rng.below(1 << 30)), value);
    }
    assert!(map.leaf_depth() >= 2 || cfg!(miri));
    assert_prefixes_cover_shared_bytes(&map);
}

/// With the default strategy, and with keys that opt in through their own
/// `InlineKey` impl.
#[test]
fn random_inserts_give_nodes_prefixes() {
    random_inserts_give_nodes_prefixes_with(Auto, 81);
    random_inserts_give_nodes_prefixes_with(WithInlineKey, 82);
}

/// Ascending inserts split the rightmost node every time, and leave its
/// left half behind for good: it gets its prefix as it splits off.
/// Descending inserts do the same at the left end.
#[test]
fn sequential_inserts_give_nodes_prefixes() {
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

/// Integers get prefixes through their big-endian bytes: keys below 2^24
/// share their first 5 bytes, and every node skips them.
#[test]
fn integer_keys_get_prefixes() {
    let rng = TestRng::new(84);
    let mut map: BBTreeMap<u64, usize, WithInlineKey> = BBTreeMap::with_strategy(WithInlineKey);
    for value in 0..key_count() {
        map.insert(rng.below(1 << 24) as u64, value);
    }
    map.check_invariants();
    assert!(map.inspect().shared_prefix().len() >= 5);
    let prefix_lens = map.node_prefix_lens();
    let short = prefix_lens.iter().filter(|&&len| len < 5).count();
    assert_eq!(
        short,
        0,
        "{short} of {} nodes skip fewer than 5 bytes",
        prefix_lens.len()
    );
}

/// Removals refit prefixes where fences move, and cuts give the nodes along
/// the cut the tree's shared prefix (D50): the trees stay valid, and lookups
/// right, throughout.
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

/// Keys that share 96 bytes, of which the tree keeps 64 (D50): queries that
/// end inside them or differ within them, before the 64th byte or past it,
/// go before or after every key, or down the borders. Inserting them
/// shrinks the shared prefix; removals, cuts and rebuilds keep it right,
/// including for embedded zeros.
#[test]
fn queries_outside_the_shared_prefix_match_std() {
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

    // Distinct lengths can have identical zero-padded encodings. The shared
    // prefix is bounded by actual bytes, even when a fence prefix is not.
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

/// Heap strings keep 4 bytes of their windows in internal nodes, and 8 in
/// leaves (D43): a window an internal node hands out, as a separator comes
/// down into a leaf or a leaf's fence moves, is read again from its key, for
/// all 8 bytes. The keys share seven bytes, so that only the eighth sets
/// their windows apart at prefix 0, where the nodes along the borders take
/// them: one key shares none, which leaves the tree no shared prefix (D50).
#[test]
fn windows_internal_nodes_hand_out_are_read_again_from_their_keys() {
    use std::collections::BTreeMap;

    let mut actual = BBTreeMap::new();
    let mut expected = BTreeMap::new();
    let mut keys = (0..2048)
        .map(|index| format!("abc/def{:04x}/tail\0", (index * 40503) % 65536))
        .chain([String::from("z")])
        .collect::<Vec<_>>();
    TestRng::new(93).shuffle(&mut keys);
    for (index, key) in keys.iter().enumerate() {
        actual.insert(key.clone(), index);
        expected.insert(key.clone(), index);
    }
    actual.check_invariants();
    let root = actual.inspect().root().expect("the map has a root");
    let mut leftmost_leaf = root;
    while let Some(child) = leftmost_leaf.child(0) {
        leftmost_leaf = child;
    }
    assert_eq!((root.prefix_len(), root.inline_key_width()), (0, Some(4)));
    let leaf_window = (leftmost_leaf.prefix_len(), leftmost_leaf.inline_key_width());
    assert_eq!(leaf_window, (0, Some(8)));
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

/// Fixed-width keys whose encodings all start with the same bytes: dense
/// integers and byte arrays behind a shared head, which the tree keeps as
/// its shared prefix (D50), the prefix of its border nodes. Lookups past
/// either end, keys that differ inside the shared bytes, inserts that
/// shrink them, and every structural change must agree with std.
fn dense_fixed_width_keys_match_std<K: Ord + Copy + core::fmt::Debug>(key: impl Fn(u64) -> K) {
    use std::collections::BTreeMap;

    let rng = TestRng::new(0xD33);
    let mut order: Vec<u64> = (0..6000).collect();
    rng.shuffle(&mut order);
    let mut expected = BTreeMap::new();
    let mut actual = BBTreeMap::new();
    for (value, &index) in order.iter().enumerate() {
        assert_eq!(
            actual.insert(key(index), value),
            expected.insert(key(index), value)
        );
    }
    actual.check_invariants();
    let mut queries: Vec<K> = (0..7000).map(&key).collect();
    for shift in [8, 16, 24, 32, 40, 48, 56, 63] {
        queries.push(key(1 << shift));
        queries.push(key((1 << shift) | 17));
        queries.push(key(u64::MAX >> (63 - shift)));
    }
    queries.push(key(u64::MAX));
    for query in &queries {
        assert_eq!(actual.get(query), expected.get(query));
        assert_eq!(
            actual.range(query..).take(3).collect::<Vec<_>>(),
            expected.range(query..).take(3).collect::<Vec<_>>()
        );
    }
    let separators: Vec<K> = actual.separator_keys().into_iter().copied().collect();
    for separator in separators.iter().step_by(3) {
        assert_eq!(actual.remove(separator), expected.remove(separator));
        actual.check_invariants();
    }
    // Appends down the right border, then keys that shrink the shared bytes.
    for (value, query) in queries.iter().enumerate() {
        assert_eq!(actual.insert(*query, value), expected.insert(*query, value));
    }
    actual.check_invariants();
    for index in (0..6000).step_by(2) {
        assert_eq!(actual.remove(&key(index)), expected.remove(&key(index)));
    }
    actual.check_invariants();
    let cut = key(3001);
    let mut actual_upper = actual.split_off(&cut);
    let mut expected_upper = expected.split_off(&cut);
    actual.check_invariants();
    actual_upper.check_invariants();
    actual.append(&mut actual_upper);
    expected.append(&mut expected_upper);
    let actual = actual.clone();
    actual.check_invariants();
    assert!(actual.iter().eq(expected.iter()));
    for query in &queries {
        assert_eq!(actual.get(query), expected.get(query));
    }
}

#[test]
fn dense_integer_keys_match_std() {
    dense_fixed_width_keys_match_std(|index| index);
    dense_fixed_width_keys_match_std(|index| u128::from(index) | (7 << 100));
    dense_fixed_width_keys_match_std(|index| (index as i64).wrapping_sub(3000));
}

#[test]
fn byte_arrays_behind_a_shared_head_match_std() {
    dense_fixed_width_keys_match_std(|index| {
        let mut key = [0x5A; 16];
        key[8..].copy_from_slice(&index.to_be_bytes());
        key
    });
    dense_fixed_width_keys_match_std(|index| {
        let mut key = [0; 32];
        key[..6].copy_from_slice(&0x0199_AABB_CCDDu64.to_be_bytes()[2..]);
        key[6..14].copy_from_slice(&index.to_be_bytes());
        key
    });
}

/// Keys that share less shrink the shared prefix, whichever way they go in:
/// one by one, appended in order by a build, or joined from another tree;
/// removals leave it. Every node along the borders follows it (D50).
#[test]
fn keys_that_share_less_shrink_the_shared_prefix() {
    let shared_len = |map: &BBTreeMap<String, usize>| map.inspect().shared_prefix().len();
    let mut map: BBTreeMap<String, usize> = (0..key_count())
        .map(|number| (shared_prefix_key(number), number))
        .collect();
    let digits_shared = format!("{:08}", key_count() - 1)
        .bytes()
        .take_while(|&digit| digit == b'0')
        .count();
    assert_eq!(shared_len(&map), SHARED.len() + digits_shared);
    // A key past the last, sharing fewer digits.
    map.insert(format!("{SHARED}9"), 0);
    map.check_invariants();
    assert_eq!(shared_len(&map), SHARED.len());
    // A key before the first, ending inside the shared bytes.
    map.insert(String::from("shared/"), 0);
    map.check_invariants();
    assert_eq!(shared_len(&map), "shared/".len());
    // Removals leave what the rest still share.
    map.remove("shared/");
    map.check_invariants();
    assert_eq!(shared_len(&map), "shared/".len());
    for number in 0..key_count() {
        assert_eq!(map.get(&shared_prefix_key(number)), Some(&number));
    }
    assert_eq!(map.get("shared/"), None);
    assert_eq!(map.get("a"), None);
    assert_eq!(map.get("z"), None);

    // A build whose keys share less as they go: clusters that differ in
    // their last digit, then their first.
    let built: BBTreeMap<String, usize> = (0..if cfg!(miri) { 4 } else { 40 })
        .flat_map(|cluster| (0..100).map(move |number| clustered_key(cluster, number)))
        .zip(0..)
        .collect();
    built.check_invariants();
    // "000/" to "039/", or under Miri, to "003/".
    assert_eq!(shared_len(&built), if cfg!(miri) { 2 } else { 1 });

    // Joining trees that share different bytes keeps what both share.
    let mut lower: BBTreeMap<String, usize> = (0..500)
        .map(|number| (clustered_key(1, number), number))
        .collect();
    let mut upper: BBTreeMap<String, usize> = (0..500)
        .map(|number| (clustered_key(2, number), number))
        .collect();
    lower.append(&mut upper);
    lower.check_invariants();
    assert_eq!(shared_len(&lower), 2);
    let mut upper = lower.split_off(&clustered_key(1, 250));
    lower.check_invariants();
    upper.check_invariants();
    assert_eq!((shared_len(&lower), shared_len(&upper)), (2, 2));
    let mut empty = BBTreeMap::new();
    upper.append(&mut empty);
    upper.check_invariants();
}

/// Integers past the shared high bytes, either way, shrink them, as does a
/// key whose encoding differs in its sign bit.
#[test]
fn integers_past_the_shared_bytes_shrink_them() {
    let mut map: BBTreeMap<i64, usize> =
        (0..key_count() as i64).map(|number| (number, 0)).collect();
    let shared_len = |map: &BBTreeMap<i64, usize>| map.inspect().shared_prefix().len();
    assert!(shared_len(&map) >= 5);
    assert_eq!(map.get(&-1), None);
    assert_eq!(map.get(&i64::MAX), None);
    map.insert(1 << 40, 1);
    map.check_invariants();
    assert_eq!(shared_len(&map), 2);
    map.insert(-1, 2);
    map.check_invariants();
    assert_eq!(shared_len(&map), 0);
    assert_eq!(map.first_key_value(), Some((&-1, &2)));
    assert_eq!(map.range(..0).count(), 1);
    assert_eq!(map.range(1 << 39..).count(), 1);
}
