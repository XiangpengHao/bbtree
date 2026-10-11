//! Inline keys (D18) against std's `BTreeMap`: std byte-string keys with the
//! default strategy, looked up by every borrowed form; keys whose inline
//! keys tie, or hold the whole key; a newtype that opts in, looked up through
//! another borrowed form too; integers, and strings, whose nodes keep 2 or 4
//! bytes of their inline keys; inline keys that break their contract, which
//! may give wrong answers but never break memory safety; and an `InlineKey`
//! that panics, which leaves the map valid.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cell::Cell;
use core::cmp::Ordering;
use core::ops::Bound;
use core::panic::AssertUnwindSafe;
use std::boxed::Box;
use std::collections::BTreeMap;
use std::format;
use std::panic::catch_unwind;
use std::string::String;
use std::vec::Vec;

use super::BBTreeMap;
use super::differential_tests::{FromNumber, MirroredMaps, Workload};
use crate::inline_key::{Auto, InlineKey, InlineWidth, InlineWord, WithInlineKey};
use crate::node::test_support::{DropLedger, TestRng, Tracked};

/// Ten digits: keys of up to 100 consecutive numbers tie on their first 8
/// bytes, so searches go through runs of equal inline keys.
fn ten_digits(number: usize) -> String {
    format!("{number:010}")
}

/// A built-in stored key can be borrowed through an unknown query type.
/// Its full ordering must remain available even in locally prefixed nodes.
#[test]
fn auto_strings_accept_an_unrecognized_borrowed_query() {
    trait TextQuery {
        fn text(&self) -> &str;
    }
    impl TextQuery for String {
        fn text(&self) -> &str {
            self
        }
    }
    impl PartialEq for dyn TextQuery {
        fn eq(&self, other: &Self) -> bool {
            self.text() == other.text()
        }
    }
    impl Eq for dyn TextQuery {}
    impl PartialOrd for dyn TextQuery {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for dyn TextQuery {
        fn cmp(&self, other: &Self) -> Ordering {
            self.text().cmp(other.text())
        }
    }
    impl Borrow<dyn TextQuery> for String {
        fn borrow(&self) -> &(dyn TextQuery + 'static) {
            self
        }
    }

    let prefix = "long\0shared/prefix/".repeat(4);
    let entries = (0..1024).map(|index| (format!("{prefix}{:04}", 2 * index), index));
    let mut ours: BBTreeMap<_, _> = entries.clone().collect();
    let mut theirs: BTreeMap<_, _> = entries.collect();
    for index in 0..2048 {
        let query = format!("{prefix}{index:04}");
        let borrowed: &dyn TextQuery = &query;
        assert_eq!(ours.get(borrowed), theirs.get(borrowed));
        assert_eq!(ours.get_mut(borrowed), theirs.get_mut(borrowed));
    }
    for query in [
        String::new(),
        String::from(&prefix[..31]),
        format!("{prefix}0000\0"),
    ] {
        let borrowed: &dyn TextQuery = &query;
        assert_eq!(ours.get(borrowed), theirs.get(borrowed));
        let bounds = (Bound::Included(borrowed), Bound::Unbounded);
        assert_eq!(
            ours.range::<dyn TextQuery, _>(bounds)
                .take(4)
                .collect::<Vec<_>>(),
            theirs
                .range::<dyn TextQuery, _>(bounds)
                .take(4)
                .collect::<Vec<_>>(),
        );
    }
    for index in (0..1024).step_by(3) {
        let query = format!("{prefix}{:04}", 2 * index);
        let borrowed: &dyn TextQuery = &query;
        assert_eq!(ours.remove(borrowed), theirs.remove(borrowed));
    }
    ours.check_invariants();
    assert!(ours.iter().eq(theirs.iter()));
}

/// Four bytes: inline keys hold every key whole, so leaf searches tell
/// equal keys by their length.
impl FromNumber for Vec<u8> {
    fn from_number(number: usize) -> Self {
        (number as u32).to_be_bytes().to_vec()
    }
}

/// Every key shares a 27-byte prefix: all inline keys tie.
impl FromNumber for Box<str> {
    fn from_number(number: usize) -> Self {
        format!("shared/prefix/of/many/keys/{number:08}").into_boxed_str()
    }
}

/// Sixteen bytes whose first eight are zero for every number: all inline
/// keys tie, and the bytes are the array itself.
impl FromNumber for [u8; 16] {
    fn from_number(number: usize) -> Self {
        (number as u128).to_be_bytes()
    }
}

/// A newtype: not a std byte string, so it has inline keys only with
/// `WithInlineKey`. It also looks up by `str`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UserName(String);

impl InlineKey for UserName {
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.0.inline_key_at(offset)
    }
}

