//! Length tags (D36): which of a lookup's candidates, the keys whose inline
//! keys equal its own, have its length. Every map here holds
//! [`OUTSIDE_KEY`], so that the tree keeps no shared prefix (D50) and the
//! other keys tie on their inline keys.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::format;
use std::iter;
use std::string::String;
use std::vec::Vec;

use crate::BBTreeMap;

/// A key that shares no byte with the others: their inline keys in the root
/// leaf, which takes its windows from the first byte, are all `shared-p`.
const OUTSIDE_KEY: &str = "z";

/// Whether the leaves here, `String` → `u64` and `String` → `usize`, keep
/// length tags: their layout has room for them on 64-bit targets, not on
/// 32-bit ones (D36). Lookups must be right either way.
const LENGTH_TAGGED: bool = cfg!(target_pointer_width = "64");

impl<V> BBTreeMap<String, V> {
    /// How many of `probe`'s candidates in the leaf its lookup ends in have
    /// its length tag: its length, truncated to 16 bits (D36); `None` if the
    /// leaf keeps no length tags.
    fn candidates_of_its_length(&self, probe: &str) -> Option<usize> {
        let trace = self.inspect().trace(probe);
        let leaf_step = trace.last().expect("the lookup reads a leaf");
        let tags: Option<Vec<u16>> = leaf_step
            .tied()
            .expect("the leaf has inline keys")
            .map(|index| leaf_step.node().length_tag(index))
            .collect();
        let tag = probe.len() as u16;
        tags.map(|tags| {
            tags.into_iter()
                .filter(|&candidate| candidate == tag)
                .count()
        })
    }
}

#[test]
fn two_length_candidates_check_hits_and_misses_on_either_side() {
    let mut map = BBTreeMap::<String, u64>::new();
    // Every probe below ties with every key but the outside one, and two of
    // them, with another key between them, have its length.
    for (index, key) in [
        "shared-prefix/a",
        "shared-prefix/c0",
        "shared-prefix/g11",
        "shared-prefix/t0",
        "shared-prefix/x111",
        OUTSIDE_KEY,
    ]
    .into_iter()
    .enumerate()
    {
        map.insert(String::from(key), index as u64);
    }
    assert_eq!(map.leaf_depth(), 0);
    map.check_invariants();
    let missing = ["shared-prefix/b0", "shared-prefix/m0", "shared-prefix/z0"];
    for probe in ["shared-prefix/c0", "shared-prefix/t0"]
        .iter()
        .chain(&missing)
    {
        assert_eq!(
            map.candidates_of_its_length(probe),
            LENGTH_TAGGED.then_some(2),
            "{probe}"
        );
    }
    assert_eq!(map.get("shared-prefix/c0"), Some(&1));
    assert_eq!(map.get("shared-prefix/t0"), Some(&3));
    for probe in missing {
        assert_eq!(map.get(probe), None);
        assert_eq!(map.get_mut(probe), None);
    }
    *map.get_mut("shared-prefix/c0").unwrap() += 10;
    *map.get_mut("shared-prefix/t0").unwrap() += 20;
    assert_eq!(map.get("shared-prefix/c0"), Some(&11));
    assert_eq!(map.get("shared-prefix/t0"), Some(&23));
}

/// Two candidates whose lengths differ by 65 536 have the same tag: equal
/// tags only let a lookup go on to compare the keys.
#[test]
fn two_length_candidates_confirm_modulo_collisions() {
    let mut map = BBTreeMap::<String, u64>::new();
    let short = String::from("shared-prefix/c0");
    let long = format!("shared-prefix/t0{}", "\0".repeat(65536));
    map.insert(short.clone(), 1);
    map.insert(String::from("shared-prefix/m11"), 2);
    map.insert(long.clone(), 3);
    map.insert(String::from(OUTSIDE_KEY), 4);
    map.check_invariants();
    let missing = [
        String::from("shared-prefix/t0"),
        format!("{short}{}", "\0".repeat(65536)),
        format!("{long}{}", "\0".repeat(65536)),
    ];
    for probe in [&short, &long].into_iter().chain(&missing) {
        assert_eq!(
            map.candidates_of_its_length(probe),
            LENGTH_TAGGED.then_some(2)
        );
    }
    assert_eq!(map.get(short.as_str()), Some(&1));
    assert_eq!(map.get(long.as_str()), Some(&3));
    for probe in &missing {
        assert_eq!(map.get(probe.as_str()), None);
    }
}

