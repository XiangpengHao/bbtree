//! Length tags (D36): which of a lookup's candidates, the keys whose inline
//! keys equal its own, have its length. The cases (`LengthTagCase`) are
//! shared with the concurrent map's tests; maps of their own here hold
//! [`OUTSIDE_KEY`], so that the tree keeps no shared prefix (D50) and the
//! other keys tie on their inline keys.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::format;
use std::iter;
use std::string::String;
use std::vec::Vec;

use crate::inline_key::Auto;
use crate::tree::test_support::LengthTagCase;
use crate::unsync::BBTreeMap;

/// A key that shares no byte with the others: their inline keys in the root
/// leaf, which takes its windows from the first byte, are all `shared-p`.
const OUTSIDE_KEY: &str = "z";

/// Whether the leaves here, `String` → `u64` and `String` → `usize`, keep
/// length tags: their layout has room for them on 64-bit targets, not on
/// 32-bit ones (D36). Lookups must be right either way.
const LENGTH_TAGGED: bool = cfg!(target_pointer_width = "64");

impl<V> BBTreeMap<String, V> {
    /// How many of `probe`'s candidates in the leaf its lookup ends in have
    /// its length tag; `None` if the leaf keeps no length tags.
    fn candidates_of_its_length(&self, probe: &str) -> Option<usize> {
        let trace = self.inspect().trace(probe);
        let leaf_step = trace.last().expect("the lookup reads a leaf");
        let tags: Option<Vec<u16>> = leaf_step
            .tied()
            .expect("the leaf has inline keys")
            .map(|index| leaf_step.node().length_tag(index))
            .collect();
        let tag = u16::try_from(probe.len()).unwrap_or(u16::MAX);
        tags.map(|tags| {
            tags.into_iter()
                .filter(|&candidate| candidate == tag)
                .count()
        })
    }
}

impl BBTreeMap<String, u64> {
    /// Asserts that every lookup of `case`'s probes answers as std's map
    /// does: hits, and misses whose candidates' tags match, by length or
    /// saturated.
    fn assert_lookups_match(&mut self, case: &LengthTagCase) {
        for probe in case.probes() {
            let probe = probe.as_str();
            let expected = case.value_of(probe).map(|index| index as u64);
            assert_eq!(self.get(probe).copied(), expected, "{probe:.20}");
            assert_eq!(
                self.get_key_value(probe).map(|(key, _)| key.as_str()),
                expected.map(|_| probe)
            );
            assert_eq!(self.contains_key(probe), expected.is_some());
            assert_eq!(self.get_mut(probe).map(|value| *value), expected);
        }
    }
}

/// A map of the case's keys, each to its index.
impl From<&LengthTagCase> for BBTreeMap<String, u64> {
    fn from(case: &LengthTagCase) -> Self {
        let mut map = Self::new();
        for (index, key) in case.keys().iter().enumerate() {
            map.insert(key.clone(), index as u64);
        }
        map.check_invariants();
        map
    }
}

#[test]
fn leaves_keep_length_tags_where_these_tests_expect() {
    assert_eq!(
        LengthTagCase::leaves_keep_tags::<u64, Auto>(),
        LENGTH_TAGGED
    );
    assert_eq!(
        LengthTagCase::leaves_keep_tags::<usize, Auto>(),
        LENGTH_TAGGED
    );
}

/// Every lookup of the shared cases answers as std's map does.
#[test]
fn length_tag_cases_match_std() {
    for case in LengthTagCase::all() {
        BBTreeMap::from(&case).assert_lookups_match(&case);
    }
}

/// The cases built to put two keys of each absent key's tag among its
/// candidates do, by length or saturated: its lookup takes the two-candidate
/// path, and answers as std's map does. Miri runs this one
/// (`.config/nextest.toml`): `length_tag_cases_match_std` is too slow for it.
#[test]
fn absent_keys_have_two_candidates_of_their_tag() {
    for case in LengthTagCase::two_candidates_per_absent_key() {
        let mut map = BBTreeMap::from(&case);
        assert_eq!(map.leaf_depth(), 0);
        for absent in case.absent() {
            assert_eq!(
                map.candidates_of_its_length(absent),
                LENGTH_TAGGED.then_some(2),
                "{absent:.20}"
            );
        }
        map.assert_lookups_match(&case);
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
}
