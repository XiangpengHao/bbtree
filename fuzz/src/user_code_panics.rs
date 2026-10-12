//! Fuzzing the map against user code that misbehaves: keys whose `Ord` and
//! `InlineKey` panic or lie, and keys and values whose `Clone` and `Drop`
//! panic. Like std's, the map promises wrong answers at worst, never
//! undefined behavior, and its unsafe code (D14) is where that promise can
//! break: this target runs under AddressSanitizer and checks, after every
//! operation, that
//!
//! - the keys and values alive are those in the map, and none was dropped
//!   twice (a ledger of ids);
//! - with honest keys, the tree is well formed (`check_invariants`) and
//!   agrees with a model: unchanged when `Ord` panicked, as the map calls it
//!   only while it searches, never while keys move (D26); taken as it is
//!   after a panic in a bulk change or a drop, which may leave it with fewer
//!   entries, but well formed. An `InlineKey` that panics may stop an
//!   operation once its change is done, before the nodes it changed are fit
//!   to their fences (`node/prefix.rs`): the entry is then in or out, and
//!   those nodes keep prefixes shorter than their fences share, which
//!   searches allow but `check_invariants` doesn't, so from then on the run
//!   checks prefix lengths (`check_prefix_lens`) and answers only;
//! - with any keys, no node's prefix is shorter than its parent's (D44),
//!   the nodes along the tree's borders keep prefix 0, which searches past
//!   the first or last key read, and the map's length is the number of
//!   entries it walks; and, with lying keys, no panic but the hostile ones:
//!   std's map panics on an inconsistent `Ord` only in `range`, which this
//!   target doesn't call.
//!
//! At the end, dropping the map drops every key and value left, once.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::Bound;
use std::panic::{self, AssertUnwindSafe};

use bbtree::bbtree_map::Entry;
use bbtree::{Auto, BBTreeMap, InlineKey, InlineKeyStrategy, InlineWord, WithInlineKey};
use libfuzzer_sys::arbitrary::{self, Arbitrary};

use crate::operation_list::Operations;
use crate::position_mask::PositionMask;

/// One input of the target.
#[derive(Arbitrary, Debug)]
pub struct HostileInput {
    strategy: Strategy,
    conduct: Conduct,
    steps: Operations<HostileStep>,
}

impl HostileInput {
    /// Silences the panics that hostile callbacks raise, and leaves every
    /// other panic to the hook installed before, which under libFuzzer
    /// aborts, reporting it. Call once, before the first run.
    pub fn silence_hostile_panics() {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !info.payload().is::<HostilePanic>() {
                previous(info);
            }
        }));
    }

    pub fn run(&self) {
        let host = Host::new(self.conduct);
        match self.strategy {
            Strategy::Auto => HostileMap::<Auto>::run(&host, &self.steps),
            Strategy::WithInlineKey => HostileMap::<WithInlineKey>::run(&host, &self.steps),
        }
    }
}

/// The payload of every panic that a hostile callback raises.
struct HostilePanic;

/// The map's strategy: which user callbacks its searches call.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum Strategy {
    /// No inline keys: `Ord` alone.
    Auto,
    /// `InlineKey`, and `Ord` where inline keys tie.
    WithInlineKey,
}

/// How the keys keep their contracts.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
enum Conduct {
    /// They keep them, but for the panics.
    Honest,
    /// `Ord` answers at random.
    LyingOrder,
    /// `InlineKey` answers at random: windows that don't order like keys.
    LyingEncoding,
}

/// A user callback that the input can make panic.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
enum Callback {
    Comparison,
    Encoding,
    Clone,
}

/// A callback armed to panic once `calls_left` more of its calls return.
#[derive(Arbitrary, Debug, Clone, Copy)]
struct Trap {
    callback: Callback,
    calls_left: u8,
}

/// What made an operation panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hostility {
    Trap(Callback),
    /// The predicate of `retain` or `extract_if`.
    Predicate,
    /// A key or value marked to panic when dropped.
    Drop,
}

/// One operation, with a trap armed while it runs, if any.
#[derive(Arbitrary, Debug)]
struct HostileStep {
    trap: Option<Trap>,
    operation: HostileOperation,
}

/// Which part of a new entry panics when dropped.
#[derive(Arbitrary, Debug, Clone, Copy, PartialEq, Eq)]
enum DropPanic {
    Neither,
    Key,
    Value,
}

