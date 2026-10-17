//! `BBTreeMap` under Shuttle, which runs the threads one at a time and
//! switches between them where the latched tree touches an atomic or waits
//! (`tree/interleaving.rs`). It explores schedules that real threads reach
//! once in billions of runs, and replays a failing one.
//!
//! Every schedule reruns the whole test, so the tests keep their trees and
//! threads small. Values are wide ([`Tracked`]), so that a leaf holds a
//! dozen entries, and each test sets up a tree one or two operations away
//! from what it races: a split, a merge, the root raised from a leaf or
//! lowered to one, a separator replaced, the shared prefix shrunk. The small
//! tests race two or three threads of a few operations each over one such
//! change, and check the tree took the shape they race; the large ones run
//! random operations against models, over many changes.
//!
//! Every value a closure sees is checked to be the requested key's, or the
//! visited key's in a scan, and not dropped ([`Tracked::check`]), after a
//! switch point: a thread that runs a closure on an entry whose leaf it has
//! not latched, through a stale page, sees another thread drop it.
//!
//! `RUSTFLAGS="--cfg shuttle" cargo test --release --lib shuttle_tests`

use core::fmt;
use core::mem;
use core::ops::{Bound, ControlFlow, RangeBounds};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};
use std::format;
use std::string::String;
use std::sync::Arc;
use std::vec::Vec;

use shuttle::rand::{Rng, thread_rng};
use shuttle::scheduler::{PctScheduler, RandomScheduler, Scheduler};
use shuttle::{Config, Runner, thread};

use super::BBTreeMap;

/// How many schedules a test explores, from Shuttle's random scheduler and
/// from PCT, which finds bugs that take a few preemptions at the right
/// points; each within a time cap, so that a test ends within two minutes
/// on a slow machine.
struct Exploration {
    random: usize,
    pct: usize,
}

impl Exploration {
    /// PCT's depth: how many preemptions it places in a schedule.
    const PCT_DEPTH: usize = 3;
    /// The most time each scheduler explores for: a schedule that starts
    /// before the cap runs to its end.
    const TIME_CAP: Duration = Duration::from_secs(50);

    /// Runs `test` under each schedule of the exploration.
    fn run(self, test: impl Fn() + Send + Sync + 'static) {
        let test = Arc::new(test);
        let random_test = Arc::clone(&test);
        Self::run_with(RandomScheduler::new(self.random), move || random_test());
        Self::run_with(PctScheduler::new(Self::PCT_DEPTH, self.pct), move || test());
    }

    fn run_with(scheduler: impl Scheduler + 'static, test: impl Fn() + Send + Sync + 'static) {
        let mut config = Config::new();
        config.max_time = Some(Self::TIME_CAP);
        Runner::new(scheduler, config).run(test);
    }
}

/// Whether the map dropped each value a test made: a flag a value, in an
/// arena that outlives the map, so that a value read through a stale copy
/// of its bytes after its drop still finds its flag.
struct DropFlags {
    flags: Vec<AtomicBool>,
    next: AtomicUsize,
}

impl DropFlags {
    fn new() -> Self {
        Self {
            flags: (0..4_096).map(|_| AtomicBool::new(false)).collect(),
            next: AtomicUsize::new(0),
        }
    }

    /// A new value for the key numbered `id`.
    fn value(&self, id: u64) -> Tracked<'_> {
        let slot = self.next.fetch_add(1, Ordering::Relaxed);
        assert!(slot < self.flags.len(), "a test makes too many values");
        Tracked {
            id,
            slot,
            flags: self,
            _wide: [Tracked::FILL; 5],
        }
    }
}

/// A map's value: the number of its key, to tell whose entry it is, and its
/// drop flag in [`DropFlags`]. Wide, so that a leaf holds a dozen, and
/// filled with words that crash a thread that follows one as a node
/// pointer.
struct Tracked<'f> {
    id: u64,
    slot: usize,
    flags: &'f DropFlags,
    _wide: [u64; 5],
}

impl Tracked<'_> {
    /// Not null, not aligned, and not a mapped address.
    const FILL: u64 = 0xDEAD_0000_0000_0BAD;

    /// Panics unless this is the value of the key numbered `id` and the map
    /// has not dropped it, after a switch point: a closure that runs on an
    /// entry whose leaf its thread has not latched lets another thread
    /// remove and drop it.
    fn check(&self, id: u64) {
        thread::yield_now();
        assert_eq!(self.id, id, "a closure ran on another key's value");
        assert!(
            !self.flags.flags[self.slot].load(Ordering::Relaxed),
            "a closure ran on key {id}'s value after the map dropped it"
        );
    }

    /// Which value this is: the same for every look at one entry.
    fn slot(&self) -> usize {
        self.slot
    }
}

impl fmt::Debug for Tracked<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tracked({}, #{})", self.id, self.slot)
    }
}

impl Clone for Tracked<'_> {
    fn clone(&self) -> Self {
        self.flags.value(self.id)
    }
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        self.flags.flags[self.slot].store(true, Ordering::Relaxed);
    }
}

type TrackedMap<'f, K = u64> = BBTreeMap<K, Tracked<'f>>;

/// The keys tests set trees up with, numbered: `key(0)`, `key(1)`, …,
/// multiples of 10, so that threads can insert keys between them.
fn key(index: u64) -> u64 {
    10 * (index + 1)
}