impl Borrow<str> for UserName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl FromNumber for UserName {
    fn from_number(number: usize) -> Self {
        Self(ten_digits(number))
    }
}

/// Half below zero: the sign flip in the encoding puts them first.
impl FromNumber for i64 {
    fn from_number(number: usize) -> Self {
        number as i64 - (1 << 17)
    }
}

/// Spread over the high half and into the low one: neighbours share 8 bytes
/// or more, so windows cross the halves.
impl FromNumber for u128 {
    fn from_number(number: usize) -> Self {
        (number as u128) << 40 | 0xabcd
    }
}

impl FromNumber for i128 {
    fn from_number(number: usize) -> Self {
        (number as i128 - (1 << 17)) << 40
    }
}

/// Ten digits, of whose inline keys the nodes keep `BYTES`, 2 or 4: past a
/// node's prefix, keys tie on them often, and the strings decide.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NarrowName<const BYTES: usize>(String);

impl<const BYTES: usize> InlineKey for NarrowName<BYTES> {
    const WIDTH: InlineWidth = if BYTES == 2 {
        InlineWidth::Two
    } else {
        InlineWidth::Four
    };

    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.0.inline_key_at(offset)
    }
}

impl<const BYTES: usize> FromNumber for NarrowName<BYTES> {
    fn from_number(number: usize) -> Self {
        Self(ten_digits(number))
    }
}

fn workload(native_random_steps: usize, native_key_space: usize) -> Workload {
    if cfg!(miri) {
        Workload {
            random_steps: 150,
            key_space: 150,
            ordered_entries: 60,
            min_sequential_height: 1,
        }
    } else {
        Workload {
            random_steps: native_random_steps,
            key_space: native_key_space,
            ordered_entries: native_key_space / 2,
            min_sequential_height: 2,
        }
    }
}

/// `String` keys have inline keys by default, and every lookup uses them:
/// by `String` and by `str`.
#[test]
fn string_keys_match_std_by_string_and_by_str() {
    let workload = workload(100_000, 1 << 16);
    workload.run::<String, u64, Auto>(61);

    let rng = TestRng::new(62);
    let mut maps = MirroredMaps::<String, u64>::new();
    maps.run_random_operations(&rng, workload.random_steps, workload.key_space);
    for number in 0..workload.key_space + 2 {
        let key = ten_digits(number);
        maps.get_borrowed(key.as_str());
        maps.get_borrowed(&key);
        // Shorter and longer keys with the same inline key.
        maps.get_borrowed(&key[..9]);
        maps.get_borrowed(format!("{key}\0").as_str());
    }
    // Ranges by `str`, between bounds that tie with stored keys on their
    // inline keys.
    for number in (0..workload.key_space + 2).step_by(97) {
        let key = ten_digits(number);
        let (shorter, longer) = (&key[..9], format!("{key}\0"));
        maps.range_borrowed(
            &rng,
            (Bound::Included(shorter), Bound::Excluded(key.as_str())),
        );
        maps.range_borrowed(
            &rng,
            (Bound::Excluded(shorter), Bound::Included(longer.as_str())),
        );
        maps.range_borrowed(&rng, (Bound::Excluded(key.as_str()), Bound::Unbounded));
    }
    for number in 0..workload.key_space + 2 {
        let key = ten_digits(number);
        maps.remove_borrowed(&key[..9]);
        maps.remove_borrowed(key.as_str());
    }
    maps.assert_same_entries();
}