/// One operation on the map and the model, with keys by their order.
#[derive(Arbitrary, Debug)]
enum HostileOperation {
    Insert {
        order: u16,
        drop_panic: DropPanic,
    },
    /// Inserts `count` keys of consecutive orders from `first`: tall trees
    /// from short inputs.
    InsertRun {
        first: u16,
        count: u8,
    },
    Get {
        order: u16,
    },
    Remove {
        order: u16,
    },
    /// One use of the entry API, picked by `choice`.
    Entry {
        order: u16,
        choice: u8,
    },
    PopFirst,
    PopLast,
    /// Splits the map at `order`, and appends the upper part back, or drops
    /// it.
    SplitOff {
        order: u16,
        append_back: bool,
    },
    /// Appends a map of these keys.
    Append {
        orders: Vec<u16>,
    },
    /// Keeps the entries that `keep` picks; the predicate panics on call
    /// `panics_at`, if any.
    Retain {
        keep: PositionMask,
        panics_at: Option<u8>,
    },
    /// Extracts the entries in `range` that `extract` picks, at most `take`
    /// of them; the predicate panics on call `panics_at`, if any.
    ExtractIf {
        range: OrderRange,
        extract: PositionMask,
        take: u8,
        panics_at: Option<u8>,
    },
    /// Clones the map, then drops the copy.
    Clone,
    /// Takes the map by value, takes `count` entries from the front, then
    /// drops the rest: the map starts over empty.
    IntoIter {
        count: u8,
    },
    Clear,
}

impl HostileOperation {
    /// Whether a panicking `callback` leaves the map as it was: `Ord` does
    /// in an operation that calls it only while it searches (D26), and
    /// `Clone` does, as only `clone` calls it, on the copy. A bulk change
    /// calls `Ord` while it moves entries, and the map fits prefixes by
    /// `InlineKey` after it changes (`node/prefix.rs`).
    fn trap_leaves_map_unchanged(&self, callback: Callback) -> bool {
        match (callback, self) {
            (Callback::Encoding, _) => false,
            (_, Self::SplitOff { append_back, .. }) => !append_back,
            (_, Self::Append { .. } | Self::Retain { .. } | Self::ExtractIf { .. }) => false,
            _ => true,
        }
    }
}

/// A range of key orders from the input: from `start`, included, up to
/// `end`, excluded, each end unbounded if absent. A range out of order is
/// taken the other way round, as both maps panic for one.
#[derive(Arbitrary, Debug, Clone, Copy)]
struct OrderRange {
    start: Option<u16>,
    end: Option<u16>,
}

impl OrderRange {
    /// The ends in order: unbounded when `Ord` lies, which may find the
    /// start after the end, for which the map panics, as std's does.
    fn ends(self, conduct: Conduct) -> (Option<u16>, Option<u16>) {
        match (self.start, self.end) {
            _ if conduct == Conduct::LyingOrder => (None, None),
            (Some(start), Some(end)) if end < start => (Some(end), Some(start)),
            ends => ends,
        }
    }

    /// The range as bounds on the model's keys.
    fn order_bounds(self, conduct: Conduct) -> (Bound<u16>, Bound<u16>) {
        let (start, end) = self.ends(conduct);
        (
            start.map_or(Bound::Unbounded, Bound::Included),
            end.map_or(Bound::Unbounded, Bound::Excluded),
        )
    }
}

/// Whether an id is a key's or a value's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Key,
    Value,
}

/// What a run's keys and values share: how the keys keep their contracts,
/// the armed trap, the lies, and the ledger of every key and value made.
struct Host {
    conduct: Conduct,
    trap: Cell<Option<Trap>>,
    /// Whether a key or value marked to panic when dropped may do so: set
    /// with each operation, spent by any hostile panic, as a second panic
    /// while the first unwinds would abort.
    drop_panic_armed: Cell<bool>,
    /// What made the current operation panic, if anything did.
    fired: Cell<Option<Hostility>>,
    /// The state of the xorshift generator that the lies come from.
    lies: Cell<u64>,
    /// Per id, whether its key or value is alive.
    alive: RefCell<Vec<bool>>,
    live_keys: Cell<usize>,
    live_values: Cell<usize>,
}

impl Host {
    fn new(conduct: Conduct) -> Self {
        Self {
            conduct,
            trap: Cell::new(None),
            drop_panic_armed: Cell::new(false),
            fired: Cell::new(None),
            lies: Cell::new(0x9E37_79B9_7F4A_7C15),
            alive: RefCell::new(Vec::new()),
            live_keys: Cell::new(0),
            live_values: Cell::new(0),
        }
    }

    fn is_honest(&self) -> bool {
        self.conduct == Conduct::Honest
    }

