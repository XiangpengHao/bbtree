//! Drop and panic tests: every key and value is dropped exactly once, and in
//! the same order as by std's `BTreeMap`, including replaced, removed and
//! panicking ones and separators, whose values live in leaf heads (D17); and
//! a panicking `Ord` leaves the map unchanged.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cell::Cell;
use core::cmp::Ordering;
use core::panic::AssertUnwindSafe;
use std::collections::BTreeMap;
use std::panic::catch_unwind;
use std::vec::Vec;

use super::BBTreeMap;
use crate::node::test_support::{DropLedger, TestRng, Tracked};

/// A drop-tracked key ordered by `order` alone: keys of the same order
/// collide in a map but stay apart in the ledger.
///
/// Padded to 64 bytes so that, with [`Tracked`] values, a 1 KB node holds 12
/// entries in a leaf and 14 in an internal node (D13, D15, D17): tall trees
/// from few entries, which Miri can afford.
struct CollidingKey<'l> {
    order: usize,
    tracked: Tracked<'l>,
    _padding: [u8; 40],
}

impl<'l> CollidingKey<'l> {
    fn new(order: usize, tracked: Tracked<'l>) -> Self {
        Self {
            order,
            tracked,
            _padding: [0; 40],
        }
    }

    fn id(&self) -> usize {
        self.tracked.id()
    }
}

impl PartialEq for CollidingKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.order == other.order
    }
}

impl Eq for CollidingKey<'_> {}

impl PartialOrd for CollidingKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CollidingKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order.cmp(&other.order)
    }
}

impl Borrow<usize> for CollidingKey<'_> {
    fn borrow(&self) -> &usize {
        &self.order
    }
}

/// Two drop ledgers fed in lockstep, one for what goes into our map and one
/// for std's: the same calls hand out the same ids, so the same drop
/// behavior records the same drop order.
struct LockstepLedgers {
    ours: DropLedger,
    theirs: DropLedger,
}

impl LockstepLedgers {
    fn new() -> Self {
        Self {
            ours: DropLedger::new(),
            theirs: DropLedger::new(),
        }
    }

    /// The same tracked value for each map.
    fn track(&self, panics_on_drop: bool) -> (Tracked<'_>, Tracked<'_>) {
        if panics_on_drop {
            (
                self.ours.track_panicking_on_drop(),
                self.theirs.track_panicking_on_drop(),
            )
        } else {
            (self.ours.track(), self.theirs.track())
        }
    }

    fn assert_same_drops_so_far(&self) {
        assert_eq!(self.ours.drop_order(), self.theirs.drop_order());
    }
}

/// Which part of a new entry panics when dropped.
#[derive(Clone, Copy, PartialEq)]
enum PanickingPart {
    Neither,
    Key,
    Value,
}

type TrackedMap<'l> = BBTreeMap<CollidingKey<'l>, Tracked<'l>>;
type StdTrackedMap<'l> = BTreeMap<CollidingKey<'l>, Tracked<'l>>;

/// Our map and std's, fed the same operations on entries from
/// [`LockstepLedgers`].
struct TrackedMaps<'l> {
    ledgers: &'l LockstepLedgers,
    ours: TrackedMap<'l>,
    theirs: StdTrackedMap<'l>,
}

impl<'l> TrackedMaps<'l> {
    fn new(ledgers: &'l LockstepLedgers) -> Self {
        Self {
            ledgers,
            ours: BBTreeMap::new(),
            theirs: BTreeMap::new(),
        }
    }

    /// Inserts a new entry with key `order` into both maps, each catching a
    /// panic and dropping the replaced value, if any. Returns whether they
    /// panicked, which they must agree on.
    fn insert(&mut self, order: usize, panicking: PanickingPart) -> bool {
        let (our_key, their_key) = self.ledgers.track(panicking == PanickingPart::Key);
        let (our_value, their_value) = self.ledgers.track(panicking == PanickingPart::Value);
        let ours = catch_unwind(AssertUnwindSafe(|| {
            let key = CollidingKey::new(order, our_key);
            let old_value = self.ours.insert(key, our_value);
            old_value.map(|old_value| old_value.id())
        }));
        let theirs = catch_unwind(AssertUnwindSafe(|| {
            let key = CollidingKey::new(order, their_key);
            let old_value = self.theirs.insert(key, their_value);
            old_value.map(|old_value| old_value.id())
        }));
        let (ours, theirs) = (ours.ok(), theirs.ok());
        assert_eq!(ours, theirs);
        assert_eq!(self.ours.len(), self.theirs.len());
        ours.is_none()
    }