#[test]
fn byte_vec_keys_match_std_by_vec_and_by_slice() {
    let workload = workload(100_000, 1 << 16);
    workload.run::<Vec<u8>, u64, Auto>(63);

    let rng = TestRng::new(64);
    let mut maps = MirroredMaps::<Vec<u8>, u64>::new();
    maps.run_random_operations(&rng, workload.random_steps, workload.key_space);
    for number in 0..workload.key_space + 2 {
        let key = Vec::from_number(number);
        maps.get_borrowed(key.as_slice());
        maps.get_borrowed(&key[..3]);
        maps.get_borrowed([key.as_slice(), &[0]].concat().as_slice());
    }
    for number in (0..workload.key_space + 2).step_by(97) {
        let key = Vec::from_number(number);
        let (shorter, longer) = (&key[..3], [key.as_slice(), &[0]].concat());
        maps.range_borrowed(
            &rng,
            (Bound::Included(shorter), Bound::Excluded(key.as_slice())),
        );
        maps.range_borrowed(
            &rng,
            (Bound::Excluded(shorter), Bound::Included(longer.as_slice())),
        );
        maps.range_borrowed(&rng, (Bound::Excluded(key.as_slice()), Bound::Unbounded));
    }
    for number in 0..workload.key_space + 2 {
        let key = Vec::from_number(number);
        maps.remove_borrowed(&key[..3]);
        maps.remove_borrowed(key.as_slice());
    }
    maps.assert_same_entries();
}

/// Every key ties on its inline key with every other: the search over inline
/// keys finds one run, the whole node, and the full keys decide.
#[test]
fn keys_sharing_long_prefixes_match_std() {
    let workload = workload(40_000, 1 << 14);
    workload.run::<Box<str>, u64, Auto>(65);

    let rng = TestRng::new(66);
    let mut maps = MirroredMaps::<Box<str>, u64>::new();
    maps.run_random_operations(&rng, workload.random_steps, workload.key_space);
    let keys: Vec<Box<str>> = maps.keys().cloned().collect();
    for key in keys {
        maps.get_borrowed(&*key);
        maps.get_borrowed(&key[..key.len() - 1]);
    }
    workload.run::<[u8; 16], u64, Auto>(67);
}

/// A random string of up to 14 bytes over a few characters, a NUL among
/// them: many are 8 bytes or fewer, which their inline keys hold whole, and
/// many share an inline key with another one of another length.
fn short_string(rng: &TestRng) -> String {
    let len = rng.below(8);
    (0..len)
        .map(|_| ['\0', 'a', 'b', 'é'][rng.below(4)])
        .collect()
}

/// Short keys, by `String`, `str` and `&str`: equal keys are told by their
/// length where the inline keys hold them whole, and only there.
#[test]
fn short_keys_match_std() {
    let rng = TestRng::new(68);
    let steps = if cfg!(miri) { 400 } else { 50_000 };
    let mut ours: BBTreeMap<String, u64> = BBTreeMap::new();
    let mut theirs: BTreeMap<String, u64> = BTreeMap::new();
    for step in 0..steps as u64 {
        let key = short_string(&rng);
        match rng.below(5) {
            0 | 1 => assert_eq!(ours.insert(key.clone(), step), theirs.insert(key, step)),
            2 => {
                assert_eq!(ours.get(key.as_str()), theirs.get(key.as_str()));
                assert_eq!(ours.get_key_value(&key), theirs.get_key_value(&key));
            }
            3 => assert_eq!(ours.remove(key.as_str()), theirs.remove(key.as_str())),
            _ => assert_eq!(ours.get_mut(key.as_str()), theirs.get_mut(key.as_str())),
        }
    }
    ours.check_invariants();
    assert_eq!(ours.len(), theirs.len());
    for (key, value) in &theirs {
        assert_eq!(ours.get(key.as_str()), Some(value));
    }

    // `&str` keys, borrowed from the strings above, by `&str` and by `str`.
    let mut borrowed: BBTreeMap<&str, u64> = BBTreeMap::new();
    for (key, value) in &theirs {
        borrowed.insert(key.as_str(), *value);
    }
    borrowed.check_invariants();
    for _ in 0..steps {
        let key = short_string(&rng);
        assert_eq!(borrowed.get(key.as_str()), theirs.get(key.as_str()));
        assert_eq!(borrowed.get(&key.as_str()), theirs.get(key.as_str()));
    }
}

