//! Inline keys (D18) against std's `BTreeMap`: std byte-string keys with the
//! default strategy, looked up by every borrowed form; keys behind a long
//! shared prefix (D50), whose inline keys tie where the tree keeps none, and
//! keys that their inline keys hold whole; a newtype that opts in, looked up
//! through another borrowed form too; integers, and strings, whose nodes
//! keep 2 or 4 bytes of their inline keys; inline keys that break their
//! contract, which may give wrong answers but never break memory safety; and
//! an `InlineKey` that panics, which leaves the map memory-safe (D48).

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

use super::differential::{MirroredMaps, Workload};
use super::prefix::shared_prefix_key;
use crate::inline_key::{Auto, InlineKey, InlineWidth, InlineWord, WithInlineKey};
use crate::map::BBTreeMap;
use crate::tree::test_support::{DropLedger, FromNumber, TestRng, Tracked};

/// A built-in stored key can be borrowed through an unknown query type.
/// Its full ordering must remain available in every node.
#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
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

/// A 27-byte head, then eight digits ([`shared_prefix_key`]): the tree keeps
/// the head, and the leading zeros of the digits, as its shared prefix
/// (D50), and its nodes take their windows past it.
impl FromNumber for Box<str> {
    fn from_number(number: usize) -> Self {
        shared_prefix_key(number).into_boxed_str()
    }
}

/// Sixteen bytes, the array itself, of which the workloads' numbers leave
/// the first fourteen zero: the tree keeps them as its shared prefix (D50),
/// and its nodes take their windows past them.
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
        Self(String::from_number(number))
    }
}

/// Half of the workloads' numbers below zero: the sign flip in the encoding
/// puts them first.
impl FromNumber for i64 {
    fn from_number(number: usize) -> Self {
        number as i64 - (1 << 15)
    }
}

/// Spread over the high half and into the low one: neighbours share the
/// high half, mostly, and differ in the low one, so windows cross the
/// halves.
impl FromNumber for u128 {
    fn from_number(number: usize) -> Self {
        (number as u128) << 56 | 0xabcd
    }
}

/// Half of the workloads' numbers below zero, as `i64`'s.
impl FromNumber for i128 {
    fn from_number(number: usize) -> Self {
        (number as i128 - (1 << 15)) << 40
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
        Self(String::from_number(number))
    }
}