/// A map of the keys `key(0)`, …, `key(len - 1)`, inserted in order, each
/// with a value numbered by its key.
fn map_of(flags: &DropFlags, len: u64) -> TrackedMap<'_> {
    let map = TrackedMap::new();
    for index in 0..len {
        assert!(map.insert(key(index), flags.value(key(index))).is_none());
    }
    map
}

/// The shapes tests start from, each one operation away from a change of
/// the tree's height: a tree of one or two levels, full along its right
/// border, which the next append raises; and a tree of two or three levels,
/// which the next removal of its last key lowers. Built with the keys
/// `key(0)`, … in order, and the last ones removed.
///
/// Found once a test, outside Shuttle, by building them: they follow from
/// the node sizes and how nodes split and merge, which this need not know.
struct Shapes {
    /// At index `levels - 1`: how many keys a tree of `levels` levels holds
    /// before the next append raises it.
    capacity: [u64; 2],
    /// At index `levels - 1`: how many keys are left of the
    /// `capacity[levels - 1] + 1` that raised the tree past `levels` levels
    /// when, removing keys from the end, the next removal lowers it back.
    lowering_len: [u64; 2],
    /// The most keys an internal node holds: the full root of two levels.
    internal_capacity: u64,
}

impl Shapes {
    fn probe() -> Self {
        let flags = DropFlags::new();
        let mut capacity = [0; 2];
        let mut lowering_len = [0; 2];
        for levels in 0..2 {
            let mut map = TrackedMap::new();
            let mut len = 0;
            while map.node_lengths().len() <= levels + 1 {
                map.insert(key(len), flags.value(key(len)));
                len += 1;
            }
            capacity[levels] = len - 1;
            while map.node_lengths().len() > levels + 1 {
                len -= 1;
                map.remove(&key(len));
            }
            lowering_len[levels] = len + 1;
        }
        let internal_capacity = map_of(&flags, capacity[1]).node_lengths()[0][0] as u64;
        Self {
            capacity,
            lowering_len,
            internal_capacity,
        }
    }

    /// A tree of `levels` levels, full along its right border: the next
    /// insert of a key past its last raises it. Returns how many keys it
    /// holds.
    fn full<'f>(&self, flags: &'f DropFlags, levels: usize) -> (TrackedMap<'f>, u64) {
        let len = self.capacity[levels - 1];
        (map_of(flags, len), len)
    }

    /// A tree of `levels + 1` levels, which a removal of its last key lowers
    /// to `levels`. Returns how many keys it holds.
    fn one_removal_from_lowering<'f>(
        &self,
        flags: &'f DropFlags,
        levels: usize,
    ) -> (TrackedMap<'f>, u64) {
        let (len, raised) = (self.lowering_len[levels - 1], self.capacity[levels - 1] + 1);
        let map = map_of(flags, raised);
        for index in len..raised {
            assert!(map.remove(&key(index)).is_some());
        }
        (map, len)
    }
}

/// Visits `range` of the map with `scan`, or `scan_rev` if `reverse`,
/// checking each value against `id_of` its key, and returns the keys
/// visited, in key order.
fn scanned_keys<K: Ord + Clone>(
    map: &TrackedMap<'_, K>,
    range: impl RangeBounds<K>,
    id_of: fn(&K) -> u64,
    reverse: bool,
) -> Vec<K> {
    let mut keys = Vec::new();
    let visit = |key: &K, value: &Tracked<'_>| {
        value.check(id_of(key));
        keys.push(key.clone());
        ControlFlow::<()>::Continue(())
    };
    if reverse {
        map.scan_rev::<K, ()>(range, visit);
        keys.reverse();
    } else {
        map.scan::<K, ()>(range, visit);
    }
    keys
}

/// Panics unless `visited` is in strictly increasing key order and holds
/// every key of `steady`.
fn assert_visited_in_order<K: Ord + fmt::Debug>(visited: &[K], steady: &[K]) {
    assert!(
        visited.is_sorted_by(|a, b| a < b),
        "visited out of order, or twice: {visited:?}"
    );
    for key in steady {
        assert!(
            visited.binary_search(key).is_ok(),
            "missed {key:?}: {visited:?}"
        );
    }
}

/// The number of a `u64` key, its value's.
fn same(key: &u64) -> u64 {
    *key
}

// Small tests: one change, two or three threads, a few operations each.

/// Scans from either end, and a cloned range's first steps, while the tree
/// lowers from two leaves into its root leaf: a scan that read the root as
/// internal must not take the leaf's bytes for edges.
#[test]
fn scans_race_the_root_lowering_to_a_leaf() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let steady: Vec<u64> = (0..len - 1).map(key).collect();
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&key(len - 1)).is_some()));
            scope.spawn(|| {
                assert_visited_in_order(&scanned_keys(&map, .., same, false), &steady);
                assert_visited_in_order(&scanned_keys(&map, .., same, true), &steady);
                let mut range = map.range_cloned::<u64, _>(..);
                let (first, value) = range.next().expect("steady keys");
                value.check(first);
                assert_eq!(first, key(0));
                let (last, value) = range.next_back().expect("steady keys");
                value.check(last);
                assert!(last >= key(len - 2));
            });
        });
        assert_eq!(map.node_lengths().len(), 1, "the root lowered to a leaf");
        map.check_invariants(steady.len());
    });
}

