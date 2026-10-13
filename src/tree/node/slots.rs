//! [`Slots`]: a fixed-capacity array of slots inside a node page, and the
//! only code that moves keys, values and edges around (D9, D12, D14, D29).
//! [`InlineKeySlots`] are a node's inline keys at its kind's width,
//! [`KeyColumns`] every array a node keeps beside its keys, [`KeySlots`]
//! moves a node's keys together with them (D18), and [`NodeKeys`] lends a
//! node's keys in place.

use core::marker::PhantomData;
use core::ops::{Index, Range};
use core::ptr::NonNull;

use super::inline_keys::{InlineSlot, InlinedKey, Window};
use super::node_plan::NodePlan;
use super::page::PageKind;
use crate::inline_key::{InlineKeyStrategy, InlineWidth, InlineWord};

/// Where `capacity` slots of `T` sit in memory: slot `i` at `stride * i`
/// bytes past `anchor`. The slots of an array of their own sit side by
/// side, `size_of::<T>()` apart; the keys and the trailing slots of a node
/// of `(key, value)` or `(key, edge)` entries are fields of its entries,
/// an entry's size apart (D29).
///
/// A `Slots` neither owns nor borrows its slots and tracks no length: which
/// slots are initialized is the caller's business, so every operation is
/// `unsafe` and states which slots it expects to be initialized.
///
/// Slots side by side move a run with one copy; strided ones move slot by
/// slot, the bytes between them belonging to other fields. Moving whole
/// strides, the other fields with them, is [`Self::shift_strides_up`] and
/// [`Self::shift_strides_down`]: how a node shifts its entries in one copy.
///
/// Invariant: `stride` is at least `size_of::<T>()` and a multiple of
/// `align_of::<T>()`, and the `capacity` slots are aligned for `T` and lie
/// inside one allocation.
pub(super) struct Slots<T> {
    anchor: NonNull<T>,
    capacity: usize,
    /// Bytes from one slot to the next.
    stride: usize,
}

impl<T> Clone for Slots<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Slots<T> {}

impl<T> Slots<T> {
    /// `capacity` slots side by side from `anchor`.
    ///
    /// # Safety
    ///
    /// The type invariant holds for `anchor` and `capacity`, with a stride
    /// of `size_of::<T>()`, and the slots stay allocated while this `Slots`
    /// or any copy of it is used.
    pub(super) unsafe fn new(anchor: NonNull<T>, capacity: usize) -> Self {
        // SAFETY: forwarded.
        unsafe { Self::with_stride(anchor, capacity, size_of::<T>()) }
    }

    /// `capacity` slots from `anchor`, `stride` bytes apart.
    ///
    /// # Safety
    ///
    /// The type invariant holds for `anchor`, `capacity` and `stride`, and
    /// the slots stay allocated while this `Slots` or any copy of it is used.
    pub(super) unsafe fn with_stride(anchor: NonNull<T>, capacity: usize, stride: usize) -> Self {
        debug_assert!(stride >= size_of::<T>() && stride.is_multiple_of(align_of::<T>()));
        Self {
            anchor,
            capacity,
            stride,
        }
    }

    /// Whether the slots sit side by side, with nothing between them.
    fn is_contiguous(self) -> bool {
        self.stride == size_of::<T>()
    }

    /// A pointer to slot `index`.
    ///
    /// # Safety
    ///
    /// `index < capacity`.
    pub(super) unsafe fn slot(self, index: usize) -> NonNull<T> {
        debug_assert!(index < self.capacity);
        // SAFETY: slot `index` is one of the slots, inside the allocation
        // (type invariant).
        unsafe { self.anchor.byte_add(index * self.stride) }
    }

    /// Where slot `index` of slots side by side is, the end of the last one
    /// included.
    ///
    /// # Safety
    ///
    /// `index <= capacity`, and the slots are side by side.
    unsafe fn contiguous_slot(self, index: usize) -> NonNull<T> {
        debug_assert!(index <= self.capacity && self.is_contiguous());
        // SAFETY: slots side by side lie within the allocation, end to end
        // (type invariant), so the result is within it or one past its last
        // slot.
        unsafe { self.anchor.add(index) }
    }