/// Lookups among keys that tie on their inline keys, whose lengths pick out
/// one candidate, two, or more; then through inserts that split, removals,
/// a cut, a join and a copy, which must keep every length tag right.
#[test]
fn length_collisions_and_mutations_preserve_exact_key_lookups() {
    // Tails of lengths that one key has, then two, three, and so on.
    let tails = (0..).flat_map(|len| iter::repeat_n(len, len + 1));
    let mut keys: Vec<String> = tails
        .take(18)
        .enumerate()
        .map(|(index, tail)| format!("shared-prefix/{index:04}/{}", "x".repeat(tail)))
        .collect();
    let mut actual = BBTreeMap::<String, usize>::new();
    let mut expected = BTreeMap::new();
    for (index, key) in keys.iter().enumerate() {
        actual.insert(key.clone(), index);
        expected.insert(key.clone(), index);
    }
    actual.insert(String::from(OUTSIDE_KEY), usize::MAX);
    expected.insert(String::from(OUTSIDE_KEY), usize::MAX);
    assert_eq!(actual.leaf_depth(), 0);
    let candidates: Vec<Option<usize>> = keys
        .iter()
        .map(|key| actual.candidates_of_its_length(key))
        .collect();
    if LENGTH_TAGGED {
        assert!(candidates.contains(&Some(1)) && candidates.contains(&Some(2)));
        assert!(candidates.iter().flatten().any(|&count| count > 2));
    } else {
        assert!(candidates.iter().all(Option::is_none));
    }
    let mut probes = Vec::new();
    for key in &keys {
        probes.push(key.clone());
        probes.push(format!("{key}missing"));
    }
    probes.push(format!("{}{}", keys[0], "\0".repeat(65536)));
    for probe in &probes {
        assert_eq!(actual.get(probe.as_str()), expected.get(probe.as_str()));
    }

    keys.extend(
        (18..512).map(|index| format!("shared-prefix/{index:04}/{}", "x".repeat(index % 17))),
    );
    for (index, key) in keys.iter().enumerate().skip(18) {
        actual.insert(key.clone(), index);
        expected.insert(key.clone(), index);
    }
    actual.check_invariants();
    for key in keys.iter().step_by(3) {
        assert_eq!(actual.remove(key.as_str()), expected.remove(key.as_str()));
    }
    let mut upper = actual.split_off("shared-prefix/0256");
    actual.check_invariants();
    upper.check_invariants();
    actual.append(&mut upper);
    actual = actual.clone();
    actual.check_invariants();
    for (key, value) in &expected {
        assert_eq!(actual.get(key.as_str()), Some(value));
    }
    assert_eq!(
        actual.iter().collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );

    // Keys of different lengths may have the same zero-padded inline key,
    // and the same tag modulo 65 536. Neither makes them equal.
    let mut zero_keys = BBTreeMap::<String, usize>::new();
    for (index, len) in [0, 1, 2, 65536].into_iter().enumerate() {
        zero_keys.insert("\0".repeat(len), index);
    }
    zero_keys.check_invariants();
    for (index, len) in [0, 1, 2, 65536].into_iter().enumerate() {
        assert_eq!(zero_keys.get("\0".repeat(len).as_str()), Some(&index));
    }
    assert_eq!(zero_keys.get("\0".repeat(65537).as_str()), None);
}