/// `String` keys have inline keys by default, and every lookup uses them:
/// by `String` and by `str`.
#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn string_keys_match_std_by_string_and_by_str() {
    let workload = Workload::new(100_000, 1 << 16);
    workload.run::<String, u64, Auto>(61);

    let rng = TestRng::new(62);
    let mut maps = MirroredMaps::<String, u64>::new();
    maps.run_random_operations(&rng, workload.random_steps, workload.key_space);
    for number in 0..workload.key_space + 2 {
        let key = String::from_number(number);
        maps.get_borrowed(key.as_str());
        maps.get_borrowed(&key);
        // The key cut short, and the key with a NUL after it, whose inline
        // key, zero-padded, is the key's own: only the length tells them
        // apart.
        maps.get_borrowed(&key[..9]);
        maps.get_borrowed(format!("{key}\0").as_str());
    }
    // Ranges by `str`, between the key, the key cut short, and the key with
    // a NUL after it.
    for number in (0..workload.key_space + 2).step_by(97) {
        let key = String::from_number(number);
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
        let key = String::from_number(number);
        maps.remove_borrowed(&key[..9]);
        maps.remove_borrowed(key.as_str());
    }
    maps.assert_same_entries();
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn byte_vec_keys_match_std_by_vec_and_by_slice() {
    let workload = Workload::new(100_000, 1 << 16);
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

/// Keys behind a long shared head, which the tree keeps as its shared prefix
/// (D50). Then one key outside the head leaves the tree no shared prefix:
/// along its borders, where nodes take their windows from the keys' first
/// byte, every inline key ties, the search over them finds one run, the whole
/// node, and the full keys decide.
#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn keys_sharing_long_prefixes_match_std() {
    let workload = Workload::new(40_000, 1 << 14);
    workload.run::<Box<str>, u64, Auto>(65);
    workload.run::<[u8; 16], u64, Auto>(67);

    let rng = TestRng::new(66);
    let mut maps = MirroredMaps::<Box<str>, u64>::new();
    maps.run_random_operations(&rng, workload.random_steps, workload.key_space);
    maps.insert(Box::from("z"), 0);
    let keys: Vec<Box<str>> = maps.keys().cloned().collect();
    assert!(maps.ours().leaf_depth() >= 2);
    for step in maps.ours().inspect().trace(&*keys[0]) {
        assert_eq!(
            step.tied(),
            Some(0..step.node().len()),
            "the left border ties"
        );
    }
    for key in keys {
        maps.get_borrowed(&*key);
        maps.get_borrowed(&key[..key.len() - 1]);
    }
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
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn an_opted_in_newtype_matches_std_by_key_and_by_str() {
    let workload = Workload::new(100_000, 1 << 16);
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
/// unsigned and signed, 8 and 16 bytes, the last with zero-sized values, as
/// in a set.
#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn integer_keys_match_std() {
    let workload = Workload::new(100_000, 1 << 16);
    workload.run::<u64, u64, WithInlineKey>(76);
    workload.run::<i64, u64, WithInlineKey>(77);
    workload.run::<u128, u64, WithInlineKey>(78);
    workload.run::<i128, (), WithInlineKey>(79);
}

/// An array's exact-key lookup may confirm its one candidate, the key whose
/// inline key is the query's, by equality alone (D38). Borrowed slices still
/// tell apart the key's prefixes, which pad to its inline key with zeros, and
/// a difference anywhere in the array counts.
#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn fixed_array_exact_key_lookups_match_std() {
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
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn narrow_inline_keys_match_std() {
    let workload = Workload::new(100_000, 1 << 16);
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
/// inserted. Inline keys that tie everywhere ([`Lie::Constant`]) must also
/// leave a well-formed tree. Returns whether every answer matched std's.
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
                let found = ours.get(&key(number)).is_some();
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
        // Lying inline keys may leave the keys unordered: check what holds
        // anyway, each node's prefix length against its parent's, which
        // calls no `InlineKey`.
        ours.check_prefix_lens();
    }
    assert!(ours.len() <= inserts);
    if matches!(lie, Lie::Constant) {
        ours.check_invariants();
    }
    drop(ours);
    ledger.assert_all_dropped_once();
    all_right
}

/// Inline keys that tie everywhere: right answers, and a well-formed tree.
#[test]
fn inline_keys_that_tie_everywhere_give_right_answers() {
    assert!(run_with_lying_keys(Lie::Constant, 72));
}

/// Wrong answers, as the contract allows, but never out of bounds: Miri
/// checks every access in CI.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn wrong_inline_keys_stay_memory_safe() {
    let _ = run_with_lying_keys(Lie::Reversed, 73);
    let _ = run_with_lying_keys(Lie::Random, 74);
}

/// A name whose `InlineKey` panics once its shared budget of calls is spent,
/// and then not again until given a new budget: a second panic while the
/// first unwinds would abort.
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
        match self.budget.get().checked_sub(1) {
            Some(left) => self.budget.set(left),
            None => {
                self.budget.set(usize::MAX);
                panic!("inline key panic");
            }
        }
        self.name.inline_key_at(offset)
    }
}