    /// Slots `start..end` of slots side by side, as one slice in index
    /// order. With `start == 0`, this is the initialized prefix of an array
    /// of length `end`, ready to be borrowed or dropped in place.
    ///
    /// # Safety
    ///
    /// `start <= end <= capacity`, and the slots are side by side.
    pub(super) unsafe fn run(self, start: usize, end: usize) -> NonNull<[T]> {
        debug_assert!(start <= end);
        // SAFETY: forwarded.
        let first = unsafe { self.contiguous_slot(start) };
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
        // SAFETY: `index <= len < len + 1 <= capacity`, so every slot named
        // is in bounds. The tail `index..len` is initialized and moves into
        // `index + 1..len + 1`, which frees slot `index` for `value`: side by
        // side in one copy (overlapping, hence `copy_to`), strided from the
        // top, so that each slot is read before it is written over.
        unsafe {
            if self.is_contiguous() {
                if index < len {
                    let tail = self.contiguous_slot(index);
                    tail.copy_to(self.contiguous_slot(index + 1), len - index);
                }
            } else {
                for from in (index..len).rev() {
                    self.move_slot(from, from + 1);
                }
            }
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
        // SAFETY: `index < len <= capacity`, so every slot named is in
        // bounds. Slot `index` is initialized and is read out; the
        // initialized tail `index + 1..len` then moves into `index..len - 1`:
        // side by side in one copy (overlapping, hence `copy_to`), strided
        // from the bottom, so that each slot is read before it is written
        // over.
        unsafe {
            let removed = self.slot(index).read();
            if self.is_contiguous() {
                let tail = self.contiguous_slot(index + 1);
                tail.copy_to(self.contiguous_slot(index), len - index - 1);
            } else {
                for from in index + 1..len {
                    self.move_slot(from, from - 1);
                }
            }
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
        debug_assert!(start + count <= self.capacity && dest_start + count <= dest.capacity);
        // SAFETY: both runs are in bounds, initialized at the source and
        // disjoint, as the caller guarantees: two arrays side by side copy
        // the run in one go, keeping its order; otherwise slot by slot.
        unsafe {
            if self.is_contiguous() && dest.is_contiguous() {
                let source = self.contiguous_slot(start);
                source.copy_to_nonoverlapping(dest.contiguous_slot(dest_start), count);
            } else {
                for offset in 0..count {
                    let source = self.slot(start + offset);
                    source.copy_to_nonoverlapping(dest.slot(dest_start + offset), 1);
                }
            }
        }
    }

    /// Shifts the whole strides of slots `index..len` up by one slot, into
    /// `index + 1..len + 1`, with the other fields between them: the entries
    /// of a node whose slots these are the first fields of. Slot `index`'s
    /// stride holds stale bytes afterwards.
    ///
    /// # Safety
    ///
    /// - `index <= len < capacity`.
    /// - The strides of slots `index..len + 1` lie inside the allocation,
    ///   and what those of `index..len` hold may move as bytes; what that of
    ///   `len` holds needs no dropping: it is overwritten.
    pub(super) unsafe fn shift_strides_up(self, index: usize, len: usize) {
        debug_assert!(index <= len && len < self.capacity);
        if index == len {
            return;
        }
        // SAFETY: the strides of slots `index..len + 1` are in the allocation
        // (caller); `copy` allows the overlap.
        unsafe {
            let first = self.slot(index).cast::<u8>();
            first.copy_to(first.add(self.stride), (len - index) * self.stride);
        }
    }

    /// Shifts the whole strides of slots `index + 1..len` down by one slot,
    /// into `index..len - 1`, with the other fields between them, over the
    /// stride of slot `index`; that of `len - 1` holds stale bytes
    /// afterwards.
    ///
    /// # Safety
    ///
    /// - `index < len <= capacity`.
    /// - The strides of slots `index..len` lie inside the allocation, what
    ///   those of `index + 1..len` hold may move as bytes, and what that of
    ///   `index` holds needs no dropping: it is overwritten.
    pub(super) unsafe fn shift_strides_down(self, index: usize, len: usize) {
        debug_assert!(index < len && len <= self.capacity);
        // SAFETY: the strides of slots `index..len` are in the allocation
        // (caller); `copy` allows the overlap.
        unsafe {
            let first = self.slot(index).cast::<u8>();
            first
                .add(self.stride)
                .copy_to(first, (len - index - 1) * self.stride);
        }
    }

    /// Sets every byte of slots side by side to `byte`.
    ///
    /// # Safety
    ///
    /// The slots are side by side, hold nothing that needs dropping, and a
    /// `T` whose bytes are all `byte` is valid.
    pub(super) unsafe fn fill_bytes(self, byte: u8) {
        debug_assert!(self.is_contiguous());
        // SAFETY: slots side by side lie within the allocation, end to end
        // (type invariant); the caller vouches for what they hold after.
        unsafe { self.anchor.write_bytes(byte, self.capacity) }
    }

    /// Moves slot `from` into slot `to`, which holds nothing that needs
    /// dropping; `from` holds nothing afterwards.
    ///
    /// # Safety
    ///
    /// Both are below `capacity` and distinct, and `from` is initialized.
    unsafe fn move_slot(self, from: usize, to: usize) {
        // SAFETY: forwarded; distinct slots don't overlap, being at least
        // `size_of::<T>()` apart.
        unsafe { self.slot(from).copy_to_nonoverlapping(self.slot(to), 1) }
    }
}

/// A node's keys `0..len`, in order, lent for `'a`: side by side in a node
/// whose keys are an array of their own, or each the key of an entry in a
/// node of `(key, value)` or `(key, edge)` entries (D29).
pub(crate) struct NodeKeys<'a, K> {
    first: NonNull<K>,
    /// Bytes from one key to the next.
    stride: usize,
    len: usize,
    _keys: PhantomData<&'a [K]>,
}

impl<K> Clone for NodeKeys<'_, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for NodeKeys<'_, K> {}

impl<'a, K> NodeKeys<'a, K> {
    /// Slots `0..len` of `keys`.
    ///
    /// # Safety
    ///
    /// `len <= capacity`, slots `0..len` are initialized and stay unchanged
    /// for `'a`, and the end of the last slot's stride lies within the
    /// allocation or one past its end, as a node's keys' do (`layout.rs`).
    pub(super) unsafe fn new(keys: Slots<K>, len: usize) -> Self {
        debug_assert!(len <= keys.capacity);
        Self {
            first: keys.anchor,
            stride: keys.stride,
            len,
            _keys: PhantomData,
        }
    }

    pub(crate) fn len(self) -> usize {
        self.len
    }

    /// Key `index`.
    ///
    /// # Safety
    ///
    /// `index < len`.
    pub(crate) unsafe fn get_unchecked(self, index: usize) -> &'a K {
        debug_assert!(index < self.len);
        // SAFETY: key `index` is initialized and unchanged for `'a` (`new`).
        unsafe { self.first.byte_add(index * self.stride).as_ref() }
    }