/// A newtype that opts in has inline keys; a lookup by `str`, another
/// borrowed form, compares full keys instead, with the same answers.
#[test]
fn an_opted_in_newtype_matches_std_by_key_and_by_str() {
    let workload = workload(100_000, 1 << 16);
    workload.run::<UserName, u64, WithInlineKey>(69);
    workload.run::<UserName, u64, Auto>(70);

    let rng = TestRng::new(71);
    let mut maps = MirroredMaps::<UserName, u64, WithInlineKey>::new();
    maps.run_random_operations(&rng, workload.random_steps, workload.key_space);
    for number in 0..workload.key_space + 2 {
        let key = UserName::from_number(number);
        maps.get_borrowed(&key);
        maps.get_borrowed(key.0.as_str());
    }
}

/// Integers opt in, and their nodes keep 2 bytes of their inline keys:
/// unsigned and signed, 8 and 16 bytes, maps and sets.
#[test]
fn integer_keys_match_std() {
    let workload = workload(100_000, 1 << 16);
    workload.run::<u64, u64, WithInlineKey>(76);
    workload.run::<i64, u64, WithInlineKey>(77);
    workload.run::<u128, u64, WithInlineKey>(78);
    workload.run::<i128, (), WithInlineKey>(79);
}

/// Array point lookup may use equality after its summary finds one
/// candidate. Borrowed slices still distinguish proper zero-padded
/// prefixes, and differences anywhere in the full array must be checked.
#[test]
fn fixed_array_point_queries_match_std() {
    struct ArrayQueries;

    impl ArrayQueries {
        fn check<const N: usize>() {
            let mut ours = BBTreeMap::new();
            let mut theirs = BTreeMap::new();
            for number in 0..1024_u16 {
                let mut key = [0; N];
                key[..2].copy_from_slice(&number.to_be_bytes());
                ours.insert(key, number);
                theirs.insert(key, number);
            }
            ours.check_invariants();
            for (&key, &value) in &theirs {
                assert_eq!(ours.get(&key), Some(&value));
                assert_eq!(
                    ours.get_mut(key.as_slice()).map(|value| *value),
                    Some(value)
                );
                let mut query = key.to_vec();
                query.push(0);
                for len in 0..=query.len() {
                    let borrowed = &query[..len];
                    assert_eq!(ours.get(borrowed), theirs.get(borrowed));
                }
                for byte in 0..N {
                    let mut different = key;
                    different[byte] ^= 0x80;
                    assert_eq!(ours.get(&different), theirs.get(&different));
                }
            }
        }
    }

    ArrayQueries::check::<16>();
    ArrayQueries::check::<20>();
    ArrayQueries::check::<32>();
}

/// Strings whose nodes keep 2 or 4 bytes of their inline keys: runs of
/// tied inline keys are long, and the full keys decide within them.
#[test]
fn narrow_inline_keys_match_std() {
    let workload = workload(100_000, 1 << 16);
    workload.run::<NarrowName<2>, u64, WithInlineKey>(80);
    workload.run::<NarrowName<4>, String, WithInlineKey>(81);
}

