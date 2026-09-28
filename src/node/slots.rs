//! [`Slots`]: a fixed-capacity array of slots inside a node page, and the
//! only code that moves keys, values and edges around (D9, D12, D14).
//! [`KeySlots`] moves a node's keys together with their inline keys (D18).

use core::marker::PhantomData;
use core::ptr::NonNull;

use super::inline_keys::{InlinedKey, Window};
use crate::inline_key::InlineWord;

/// The direction in which an array of slots grows from its anchor.
pub(super) trait Direction {
    /// `true`: slot `i` sits at `anchor + i`.
    /// `false`: slot `i` sits at `anchor - (i + 1)`.
    const GROWS_UP: bool;
}

/// Slot `i` sits at `anchor + i`: keys and edges.
pub(super) enum Up {}

/// Slot `i` sits at `anchor - (i + 1)`: values, which grow down from the
/// end of the node (D9).
pub(super) enum Down {}

impl Direction for Up {
    const GROWS_UP: bool = true;
}

impl Direction for Down {
    const GROWS_UP: bool = false;
}

/// Where `capacity` slots of `T` sit in memory, growing in direction `Dir`
/// from `anchor`.
///
/// A `Slots` neither owns nor borrows its slots and tracks no length: which
/// slots are initialized is the caller's business, so every operation is
/// `unsafe` and states which slots it expects to be initialized.
///
/// A run of slots `start..end` is contiguous in memory: in index order when
/// growing up, in reverse index order when growing down. Two arrays of the
/// same direction store a run in the same order, so moving a run from one to
/// the other is a single copy that keeps the order.
///
/// Invariant: the `capacity` slots are aligned for `T` and lie inside one
/// allocation, at `anchor..anchor + capacity` when growing up and at
/// `anchor - capacity..anchor` when growing down.
pub(super) struct Slots<T, Dir> {
    anchor: NonNull<T>,
    capacity: usize,
    _direction: PhantomData<Dir>,
}

impl<T, Dir> Clone for Slots<T, Dir> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, Dir> Copy for Slots<T, Dir> {}

impl<T, Dir: Direction> Slots<T, Dir> {
    /// # Safety
    ///
    /// The type invariant holds for `anchor` and `capacity`, and the slots
    /// stay allocated while this `Slots` or any copy of it is used.
    pub(super) unsafe fn new(anchor: NonNull<T>, capacity: usize) -> Self {
        Self {
            anchor,
            capacity,
            _direction: PhantomData,
        }
    }

    /// A pointer to slot `index`.
    ///
    /// # Safety
    ///
    /// `index < capacity`.
    pub(super) unsafe fn slot(self, index: usize) -> NonNull<T> {
        debug_assert!(index < self.capacity);
        // SAFETY: `index + 1 <= capacity`, forwarded.
        unsafe { self.run_start(index, index + 1) }
    }

    /// Slots `start..end` as one slice in memory order: index order when
    /// growing up, reverse index order when growing down. With `start == 0`,
    /// this is the initialized prefix of an array of length `end`, ready to
    /// be borrowed or dropped in place.
    ///
    /// # Safety
    ///
    /// `start <= end <= capacity`.
    pub(super) unsafe fn run(self, start: usize, end: usize) -> NonNull<[T]> {
        // SAFETY: forwarded.
        let first = unsafe { self.run_start(start, end) };
        NonNull::slice_from_raw_parts(first, end - start)
    }

    /// Writes `value` into slot `index`, first shifting slots `index..len` up
    /// by one index into `index + 1..len + 1`.
    ///
    /// # Safety
    ///
    /// - `index <= len < capacity`.
    /// - Slots `index..len` are initialized. Slot `len` holds nothing that
    ///   needs dropping: it is overwritten.
    ///
    /// Afterwards slots `index..len + 1` are initialized.
    pub(super) unsafe fn insert(self, index: usize, len: usize, value: T) {
        debug_assert!(index <= len && len < self.capacity);
        // SAFETY: `index <= len < len + 1 <= capacity`, so both runs and the
        // slot are in bounds. The tail `index..len` is initialized and moves
        // into `index + 1..len + 1` (overlapping, hence `copy_to`), which
        // frees slot `index` for `value`.
        unsafe {
            let tail = self.run_start(index, len);
            let shifted_tail = self.run_start(index + 1, len + 1);
            tail.copy_to(shifted_tail, len - index);
            self.slot(index).write(value);
        }
    }