    /// Key `index`, or `None` if `index >= len`.
    pub(crate) fn get(self, index: usize) -> Option<&'a K> {
        // SAFETY: `index < len`.
        (index < self.len).then(|| unsafe { self.get_unchecked(index) })
    }

    pub(crate) fn first(self) -> Option<&'a K> {
        self.get(0)
    }

    /// Keys `range` of these.
    ///
    /// # Panics
    ///
    /// If `range` is not within `0..len`.
    pub(crate) fn slice(self, range: Range<usize>) -> Self {
        assert!(
            range.start <= range.end && range.end <= self.len,
            "keys out of bounds"
        );
        Self {
            // SAFETY: `range.start <= len`: a key's slot, or the end of the
            // last key's stride, within the allocation or one past it
            // (`new`).
            first: unsafe { self.first.byte_add(range.start * self.stride) },
            len: range.end - range.start,
            ..self
        }
    }

    pub(crate) fn iter(self) -> impl DoubleEndedIterator<Item = &'a K> + ExactSizeIterator {
        // SAFETY: `index < len`.
        (0..self.len).map(move |index| unsafe { self.get_unchecked(index) })
    }
}

impl<K> Index<usize> for NodeKeys<'_, K> {
    type Output = K;

    fn index(&self, index: usize) -> &K {
        self.get(index).expect("key index out of bounds")
    }
}

/// A node's inline-key slots (D18): `capacity` of them, side by side, each
/// the first bytes of a window, as many as the node kind's [`InlineWidth`], stored
/// as the [`InlineSlot`] of that width. The operations mirror those of
/// [`Slots`], with the same contracts, and speak in windows: a write keeps a
/// window's first bytes, and a read gives them back, zeros after.
///
/// Every node of the same kind and map has the same width, a constant of its
/// kind, key type and strategy, so each typed operation uses one width.
#[derive(Clone, Copy)]
pub(super) enum InlineKeySlots {
    Two(Slots<u16>),
    Four(Slots<u32>),
    Eight(Slots<u64>),
}

/// `$body`, with `$slots` bound to the slots of `$inline_keys`, whichever
/// their width.
macro_rules! with_slots_of_width {
    ($inline_keys:expr, |$slots:ident| $body:expr) => {
        match $inline_keys {
            InlineKeySlots::Two($slots) => $body,
            InlineKeySlots::Four($slots) => $body,
            InlineKeySlots::Eight($slots) => $body,
        }
    };
}

impl InlineKeySlots {
    /// `capacity` slots of `width` side by side from `anchor`.
    ///
    /// # Safety
    ///
    /// As for [`Slots::new`], for slots of `width` bytes.
    pub(super) unsafe fn new(width: InlineWidth, anchor: NonNull<u8>, capacity: usize) -> Self {
        // SAFETY: forwarded, for the slot type of `width`.
        unsafe {
            match width {
                InlineWidth::Two => Self::Two(Slots::new(anchor.cast(), capacity)),
                InlineWidth::Four => Self::Four(Slots::new(anchor.cast(), capacity)),
                InlineWidth::Eight => Self::Eight(Slots::new(anchor.cast(), capacity)),
            }
        }
    }

    pub(super) fn width(self) -> InlineWidth {
        match self {
            Self::Two(_) => InlineWidth::Two,
            Self::Four(_) => InlineWidth::Four,
            Self::Eight(_) => InlineWidth::Eight,
        }
    }

    pub(super) fn capacity(self) -> usize {
        with_slots_of_width!(self, |slots| slots.capacity)
    }

    /// Slot `index`'s inline key: the bytes it keeps of a window, zeros
    /// after them.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    pub(super) unsafe fn read(self, index: usize) -> InlineWord {
        // SAFETY: forwarded.
        unsafe { with_slots_of_width!(self, |slots| slots.slot(index).read().window()) }
    }

    /// Fills every slot with the greatest inline key, all ones.
    ///
    /// # Safety
    ///
    /// As for [`Slots::fill_bytes`]: inline keys are plain data, and any
    /// bytes are a valid one.
    pub(super) unsafe fn fill_with_greatest(self) {
        // SAFETY: forwarded.
        unsafe { with_slots_of_width!(self, |slots| slots.fill_bytes(u8::MAX)) }
    }

    /// Writes the first bytes of `window` into slot `index`, which is
    /// initialized afterwards. Inline keys need no dropping.
    ///
    /// # Safety
    ///
    /// `index < capacity`.
    pub(super) unsafe fn write(self, index: usize, window: InlineWord) {
        // SAFETY: forwarded.
        unsafe {
            with_slots_of_width!(self, |slots| {
                slots.slot(index).write(InlineSlot::of_window(window));
            });
        }
    }

    /// See [`Slots::insert`].
    ///
    /// # Safety
    ///
    /// As for [`Slots::insert`].
    pub(super) unsafe fn insert(self, index: usize, len: usize, window: InlineWord) {
        // SAFETY: forwarded.
        unsafe {
            with_slots_of_width!(self, |slots| {
                slots.insert(index, len, InlineSlot::of_window(window));
            });
        }
    }

    /// See [`Slots::remove`]; returns the inline key taken out, as
    /// [`Self::read`] does.
    ///
    /// # Safety
    ///
    /// As for [`Slots::remove`].
    pub(super) unsafe fn remove(self, index: usize, len: usize) -> InlineWord {
        // SAFETY: forwarded.
        unsafe { with_slots_of_width!(self, |slots| slots.remove(index, len).window()) }
    }

    /// See [`Slots::move_run_to`]. `dest` has this array's width: runs move
    /// between nodes of the same kind. `KeySlots` checks that in debug
    /// builds, before anything moves.
    ///
    /// # Safety
    ///
    /// As for [`Slots::move_run_to`].
    pub(super) unsafe fn move_run_to(
        self,
        start: usize,
        count: usize,
        dest: Self,
        dest_start: usize,
    ) {
        // SAFETY: forwarded.
        unsafe {
            match (self, dest) {
                (Self::Two(source), Self::Two(destination)) => {
                    source.move_run_to(start, count, destination, dest_start);
                }
                (Self::Four(source), Self::Four(destination)) => {
                    source.move_run_to(start, count, destination, dest_start);
                }
                (Self::Eight(source), Self::Eight(destination)) => {
                    source.move_run_to(start, count, destination, dest_start);
                }
                // Unreachable: whatever it left in `dest`, inline keys
                // steer searches only (`handle.rs`).
                _ => {}
            }
        }
    }
}