/// An `InlineKey` that panics in the middle of inserts and removals, which
/// split, merge and refit nodes, leaves a map that is still memory-safe to
/// search, change and drop (D48): the map calls it only before a change, or
/// after the entry is in or out and counted, so its length still counts its
/// entries, which its walks rely on. A refit it stops may leave a node's
/// inline keys off its prefix, which only makes searches wrong.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn a_panicking_inline_key_leaves_a_sound_map() {
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
    let mut map: BBTreeMap<PanickingName<'_>, usize, WithInlineKey> =
        BBTreeMap::with_strategy(WithInlineKey);
    let mut panics = 0;
    for step in 0..steps {
        let number = rng.below(key_space);
        let insert = rng.below(3) != 0;
        budget.set(rng.below(40));
        let changed = catch_unwind(AssertUnwindSafe(|| {
            if insert {
                map.insert(key(number), step);
            } else {
                map.remove(&key(number));
            }
        }));
        budget.set(usize::MAX);
        if changed.is_err() {
            panics += 1;
            map.get(&key(number));
        }
    }
    assert!(panics > 0);
    assert_eq!(map.len(), map.iter().count());
    assert_eq!(map.len(), map.iter().rev().count());
}

/// An `InlineKey` that panics while `retain`, `extract_if` or `append` builds
/// a tree of the entries it keeps loses at most the entries it was placing:
/// the map counts only those it holds, every value is dropped once, none
/// leaked, and the tree's borders keep its shared prefix (D50), so that a key
/// past the last goes in last. The panic lands on every call in turn:
/// computing an entry's inline key, fitting the prefix of a node a new leaf
/// closes, fitting the border when the walk or the merge finishes, joining
/// back the entries past a bounded range, and putting back what a dropped
/// `extract_if` did not visit.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn a_panicking_inline_key_in_a_bulk_change_leaves_a_valid_map() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum BulkChange {
        Retain,
        ExtractIfAll,
        ExtractIfRange,
        /// Appends the upper half to the lower: the merge takes all of the
        /// lower half first, and a panic leaves nothing of it to take after.
        Append,
    }
    let budget = Cell::new(usize::MAX);
    let ledger = DropLedger::new();
    let count = if cfg!(miri) { 30 } else { 120 };
    let key = |number: usize| PanickingName {
        name: UserName::from_number(number),
        budget: &budget,
    };
    // Runs `change` on fresh maps, the `left`-th call of `InlineKey`
    // panicking; returns the calls left.
    let run = |change: BulkChange, left: usize| {
        let mut map: BBTreeMap<PanickingName<'_>, Tracked<'_>, WithInlineKey> =
            BBTreeMap::with_strategy(WithInlineKey);
        let mut other = BBTreeMap::with_strategy(WithInlineKey);
        for number in 0..count {
            let target = if change == BulkChange::Append && number >= count / 2 {
                &mut other
            } else {
                &mut map
            };
            target.insert(key(number), ledger.track());
        }
        let mut position = 0;
        let mut every_third = |_: &PanickingName<'_>, _: &mut Tracked<'_>| {
            position += 1;
            position % 3 == 0
        };
        budget.set(left);
        let _ = catch_unwind(AssertUnwindSafe(|| match change {
            BulkChange::Retain => map.retain(|key, value| !every_third(key, value)),
            BulkChange::ExtractIfAll => {
                map.extract_if(.., &mut every_third)
                    .take(count / 10)
                    .for_each(drop);
            }
            BulkChange::ExtractIfRange => {
                map.extract_if(key(count / 4)..key(3 * count / 4), &mut every_third)
                    .take(count / 10)
                    .for_each(drop);
            }
            BulkChange::Append => map.append(&mut other),
        }));
        let calls_left = budget.replace(usize::MAX);
        assert_eq!(map.len(), map.iter().count(), "{change:?}, budget {left}");
        assert_eq!(
            map.len(),
            map.iter().rev().count(),
            "{change:?}, budget {left}"
        );
        map.check_prefix_lens();
        map.insert(key(count), ledger.track());
        assert!(
            map.keys().is_sorted(),
            "{change:?}, budget {left}: a key past the last went in elsewhere"
        );
        calls_left
    };
    for change in [
        BulkChange::Retain,
        BulkChange::ExtractIfAll,
        BulkChange::ExtractIfRange,
        BulkChange::Append,
    ] {
        let calls = usize::MAX - run(change, usize::MAX);
        let step = if cfg!(miri) { calls / 8 + 1 } else { 1 };
        for left in (0..calls).step_by(step) {
            run(change, left);
            ledger.assert_all_dropped_once();
        }
    }
}