    /// Takes slot `index` out, then shifts slots `index + 1..len` down by one
    /// index into `index..len - 1`.
    ///
    /// # Safety
    ///
    /// - `index < len <= capacity`.
    /// - Slots `index..len` are initialized.
    ///
    /// Afterwards slots `index..len - 1` are initialized and slot `len - 1`
    /// holds nothing (its bytes are a stale copy).
    pub(super) unsafe fn remove(self, index: usize, len: usize) -> T {
        debug_assert!(index < len && len <= self.capacity);
        // SAFETY: `index < len <= capacity`, so the slot and both runs are in
        // bounds. Slot `index` is initialized and is read out; the
        // initialized tail `index + 1..len` then moves into `index..len - 1`
        // (overlapping, hence `copy_to`).
        unsafe {
            let removed = self.slot(index).read();
            let tail = self.run_start(index + 1, len);
            let shifted_tail = self.run_start(index, len - 1);
            tail.copy_to(shifted_tail, len - index - 1);
            removed
        }
    }

    /// Moves slots `start..start + count` of `self` into slots
    /// `dest_start..dest_start + count` of `dest`, keeping their order.
    ///
    /// # Safety
    ///
    /// - `start + count <= self.capacity` and `dest_start + count <= dest.capacity`.
    /// - The source slots are initialized; afterwards they hold nothing.
    /// - The destination slots hold nothing that needs dropping: they are
    ///   overwritten, and are initialized afterwards.
    /// - The two runs do not overlap, e.g. because they are in different pages.
    pub(super) unsafe fn move_run_to(
        self,
        start: usize,
        count: usize,
        dest: Self,
        dest_start: usize,
    ) {
        // SAFETY: both runs are in bounds, initialized at the source and
        // disjoint, as the caller guarantees. Both arrays grow the same way,
        // so one copy keeps the order.
        unsafe {
            let source = self.run_start(start, start + count);
            let destination = dest.run_start(dest_start, dest_start + count);
            source.copy_to_nonoverlapping(destination, count);
        }
    }

    /// The lowest address of the run `start..end`.
    ///
    /// # Safety
    ///
    /// `start <= end <= capacity`.
    unsafe fn run_start(self, start: usize, end: usize) -> NonNull<T> {
        debug_assert!(start <= end && end <= self.capacity);
        // SAFETY: by the type invariant, the result lies within the slots or
        // one past the last one when growing up (`start <= capacity`), and
        // within the slots when growing down (`end <= capacity`).
        unsafe {
            if Dir::GROWS_UP {
                self.anchor.add(start)
            } else {
                self.anchor.sub(end)
            }
        }
    }
}

/// A node's key slots and, in a node with inline keys, its inline-key slots
/// beside them (D18), for a map with strategy `S`. Every operation moves a
/// key and its inline key together, so that inline key `i` is always key
/// `i`'s; each mirrors the [`Slots`] operation of its name, with the same
/// contract for both arrays.
///
/// The node stores its keys' windows at its prefix length (05-fence-key.md):
/// a key read out comes with its window there, and a key written in must
/// come with its window there, which debug builds check. A key read out of
/// a node without inline keys comes with [`Window::NONE`].
///
/// Invariant: `inline_keys`, if any, has the capacity of `keys`.
pub(super) struct KeySlots<K, S> {
    keys: Slots<K, Up>,
    inline_keys: Option<Slots<InlineWord, Up>>,
    /// The node's prefix length; 0 without inline keys.
    prefix_len: usize,
    _strategy: PhantomData<fn() -> S>,
}

impl<K, S> Clone for KeySlots<K, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, S> Copy for KeySlots<K, S> {}

impl<K, S> KeySlots<K, S> {
    /// # Safety
    ///
    /// `inline_keys`, if any, has the capacity of `keys`.
    pub(super) unsafe fn new(
        keys: Slots<K, Up>,
        inline_keys: Option<Slots<InlineWord, Up>>,
        prefix_len: usize,
    ) -> Self {
        debug_assert!(inline_keys.is_none_or(|inline_keys| inline_keys.capacity == keys.capacity));
        Self {
            keys,
            inline_keys,
            prefix_len,
            _strategy: PhantomData,
        }
    }