/// What a node keeps beside its keys, one slot per key, in arrays of their
/// own that every move of the keys moves alike: its inline keys (D18) and,
/// in a leaf whose plan keeps them, its keys' length tags (D36). Another
/// such array is a field here and a cell of [`KeyCells`]; the code that
/// moves keys ([`KeySlots`]) doesn't change.
///
/// Each operation mirrors the [`Slots`] operation of its name, with the same
/// contract for every array.
///
/// Invariant: each array present has the keys' capacity.
#[derive(Clone, Copy)]
pub(super) struct KeyColumns {
    inline_keys: Option<InlineKeySlots>,
    length_tags: Option<Slots<u16>>,
}

/// What the [`KeyColumns`] of a node hold for one key.
#[derive(Clone, Copy)]
pub(super) struct KeyCells {
    /// The first bytes of the key's window, which the inline keys keep.
    inline_key: InlineWord,
    /// The key's length tag, if the node keeps them.
    length_tag: Option<u16>,
}

/// Every method is `#[inline(always)]`: which arrays a node keeps, and at
/// what width, is a constant of its plan (`node_plan.rs`) only where the
/// method is inlined into code that knows the node's kind and map; called,
/// it would be a value tested at run time.
impl KeyColumns {
    /// # Safety
    ///
    /// Each array present has `capacity` slots, as [`Slots::new`] requires
    /// of them.
    #[inline(always)]
    pub(super) unsafe fn new(
        inline_keys: Option<InlineKeySlots>,
        length_tags: Option<Slots<u16>>,
        capacity: usize,
    ) -> Self {
        debug_assert!(inline_keys.is_none_or(|inline_keys| inline_keys.capacity() == capacity));
        debug_assert!(length_tags.is_none_or(|tags| tags.capacity == capacity));
        Self {
            inline_keys,
            length_tags,
        }
    }

    /// The inline-key slots, if the node has them.
    #[inline(always)]
    pub(super) fn inline_keys(self) -> Option<InlineKeySlots> {
        self.inline_keys
    }

    /// The length-tag slots, if the node has them.
    #[inline(always)]
    pub(super) fn length_tags(self) -> Option<Slots<u16>> {
        self.length_tags
    }

    /// Fills every slot of a node that holds no keys yet: each inline key
    /// with the greatest, all ones, as slots past the length hold (invariant
    /// 6 in `handle.rs`), and each length tag with 0.
    ///
    /// # Safety
    ///
    /// The node holds no keys, and its handle has exclusive access.
    #[inline(always)]
    pub(super) unsafe fn initialize(self) {
        // SAFETY: the slots are in the page (type invariant) and hold plain
        // data; the caller has exclusive access.
        unsafe {
            if let Some(inline_keys) = self.inline_keys {
                inline_keys.fill_with_greatest();
            }
            if let Some(tags) = self.length_tags {
                tags.fill_bytes(0);
            }
        }
    }

    /// Gives slots `vacated`, which a node no longer holds keys in, the
    /// greatest inline key again (invariant 6). Their length tags stay
    /// stale: nothing reads a tag past the length.
    ///
    /// # Safety
    ///
    /// `vacated` is within the capacity, and the node's handle has exclusive
    /// access.
    #[inline(always)]
    pub(super) unsafe fn mark_vacant(self, vacated: Range<usize>) {
        if let Some(inline_keys) = self.inline_keys {
            for index in vacated {
                // SAFETY: `index < capacity` (caller); inline keys are plain
                // data.
                unsafe { inline_keys.write(index, InlineWord::MAX) };
            }
        }
    }

    /// Writes `cells` into slot `index`.
    ///
    /// # Safety
    ///
    /// As for [`Slots::slot`]: `index < capacity`.
    #[inline(always)]
    unsafe fn write(self, index: usize, cells: KeyCells) {
        // SAFETY: forwarded; every array has the keys' capacity.
        unsafe {
            if let Some(inline_keys) = self.inline_keys {
                inline_keys.write(index, cells.inline_key);
            }
            if let (Some(tags), Some(tag)) = (self.length_tags, cells.length_tag) {
                tags.slot(index).write(tag);
            }
        }
    }

    /// See [`Slots::insert`].
    ///
    /// # Safety
    ///
    /// As for [`Slots::insert`].
    #[inline(always)]
    unsafe fn insert(self, index: usize, len: usize, cells: KeyCells) {
        // SAFETY: forwarded; every array has the keys' capacity, and is
        // initialized like them.
        unsafe {
            if let Some(inline_keys) = self.inline_keys {
                inline_keys.insert(index, len, cells.inline_key);
            }
            if let (Some(tags), Some(tag)) = (self.length_tags, cells.length_tag) {
                tags.insert(index, len, tag);
            }
        }
    }

