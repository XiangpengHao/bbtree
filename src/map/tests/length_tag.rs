//! Length-filter collisions and structural metadata maintenance.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::vec::Vec;

use crate::BBTreeMap;

#[test]
fn two_length_candidates_check_hits_and_misses_on_either_side() {
    let mut map = BBTreeMap::<String, u64>::new();
    // One primary run, with two equal-length candidates separated by an
    // unrelated key. Every probe has the two candidates' length tag.
    for (index, key) in [
        "shared-prefix/a",
        "shared-prefix/c0",
        "shared-prefix/g11",
        "shared-prefix/t0",
        "shared-prefix/x111",
    ]
    .into_iter()
    .enumerate()
    {
        map.insert(String::from(key), index as u64);
    }
    assert_eq!(map.height(), 0);
    map.check_invariants();
    assert_eq!(map.get("shared-prefix/c0"), Some(&1));
    assert_eq!(map.get("shared-prefix/t0"), Some(&3));
    for missing in ["shared-prefix/b0", "shared-prefix/m0", "shared-prefix/z0"] {
        assert_eq!(map.get(missing), None);
        assert_eq!(map.get_mut(missing), None);
    }
    *map.get_mut("shared-prefix/c0").unwrap() += 10;
    *map.get_mut("shared-prefix/t0").unwrap() += 20;
    assert_eq!(map.get("shared-prefix/c0"), Some(&11));
    assert_eq!(map.get("shared-prefix/t0"), Some(&23));
}

#[test]
fn two_length_candidates_confirm_modulo_collisions() {
    let mut map = BBTreeMap::<String, u64>::new();
    let short = String::from("shared-prefix/c0");
    let long = format!("shared-prefix/t0{}", "\0".repeat(65536));
    map.insert(short.clone(), 1);
    map.insert(String::from("shared-prefix/m11"), 2);
    map.insert(long.clone(), 3);
    map.check_invariants();
    assert_eq!(map.get(short.as_str()), Some(&1));
    assert_eq!(map.get(long.as_str()), Some(&3));
    for missing in [
        String::from("shared-prefix/t0"),
        format!("{short}{}", "\0".repeat(65536)),
        format!("{long}{}", "\0".repeat(65536)),
    ] {
        assert_eq!(map.get(missing.as_str()), None);
    }
}

#[test]
fn length_collisions_and_mutations_preserve_point_lookups() {
    // The initial leaf has one primary run. Distinct lengths can select
    // one candidate; repeated lengths must retain the ordered fallback.
    let mut actual = BBTreeMap::<String, usize>::new();
    let mut expected = BTreeMap::new();
    let mut probes = Vec::new();
    for index in 0..18 {
        let key = format!("shared-prefix/{index:04}/{}", "x".repeat(index / 3));
        actual.insert(key.clone(), index);
        expected.insert(key.clone(), index);
        probes.push(key.clone());
        probes.push(format!("{key}missing"));
    }
    probes.push(format!("{}{}", probes[0], "\0".repeat(65536)));
    for probe in &probes {
        assert_eq!(actual.get(probe.as_str()), expected.get(probe.as_str()));
    }

    for index in 18..512 {
        let key = format!("shared-prefix/{index:04}/{}", "x".repeat(index % 17));
        actual.insert(key.clone(), index);
        expected.insert(key, index);
    }
    actual.check_invariants();
    for index in (0..512).step_by(3) {
        let key = if index < 18 {
            format!("shared-prefix/{index:04}/{}", "x".repeat(index / 3))
        } else {
            format!("shared-prefix/{index:04}/{}", "x".repeat(index % 17))
        };
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

    // Different byte lengths may have identical zero-padded summaries
    // and identical tags modulo 65536. Neither kind of collision is equality.
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