    /// The key slots alone, to read keys in place.
    pub(super) fn keys(self) -> Slots<K, Up> {
        self.keys
    }

    /// The inline-key slots alone, to read inline keys in place, if the node
    /// has them.
    pub(super) fn inline_keys(self) -> Option<Slots<InlineWord, Up>> {
        self.inline_keys
    }

    /// The window that inline key `word` of this node is.
    fn window(self, word: InlineWord) -> Window {
        match self.inline_keys {
            Some(_) => Window::new(self.prefix_len, word),
            None => Window::NONE,
        }
    }

    /// The inline key to store for `key`, whose window must be at this
    /// node's prefix length.
    fn inline_key_of(self, key: &InlinedKey<K, S>) -> InlineWord {
        let window = key.window();
        debug_assert!(
            self.inline_keys.is_none() || window.prefix_len() == self.prefix_len,
            "a key at prefix length {} goes into a node at {}",
            window.prefix_len(),
            self.prefix_len
        );
        window.inline_key()
    }

    /// Reads key `index` and its inline key out; the slot holds nothing
    /// afterwards (its bytes are a stale copy).
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    pub(super) unsafe fn read(self, index: usize) -> InlinedKey<K, S> {
        // SAFETY: forwarded; the inline-key slots have the keys' capacity
        // (type invariant).
        unsafe {
            let key = self.keys.slot(index).read();
            let inline_key = match self.inline_keys {
                Some(inline_keys) => inline_keys.slot(index).read(),
                None => 0,
            };
            InlinedKey::from_parts(key, self.window(inline_key))
        }
    }

    /// Writes `key` and its inline key into slot `index`.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` holds nothing that needs
    /// dropping: it is overwritten, and is initialized afterwards.
    pub(super) unsafe fn write(self, index: usize, key: InlinedKey<K, S>) {
        let inline_key = self.inline_key_of(&key);
        let (key, _) = key.into_parts();
        // SAFETY: forwarded; the inline-key slots have the keys' capacity
        // (type invariant).
        unsafe {
            self.keys.slot(index).write(key);
            if let Some(inline_keys) = self.inline_keys {
                inline_keys.slot(index).write(inline_key);
            }
        }
    }