    /// See [`Slots::remove`]; what slot `index` held is dropped, being plain
    /// data.
    ///
    /// # Safety
    ///
    /// As for [`Slots::remove`].
    #[inline(always)]
    unsafe fn remove(self, index: usize, len: usize) {
        // SAFETY: forwarded; every array has the keys' capacity, and is
        // initialized like them.
        unsafe {
            if let Some(inline_keys) = self.inline_keys {
                inline_keys.remove(index, len);
            }
            if let Some(tags) = self.length_tags {
                tags.remove(index, len);
            }
        }
    }

    /// See [`Slots::move_run_to`]. `dest` holds the same arrays: it is a
    /// node of the same map and kind.
    ///
    /// # Safety
    ///
    /// As for [`Slots::move_run_to`].
    #[inline(always)]
    unsafe fn move_run_to(self, start: usize, count: usize, dest: Self, dest_start: usize) {
        // SAFETY: forwarded; every array of both has its keys' capacity, and
        // is initialized like them.
        unsafe {
            if let (Some(source), Some(destination)) = (self.inline_keys, dest.inline_keys) {
                source.move_run_to(start, count, destination, dest_start);
            }
            if let (Some(source), Some(destination)) = (self.length_tags, dest.length_tags) {
                source.move_run_to(start, count, destination, dest_start);
            }
        }
    }
}

/// A node's key slots and the columns beside them ([`KeyColumns`]), for a
/// node of kind `Kind` of a map with strategy `S`. Every operation moves a
/// key and its cells together, so that inline key `i` is always key `i`'s;
/// each mirrors the [`Slots`] operation of its name, with the same contract
/// for every array.
///
/// The node stores its keys' windows at its prefix length (05-fence-key.md):
/// a key read out comes with its window there, and a key written in must
/// come with its window there, which debug builds check. A node whose plan
/// stores fewer bytes of each window than a leaf reads a window it hands out
/// from its key again (D43). A key read out of a node without inline keys
/// comes with [`Window::NONE`].
///
/// Invariant: the columns have the keys' capacity, and are those of the
/// plan of `Kind` (`node_plan.rs`). A live length tag is its key's length,
/// truncated to 16 bits; tags never order keys and need no prefix
/// adjustment.
pub(super) struct KeySlots<K, S, Kind> {
    keys: Slots<K>,
    columns: KeyColumns,
    /// The node's prefix length; 0 without inline keys.
    prefix_len: usize,
    _plan: PhantomData<fn() -> (S, Kind)>,
}

impl<K, S, Kind> Clone for KeySlots<K, S, Kind> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, S, Kind> Copy for KeySlots<K, S, Kind> {}

impl<K, S: InlineKeyStrategy<K>, Kind: PageKind> KeySlots<K, S, Kind> {
    /// # Safety
    ///
    /// The columns have the keys' capacity, and are those of the plan of
    /// `Kind`.
    pub(super) unsafe fn new(keys: Slots<K>, columns: KeyColumns, prefix_len: usize) -> Self {
        Self {
            keys,
            columns,
            prefix_len,
            _plan: PhantomData,
        }
    }

    /// The key slots alone, to read keys in place.
    pub(super) fn keys(self) -> Slots<K> {
        self.keys
    }

    /// The columns beside the keys, to read or rewrite in place.
    pub(super) fn columns(self) -> KeyColumns {
        self.columns
    }

    /// The inline-key slots alone, to read inline keys in place, if the node
    /// has them.
    pub(super) fn inline_keys(self) -> Option<InlineKeySlots> {
        self.columns.inline_keys()
    }

    /// Key `index`'s window at the node's fence prefix, to hand out: as many
    /// of its bytes as a leaf keeps, read from the key again where the plan
    /// stores fewer (D43). Reading a std byte string calls no user code and
    /// can't panic.
    ///
    /// # Safety
    ///
    /// Key `index` and its inline key are initialized, `index < capacity`,
    /// and any local search prefix has been normalized to the fences.
    pub(super) unsafe fn window(self, index: usize) -> Window {
        let Some(slots) = self.columns.inline_keys() else {
            return Window::NONE;
        };
        let plan = NodePlan::<K>::of::<S, Kind>();
        let word = match plan.inliner() {
            Some(inliner) if plan.rehydrates_windows() => {
                // SAFETY: this key is initialized and unchanged during the
                // read.
                inliner.inline_key_at(unsafe { self.keys.slot(index).as_ref() }, self.prefix_len)
            }
            // SAFETY: forwarded; the inline-key slots have the keys'
            // capacity.
            _ => unsafe { slots.read(index) },
        };
        Window::new(self.prefix_len, word)
    }

    /// `key`, and what the columns hold for it. Its window must be at this
    /// node's prefix length.
    fn cells_of(self, key: InlinedKey<K, S>) -> (K, KeyCells) {
        let (key, window) = key.into_parts();
        debug_assert!(
            self.columns.inline_keys().is_none() || window.prefix_len() == self.prefix_len,
            "a key at prefix length {} goes into a node at {}",
            window.prefix_len(),
            self.prefix_len
        );
        let length_tag = self
            .columns
            .length_tags()
            .and_then(|_| NodePlan::<K>::of::<S, Kind>().length_tag(&key));
        let cells = KeyCells {
            inline_key: window.inline_key(),
            length_tag,
        };
        (key, cells)
    }

    /// Reads key `index` and its inline key out; the slot holds nothing
    /// afterwards (its bytes are a stale copy).
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    /// Any local search windows have been normalized to the fence prefix.
    pub(super) unsafe fn read(self, index: usize) -> InlinedKey<K, S> {
        // SAFETY: forwarded.
        unsafe {
            let window = self.window(index);
            let key = self.keys.slot(index).read();
            InlinedKey::from_parts(key, window)
        }
    }