    /// Replaces the value of key `order` through `get_mut`, if it is there.
    fn update(&mut self, order: usize) {
        let (our_value, their_value) = self.ledgers.track(false);
        match (self.ours.get_mut(&order), self.theirs.get_mut(&order)) {
            (Some(ours), Some(theirs)) => {
                assert_eq!(ours.id(), theirs.id());
                *ours = our_value;
                *theirs = their_value;
            }
            (ours, theirs) => assert_eq!(ours.is_some(), theirs.is_some()),
        }
    }

    /// Removes key `order` from both maps, through `remove_entry` if
    /// `with_key`, otherwise through `remove`, which drops the key itself.
    /// The caller gets the same entry from both, and drops it.
    fn remove(&mut self, order: usize, with_key: bool) {
        if with_key {
            let ids = |(key, value): (CollidingKey<'_>, Tracked<'_>)| (key.id(), value.id());
            let ours = self.ours.remove_entry(&order).map(ids);
            assert_eq!(ours, self.theirs.remove_entry(&order).map(ids));
        } else {
            let ours = self.ours.remove(&order).map(|value| value.id());
            assert_eq!(ours, self.theirs.remove(&order).map(|value| value.id()));
        }
        assert_eq!(self.ours.len(), self.theirs.len());
    }

    /// Removes the first entry, or the last if `last`, from both maps.
    fn pop(&mut self, last: bool) {
        let ids = |(key, value): (CollidingKey<'_>, Tracked<'_>)| (key.id(), value.id());
        let (ours, theirs) = if last {
            (self.ours.pop_last(), self.theirs.pop_last())
        } else {
            (self.ours.pop_first(), self.theirs.pop_first())
        };
        assert_eq!(ours.map(ids), theirs.map(ids));
        assert_eq!(self.ours.len(), self.theirs.len());
    }

    /// Looks up key `order` in both maps: the same key, by its tracked id,
    /// and the same value.
    fn get(&self, order: usize) {
        let ours = self.ours.get(&order).map(Tracked::id);
        assert_eq!(ours, self.theirs.get(&order).map(Tracked::id));
        let ids = |(key, value): (&CollidingKey<'_>, &Tracked<'_>)| (key.id(), value.id());
        let ours = self.ours.get_key_value(&order).map(ids);
        assert_eq!(ours, self.theirs.get_key_value(&order).map(ids));
        assert_eq!(
            self.ours.contains_key(&order),
            self.theirs.contains_key(&order)
        );
    }

    /// Drops both maps, each catching a panic, and checks that they dropped
    /// the same values in the same order, every value once. Returns whether
    /// they panicked, which they must agree on.
    fn drop_both(self) -> bool {
        let Self {
            ledgers,
            ours,
            theirs,
        } = self;
        let ours = catch_unwind(AssertUnwindSafe(|| drop(ours)));
        let theirs = catch_unwind(AssertUnwindSafe(|| drop(theirs)));
        assert_eq!(ours.is_err(), theirs.is_err());
        ledgers.assert_same_drops_so_far();
        ledgers.ours.assert_all_dropped_once();
        ledgers.theirs.assert_all_dropped_once();
        ours.is_err()
    }
}

#[test]
fn drops_match_std() {
    let ledgers = LockstepLedgers::new();
    let rng = TestRng::new(21);
    let mut maps = TrackedMaps::new(&ledgers);
    let (steps, key_space) = if cfg!(miri) {
        (800, 400)
    } else {
        (20_000, 4000)
    };
    for _ in 0..steps {
        let order = rng.below(key_space);
        match rng.below(4) {
            0 | 1 => assert!(!maps.insert(order, PanickingPart::Neither)),
            2 => maps.update(order),
            _ => maps.get(order),
        }
    }
    ledgers.assert_same_drops_so_far();
    assert!(maps.ours.height() >= 2);
    maps.ours.check_invariants();
    assert!(!maps.drop_both());
}