    /// Replaces key `index` and its inline key, and returns the old ones.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    pub(super) unsafe fn replace(self, index: usize, key: InlinedKey<K, S>) -> InlinedKey<K, S> {
        // SAFETY: forwarded: slot `index` is read out, then written again.
        unsafe {
            let old_key = self.read(index);
            self.write(index, key);
            old_key
        }
    }

    /// See [`Slots::insert`].
    ///
    /// # Safety
    ///
    /// As for [`Slots::insert`].
    pub(super) unsafe fn insert(self, index: usize, len: usize, key: InlinedKey<K, S>) {
        let inline_key = self.inline_key_of(&key);
        let (key, _) = key.into_parts();
        // SAFETY: forwarded; the inline-key slots have the keys' capacity and
        // are initialized like them (type invariant).
        unsafe {
            self.keys.insert(index, len, key);
            if let Some(inline_keys) = self.inline_keys {
                inline_keys.insert(index, len, inline_key);
            }
        }
    }

    /// See [`Slots::remove`].
    ///
    /// # Safety
    ///
    /// As for [`Slots::remove`].
    pub(super) unsafe fn remove(self, index: usize, len: usize) -> InlinedKey<K, S> {
        // SAFETY: forwarded; the inline-key slots have the keys' capacity and
        // are initialized like them (type invariant).
        unsafe {
            let key = self.keys.remove(index, len);
            let inline_key = match self.inline_keys {
                Some(inline_keys) => inline_keys.remove(index, len),
                None => 0,
            };
            InlinedKey::from_parts(key, self.window(inline_key))
        }
    }

    /// See [`Slots::move_run_to`].
    ///
    /// # Safety
    ///
    /// As for [`Slots::move_run_to`], and `dest` is a node of the same map,
    /// which has inline keys exactly if this one has. The inline keys move
    /// as they are, so `dest` must have this node's prefix length.
    pub(super) unsafe fn move_run_to(
        self,
        start: usize,
        count: usize,
        dest: Self,
        dest_start: usize,
    ) {
        debug_assert_eq!(self.inline_keys.is_some(), dest.inline_keys.is_some());
        debug_assert_eq!(
            self.prefix_len, dest.prefix_len,
            "a run moves between nodes of different prefix lengths"
        );
        // SAFETY: forwarded; the inline-key slots of both have their keys'
        // capacity and are initialized like them (type invariant).
        unsafe {
            self.keys.move_run_to(start, count, dest.keys, dest_start);
            if let (Some(source), Some(destination)) = (self.inline_keys, dest.inline_keys) {
                source.move_run_to(start, count, destination, dest_start);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::mem::MaybeUninit;
    use core::ptr::NonNull;
    use std::vec::Vec;

    use super::{Direction, Down, Slots, Up};
    use crate::node::test_support::{DropLedger, Tracked};

    /// Small enough to try every index combination, also under Miri.
    const CAPACITY: usize = 5;

    /// A `Slots` over a buffer it owns, next to a model of which tracked
    /// value each slot holds (`None`: nothing).
    struct ModelledSlots<'l, Dir> {
        _buffer: Vec<MaybeUninit<Tracked<'l>>>,
        slots: Slots<Tracked<'l>, Dir>,
        model: Vec<Option<usize>>,
    }

    impl<'l, Dir: Direction> ModelledSlots<'l, Dir> {
        /// Slots `0..len` hold fresh tracked values.
        fn with_len(ledger: &'l DropLedger, len: usize) -> Self {
            let mut buffer: Vec<MaybeUninit<Tracked<'l>>> = Vec::with_capacity(CAPACITY);
            buffer.resize_with(CAPACITY, MaybeUninit::uninit);
            let base = NonNull::new(buffer.as_mut_ptr().cast::<Tracked<'l>>()).unwrap();
            let anchor = if Dir::GROWS_UP {
                base
            } else {
                // SAFETY: one past the end of the buffer.
                unsafe { base.add(CAPACITY) }
            };
            // SAFETY: `CAPACITY` aligned slots of the buffer, which outlives
            // `slots` and is not otherwise accessed.
            let slots = unsafe { Slots::new(anchor, CAPACITY) };
            let mut this = Self {
                _buffer: buffer,
                slots,
                model: std::vec![None; CAPACITY],
            };
            for index in 0..len {
                let value = ledger.track();
                this.model[index] = Some(value.id());
                // SAFETY: `index < CAPACITY` and the slot is empty.
                unsafe { this.slots.slot(index).write(value) };
            }
            this
        }

        /// Checks every initialized slot against the model, through `slot`
        /// and through `run` over the initialized prefix.
        fn assert_matches_model(&self) {
            for (index, expected) in self.model.iter().enumerate() {
                if let Some(id) = expected {
                    // SAFETY: the model says the slot is initialized.
                    assert_eq!(unsafe { self.slots.slot(index).as_ref() }.id(), *id);
                }
            }
            let len = self.model.iter().take_while(|id| id.is_some()).count();
            // SAFETY: slots `0..len` are initialized.
            let prefix = unsafe { self.slots.run(0, len).as_ref() };
            let mut in_memory: Vec<usize> = prefix.iter().map(Tracked::id).collect();
            if !Dir::GROWS_UP {
                in_memory.reverse();
            }
            let expected: Vec<usize> = self.model[..len].iter().flatten().copied().collect();
            assert_eq!(in_memory, expected);
        }

        /// Drops every initialized slot.
        fn drop_all(mut self) {
            for (index, id) in self.model.iter_mut().enumerate() {
                if id.take().is_some() {
                    // SAFETY: the model says the slot is initialized; it is
                    // dropped once and then marked empty.
                    unsafe { self.slots.slot(index).drop_in_place() };
                }
            }
        }
    }

    fn insert_at_every_index<Dir: Direction>() {
        let ledger = DropLedger::new();
        for len in 0..CAPACITY {
            for index in 0..=len {
                let mut slots = ModelledSlots::<Dir>::with_len(&ledger, len);
                let value = ledger.track();
                let id = value.id();
                // SAFETY: `index <= len < CAPACITY`; slots `0..len` are
                // initialized and slot `len` is empty.
                unsafe { slots.slots.insert(index, len, value) };
                slots.model.insert(index, Some(id));
                slots.model.truncate(CAPACITY);
                slots.assert_matches_model();
                slots.drop_all();
            }
        }
        ledger.assert_all_dropped_once();
    }

    fn remove_at_every_index<Dir: Direction>() {
        let ledger = DropLedger::new();
        for len in 1..=CAPACITY {
            for index in 0..len {
                let mut slots = ModelledSlots::<Dir>::with_len(&ledger, len);
                // SAFETY: `index < len <= CAPACITY`; slots `0..len` are initialized.
                let removed = unsafe { slots.slots.remove(index, len) };
                assert_eq!(Some(removed.id()), slots.model.remove(index));
                slots.model.push(None);
                slots.assert_matches_model();
                slots.drop_all();
            }
        }
        ledger.assert_all_dropped_once();
    }

    /// Moves every run of a full array into every place of an empty one.
    fn move_every_run<Dir: Direction>() {
        let ledger = DropLedger::new();
        for start in 0..=CAPACITY {
            for count in 0..=CAPACITY - start {
                for dest_start in 0..=CAPACITY - count {
                    let mut source = ModelledSlots::<Dir>::with_len(&ledger, CAPACITY);
                    let mut dest = ModelledSlots::<Dir>::with_len(&ledger, 0);
                    // SAFETY: both runs are in bounds, the source run is
                    // initialized, the destination is empty, and the two
                    // buffers are distinct.
                    unsafe {
                        source
                            .slots
                            .move_run_to(start, count, dest.slots, dest_start)
                    };
                    for offset in 0..count {
                        dest.model[dest_start + offset] = source.model[start + offset].take();
                    }
                    source.assert_matches_model();
                    dest.assert_matches_model();
                    source.drop_all();
                    dest.drop_all();
                }
            }
        }
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn insert_grows_up() {
        insert_at_every_index::<Up>();
    }

    #[test]
    fn insert_grows_down() {
        insert_at_every_index::<Down>();
    }

    #[test]
    fn remove_grows_up() {
        remove_at_every_index::<Up>();
    }

    #[test]
    fn remove_grows_down() {
        remove_at_every_index::<Down>();
    }

    #[test]
    fn move_run_grows_up() {
        move_every_run::<Up>();
    }

    #[test]
    fn move_run_grows_down() {
        move_every_run::<Down>();
    }

    #[test]
    fn down_slots_end_at_the_anchor() {
        let ledger = DropLedger::new();
        let slots = ModelledSlots::<Down>::with_len(&ledger, 2);
        // SAFETY: indices are within capacity; no pointer is dereferenced.
        let (slot_0, slot_1, anchor) = unsafe {
            (
                slots.slots.slot(0).addr().get(),
                slots.slots.slot(1).addr().get(),
                slots.slots.run(0, 0).cast::<Tracked<'_>>().addr().get(),
            )
        };
        let size = size_of::<Tracked<'_>>();
        assert_eq!(slot_0 + size, anchor);
        assert_eq!(slot_1 + size, slot_0);
        slots.drop_all();
        ledger.assert_all_dropped_once();
    }

    fn zero_sized_slots<Dir: Direction>() {
        // SAFETY: zero-sized slots occupy no memory, so any aligned anchor works.
        let slots = unsafe { Slots::<(), Dir>::new(NonNull::dangling(), CAPACITY) };
        for len in 0..CAPACITY {
            // SAFETY: `len < CAPACITY`; `()` is always initialized.
            unsafe {
                slots.insert(0, len, ());
                assert_eq!(slots.run(0, len + 1).len(), len + 1);
                slots.remove(len, len + 1);
            }
        }
    }

    #[test]
    fn zero_sized_slots_grow_up() {
        zero_sized_slots::<Up>();
    }

    #[test]
    fn zero_sized_slots_grow_down() {
        zero_sized_slots::<Down>();
    }
}
