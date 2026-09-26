//! Inline keys (D18) against std's `BTreeMap`: std byte-string keys with the
//! default strategy, looked up by every borrowed form; keys whose inline
//! keys tie, or hold the whole key; a newtype that opts in, looked up through
//! another borrowed form too; and inline keys that break their contract,
//! which may give wrong answers but never break memory safety.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;
use std::boxed::Box;
use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::vec::Vec;

use super::BBTreeMap;
use super::differential_tests::{FromNumber, MirroredMaps, Workload};
use crate::inline_key::{Auto, InlineKey, InlineWord, WithInlineKey};
use crate::node::test_support::{DropLedger, TestRng, Tracked};

/// Ten digits: keys of up to 100 consecutive numbers tie on their first 8
/// bytes, so searches go through runs of equal inline keys.
fn ten_digits(number: usize) -> String {
    format!("{number:010}")
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
    fn inline_key(&self) -> InlineWord {
        self.0.inline_key()
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
    fn inline_key(&self) -> InlineWord {
        match self.lie {
            Lie::Constant => 7,
            Lie::Reversed => InlineWord::from(u32::MAX - self.number),
            Lie::Random => self.rng.below(4) as InlineWord,
        }
    }
}

/// Inserts, looks up and removes keys whose inline keys lie, checking only
/// what any map must do whatever its answers: every value inserted is
/// dropped once, and the map never holds more entries than were inserted.
/// Returns whether every answer matched std's.
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
            _ => {
                if let Some(value) = ours.get_mut(&key(number)) {
                    *value = ledger.track();
                }
            }
        }
        assert!(ours.len() <= inserts);
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