/// Removals hand the entry to the caller, except that `remove` drops the
/// key, as std's does: random removals among inserts, then every separator
/// by key, whose value comes from a leaf's head (D17), then the rest.
#[test]
fn removals_drop_like_std() {
    let ledgers = LockstepLedgers::new();
    let rng = TestRng::new(24);
    let mut maps = TrackedMaps::new(&ledgers);
    let (count, steps) = if cfg!(miri) {
        (250, 500)
    } else {
        (3000, 20_000)
    };
    let mut orders: Vec<usize> = (0..count).collect();
    rng.shuffle(&mut orders);
    for order in orders {
        assert!(!maps.insert(order, PanickingPart::Neither));
    }
    for _ in 0..steps {
        let order = rng.below(count);
        match rng.below(8) {
            0..=2 => assert!(!maps.insert(order, PanickingPart::Neither)),
            3 => maps.remove(order, false),
            4 => maps.remove(order, true),
            5 => maps.pop(rng.below(2) == 0),
            _ => maps.get(order),
        }
        ledgers.assert_same_drops_so_far();
    }
    assert!(maps.ours.height() >= 2);
    maps.ours.check_invariants();

    let separators: Vec<usize> = maps
        .ours
        .separator_keys()
        .iter()
        .map(|key| key.order)
        .collect();
    for order in separators {
        maps.remove(order, rng.below(2) == 0);
        ledgers.assert_same_drops_so_far();
    }
    maps.ours.check_invariants();
    while !maps.theirs.is_empty() {
        maps.pop(rng.below(2) == 0);
    }
    ledgers.assert_same_drops_so_far();
    maps.ours.check_invariants();
    assert!(!maps.drop_both());
}

#[test]
fn replacing_keeps_the_old_key_and_drops_each_once() {
    let ledger = DropLedger::new();
    let mut map = BBTreeMap::new();
    let (old_key, old_value) = (ledger.track(), ledger.track());
    let (old_key_id, old_value_id) = (old_key.id(), old_value.id());
    let old_key = CollidingKey::new(7, old_key);
    assert!(map.insert(old_key, old_value).is_none());

    let (new_key, new_value) = (ledger.track(), ledger.track());
    let (new_key_id, new_value_id) = (new_key.id(), new_value.id());
    let new_key = CollidingKey::new(7, new_key);
    let replaced = map.insert(new_key, new_value).expect("key 7 is there");
    assert_eq!(replaced.id(), old_value_id);
    assert_eq!(
        ledger.drop_order(),
        [new_key_id],
        "only the new key is gone"
    );
    assert_eq!(map.get(&7_usize).map(Tracked::id), Some(new_value_id));
    let ids = map
        .get_key_value(&7_usize)
        .map(|(key, value)| (key.id(), value.id()));
    assert_eq!(ids, Some((old_key_id, new_value_id)));
    assert_eq!(map.len(), 1);

    drop(replaced);
    drop(map);
    assert_eq!(
        ledger.drop_order(),
        [new_key_id, old_value_id, old_key_id, new_value_id]
    );
    ledger.assert_all_dropped_once();
}

/// Inserting the key of a separator replaces its value, a leaf's head, and
/// keeps the old key in its internal node, as std keeps its key (D17).
#[test]
fn replacing_separators_matches_std() {
    let ledgers = LockstepLedgers::new();
    let mut maps = TrackedMaps::new(&ledgers);
    let count = if cfg!(miri) { 200 } else { 2000 };
    let mut orders: Vec<usize> = (0..count).collect();
    TestRng::new(23).shuffle(&mut orders);
    for &order in &orders {
        assert!(!maps.insert(order, PanickingPart::Neither));
    }
    assert!(maps.ours.height() >= 2);
    let separators: Vec<usize> = maps
        .ours
        .separator_keys()
        .iter()
        .map(|key| key.order)
        .collect();
    assert!(!separators.is_empty());
    for order in separators {
        assert!(!maps.insert(order, PanickingPart::Neither));
        maps.update(order);
        maps.get(order);
        ledgers.assert_same_drops_so_far();
    }
    maps.ours.check_invariants();
    assert!(!maps.drop_both());
}

#[test]
fn a_panicking_new_key_leaves_both_maps_unchanged() {
    let ledgers = LockstepLedgers::new();
    let mut maps = TrackedMaps::new(&ledgers);
    let count = if cfg!(miri) { 150 } else { 1000 };
    for order in 0..count {
        assert!(!maps.insert(order, PanickingPart::Neither));
    }
    for order in [0, count / 2, count - 1] {
        assert!(maps.insert(order, PanickingPart::Key));
        maps.get(order);
        ledgers.assert_same_drops_so_far();
    }
    maps.ours.check_invariants();
    assert!(!maps.drop_both());
}