/// A batched lookup of a separator key while its leaf merges into its left
/// sibling, freed, and the key is removed: the lookup must not find the
/// entry in the freed leaf's stale bytes.
#[test]
fn batched_separator_lookups_race_a_merge() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let separator = map.leaf_keys()[1][0];
        thread::scope(|scope| {
            scope.spawn(|| {
                let found = map.read_many([&separator, &key(0)], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found[1], Some(key(0)));
            });
            scope.spawn(|| {
                assert!(map.remove(&key(len - 1)).is_some());
                assert!(map.remove(&separator).is_some());
            });
        });
        assert_eq!(map.node_lengths().len(), 1, "the root lowered to a leaf");
        map.check_invariants(len as usize - 2);
    });
}

/// A lookup of a separator key while its leaf merges into its left sibling,
/// freed, and the key is removed: the lookup must not find the entry in the
/// freed leaf's stale bytes.
#[test]
fn separator_lookups_race_a_merge() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let separator = map.leaf_keys()[1][0];
        thread::scope(|scope| {
            scope.spawn(|| {
                map.read(&separator, |key, value| value.check(*key));
            });
            scope.spawn(|| {
                assert!(map.remove(&key(len - 1)).is_some());
                assert!(map.remove(&separator).is_some());
            });
        });
        assert_eq!(map.node_lengths().len(), 1, "the root lowered to a leaf");
        map.check_invariants(len as usize - 2);
    });
}

/// Lookups, batched and not, while a full root leaf splits into an internal
/// root, and the tree lowers back to a leaf: every descent reads the root as
/// it is.
#[test]
fn lookups_race_the_root_raised_and_lowered() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.full(&flags, 1);
        let (first, last) = (key(0), key(len - 1));
        let added = last - 5;
        // With `added` removed, the tree is the one `Shapes` lowered by
        // removing keys from the end.
        let lowering_len = shapes.lowering_len[0];
        thread::scope(|scope| {
            scope.spawn(|| {
                assert!(map.insert(added, flags.value(added)).is_none());
                assert!(map.remove(&added).is_some());
                for index in (lowering_len - 1..len).rev() {
                    assert!(map.remove(&key(index)).is_some());
                }
            });
            scope.spawn(|| {
                let found = map.read_many([&first, &added], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found[0], Some(first));
                map.read(&added, |_, value| value.check(added));
                map.read(&last, |_, value| value.check(last));
            });
        });
        assert_eq!(map.node_lengths().len(), 1, "the root lowered to a leaf");
        map.check_invariants(lowering_len as usize - 1);
    });
}

/// Removing a separator, its successor taking its place, and then the
/// successor, which merges the leaf, while another thread reads the keys
/// around them and scans across them.
#[test]
fn removing_separators_races_lookups_around_them() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let right = map.leaf_keys()[1].clone();
        let (separator, successor) = (right[0], right[1]);
        let before = separator - 10;
        let steady: Vec<u64> = (0..len)
            .map(key)
            .filter(|&key| key != separator && key != successor)
            .collect();
        thread::scope(|scope| {
            scope.spawn(|| {
                assert!(map.remove(&separator).is_some());
                assert!(map.remove(&successor).is_some());
            });
            scope.spawn(|| {
                map.read(&successor, |_, value| value.check(successor));
                let updated = map.update(&before, |_, value| value.check(before));
                assert!(updated.is_some());
                let mut visited = Vec::new();
                map.scan::<u64, ()>(before.., |key, value| {
                    value.check(*key);
                    visited.push(*key);
                    ControlFlow::Continue(())
                });
                let after: Vec<u64> = steady.iter().copied().filter(|&k| k >= before).collect();
                assert_visited_in_order(&visited, &after);
            });
        });
        assert_eq!(map.node_lengths().len(), 1, "the root lowered to a leaf");
        map.check_invariants(steady.len());
    });
}

/// An append to the last leaf, which splits it, while another thread
/// inserts a key that lacks what every key shares, which shrinks the shared
/// prefix along both borders, and a third reads the first key, on the left
/// border.
#[test]
fn appends_race_a_shrinking_shared_prefix() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let base = 0xAB << 40;
        let len = 2 * shapes.capacity[0];
        let mut map = TrackedMap::new();
        for index in 0..len {
            map.insert(base + key(index), flags.value(base + key(index)));
        }
        assert!(map.shared_prefix_len() >= 3);
        let (appended, outside, first) = (base + key(len), 5, base + key(0));
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.insert(appended, flags.value(appended)).is_none()));
            scope.spawn(|| assert!(map.insert(outside, flags.value(outside)).is_none()));
            scope.spawn(|| {
                assert!(map.read(&first, |_, value| value.check(first)).is_some());
                map.read(&outside, |_, value| value.check(outside));
            });
        });
        map.check_invariants(len as usize + 2);
        assert!(map.shared_prefix_len() < 3);
        for key in [appended, outside, first] {
            assert!(map.read(&key, |_, value| value.check(key)).is_some());
        }
    });
}

/// Appends while a removal merges the last leaf into its sibling, frees it,
/// and lowers the tree into the root leaf: an append that read the pointer
/// to the last leaf before must not insert into the freed page.
#[test]
fn appends_race_a_merge_that_frees_the_last_leaf() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let appended = [key(len), key(len + 1)];
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&key(len - 1)).is_some()));
            for key in appended {
                let map = &map;
                let flags = &flags;
                scope.spawn(move || assert!(map.insert(key, flags.value(key)).is_none()));
            }
        });
        map.check_invariants(len as usize + 1);
        for key in appended {
            assert!(map.read(&key, |_, value| value.check(key)).is_some());
        }
    });
}