/// A key with a consistent `Ord` and an `InlineKey` that breaks its
/// contract in one of the ways of [`Lie`].
struct LyingKey<'r> {
    number: u32,
    lie: Lie,
    rng: &'r TestRng,
}

#[derive(Clone, Copy)]
enum Lie {
    /// The same inline key for every key: consistent after all, as ties
    /// decide nothing, so answers stay right.
    Constant,
    /// Inline keys ordered against the keys.
    Reversed,
    /// A random inline key on every call.
    Random,
}

impl PartialEq for LyingKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.number == other.number
    }
}

impl Eq for LyingKey<'_> {}

impl PartialOrd for LyingKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LyingKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.number.cmp(&other.number)
    }
}

impl InlineKey for LyingKey<'_> {
    fn inline_key_at(&self, _offset: usize) -> InlineWord {
        match self.lie {
            Lie::Constant => 7,
            Lie::Reversed => InlineWord::from(u32::MAX - self.number),
            Lie::Random => self.rng.below(4) as InlineWord,
        }
    }
}

/// Inserts, looks up, removes and ranges over keys whose inline keys lie,
/// checking only what any map must do whatever its answers: every value
/// inserted is dropped once, and the map never holds more entries than were
/// inserted. Returns whether every answer matched std's.
fn run_with_lying_keys(lie: Lie, seed: u64) -> bool {
    let rng = TestRng::new(seed);
    let ledger = DropLedger::new();
    let (steps, key_space) = if cfg!(miri) {
        (300, 200)
    } else {
        (20_000, 4000)
    };
    let key = |number: usize| LyingKey {
        number: number as u32,
        lie,
        rng: &rng,
    };
    let mut ours: BBTreeMap<LyingKey<'_>, Tracked<'_>, WithInlineKey> =
        BBTreeMap::with_strategy(WithInlineKey);
    let mut theirs: BTreeMap<u32, ()> = BTreeMap::new();
    let mut all_right = true;
    let mut inserts = 0;
    for _ in 0..steps {
        let number = rng.below(key_space);
        match rng.below(5) {
            0 | 1 => {
                inserts += 1;
                let replaced = ours.insert(key(number), ledger.track()).is_some();
                all_right &= replaced == theirs.insert(number as u32, ()).is_some();
            }
            2 => {
                let found = ours.get(&key(number)).map(Tracked::id).is_some();
                all_right &= found == theirs.contains_key(&(number as u32));
            }
            3 => {
                let removed = ours.remove(&key(number)).is_some();
                all_right &= removed == theirs.remove(&(number as u32)).is_some();
            }
            _ if rng.below(8) == 0 => {
                let popped = ours.pop_first().map(|(key, _)| key.number);
                all_right &= popped == theirs.pop_first().map(|(number, ())| number);
            }
            _ if rng.below(8) == 0 => {
                // The bounds are in order, but inline keys that lie may find
                // the start after the end, which panics as an ill-defined
                // `Ord` does.
                let high = number + rng.below(64);
                let ranged = catch_unwind(AssertUnwindSafe(|| {
                    let range = ours.range(key(number)..key(high));
                    range.map(|(key, _)| key.number).collect::<Vec<u32>>()
                }));
                let expected = theirs.range(number as u32..high as u32);
                let expected: Vec<u32> = expected.map(|(&number, ())| number).collect();
                all_right &= ranged.is_ok_and(|numbers| numbers == expected);
            }
            _ => {
                if let Some(value) = ours.get_mut(&key(number)) {
                    *value = ledger.track();
                }
            }
        }
        assert!(ours.len() <= inserts);
        // A split-armed append changed this reversed-encoding history and
        // exposed a refit that shortened a leaf below its parent. Check
        // every edge after every operation, without calling the lying
        // encoding or assuming the keys are sorted. The ledger below also
        // verifies that later promotions/removals keep ownership intact.
        ours.check_prefix_order();
    }
    assert!(ours.len() <= inserts);
    drop(ours);
    ledger.assert_all_dropped_once();
    all_right
}