/// `clear` drops the entries as dropping the map does, and leaves the map
/// empty and usable even if one of their `Drop`s panics, as std's does.
#[test]
fn a_panicking_drop_while_clearing_matches_std() {
    let ledgers = LockstepLedgers::new();
    let mut maps = TrackedMaps::new(&ledgers);
    let count = if cfg!(miri) { 200 } else { 600 };
    for order in 0..count {
        let panicking = if order == count / 2 {
            PanickingPart::Value
        } else {
            PanickingPart::Neither
        };
        assert!(!maps.insert(order, panicking));
    }
    assert!(maps.ours.height() >= 2);
    let ours = catch_unwind(AssertUnwindSafe(|| maps.ours.clear()));
    let theirs = catch_unwind(AssertUnwindSafe(|| maps.theirs.clear()));
    assert!(ours.is_err() && theirs.is_err());
    ledgers.assert_same_drops_so_far();
    assert!(maps.ours.is_empty() && maps.theirs.is_empty());
    maps.ours.check_invariants();

    assert!(!maps.insert(1, PanickingPart::Neither));
    maps.get(1);
    maps.ours.clear();
    maps.theirs.clear();
    ledgers.assert_same_drops_so_far();
    assert!(!maps.drop_both());
}

#[test]
fn a_panicking_drop_while_dropping_the_map_matches_std() {
    // Above 194 entries (15 leaves of 12, plus 14 separators), the tree
    // needs a height of at least 2.
    let count = if cfg!(miri) { 200 } else { 600 };
    let cases = [
        (0, PanickingPart::Key),
        (count / 3, PanickingPart::Value),
        (count / 2, PanickingPart::Key),
        (count - 1, PanickingPart::Value),
    ];
    for (panicking_order, part) in cases {
        let ledgers = LockstepLedgers::new();
        let mut maps = TrackedMaps::new(&ledgers);
        let mut orders: Vec<usize> = (0..count).collect();
        TestRng::new(panicking_order as u64).shuffle(&mut orders);
        for order in orders {
            let panicking = if order == panicking_order {
                part
            } else {
                PanickingPart::Neither
            };
            assert!(!maps.insert(order, panicking));
        }
        // Checked once the maps are gone: unwinding through a map with a
        // panicking entry would abort.
        let height = maps.ours.height();
        assert!(maps.drop_both(), "both maps panic, and finish dropping");
        assert!(height >= 2);
    }
}

/// A key whose comparisons spend a shared budget, and panic once it is spent.
///
/// Padded to 72 bytes so that, with `u64` values, a 1 KB node holds 12
/// entries in both kinds (D13, D15, D17): tall trees from few entries.
struct BudgetedKey<'b> {
    number: u64,
    budget: &'b Cell<usize>,
    _padding: [u8; 56],
}

impl<'b> BudgetedKey<'b> {
    fn new(number: u64, budget: &'b Cell<usize>) -> Self {
        Self {
            number,
            budget,
            _padding: [0; 56],
        }
    }
}

impl PartialEq for BudgetedKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for BudgetedKey<'_> {}

impl PartialOrd for BudgetedKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BudgetedKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let budget = self.budget.get();
        assert!(budget > 0, "the comparison budget is spent");
        self.budget.set(budget - 1);
        self.number.cmp(&other.number)
    }
}

impl Borrow<u64> for BudgetedKey<'_> {
    fn borrow(&self) -> &u64 {
        &self.number
    }
}

#[test]
fn a_panicking_ord_leaves_the_map_unchanged() {
    let budget = Cell::new(usize::MAX);
    let rng = TestRng::new(22);
    let (count, trials) = if cfg!(miri) { (200, 12) } else { (1000, 1000) };
    let mut map: BBTreeMap<BudgetedKey<'_>, u64> = BBTreeMap::new();
    let mut model: BTreeMap<u64, u64> = BTreeMap::new();
    for _ in 0..count {
        let number = rng.below(2 * count) as u64;
        let key = BudgetedKey::new(number, &budget);
        assert_eq!(map.insert(key, number), model.insert(number, number));
    }
    assert!(map.height() >= 2);

    let (mut panicked, mut completed) = (0, 0);
    for trial in 0..trials {
        let number = rng.below(2 * count) as u64;
        let key = BudgetedKey::new(number, &budget);
        let seed = (count + trial) as u64;
        let inserting = rng.below(2) == 0;
        budget.set(rng.below(20));
        // A removal by `BudgetedKey`, not by `u64`, so that its `Ord` runs.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if inserting {
                map.insert(key, seed)
            } else {
                map.remove(&key)
            }
        }));
        budget.set(usize::MAX);
        match outcome {
            Ok(old_value) if inserting => {
                completed += 1;
                assert_eq!(old_value, model.insert(number, seed));
            }
            Ok(removed_value) => {
                completed += 1;
                assert_eq!(removed_value, model.remove(&number));
            }
            Err(_) => panicked += 1,
        }
        map.check_invariants();
        assert_eq!(map.len(), model.len());
        for (number, &seed) in &model {
            assert_eq!(map.get(number), Some(&seed));
        }
    }
    assert!(
        panicked > 0 && completed > 0,
        "{panicked} panicked, {completed} completed"
    );
}