    /// Writes `key` and its cells into slot `index`.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` holds nothing that needs
    /// dropping: it is overwritten, and is initialized afterwards.
    pub(super) unsafe fn write(self, index: usize, key: InlinedKey<K, S>) {
        let (key, cells) = self.cells_of(key);
        // SAFETY: forwarded; the columns have the keys' capacity (type
        // invariant).
        unsafe {
            self.keys.slot(index).write(key);
            self.columns.write(index, cells);
        }
    }

    /// Replaces key `index` and its cells, and returns the old key.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    /// Any local search windows have been normalized to the fence prefix.
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
        let (key, cells) = self.cells_of(key);
        // SAFETY: forwarded; the columns have the keys' capacity and are
        // initialized like them (type invariant).
        unsafe {
            self.keys.insert(index, len, key);
            self.columns.insert(index, len, cells);
        }
    }

    /// [`Self::insert`] in a node of entries whose first fields the keys
    /// are (D29): shifts entries `index..len` up whole, with what they pair
    /// the keys with, in one copy, then writes the key. The caller writes
    /// what pairs with it.
    ///
    /// # Safety
    ///
    /// As for [`Self::insert`], and as for [`Slots::shift_strides_up`] for
    /// the keys: the strides of keys `index..len + 1`, their entries, lie in
    /// the node.
    pub(super) unsafe fn insert_shifting_entries(
        self,
        index: usize,
        len: usize,
        key: InlinedKey<K, S>,
    ) {
        let (key, cells) = self.cells_of(key);
        // SAFETY: forwarded; the columns have the keys' capacity and are
        // initialized like them (type invariant).
        unsafe {
            self.keys.shift_strides_up(index, len);
            self.keys.slot(index).write(key);
            self.columns.insert(index, len, cells);
        }
    }

    /// See [`Slots::remove`].
    ///
    /// # Safety
    ///
    /// As for [`Slots::remove`], with any local search windows normalized
    /// to the fence prefix.
    pub(super) unsafe fn remove(self, index: usize, len: usize) -> InlinedKey<K, S> {
        // SAFETY: forwarded; the columns have the keys' capacity and are
        // initialized like them (type invariant).
        unsafe {
            let window = self.window(index);
            let key = self.keys.remove(index, len);
            self.columns.remove(index, len);
            InlinedKey::from_parts(key, window)
        }
    }

    /// [`Self::remove`] in a node of entries whose first fields the keys
    /// are (D29): takes key `index` out, then shifts entries `index + 1..len`
    /// down whole, with what they pair the keys with, in one copy. The
    /// caller has taken out what pairs with key `index` first.
    ///
    /// # Safety
    ///
    /// As for [`Self::remove`], and as for [`Slots::shift_strides_down`] for
    /// the keys: the strides of keys `index..len`, their entries, lie in the
    /// node, and what pairs with key `index` holds nothing that needs
    /// dropping.
    pub(super) unsafe fn remove_shifting_entries(
        self,
        index: usize,
        len: usize,
    ) -> InlinedKey<K, S> {
        // SAFETY: forwarded; the columns have the keys' capacity and are
        // initialized like them (type invariant).
        unsafe {
            let window = self.window(index);
            let key = self.keys.slot(index).read();
            self.columns.remove(index, len);
            self.keys.shift_strides_down(index, len);
            InlinedKey::from_parts(key, window)
        }
    }

    /// See [`Slots::move_run_to`]: the keys and their cells, into `dest`, a
    /// node of the same map and kind.
    ///
    /// # Safety
    ///
    /// As for [`Slots::move_run_to`]. Local search windows have been
    /// normalized, and `dest` has this node's fence prefix length, since the
    /// inline keys move without changing their windows.
    pub(super) unsafe fn move_run_to(
        self,
        start: usize,
        count: usize,
        dest: Self,
        dest_start: usize,
    ) {
        debug_assert_eq!(
            self.prefix_len, dest.prefix_len,
            "a run moves between nodes of different prefix lengths"
        );
        // SAFETY: forwarded; the columns of both have their keys' capacity
        // and are initialized like them (type invariant).
        unsafe {
            self.keys.move_run_to(start, count, dest.keys, dest_start);
            self.columns
                .move_run_to(start, count, dest.columns, dest_start);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::mem::MaybeUninit;
    use core::ptr::NonNull;
    use std::vec::Vec;

    use super::{NodeKeys, Slots};
    use crate::tree::test_support::{DropLedger, Tracked};

    /// Small enough to try every index combination, also under Miri.
    const CAPACITY: usize = 5;

    /// A buffer of `len` elements, kept alive while the returned pointer to
    /// its first one is used; every access goes through that pointer, which
    /// nothing else invalidates.
    fn buffer<'l>(len: usize) -> (Vec<MaybeUninit<Tracked<'l>>>, NonNull<Tracked<'l>>) {
        let mut buffer: Vec<MaybeUninit<Tracked<'l>>> = Vec::new();
        buffer.resize_with(len, MaybeUninit::uninit);
        let base = NonNull::new(buffer.as_mut_ptr().cast::<Tracked<'l>>()).unwrap();
        (buffer, base)
    }

    /// `CAPACITY` slots, `spacing` elements apart, over a buffer of tracked
    /// values they own, next to a model of which tracked value each slot
    /// holds (`None`: nothing). With a spacing of 2, the element after each
    /// slot is another field, as an entry's value is to its key, which
    /// always holds a tracked value of its own: moving the slots must leave
    /// those where they are.
    struct ModelledSlots<'l> {
        _buffer: Vec<MaybeUninit<Tracked<'l>>>,
        base: NonNull<Tracked<'l>>,
        slots: Slots<Tracked<'l>>,
        spacing: usize,
        model: Vec<Option<usize>>,
        /// For a spacing of 2, the tracked value of the field after each
        /// slot.
        others: Vec<usize>,
    }

    impl<'l> ModelledSlots<'l> {
        /// Slots `0..len` hold fresh tracked values, and so does every other
        /// field.
        fn with_len(ledger: &'l DropLedger, len: usize, spacing: usize) -> Self {
            let (buffer, base) = buffer(CAPACITY * spacing);
            let stride = spacing * size_of::<Tracked<'l>>();
            // SAFETY: `CAPACITY` aligned slots of the buffer, `stride` apart,
            // which outlives `slots`.
            let slots = unsafe { Slots::with_stride(base, CAPACITY, stride) };
            let mut this = Self {
                _buffer: buffer,
                base,
                slots,
                spacing,
                model: std::vec![None; CAPACITY],
                others: Vec::new(),
            };
            if spacing == 2 {
                for index in 0..CAPACITY {
                    let other = ledger.track();
                    this.others.push(other.id());
                    // SAFETY: the field is in the buffer, and empty.
                    unsafe { this.other(index).write(other) };
                }
            }
            for index in 0..len {
                let value = ledger.track();
                this.model[index] = Some(value.id());
                // SAFETY: `index < CAPACITY` and the slot is empty.
                unsafe { this.slots.slot(index).write(value) };
            }
            this
        }

        /// The field after slot `index`, for a spacing of 2.
        fn other(&self, index: usize) -> NonNull<Tracked<'l>> {
            debug_assert_eq!(self.spacing, 2);
            // SAFETY: element `2 * index + 1` is in the buffer.
            unsafe { self.base.add(2 * index + 1) }
        }

        /// Checks every initialized slot against the model, through `slot`
        /// and, for slots side by side, through `run` over the initialized
        /// prefix; and that every other field is where it was.
        fn assert_matches_model(&self) {
            for (index, expected) in self.model.iter().enumerate() {
                if let Some(id) = expected {
                    // SAFETY: the model says the slot is initialized.
                    assert_eq!(unsafe { self.slots.slot(index).as_ref() }.id(), *id);
                }
            }
            if self.spacing == 1 {
                let len = self.model.iter().take_while(|id| id.is_some()).count();
                // SAFETY: slots `0..len` are initialized, side by side.
                let prefix = unsafe { self.slots.run(0, len).as_ref() };
                let in_memory: Vec<usize> = prefix.iter().map(Tracked::id).collect();
                let expected: Vec<usize> = self.model[..len].iter().flatten().copied().collect();
                assert_eq!(in_memory, expected);
            }
            for (index, &id) in self.others.iter().enumerate() {
                // SAFETY: every other field holds a tracked value.
                assert_eq!(unsafe { self.other(index).as_ref() }.id(), id);
            }
        }

        /// Drops every initialized slot and every other field.
        fn drop_all(mut self) {
            for (index, id) in self.model.iter_mut().enumerate() {
                if id.take().is_some() {
                    // SAFETY: the model says the slot is initialized; it is
                    // dropped once and then marked empty.
                    unsafe { self.slots.slot(index).drop_in_place() };
                }
            }
            for index in 0..self.others.len() {
                // SAFETY: every other field holds a tracked value, dropped
                // once here.
                unsafe { self.other(index).drop_in_place() };
            }
        }
    }

    /// Both spacings: slots side by side, and strided.
    const SPACINGS: [usize; 2] = [1, 2];

    #[test]
    fn insert_at_every_index() {
        let ledger = DropLedger::new();
        for spacing in SPACINGS {
            for len in 0..CAPACITY {
                for index in 0..=len {
                    let mut slots = ModelledSlots::with_len(&ledger, len, spacing);
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
        }
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn remove_at_every_index() {
        let ledger = DropLedger::new();
        for spacing in SPACINGS {
            for len in 1..=CAPACITY {
                for index in 0..len {
                    let mut slots = ModelledSlots::with_len(&ledger, len, spacing);
                    // SAFETY: `index < len <= CAPACITY`; slots `0..len` are
                    // initialized.
                    let removed = unsafe { slots.slots.remove(index, len) };
                    assert_eq!(Some(removed.id()), slots.model.remove(index));
                    slots.model.push(None);
                    slots.assert_matches_model();
                    slots.drop_all();
                }
            }
        }
        ledger.assert_all_dropped_once();
    }

    /// Moves every run of a full array into every place of an empty one,
    /// between every pair of spacings.
    #[test]
    fn move_every_run() {
        let ledger = DropLedger::new();
        for (from, to) in [(1, 1), (2, 2), (1, 2), (2, 1)] {
            for start in 0..=CAPACITY {
                for count in 0..=CAPACITY - start {
                    for dest_start in 0..=CAPACITY - count {
                        let mut source = ModelledSlots::with_len(&ledger, CAPACITY, from);
                        let mut dest = ModelledSlots::with_len(&ledger, 0, to);
                        // SAFETY: both runs are in bounds, the source run is
                        // initialized, the destination is empty, and the two
                        // buffers are distinct.
                        unsafe {
                            source
                                .slots
                                .move_run_to(start, count, dest.slots, dest_start);
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
        }
        ledger.assert_all_dropped_once();
    }

    /// `CAPACITY` entries of two tracked values each, a key and what pairs
    /// with it, over a buffer they own, the keys as slots an entry apart;
    /// entries `0..len` are initialized.
    struct ModelledEntries<'l> {
        _buffer: Vec<MaybeUninit<Tracked<'l>>>,
        base: NonNull<Tracked<'l>>,
        keys: Slots<Tracked<'l>>,
        /// The ids of each entry's key and other field, `None` if empty.
        model: Vec<Option<(usize, usize)>>,
    }

    impl<'l> ModelledEntries<'l> {
        fn with_len(ledger: &'l DropLedger, len: usize) -> Self {
            let (buffer, base) = buffer(2 * CAPACITY);
            // SAFETY: the keys, every other element of the buffer, which
            // outlives them.
            let keys = unsafe { Slots::with_stride(base, CAPACITY, 2 * size_of::<Tracked<'l>>()) };
            let mut this = Self {
                _buffer: buffer,
                base,
                keys,
                model: std::vec![None; CAPACITY],
            };
            for index in 0..len {
                this.write(index, ledger.track(), ledger.track());
            }
            this
        }

        /// The key and the other field of entry `index`.
        fn fields(&self, index: usize) -> (NonNull<Tracked<'l>>, NonNull<Tracked<'l>>) {
            // SAFETY: elements `2 * index` and `2 * index + 1` are in the
            // buffer.
            unsafe { (self.base.add(2 * index), self.base.add(2 * index + 1)) }
        }

        /// Writes entry `index`, which holds nothing.
        fn write(&mut self, index: usize, key: Tracked<'l>, other: Tracked<'l>) {
            self.model[index] = Some((key.id(), other.id()));
            let (key_field, other_field) = self.fields(index);
            // SAFETY: both fields are in the buffer, and empty.
            unsafe {
                key_field.write(key);
                other_field.write(other);
            }
        }

        /// Reads entry `index` out, which then holds nothing.
        fn read(&mut self, index: usize) -> (Tracked<'l>, Tracked<'l>) {
            self.model[index] = None;
            let (key_field, other_field) = self.fields(index);
            // SAFETY: the entry is initialized, and read out once.
            unsafe { (key_field.read(), other_field.read()) }
        }

        fn assert_matches_model(&self) {
            for (index, expected) in self.model.iter().enumerate() {
                if let Some((key, other)) = expected {
                    let (_, other_field) = self.fields(index);
                    // SAFETY: the model says the entry is initialized.
                    unsafe {
                        assert_eq!(self.keys.slot(index).as_ref().id(), *key);
                        assert_eq!(other_field.as_ref().id(), *other);
                    }
                }
            }
        }

        fn drop_all(mut self) {
            for index in 0..CAPACITY {
                if self.model[index].is_some() {
                    drop(self.read(index));
                }
            }
        }
    }

    /// Shifting the keys' strides moves whole entries: a key and what pairs
    /// with it go together.
    #[test]
    fn shifting_strides_moves_whole_entries() {
        let ledger = DropLedger::new();
        for len in 0..CAPACITY {
            for index in 0..=len {
                let mut entries = ModelledEntries::with_len(&ledger, len);
                // SAFETY: `index <= len < CAPACITY`, the strides of keys
                // `0..CAPACITY` lie in the buffer, entries `index..len` are
                // initialized and entry `len` holds nothing.
                unsafe { entries.keys.shift_strides_up(index, len) };
                entries.model.insert(index, None);
                entries.model.truncate(CAPACITY);
                entries.write(index, ledger.track(), ledger.track());
                entries.assert_matches_model();
                entries.drop_all();
            }
        }
        for len in 1..=CAPACITY {
            for index in 0..len {
                let mut entries = ModelledEntries::with_len(&ledger, len);
                drop(entries.read(index));
                // SAFETY: `index < len <= CAPACITY`, the strides of keys
                // `0..CAPACITY` lie in the buffer, entries `index + 1..len`
                // are initialized and entry `index` holds nothing.
                unsafe { entries.keys.shift_strides_down(index, len) };
                entries.model.remove(index);
                entries.model.push(None);
                entries.assert_matches_model();
                entries.drop_all();
            }
        }
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn node_keys_lend_strided_keys() {
        let ledger = DropLedger::new();
        let entries = ModelledEntries::with_len(&ledger, 4);
        let ids: Vec<usize> = entries
            .model
            .iter()
            .flatten()
            .map(|&(key, _)| key)
            .collect();
        // SAFETY: keys `0..4` are initialized and unchanged while the keys
        // are lent, and their strides lie in the buffer.
        let keys = unsafe { NodeKeys::new(entries.keys, 4) };
        assert_eq!(keys.len(), 4);
        assert_eq!(keys.iter().map(Tracked::id).collect::<Vec<_>>(), ids);
        assert_eq!(keys.first().map(Tracked::id), Some(ids[0]));
        assert_eq!(keys[3].id(), ids[3]);
        assert!(keys.get(4).is_none());
        let middle = keys.slice(1..3);
        assert_eq!(
            middle.iter().map(Tracked::id).collect::<Vec<_>>(),
            ids[1..3]
        );
        assert_eq!(keys.slice(4..4).len(), 0);
        entries.drop_all();
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn zero_sized_slots() {
        // Strided zero-sized slots, as the keys of a node of `((), V)`
        // entries are, sit in a buffer of their stride; side by side, they
        // occupy no memory.
        let mut strides = [0_u64; CAPACITY];
        let anchor = NonNull::from(&mut strides).cast::<()>();
        for stride in [0, size_of::<u64>()] {
            // SAFETY: the slots, of no size, lie in `strides`, which outlives
            // them, at either stride.
            let slots = unsafe { Slots::with_stride(anchor, CAPACITY, stride) };
            for len in 0..CAPACITY {
                // SAFETY: `len < CAPACITY`; `()` is always initialized.
                unsafe {
                    slots.insert(0, len, ());
                    slots.remove(len, len + 1);
                }
            }
        }
    }
}