/// Two threads `get_or_insert` the same key into a full root leaf, which
/// splits, while a third removes another key under a condition: both see
/// the one entry the map keeps.
#[test]
fn racing_get_or_inserts_see_one_entry() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.full(&flags, 1);
        let contested = key(3) + 5;
        let get_or_insert = || {
            map.get_or_insert(contested, flags.value(contested), |_, value| {
                value.check(contested);
                value.slot()
            })
        };
        let (first, second) = thread::scope(|scope| {
            let first = scope.spawn(get_or_insert);
            let second = scope.spawn(get_or_insert);
            scope.spawn(|| {
                let removed = map.remove_if(&key(0), |_, value| {
                    value.check(key(0));
                    true
                });
                assert!(removed.is_some());
            });
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(first, second, "both saw the entry the map keeps");
        map.check_invariants(len as usize);
    });
}

/// A cloned range copies entries ahead, and resumes its scan where the
/// last copy stopped, while another thread merges the leaf it stopped in,
/// which frees its page, and splits the root leaf again, which takes the
/// page back: the range hands out each steady key once, in order.
#[test]
fn cloned_ranges_resume_across_a_freed_and_reused_leaf() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let steady: Vec<u64> = (0..len - 1).map(key).collect();
        // Enough keys, between the steady ones, to split the root leaf.
        let added: Vec<u64> = (0..shapes.capacity[0] + 2 - len)
            .map(|index| key(index) + 5)
            .collect();
        thread::scope(|scope| {
            scope.spawn(|| {
                assert!(map.remove(&key(len - 1)).is_some());
                for &key in &added {
                    assert!(map.insert(key, flags.value(key)).is_none());
                }
            });
            scope.spawn(|| {
                let handed_out: Vec<u64> = (map.range_cloned::<u64, _>(..))
                    .map(|(key, value)| {
                        value.check(key);
                        key
                    })
                    .collect();
                assert_visited_in_order(&handed_out, &steady);
            });
        });
        assert_eq!(map.node_lengths().len(), 2, "the root leaf split again");
        map.check_invariants(steady.len() + added.len());
    });
}

/// Two splits of full leaves under a full root, each of which splits the
/// root and raises the tree to three levels, the first to take the root,
/// while a third thread scans and looks up keys.
#[test]
fn splits_race_to_raise_a_full_internal_root() {
    let shapes = Shapes::probe();
    Exploration {
        random: 5_000,
        pct: 5_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.full(&flags, 2);
        // The first leaf filled too, with keys between its first ones.
        let first_leaf = map.leaf_keys()[0].len() as u64;
        let filled: Vec<u64> = (0..shapes.capacity[0] - first_leaf)
            .map(|index| key(index) + 5)
            .collect();
        for &key in &filled {
            map.insert(key, flags.value(key));
        }
        let (appended, between) = (key(len), key(0) + 1);
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.insert(appended, flags.value(appended)).is_none()));
            scope.spawn(|| assert!(map.insert(between, flags.value(between)).is_none()));
            scope.spawn(|| {
                let steady: Vec<u64> = (0..len).map(key).collect();
                assert_visited_in_order(&scanned_keys(&map, .., same, true), &steady);
                let last = key(len - 1);
                let found = map.read_many([&last, &key(0)], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found, [Some(last), Some(key(0))]);
            });
        });
        assert_eq!(map.node_lengths().len(), 3, "the tree has three levels");
        map.check_invariants(len as usize + filled.len() + 2);
    });
}

/// Lookups of the root's separator key, which they match in the root,
/// while another thread removes it, its successor taking its place as the
/// separator and its leaf's head: a descent that matched it must find it,
/// or nothing, not the successor.
#[test]
fn lookups_race_removing_the_roots_separator() {
    let shapes = Shapes::probe();
    Exploration {
        random: 5_000,
        pct: 5_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 2);
        let separator = map.leaf_keys()[map.node_lengths()[1][0] + 1][0];
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&separator).is_some()));
            scope.spawn(|| {
                map.read(&separator, |_, value| value.check(separator));
                map.update(&separator, |_, value| value.check(separator));
            });
        });
        assert_eq!(
            map.node_lengths().len(),
            3,
            "the separator's leaf kept its minimum"
        );
        map.check_invariants(len as usize - 1);
    });
}

/// A removal that merges leaves, then internal nodes, and lowers the tree
/// from three levels to two, while threads scan, look up and insert across
/// the root's separator.
#[test]
fn lookups_race_three_levels_lowering_to_two() {
    let shapes = Shapes::probe();
    Exploration {
        random: 5_000,
        pct: 5_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 2);
        let separator = map.leaf_keys()[map.node_lengths()[1][0] + 1][0];
        let (before, inserted) = (separator - 10, separator + 5);
        let steady: Vec<u64> = (0..len - 1).map(key).collect();
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&key(len - 1)).is_some()));
            scope.spawn(|| assert!(map.insert(inserted, flags.value(inserted)).is_none()));
            scope.spawn(|| {
                let found = map.read_many([&before, &separator], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found, [Some(before), Some(separator)]);
                assert_visited_in_order(&scanned_keys(&map, .., same, false), &steady);
            });
        });
        assert_eq!(
            map.node_lengths().len(),
            2,
            "the tree lowered to two levels"
        );
        map.check_invariants(steady.len() + 1);
    });
}