    /// Asserts that the map answered `ours` where the model answers
    /// `model`, if the keys are honest: lying keys give wrong answers.
    fn expect<T: PartialEq + std::fmt::Debug>(&self, ours: T, model: T) {
        if self.is_honest() {
            assert_eq!(ours, model);
        }
    }

    /// Arms `trap` and drop panics for the next operation.
    fn arm(&self, trap: Option<Trap>) {
        self.trap.set(trap);
        self.drop_panic_armed.set(true);
        self.fired.set(None);
    }

    fn disarm(&self) {
        self.trap.set(None);
        self.drop_panic_armed.set(false);
    }

    /// Counts a call of `callback`, which panics if its trap is spent.
    fn call(&self, callback: Callback) {
        let Some(trap) = self.trap.get().filter(|trap| trap.callback == callback) else {
            return;
        };
        match trap.calls_left.checked_sub(1) {
            Some(calls_left) => self.trap.set(Some(Trap { calls_left, ..trap })),
            None => self.raise(Hostility::Trap(callback)),
        }
    }

    /// Panics for `hostility`, disarming everything first.
    fn raise(&self, hostility: Hostility) -> ! {
        self.disarm();
        self.fired.set(Some(hostility));
        panic::panic_any(HostilePanic)
    }

    fn lie(&self) -> u64 {
        let mut state = self.lies.get();
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.lies.set(state);
        state
    }

    fn new_id(&self, kind: Kind) -> u32 {
        let live = self.live(kind);
        live.set(live.get() + 1);
        let mut alive = self.alive.borrow_mut();
        alive.push(true);
        u32::try_from(alive.len() - 1).expect("fewer than 2^32 keys and values")
    }

    /// Records the drop of `id`, which then panics if it is marked to and
    /// may.
    fn drop_id(&self, id: u32, kind: Kind, panics_on_drop: bool) {
        let was_alive = std::mem::replace(&mut self.alive.borrow_mut()[id as usize], false);
        assert!(was_alive, "a key or value is dropped twice");
        let live = self.live(kind);
        live.set(live.get() - 1);
        if panics_on_drop && self.drop_panic_armed.get() && !std::thread::panicking() {
            self.raise(Hostility::Drop);
        }
    }

    /// A predicate for `retain` or `extract_if`: picks by `mask`, and panics
    /// on call `panics_at`, if any.
    fn predicate(&self, mask: PositionMask, panics_at: Option<u8>) -> impl FnMut() -> bool {
        let (mut pick, mut calls) = (mask.picker(), 0_u32);
        move || {
            if panics_at.is_some_and(|panics_at| calls == u32::from(panics_at)) {
                self.raise(Hostility::Predicate);
            }
            calls += 1;
            pick()
        }
    }

    fn live(&self, kind: Kind) -> &Cell<usize> {
        match kind {
            Kind::Key => &self.live_keys,
            Kind::Value => &self.live_values,
        }
    }

    fn key(&self, order: u16, panics_on_drop: bool) -> HostileKey<'_> {
        HostileKey {
            order,
            id: self.new_id(Kind::Key),
            panics_on_drop,
            host: self,
            _padding: [0; 48],
        }
    }

    fn value(&self, panics_on_drop: bool) -> HostileValue<'_> {
        HostileValue {
            id: self.new_id(Kind::Value),
            panics_on_drop,
            host: self,
        }
    }
}

/// A key ordered by `order`, whose callbacks the host may make panic or
/// lie. Padded to 64 bytes, about a dozen to a node of either kind, so that
/// trees grow tall from short inputs.
struct HostileKey<'h> {
    order: u16,
    id: u32,
    panics_on_drop: bool,
    host: &'h Host,
    _padding: [u8; 48],
}

impl PartialEq for HostileKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for HostileKey<'_> {}

impl PartialOrd for HostileKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HostileKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.host.call(Callback::Comparison);
        match self.host.conduct {
            Conduct::LyingOrder => (self.host.lie() % 3).cmp(&1),
            Conduct::Honest | Conduct::LyingEncoding => self.order.cmp(&other.order),
        }
    }
}

impl InlineKey for HostileKey<'_> {
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        self.host.call(Callback::Encoding);
        match self.host.conduct {
            Conduct::LyingEncoding => self.host.lie(),
            Conduct::Honest | Conduct::LyingOrder => u64::from(self.order).inline_key_at(offset),
        }
    }
}

impl Clone for HostileKey<'_> {
    fn clone(&self) -> Self {
        self.host.call(Callback::Clone);
        self.host.key(self.order, self.panics_on_drop)
    }
}

impl Drop for HostileKey<'_> {
    fn drop(&mut self) {
        self.host.drop_id(self.id, Kind::Key, self.panics_on_drop);
    }
}