#[test]
fn inline_keys_that_tie_everywhere_give_right_answers() {
    assert!(run_with_lying_keys(Lie::Constant, 72));
}

/// Wrong answers, as the contract allows, but never out of bounds: Miri
/// checks every access in CI.
#[test]
fn wrong_inline_keys_stay_memory_safe() {
    let _ = run_with_lying_keys(Lie::Reversed, 73);
    let _ = run_with_lying_keys(Lie::Random, 74);
}

/// Inline keys that tie everywhere still build a well-formed tree, whose
/// inline keys are its keys'.
#[test]
fn a_map_with_constant_inline_keys_keeps_its_invariants() {
    let rng = TestRng::new(75);
    let mut map = BBTreeMap::with_strategy(WithInlineKey);
    let count = if cfg!(miri) { 200 } else { 3000 };
    for _ in 0..count {
        let number = rng.below(10 * count);
        map.insert(
            LyingKey {
                number: number as u32,
                lie: Lie::Constant,
                rng: &rng,
            },
            number,
        );
    }
    map.check_invariants();
    assert!(map.height() >= 1);
}

/// A name whose `InlineKey` panics once its shared budget of calls is spent.
struct PanickingName<'b> {
    name: UserName,
    budget: &'b Cell<usize>,
}

impl PartialEq for PanickingName<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for PanickingName<'_> {}

impl PartialOrd for PanickingName<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PanickingName<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}

impl InlineKey for PanickingName<'_> {
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        let left = self.budget.get();
        assert!(left > 0, "inline key panic");
        self.budget.set(left - 1);
        self.name.inline_key_at(offset)
    }
}

/// An `InlineKey` that panics in the middle of inserts and removals, which
/// split, merge and refit nodes, leaves a valid map, which agrees with std's
/// on every key it was not changing: the map calls it only before a change,
/// or after the entry is in or out and counted, never while keys move. A
/// refit it stops leaves a prefix shorter than its fences share, which
/// searches as well, so prefixes are checked only against their parents'.
#[test]
fn a_panicking_inline_key_leaves_a_valid_map() {
    let rng = TestRng::new(96);
    let budget = Cell::new(usize::MAX);
    let (steps, key_space) = if cfg!(miri) {
        (300, 300)
    } else {
        (20_000, 6000)
    };
    let key = |number: usize| PanickingName {
        name: UserName::from_number(number),
        budget: &budget,
    };
    let mut ours: BBTreeMap<PanickingName<'_>, usize, WithInlineKey> =
        BBTreeMap::with_strategy(WithInlineKey);
    let mut theirs: BTreeMap<usize, usize> = BTreeMap::new();
    let mut panics = 0;
    for step in 0..steps {
        let number = rng.below(key_space);
        let insert = rng.below(3) != 0;
        budget.set(rng.below(40));
        let changed = catch_unwind(AssertUnwindSafe(|| {
            if insert {
                ours.insert(key(number), step);
            } else {
                ours.remove(&key(number));
            }
        }));
        budget.set(usize::MAX);
        if changed.is_err() {
            panics += 1;
            // The entry may or may not have gone in or out: std's map
            // follows ours, which must hold it or not.
            match ours.get(&key(number)) {
                Some(&value) => theirs.insert(number, value),
                None => theirs.remove(&number),
            };
        } else if insert {
            theirs.insert(number, step);
        } else {
            theirs.remove(&number);
        }
        ours.check_prefix_order();
    }
    assert!(panics > 0);
    assert_eq!(ours.len(), theirs.len());
    assert!(
        ours.keys()
            .zip(theirs.keys())
            .all(|(ours, &number)| ours.name == UserName::from_number(number))
    );
    for (&number, value) in &theirs {
        assert_eq!(ours.get(&key(number)), Some(value));
    }
}