/// Inserts into an empty map, each of which makes the tree unless another
/// thread did, one of them enough keys to split the root leaf on its own:
/// one tree, with every key, whose appends go to its last leaf. The thread
/// that makes the tree holds its root leaf until it points appends there,
/// else a split of it meanwhile would leave them pointing at an internal
/// root. Two threads only: a third holding the leaf shared would make the
/// splitting one wait, and yield to the one that made the tree.
#[test]
fn inserts_race_to_start_the_tree() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let mut map = TrackedMap::new();
        let len = shapes.capacity[0] + 2;
        let low = key(0);
        thread::scope(|scope| {
            scope.spawn(|| {
                let slot = map.get_or_insert(low, flags.value(low), |_, value| {
                    value.check(low);
                    value.slot()
                });
                map.read(&low, |_, value| assert_eq!(value.slot(), slot));
                let found = map.read_many([&low, &key(len - 1)], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found[0], Some(low));
                assert_visited_in_order(&scanned_keys(&map, .., same, true), &[low]);
            });
            scope.spawn(|| {
                for index in 1..len {
                    assert!(map.insert(key(index), flags.value(key(index))).is_none());
                }
            });
        });
        assert_eq!(map.node_lengths().len(), 2, "the root leaf split");
        map.check_invariants(len as usize);
    });
}

/// The last key removed, which empties the tree, while threads insert a key
/// that lacks what the removed key shared with the tree, and one that shares
/// it: what every key shares starts anew from the keys left, or shrinks.
#[test]
fn emptying_the_tree_races_inserts() {
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(|| {
        let flags = DropFlags::new();
        let mut map = map_of(&flags, 1);
        let (outside, sharing) = (1 << 40, key(1));
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&key(0)).is_some()));
            scope.spawn(|| assert!(map.insert(outside, flags.value(outside)).is_none()));
            scope.spawn(|| {
                assert!(map.insert(sharing, flags.value(sharing)).is_none());
                map.read(&key(0), |_, value| value.check(key(0)));
            });
        });
        map.check_invariants(2);
        for key in [outside, sharing] {
            assert!(map.read(&key, |_, value| value.check(key)).is_some());
        }
    });
}

/// Threads race to remove the same key, from a leaf a removal merges, one
/// with `remove`, one with `remove_entry` and one with `remove_if`: exactly
/// one takes it.
#[test]
fn racing_removals_take_an_entry_once() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let contested = key(len - 1);
        let taken = thread::scope(|scope| {
            let removals = [
                scope.spawn(|| map.remove(&contested).map(|value| value.slot())),
                scope.spawn(|| map.remove_entry(&contested).map(|(_, value)| value.slot())),
                scope.spawn(|| {
                    let removed = map.remove_if(&contested, |_, value| {
                        value.check(contested);
                        true
                    });
                    removed.map(|(_, value)| value.slot())
                }),
            ];
            removals.map(|removal| removal.join().unwrap())
        });
        assert_eq!(taken.iter().flatten().count(), 1, "taken once: {taken:?}");
        assert_eq!(map.node_lengths().len(), 1, "the root lowered to a leaf");
        map.check_invariants(len as usize - 1);
    });
}

/// A removal from a leaf at its minimum, whose left sibling is full: the
/// leaf takes entries from it, and the separator between them moves, while
/// threads look up the moving entries and the separator, scan across them,
/// and update the leaf that gives.
#[test]
fn removals_take_entries_from_a_full_sibling_leaf() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 1);
        let left_len = map.leaf_keys()[0].len() as u64;
        // The left leaf filled, with keys between its own.
        for index in 0..shapes.capacity[0] - left_len {
            map.insert(key(index) + 5, flags.value(key(index) + 5));
        }
        let leaves = map.leaf_keys();
        let (giving, separator) = (*leaves[0].last().expect("a full leaf"), leaves[1][0]);
        let steady: Vec<u64> = leaves
            .concat()
            .into_iter()
            .filter(|&k| k != key(len - 1))
            .collect();
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&key(len - 1)).is_some()));
            scope.spawn(|| {
                let found = map.read_many([&giving, &separator], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found, [Some(giving), Some(separator)]);
                assert_visited_in_order(&scanned_keys(&map, .., same, true), &steady);
            });
            scope.spawn(|| {
                assert!(
                    map.update(&giving, |_, value| value.check(giving))
                        .is_some()
                );
                assert!(
                    map.read(&separator, |_, value| value.check(separator))
                        .is_some()
                );
            });
        });
        assert_eq!(map.node_lengths().len(), 2, "the leaves did not merge");
        map.check_invariants(steady.len());
    });
}