struct HostileValue<'h> {
    id: u32,
    panics_on_drop: bool,
    host: &'h Host,
}

impl Clone for HostileValue<'_> {
    fn clone(&self) -> Self {
        self.host.call(Callback::Clone);
        self.host.value(self.panics_on_drop)
    }
}

impl Drop for HostileValue<'_> {
    fn drop(&mut self) {
        self.host.drop_id(self.id, Kind::Value, self.panics_on_drop);
    }
}

/// The map under test and a model of what it holds: each key's order and
/// its value's id.
struct HostileMap<'h, S: InlineKeyStrategy<HostileKey<'h>>> {
    host: &'h Host,
    map: BBTreeMap<HostileKey<'h>, HostileValue<'h>, S>,
    model: BTreeMap<u16, u32>,
    /// Whether every node's prefix is what its fences share: until an
    /// `InlineKey` panics.
    exact_prefixes: bool,
}

impl<'h, S: InlineKeyStrategy<HostileKey<'h>> + Default> HostileMap<'h, S> {
    fn run(host: &'h Host, steps: &Operations<HostileStep>) {
        let mut map = Self {
            host,
            map: BBTreeMap::with_strategy(S::default()),
            model: BTreeMap::new(),
            exact_prefixes: true,
        };
        for step in steps {
            map.step(step);
        }
        let map = map.map;
        host.arm(None);
        let _ = panic::catch_unwind(AssertUnwindSafe(|| drop(map)));
        host.disarm();
        assert_eq!(
            (host.live_keys.get(), host.live_values.get()),
            (0, 0),
            "keys or values outlived the map"
        );
    }

    fn step(&mut self, step: &HostileStep) {
        self.host.arm(step.trap);
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| self.apply(&step.operation)));
        self.host.disarm();
        if let Err(payload) = outcome
            && !payload.is::<HostilePanic>()
        {
            panic::resume_unwind(payload);
        }
        self.check(&step.operation);
    }

    /// Checks the map after an operation, panicking or not.
    fn check(&mut self, operation: &HostileOperation) {
        let len = self.map.len();
        assert_eq!(self.map.iter().count(), len);
        assert_eq!(self.map.iter().rev().count(), len);
        assert_eq!(self.host.live_keys.get(), len, "keys lost or kept alive");
        assert_eq!(
            self.host.live_values.get(),
            len,
            "values lost or kept alive"
        );
        self.map.check_prefix_lens();
        if !self.host.is_honest() {
            return;
        }
        let fired = self.host.fired.get();
        self.exact_prefixes &= fired != Some(Hostility::Trap(Callback::Encoding));
        if self.exact_prefixes {
            self.map.check_invariants();
        }
        let unchanged = match fired {
            None => true,
            Some(Hostility::Trap(callback)) => operation.trap_leaves_map_unchanged(callback),
            Some(Hostility::Predicate | Hostility::Drop) => false,
        };
        if unchanged {
            assert!(
                self.contents()
                    .eq(self.model.iter().map(|(&order, &id)| (order, id))),
                "the map differs from the model"
            );
        } else {
            self.model = self.contents().collect();
        }
    }

    /// Each entry's key order and value id, in the map's order: a walk,
    /// which calls no user code.
    fn contents(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.map.iter().map(|(key, value)| (key.order, value.id))
    }

    fn apply(&mut self, operation: &HostileOperation) {
        let host = self.host;
        match *operation {
            HostileOperation::Insert { order, drop_panic } => {
                let key = host.key(order, drop_panic == DropPanic::Key);
                let value = host.value(drop_panic == DropPanic::Value);
                let id = value.id;
                let replaced = self.map.insert(key, value);
                host.expect(
                    replaced.as_ref().map(|value| value.id),
                    self.model.insert(order, id),
                );
            }
            HostileOperation::InsertRun { first, count } => {
                for order in (0..u16::from(count)).map(|offset| first.wrapping_add(offset)) {
                    let value = host.value(false);
                    let id = value.id;
                    let replaced = self.map.insert(host.key(order, false), value);
                    host.expect(replaced.map(|value| value.id), self.model.insert(order, id));
                }
            }
            HostileOperation::Get { order } => {
                let key = host.key(order, false);
                let found = self.map.get_key_value(&key);
                let found = found.map(|(key, value)| (key.order, value.id));
                let expected = self.model.get_key_value(&order);
                host.expect(found, expected.map(|(&order, &id)| (order, id)));
                host.expect(self.map.contains_key(&key), self.model.contains_key(&order));
            }
            HostileOperation::Remove { order } => {
                let removed = self.map.remove_entry(&host.key(order, false));
                let removed_ids = removed.as_ref().map(|(key, value)| (key.order, value.id));
                host.expect(removed_ids, self.model.remove_entry(&order));
            }
            HostileOperation::Entry { order, choice } => {
                let (key, value) = (host.key(order, false), host.value(false));
                let id = value.id;
                match (self.map.entry(key), choice % 3) {
                    (Entry::Occupied(entry), 0) => {
                        let (key, value) = entry.remove_entry();
                        let expected = self.model.remove_entry(&order);
                        host.expect(Some((key.order, value.id)), expected);
                    }
                    (Entry::Occupied(mut entry), _) => {
                        let replaced = entry.insert(value);
                        host.expect(Some(replaced.id), self.model.insert(order, id));
                    }
                    (Entry::Vacant(entry), 0) => {
                        drop(entry.into_key());
                        host.expect(false, self.model.contains_key(&order));
                    }
                    (Entry::Vacant(entry), _) => {
                        entry.insert(value);
                        host.expect(None, self.model.insert(order, id));
                    }
                }
            }
            HostileOperation::PopFirst => {
                let popped = self.map.pop_first();
                let popped_ids = popped.as_ref().map(|(key, value)| (key.order, value.id));
                host.expect(popped_ids, self.model.pop_first());
            }
            HostileOperation::PopLast => {
                let popped = self.map.pop_last();
                let popped_ids = popped.as_ref().map(|(key, value)| (key.order, value.id));
                host.expect(popped_ids, self.model.pop_last());
            }
            HostileOperation::SplitOff { order, append_back } => {
                let mut upper = self.map.split_off(&host.key(order, false));
                let mut model_upper = self.model.split_off(&order);
                let upper_ids: Vec<_> = upper
                    .iter()
                    .map(|(key, value)| (key.order, value.id))
                    .collect();
                host.expect(
                    upper_ids,
                    model_upper
                        .iter()
                        .map(|(&order, &id)| (order, id))
                        .collect(),
                );
                if append_back {
                    self.map.append(&mut upper);
                    self.model.append(&mut model_upper);
                    host.expect(upper.len(), 0);
                }
            }
            HostileOperation::Append { ref orders } => {
                let mut other = BBTreeMap::with_strategy(S::default());
                let mut model_other = BTreeMap::new();
                for &order in orders {
                    let value = host.value(false);
                    model_other.insert(order, value.id);
                    other.insert(host.key(order, false), value);
                }
                self.map.append(&mut other);
                self.model.append(&mut model_other);
                host.expect(other.len(), 0);
            }
            HostileOperation::Retain { keep, panics_at } => {
                let mut predicate = host.predicate(keep, panics_at);
                self.map.retain(|_, _| predicate());
                let mut model_keep = keep.picker();
                self.model.retain(|_, _| model_keep());
            }
            HostileOperation::ExtractIf {
                range,
                extract,
                take,
                panics_at,
            } => {
                let key_bound = |bound: Bound<u16>| bound.map(|order| host.key(order, false));
                let (start, end) = range.order_bounds(host.conduct);
                let mut predicate = host.predicate(extract, panics_at);
                let key_range = (key_bound(start), key_bound(end));
                let extracted: Vec<_> = (self.map.extract_if(key_range, |_, _| predicate()))
                    .take(usize::from(take))
                    .collect();
                let mut model_extract = extract.picker();
                let model_range = (start, end);
                let expected: Vec<_> = (self.model)
                    .extract_if(model_range, |_, _| model_extract())
                    .take(usize::from(take))
                    .collect();
                let extracted_ids = extracted.iter().map(|(key, value)| (key.order, value.id));
                host.expect(extracted_ids.collect(), expected);
            }
            HostileOperation::Clone => {
                let copy = self.map.clone();
                let orders = copy.keys().map(|key| key.order);
                host.expect(
                    orders.collect::<Vec<_>>(),
                    self.model.keys().copied().collect(),
                );
            }
            HostileOperation::IntoIter { count } => {
                let empty = BBTreeMap::with_strategy(S::default());
                let mut entries = std::mem::replace(&mut self.map, empty).into_iter();
                for _ in 0..count {
                    let entry = entries.next();
                    let entry_ids = entry.as_ref().map(|(key, value)| (key.order, value.id));
                    host.expect(entry_ids, self.model.pop_first());
                }
                drop(entries);
                self.model.clear();
            }
            HostileOperation::Clear => {
                self.map.clear();
                self.model.clear();
            }
        }
    }
}