/// A removal that merges two leaves under the root's last child, which has
/// its minimum of children, while its sibling has too many to merge with:
/// the last child takes a child from it, through the root, while threads
/// look up and insert keys in the child that moves, and scan across it.
#[test]
fn internal_nodes_take_a_child_from_a_full_sibling() {
    let shapes = Shapes::probe();
    Exploration {
        random: 5_000,
        pct: 5_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 2);
        let right_len = map.node_lengths()[1][1] as u64;
        // The left child given too many children to merge with the right,
        // by keys inserted between its first ones.
        let too_many = shapes.internal_capacity - right_len + 1;
        let mut added = 0;
        'filling: for index in 0.. {
            for offset in 1..4 {
                if map.node_lengths()[1][0] as u64 >= too_many {
                    break 'filling;
                }
                map.insert(key(index) + offset, flags.value(key(index) + offset));
                added += 1;
            }
        }
        let leaves = map.leaf_keys();
        let lengths = map.node_lengths();
        let moving = leaves[lengths[1][0]].clone();
        let separator = leaves[lengths[1][0] + 1][0];
        let inserted = moving[0] + 5;
        let steady: Vec<u64> = leaves
            .concat()
            .into_iter()
            .filter(|&k| k != key(len - 1))
            .collect();
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&key(len - 1)).is_some()));
            scope.spawn(|| assert!(map.insert(inserted, flags.value(inserted)).is_none()));
            scope.spawn(|| {
                let found = map.read_many([&moving[0], &separator], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found, [Some(moving[0]), Some(separator)]);
                let visited = scanned_keys(&map, moving[0].., same, false);
                let after: Vec<u64> = steady.iter().copied().filter(|&k| k >= moving[0]).collect();
                assert_visited_in_order(&visited, &after);
            });
        });
        let lengths = map.node_lengths();
        assert_eq!(lengths.len(), 3, "the internal nodes did not merge");
        assert_eq!(
            lengths[1][1] as u64, right_len,
            "the right one took a child"
        );
        map.check_invariants(len as usize + added);
    });
}

/// Removing the root's separator from a leaf at its minimum, under the
/// root's last child, also at its minimum: the leaf merges, then the
/// internal nodes, and the tree lowers to two levels, in one removal. Other
/// threads insert a key between the separator and its successor, which must
/// go where the separator's successor, not the separator, now bounds it,
/// look up keys on both sides, and scan across. An insert into the leaf
/// first keeps it from merging: some schedules must lower the tree.
#[test]
fn removing_the_roots_separator_lowers_three_levels() {
    let shapes = Shapes::probe();
    let lowered = Arc::new(AtomicUsize::new(0));
    let schedules_lowered = Arc::clone(&lowered);
    Exploration {
        random: 5_000,
        pct: 5_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.one_removal_from_lowering(&flags, 2);
        let lengths = map.node_lengths();
        let separator_leaf = map.leaf_keys()[lengths[1][0] + 1].clone();
        // Down to its minimum, the last leaf's, by removing its last keys:
        // it keeps its head and that many.
        let kept = lengths[2].last().copied().expect("leaves") + 1;
        let trimmed = separator_leaf.len() - kept;
        for key in &separator_leaf[kept..] {
            assert!(map.remove(key).is_some());
        }
        let (separator, successor) = (separator_leaf[0], separator_leaf[1]);
        let (before, between) = (separator - 10, separator + 5);
        let steady: Vec<u64> = (0..len)
            .map(key)
            .filter(|key| *key != separator && !separator_leaf[kept..].contains(key))
            .collect();
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.remove(&separator).is_some()));
            scope.spawn(|| assert!(map.insert(between, flags.value(between)).is_none()));
            scope.spawn(|| {
                let found = map.read_many([&before, &successor], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found, [Some(before), Some(successor)]);
                let visited = scanned_keys(&map, before.., same, true);
                let after: Vec<u64> = steady.iter().copied().filter(|&k| k >= before).collect();
                assert_visited_in_order(&visited, &after);
            });
        });
        if map.node_lengths().len() == 2 {
            schedules_lowered.fetch_add(1, Ordering::Relaxed);
        }
        map.check_invariants(len as usize - trimmed);
    });
    assert!(
        lowered.load(Ordering::Relaxed) > 0,
        "no schedule lowered the tree"
    );
}

/// A key that lacks what every key shares, past every key: its insert
/// shrinks the shared prefix along both borders of a full tree of two
/// levels, then splits the full last leaf and the full root, raising the
/// tree to three levels, while threads insert into the first leaf, on the
/// left border, and look up keys on both.
#[test]
fn shrinking_the_shared_prefix_races_raising_the_root() {
    let shapes = Shapes::probe();
    Exploration {
        random: 5_000,
        pct: 5_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (mut map, len) = shapes.full(&flags, 2);
        let shared = map.shared_prefix_len();
        let (outside, first_leaf, last) = (1 << 40, key(0) + 5, key(len - 1));
        thread::scope(|scope| {
            scope.spawn(|| assert!(map.insert(outside, flags.value(outside)).is_none()));
            scope.spawn(|| assert!(map.insert(first_leaf, flags.value(first_leaf)).is_none()));
            scope.spawn(|| {
                let found = map.read_many([&key(0), &last], |key, value| {
                    value.check(*key);
                    *key
                });
                assert_eq!(found, [Some(key(0)), Some(last)]);
                map.read(&outside, |_, value| value.check(outside));
            });
        });
        assert_eq!(map.node_lengths().len(), 3, "the tree has three levels");
        assert!(map.shared_prefix_len() < shared);
        map.check_invariants(len as usize + 2);
    });
}

// Large tests: random operations against models.

/// A thread that runs random operations on keys of its own, numbered, and
/// checks the map's answers against its model of them; and lookups, scans
/// and cloned ranges over the span of the map the test works in, which hand
/// out the keys it holds.
struct ModelThread<'m, 'f, K> {
    run: &'m ModelRun<'f, K>,
    /// The numbers of the keys this thread owns.
    ids: Vec<u64>,
    /// The values the map holds of them, by slot.
    model: BTreeMap<u64, usize>,
}

impl<K: Ord + Clone + fmt::Debug> ModelThread<'_, '_, K> {
    /// Runs `operations` random operations, then checks every key it owns,
    /// and returns its model.
    fn run(mut self, operations: usize) -> BTreeMap<u64, usize> {
        let mut rng = thread_rng();
        for _ in 0..operations {
            let id = self.ids[rng.gen_range(0..self.ids.len())];
            match rng.gen_range(0..100) {
                0..25 => self.insert(id),
                25..45 => self.remove(id),
                45..52 => self.update(id),
                52..58 => self.remove_if(id),
                58..66 => self.read(id),
                66..76 => {
                    let other = self.ids[rng.gen_range(0..self.ids.len())];
                    self.read_many(id, other);
                }
                76..85 => self.scan(rng.gen_range(0..2) == 1),
                85..93 => self.range_cloned(&mut rng),
                _ => self.get_or_insert(id),
            }
        }
        for id in self.ids.clone() {
            self.read(id);
        }
        self.model
    }

    fn key(&self, id: u64) -> K {
        (self.run.key_of)(id)
    }

    fn insert(&mut self, id: u64) {
        let value = self.run.flags.value(id);
        let slot = value.slot();
        let old = self.run.map.insert(self.key(id), value);
        assert_eq!(old.map(|old| old.slot()), self.model.insert(id, slot));
    }

    fn remove(&mut self, id: u64) {
        let removed = self.run.map.remove(&self.key(id));
        assert_eq!(removed.map(|old| old.slot()), self.model.remove(&id));
    }

    /// Replaces the value in place, which drops the old one while the leaf
    /// is latched exclusively.
    fn update(&mut self, id: u64) {
        let value = self.run.flags.value(id);
        let slot = value.slot();
        let replaced = self.run.map.update(&self.key(id), |_, old| {
            old.check(id);
            mem::replace(old, value).slot()
        });
        let modeled = self.model.get_mut(&id).map(|old| mem::replace(old, slot));
        assert_eq!(replaced, modeled);
    }

    /// Removes the entry if its value's slot is even.
    fn remove_if(&mut self, id: u64) {
        let removed = self.run.map.remove_if(&self.key(id), |_, value| {
            value.check(id);
            value.slot() % 2 == 0
        });
        let modeled = (self.model.get(&id))
            .filter(|&&slot| slot % 2 == 0)
            .copied();
        if modeled.is_some() {
            self.model.remove(&id);
        }
        assert_eq!(removed.map(|(_, value)| value.slot()), modeled);
    }

    fn read(&self, id: u64) {
        let slot = self.run.map.read(&self.key(id), |_, value| {
            value.check(id);
            value.slot()
        });
        assert_eq!(slot.as_ref(), self.model.get(&id));
    }

    /// A batch of two keys of its own and the key numbered after the
    /// first, which another thread owns or no key has.
    fn read_many(&self, id: u64, other: u64) {
        let keys = [self.key(id), self.key(other), self.key(id + 1)];
        let slots = self
            .run
            .map
            .read_many([&keys[0], &keys[1], &keys[2]], |key, value| {
                value.check((self.run.id_of)(key));
                value.slot()
            });
        assert_eq!(slots[0].as_ref(), self.model.get(&id));
        assert_eq!(slots[1].as_ref(), self.model.get(&other));
    }

    fn get_or_insert(&mut self, id: u64) {
        let value = self.run.flags.value(id);
        let slot = value.slot();
        let seen = self.run.map.get_or_insert(self.key(id), value, |_, value| {
            value.check(id);
            value.slot()
        });
        assert_eq!(seen, *self.model.entry(id).or_insert(slot));
    }

    /// A scan of the span, in either direction.
    fn scan(&self, reverse: bool) {
        let visited = scanned_keys(
            &self.run.map,
            self.run.span.clone(),
            self.run.id_of,
            reverse,
        );
        self.assert_hands_out_its_keys(&visited);
    }

    /// A cloned range over the span, taken from either end at random until
    /// the ends meet.
    fn range_cloned(&self, rng: &mut impl Rng) {
        let mut range = self.run.map.range_cloned::<K, _>(self.run.span.clone());
        let (mut front, mut back) = (Vec::new(), Vec::new());
        loop {
            let from_front = rng.gen_range(0..2) == 0;
            let next = if from_front {
                range.next()
            } else {
                range.next_back()
            };
            let Some((key, value)) = next else {
                break;
            };
            value.check((self.run.id_of)(&key));
            if from_front {
                front.push(key);
            } else {
                back.push(key);
            }
        }
        front.extend(back.into_iter().rev());
        self.assert_hands_out_its_keys(&front);
    }

    /// Panics unless `handed_out`, from a scan or a cloned range over the
    /// span, is in order, once each, with every key the model holds and
    /// none of this thread's that it lacks.
    fn assert_hands_out_its_keys(&self, handed_out: &[K]) {
        let held: Vec<K> = self.model.keys().map(|&id| self.key(id)).collect();
        assert_visited_in_order(handed_out, &held);
        for &id in &self.ids {
            if !self.model.contains_key(&id) {
                let key = self.key(id);
                assert!(
                    handed_out.binary_search(&key).is_err(),
                    "handed out removed {key:?}"
                );
            }
        }
    }
}

/// A map shared by model threads, how to tell its keys by number, and the
/// span of it the threads' keys lie in, which their scans cover.
struct ModelRun<'f, K> {
    map: TrackedMap<'f, K>,
    flags: &'f DropFlags,
    key_of: fn(u64) -> K,
    id_of: fn(&K) -> u64,
    span: (Bound<K>, Bound<K>),
}

impl<'f, K: Ord + Clone + fmt::Debug + Send + Sync> ModelRun<'f, K> {
    /// Runs a thread for each of `owned`, the numbers of the keys it owns,
    /// with `operations` random operations; then checks that the map holds
    /// what the threads' models do and the keys no thread owns, in a
    /// well-formed tree.
    fn run(mut self, owned: Vec<Vec<u64>>, operations: usize) {
        let all = scanned_keys(&self.map, .., self.id_of, false);
        let owned_ids: BTreeSet<u64> = owned.iter().flatten().copied().collect();
        let mut expected: BTreeSet<u64> = (all.iter().map(self.id_of))
            .filter(|id| !owned_ids.contains(id))
            .collect();
        let threads: Vec<ModelThread<'_, 'f, K>> = (owned.into_iter())
            .map(|ids| ModelThread {
                run: &self,
                model: self.model_of(&ids),
                ids,
            })
            .collect();
        let models: Vec<BTreeMap<u64, usize>> = thread::scope(|scope| {
            let handles: Vec<_> = (threads.into_iter())
                .map(|thread| scope.spawn(move || thread.run(operations)))
                .collect();
            (handles.into_iter())
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        expected.extend(models.iter().flat_map(|model| model.keys().copied()));
        let held: BTreeSet<u64> = (scanned_keys(&self.map, .., self.id_of, false).iter())
            .map(self.id_of)
            .collect();
        assert_eq!(held, expected);
        self.map.check_invariants(held.len());
    }

    /// What the map holds of the keys numbered `ids`, by slot.
    fn model_of(&self, ids: &[u64]) -> BTreeMap<u64, usize> {
        (ids.iter())
            .filter_map(|&id| {
                let slot = self.map.read(&(self.key_of)(id), |_, value| value.slot());
                slot.map(|slot| (id, slot))
            })
            .collect()
    }
}

/// `ids` dealt out to `threads` threads in turn.
fn dealt(ids: impl Iterator<Item = u64>, threads: usize) -> Vec<Vec<u64>> {
    let mut owned = std::vec![Vec::new(); threads];
    for (turn, id) in ids.enumerate() {
        owned[turn % threads].push(id);
    }
    owned
}

/// Three threads on keys about a leaf and a half of entries: the root leaf
/// splits, the tree lowers back, and leaves split and merge, over and over,
/// under lookups and scans.
#[test]
fn threads_match_their_models_around_one_leaf() {
    let shapes = Shapes::probe();
    Exploration {
        random: 100_000,
        pct: 100_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let capacity = shapes.capacity[0];
        let map = map_of(&flags, capacity / 2);
        let owned = dealt((0..capacity + capacity / 2).map(key), 3);
        ModelRun {
            map,
            flags: &flags,
            key_of: |id| id,
            id_of: same,
            span: (Bound::Unbounded, Bound::Unbounded),
        }
        .run(owned, 12);
    });
}

/// Three threads on the keys of the last leaves of a tree of three levels,
/// which a merge of leaves under the root's last child lowers to two:
/// leaves split and merge, the last child takes children from its sibling
/// or merges with it, and the tree lowers and grows back.
#[test]
fn threads_match_their_models_in_three_levels() {
    let shapes = Shapes::probe();
    Exploration {
        random: 20_000,
        pct: 20_000,
    }
    .run(move || {
        let flags = DropFlags::new();
        let (map, len) = shapes.one_removal_from_lowering(&flags, 2);
        // Three leaves' worth of keys from the end, and keys between them.
        let (first, last) = (key(len - 1) - 3 * shapes.capacity[0] / 2 * 10, key(len + 2));
        let owned = dealt((first..last).step_by(5), 3);
        ModelRun {
            map,
            flags: &flags,
            key_of: |id| id,
            id_of: same,
            span: (Bound::Included(first), Bound::Excluded(last)),
        }
        .run(owned, 12);
    });
}

/// [`threads_match_their_models_around_one_leaf`] with string keys in two
/// runs that share their first bytes past what every key shares: internal
/// nodes keep 4-byte windows of them, which tie, and a descent compares the
/// whole key at the head of the leaf right of a tied separator, latched.
#[test]
fn string_keys_tie_at_separators_while_threads_change_them() {
    Exploration {
        random: 50_000,
        pct: 50_000,
    }
    .run(|| {
        fn key_of(id: u64) -> String {
            format!("tie/{}/{id:06}", id % 2)
        }
        #[expect(clippy::ptr_arg, reason = "the map's keys are `String`s")]
        fn id_of(key: &String) -> u64 {
            key[key.len() - 6..].parse().expect("a numbered key")
        }
        let flags = DropFlags::new();
        let map = TrackedMap::<String>::new();
        for id in 0..12 {
            map.insert(key_of(id), flags.value(id));
        }
        ModelRun {
            map,
            flags: &flags,
            key_of,
            id_of,
            span: (Bound::Unbounded, Bound::Unbounded),
        }
        .run(dealt(0..30, 3), 10);
    });
}
