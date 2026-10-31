//! [`Slots`]: a fixed-capacity array of slots inside a node page. Apart from
//! whole pages (`page.rs`), this is the only code that moves keys, values
//! and edges within and between nodes (D9, D12, D14, D29).
//!
//! - [`InlineKeySlots`]: a node's inline keys at its kind's width.
//! - [`KeyColumns`]: every array a node keeps beside its keys.
//! - [`KeySlots`]: moves a node's keys together with their columns (D18).
//! - [`SeparatorSlots`]: an internal node's separators, keys or windows
//!   (D49).
//! - [`NodeKeys`]: lends a node's keys in place, as a [`KeyList`], which
//!   holds the node's two binary searches (D19).

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::hint;
use core::marker::PhantomData;
use core::ops::{Index, Range};
use core::ptr::NonNull;

use super::inline_keys::{InlineSlot, Separator, Window, WindowedKey};
use super::internal::Internal;
use super::node_plan::NodePlan;
use super::page::NodeKind;
use crate::inline_key::{InlineKeyStrategy, InlineWidth, InlineWord};

/// Where `capacity` slots of `T` sit in memory: slot `i` at `stride * i`
/// bytes past `anchor`. The slots of an array of their own sit side by
/// side, `size_of::<T>()` apart; the keys and values of a leaf of
/// `(key, value)` entries are fields of its entries, an entry's size apart
/// (D29). Internal nodes never pair their edges with keys (D32).
///
/// A `Slots` neither owns nor borrows its slots and tracks no length: which
/// slots are initialized is the caller's business, so every operation is
/// `unsafe` and states which slots it expects to be initialized.
///
/// Slots side by side move a run with one copy; strided ones move slot by
/// slot, as the bytes between them belong to other fields.
/// [`Self::shift_strides_up`] and [`Self::shift_strides_down`] move whole
/// strides, other fields included: that is how a node shifts its entries in
/// one copy.
///
/// Invariant: `stride` is at least `size_of::<T>()` and a multiple of
/// `align_of::<T>()`, and the `capacity` slots are aligned for `T` and lie
/// inside one allocation.
pub(in crate::tree) struct Slots<T> {
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

    #[inline(always)]
    pub(super) fn capacity(self) -> usize {
        self.capacity
    }

    /// Slot 0 of slots side by side: where a vector load over them starts.
    /// Vectors are SSE2's on x86-64, which a target for kernels lacks.
    #[cfg(any(
        all(target_arch = "x86_64", target_feature = "sse2"),
        target_arch = "aarch64"
    ))]
    #[inline(always)]
    pub(super) fn as_ptr(self) -> *const T {
        debug_assert!(self.is_contiguous());
        self.anchor.as_ptr()
    }

    /// A copy of slot `index`'s value, loaded through the slots' pointer,
    /// forming no reference. In a latched tree, a reader may load the slots
    /// of an internal node that a writer is changing. Its version check then
    /// throws away what it loaded (`optimistic.rs`).
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    #[inline(always)]
    pub(super) unsafe fn load(self, index: usize) -> T
    where
        T: Copy,
    {
        // SAFETY: forwarded.
        unsafe { self.slot(index).read() }
    }

    /// The slots of `slice`, for tests of code that reads slots.
    #[cfg(test)]
    pub(super) fn of_slice(slice: &[T]) -> Self {
        // SAFETY: a slice's elements are aligned, side by side, in one
        // allocation, which outlives the borrow the caller keeps.
        unsafe { Self::new(NonNull::from(slice).cast(), slice.len()) }
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

    /// Slots `0..len` of slots side by side, as one slice in index order.
    ///
    /// # Safety
    ///
    /// `len <= capacity`, and the slots are side by side.
    pub(super) unsafe fn slice_ptr(self, len: usize) -> NonNull<[T]> {
        debug_assert!(len <= self.capacity);
        // SAFETY: forwarded.
        let first = unsafe { self.contiguous_slot(0) };
        NonNull::slice_from_raw_parts(first, len)
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
        // `index + 1..len + 1`, which frees slot `index` for `value`. Side by
        // side, it moves in one copy (overlapping, hence `copy_to`); strided,
        // slot by slot from the top, so that each slot is read before it is
        // written over.
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
        // initialized tail `index + 1..len` then moves into `index..len - 1`.
        // Side by side, it moves in one copy (overlapping, hence `copy_to`);
        // strided, slot by slot from the bottom, so that each slot is read
        // before it is written over.
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
    /// - `start + count <= self.capacity` and
    ///   `dest_start + count <= dest.capacity`.
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
    /// `index + 1..len + 1`, with the other fields between them. They are
    /// the entries of a node whose first fields are these slots. Slot
    /// `index`'s stride holds stale bytes afterwards.
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
    /// stride of slot `index`. The stride of slot `len - 1` holds stale bytes
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
/// leaf of `(key, value)` entries (D29).
pub(in crate::tree) struct NodeKeys<'a, K> {
    anchor: NonNull<K>,
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
            anchor: keys.anchor,
            stride: keys.stride,
            len,
            _keys: PhantomData,
        }
    }

    pub(in crate::tree) fn len(self) -> usize {
        self.len
    }

    /// Key `index`.
    ///
    /// # Safety
    ///
    /// `index < len`.
    pub(in crate::tree) unsafe fn get_unchecked(self, index: usize) -> &'a K {
        debug_assert!(index < self.len);
        // SAFETY: key `index` is initialized and unchanged for `'a` (`new`).
        unsafe { self.anchor.byte_add(index * self.stride).as_ref() }
    }

    /// Key `index`, or `None` if `index >= len`.
    pub(in crate::tree) fn get(self, index: usize) -> Option<&'a K> {
        // SAFETY: `index < len`.
        (index < self.len).then(|| unsafe { self.get_unchecked(index) })
    }

    pub(in crate::tree) fn first(self) -> Option<&'a K> {
        self.get(0)
    }

    /// Keys `range` of these.
    ///
    /// # Panics
    ///
    /// If `range` is not within `0..len`.
    pub(in crate::tree) fn slice(self, range: Range<usize>) -> Self {
        assert!(
            range.start <= range.end && range.end <= self.len,
            "keys out of bounds"
        );
        Self {
            // SAFETY: `range.start <= len`: a key's slot, or the end of the
            // last key's stride, within the allocation or one past it
            // (`new`).
            anchor: unsafe { self.anchor.byte_add(range.start * self.stride) },
            len: range.end - range.start,
            ..self
        }
    }

    #[cfg(any(test, fuzzing))]
    pub(in crate::tree) fn iter(
        self,
    ) -> impl DoubleEndedIterator<Item = &'a K> + ExactSizeIterator {
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

/// Keys in order, lent for `'a`, as a node search reads them: a node's own,
/// in place ([`NodeKeys`]), or the head keys an internal node's separators
/// lead to (`HeadKeys`, D49).
pub(super) trait KeyList<'a, K: 'a>: Copy {
    fn len(self) -> usize;

    /// Key `index`.
    ///
    /// # Safety
    ///
    /// `index < len`.
    unsafe fn get_unchecked(self, index: usize) -> &'a K;

    /// Keys `range` of these.
    ///
    /// # Panics
    ///
    /// If `range` is not within `0..len`.
    fn slice(self, range: Range<usize>) -> Self;

    /// `Ok(i)` if key `i` equals `key`, otherwise `Err(i)` with `i` where it
    /// belongs, by a branchless binary search. Every search runs the full
    /// `log2(len)` steps with no early exit. Each step only picks the next
    /// `base` with `select_unpredictable`, which compiles to a conditional
    /// move instead of a mispredicted branch. The loop narrows to `base`, the
    /// last key not greater than `key` (or 0), and one final comparison
    /// classifies it.
    ///
    /// In bounds by construction, whatever `Ord` answers: the loop keeps
    /// `base + size <= len` and probes only
    /// `mid = base + size / 2 < base + size`, so `base < len` at the end.
    /// `Ok(base)` is then `< len`, and `Err(base)`/`Err(base + 1)` are
    /// `<= len`.
    fn branchless_search_index<Q>(self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut size = self.len();
        if size == 0 {
            return Err(0);
        }
        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            // SAFETY: `mid < base + size <= len`.
            let probe = unsafe { self.get_unchecked(mid) };
            let probe_is_greater = probe.borrow().cmp(key) == Ordering::Greater;
            base = hint::select_unpredictable(probe_is_greater, base, mid);
            size -= half;
        }
        // SAFETY: `base < base + size <= len`, with `size == 1` here.
        let last_not_greater = unsafe { self.get_unchecked(base) };
        match last_not_greater.borrow().cmp(key) {
            Ordering::Equal => Ok(base),
            Ordering::Less => Err(base + 1),
            Ordering::Greater => Err(base),
        }
    }

    /// `Ok(i)` if key `i` is the one searched for, otherwise `Err(i)` with
    /// `i` where it belongs, by a binary search that stops as soon as it
    /// finds it. `cmp_key(probe)` orders the key searched for against key
    /// `probe`.
    ///
    /// In bounds by construction, whatever `cmp_key` answers: the loop keeps
    /// `low <= high <= len` and probes only `mid` with `low <= mid < high`,
    /// so `Ok(mid)` is `< len` and `Err(low)` is `<= len`.
    fn early_exit_search_index(self, cmp_key: impl Fn(&K) -> Ordering) -> Result<usize, usize> {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let mid = low + (high - low) / 2;
            // SAFETY: `mid < high <= len`.
            let probe = unsafe { self.get_unchecked(mid) };
            match cmp_key(probe) {
                Ordering::Less => high = mid,
                Ordering::Equal => return Ok(mid),
                Ordering::Greater => low = mid + 1,
            }
        }
        Err(low)
    }
}

impl<'a, K> KeyList<'a, K> for NodeKeys<'a, K> {
    #[inline(always)]
    fn len(self) -> usize {
        Self::len(self)
    }

    #[inline(always)]
    unsafe fn get_unchecked(self, index: usize) -> &'a K {
        // SAFETY: forwarded.
        unsafe { Self::get_unchecked(self, index) }
    }

    #[inline(always)]
    fn slice(self, range: Range<usize>) -> Self {
        Self::slice(self, range)
    }
}

/// A node's inline-key slots (D18): `capacity` [`InlineSlot`]s of the node
/// kind's [`InlineWidth`], side by side. The operations mirror those of
/// [`Slots`], with the same contracts, and speak in inline keys: a write
/// keeps an inline key's first bytes, and a read gives them back, zeros
/// after.
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

/// Every method is `#[inline(always)]`, as [`KeyColumns`]' are: the width
/// is a constant of the node's plan, which a call would test at run time.
impl InlineKeySlots {
    /// `capacity` slots of `width` side by side from `anchor`.
    ///
    /// # Safety
    ///
    /// As for [`Slots::new`], for slots of `width` bytes.
    #[inline(always)]
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

    #[cfg(test)]
    pub(super) fn width(self) -> InlineWidth {
        match self {
            Self::Two(_) => InlineWidth::Two,
            Self::Four(_) => InlineWidth::Four,
            Self::Eight(_) => InlineWidth::Eight,
        }
    }

    #[inline(always)]
    pub(super) fn capacity(self) -> usize {
        with_slots_of_width!(self, |slots| slots.capacity)
    }

    /// Slot `index`'s inline key: the bytes it keeps of a window, zeros
    /// after them.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    #[inline(always)]
    pub(super) unsafe fn read(self, index: usize) -> InlineWord {
        // SAFETY: forwarded.
        unsafe { with_slots_of_width!(self, |slots| slots.slot(index).read().inline_key()) }
    }

    /// Fills every slot with the greatest inline key, all ones.
    ///
    /// # Safety
    ///
    /// As for [`Slots::fill_bytes`]: inline keys are plain data, and any
    /// bytes are a valid one.
    #[inline(always)]
    pub(super) unsafe fn fill_with_greatest(self) {
        // SAFETY: forwarded.
        unsafe { with_slots_of_width!(self, |slots| slots.fill_bytes(u8::MAX)) }
    }

    /// Writes the first bytes of `inline_key` into slot `index`, which is
    /// initialized afterwards. Inline keys need no dropping.
    ///
    /// # Safety
    ///
    /// `index < capacity`.
    #[inline(always)]
    pub(super) unsafe fn write(self, index: usize, inline_key: InlineWord) {
        // SAFETY: forwarded.
        unsafe {
            with_slots_of_width!(self, |slots| {
                slots
                    .slot(index)
                    .write(InlineSlot::of_inline_key(inline_key));
            });
        }
    }

    /// See [`Slots::insert`].
    ///
    /// # Safety
    ///
    /// As for [`Slots::insert`].
    #[inline(always)]
    pub(super) unsafe fn insert(self, index: usize, len: usize, inline_key: InlineWord) {
        // SAFETY: forwarded.
        unsafe {
            with_slots_of_width!(self, |slots| {
                slots.insert(index, len, InlineSlot::of_inline_key(inline_key));
            });
        }
    }

    /// See [`Slots::remove`]; returns the inline key taken out, as
    /// [`Self::read`] does.
    ///
    /// # Safety
    ///
    /// As for [`Slots::remove`].
    #[inline(always)]
    pub(super) unsafe fn remove(self, index: usize, len: usize) -> InlineWord {
        // SAFETY: forwarded.
        unsafe { with_slots_of_width!(self, |slots| slots.remove(index, len).inline_key()) }
    }

    /// See [`Slots::move_run_to`]. `dest` has this array's width: runs move
    /// between nodes of the same kind and map.
    ///
    /// # Safety
    ///
    /// As for [`Slots::move_run_to`].
    #[inline(always)]
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
                _ => unreachable!("runs move between nodes of one width"),
            }
        }
    }
}

/// The arrays a node keeps beside its keys, one slot per key or separator,
/// which every move moves alike. With inline keys, an internal node keeps
/// them in place of keys (D49). They are the node's inline keys (D18) and,
/// in a leaf whose plan keeps them and whose layout has room, its keys'
/// hash tags (D36). Adding another such array takes a field here and a
/// cell of [`KeyCells`]; the code that moves keys ([`KeySlots`]) doesn't
/// change.
///
/// Each operation mirrors the [`Slots`] operation of its name, with the same
/// contract for every array.
///
/// Invariant: each array present has the keys' capacity.
#[derive(Clone, Copy)]
pub(super) struct KeyColumns {
    inline_keys: Option<InlineKeySlots>,
    hash_tags: Option<Slots<u8>>,
}

/// What the [`KeyColumns`] of a node hold for one key.
#[derive(Clone, Copy)]
pub(super) struct KeyCells {
    /// The first bytes of the key's window, which the inline keys keep.
    inline_key: InlineWord,
    /// The key's hash tag, which a node that keeps them stores.
    hash_tag: u8,
}

/// Every method is `#[inline(always)]`. Which arrays a node keeps, and at
/// what width, is a constant of its plan (`node_plan.rs`) only where the
/// method is inlined into code that knows the node's kind and map. Behind a
/// call, it would be a value tested at run time.
impl KeyColumns {
    /// # Safety
    ///
    /// Each array present has `capacity` slots, as [`Slots::new`] requires
    /// of them.
    #[inline(always)]
    pub(super) unsafe fn new(
        inline_keys: Option<InlineKeySlots>,
        hash_tags: Option<Slots<u8>>,
        capacity: usize,
    ) -> Self {
        debug_assert!(inline_keys.is_none_or(|inline_keys| inline_keys.capacity() == capacity));
        debug_assert!(hash_tags.is_none_or(|tags| tags.capacity == capacity));
        Self {
            inline_keys,
            hash_tags,
        }
    }

    #[inline(always)]
    pub(super) fn inline_keys(self) -> Option<InlineKeySlots> {
        self.inline_keys
    }

    #[inline(always)]
    pub(super) fn hash_tags(self) -> Option<Slots<u8>> {
        self.hash_tags
    }

    /// Fills every slot of a node that holds no keys yet. Each inline key
    /// gets the greatest, all ones, as slots past the length hold (invariant
    /// 4 in `tree/node/mod.rs`); each hash tag gets 0.
    ///
    /// # Safety
    ///
    /// The node's handle has exclusive access.
    #[inline(always)]
    pub(super) unsafe fn initialize(self) {
        // SAFETY: the slots are in the page (type invariant) and hold plain
        // data; the caller has exclusive access.
        unsafe {
            if let Some(inline_keys) = self.inline_keys {
                inline_keys.fill_with_greatest();
            }
            if let Some(tags) = self.hash_tags {
                tags.fill_bytes(0);
            }
        }
    }

    /// Gives slots `vacated`, which a node no longer holds keys in, the
    /// greatest inline key again (invariant 4). Their hash tags stay
    /// stale: a search reads them, but masks out every slot outside its
    /// candidates (invariant 5).
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
            if let Some(tags) = self.hash_tags {
                tags.slot(index).write(cells.hash_tag);
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
            if let Some(tags) = self.hash_tags {
                tags.insert(index, len, cells.hash_tag);
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
            if let Some(tags) = self.hash_tags {
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
            if let (Some(source), Some(destination)) = (self.hash_tags, dest.hash_tags) {
                source.move_run_to(start, count, destination, dest_start);
            }
        }
    }
}

/// A node's key slots and the columns beside them ([`KeyColumns`]), for a
/// node of kind `Kind` of a map with strategy `S`. Every operation moves a
/// key and its cells together, so that inline key `i` is always key `i`'s.
/// Each mirrors the [`Slots`] operation of its name, with the same contract
/// for every array.
///
/// The node stores its keys' windows at its prefix length (05-fence-key.md).
/// A key read out comes with its window there. A key written in must come
/// with its window there, which debug builds check. A key read out of a
/// node without inline keys comes with [`Window::NONE`].
///
/// Invariant: the columns have the keys' capacity, and are those of the
/// plan and layout of `Kind` (`node_plan.rs`, `layout.rs`). A live hash
/// tag is its key's (`KeyInliner::hash_tag`); tags never order keys and
/// need no prefix adjustment.
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

/// Every method is `#[inline(always)]`, as [`KeyColumns`]' are: the
/// columns it holds are constants of the node's plan only where inlined.
impl<K, S: InlineKeyStrategy<K>, Kind: NodeKind> KeySlots<K, S, Kind> {
    /// # Safety
    ///
    /// The columns have the keys' capacity, and are those of the plan of
    /// `Kind`.
    #[inline(always)]
    pub(super) unsafe fn new(keys: Slots<K>, columns: KeyColumns, prefix_len: usize) -> Self {
        Self {
            keys,
            columns,
            prefix_len,
            _plan: PhantomData,
        }
    }

    /// The key slots alone, to read keys in place.
    #[inline(always)]
    pub(super) fn keys(self) -> Slots<K> {
        self.keys
    }

    /// Key `index`'s window at the node's prefix length, as the node keeps
    /// it.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and its inline key is initialized.
    #[inline(always)]
    pub(super) unsafe fn window(self, index: usize) -> Window {
        let Some(slots) = self.columns.inline_keys() else {
            return Window::NONE;
        };
        // SAFETY: forwarded; the inline-key slots have the keys' capacity.
        Window::new(self.prefix_len, unsafe { slots.read(index) })
    }

    /// `key`, and what the columns hold for it. Its window must be at this
    /// node's prefix length.
    #[inline(always)]
    fn cells_of(self, key: WindowedKey<K, S>) -> (K, KeyCells) {
        let (key, window) = key.into_parts();
        debug_assert!(
            self.columns.inline_keys().is_none() || window.prefix_len() == self.prefix_len,
            "a key at prefix length {} goes into a node at {}",
            window.prefix_len(),
            self.prefix_len
        );
        let hash_tag = match self.columns.hash_tags() {
            Some(_) => NodePlan::<K>::of::<S, Kind>().hash_tag(&key).unwrap_or(0),
            None => 0,
        };
        let cells = KeyCells {
            inline_key: window.inline_key(),
            hash_tag,
        };
        (key, cells)
    }

    /// Reads key `index` and its inline key out; the slot holds nothing
    /// afterwards (its bytes are a stale copy).
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` is initialized.
    #[inline(always)]
    pub(super) unsafe fn read(self, index: usize) -> WindowedKey<K, S> {
        // SAFETY: forwarded.
        unsafe {
            let window = self.window(index);
            let key = self.keys.slot(index).read();
            WindowedKey::from_parts(key, window)
        }
    }

    /// Writes `key` and its cells into slot `index`.
    ///
    /// # Safety
    ///
    /// `index < capacity`, and slot `index` holds nothing that needs
    /// dropping: it is overwritten, and is initialized afterwards.
    #[inline(always)]
    pub(super) unsafe fn write(self, index: usize, key: WindowedKey<K, S>) {
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
    #[inline(always)]
    pub(super) unsafe fn replace(self, index: usize, key: WindowedKey<K, S>) -> WindowedKey<K, S> {
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
    #[inline(always)]
    pub(super) unsafe fn insert(self, index: usize, len: usize, key: WindowedKey<K, S>) {
        let (key, cells) = self.cells_of(key);
        // SAFETY: forwarded; the columns have the keys' capacity and are
        // initialized like them (type invariant).
        unsafe {
            self.keys.insert(index, len, key);
            self.columns.insert(index, len, cells);
        }
    }

    /// [`Self::insert`] in a node of entries whose first fields are the
    /// keys (D29). It shifts entries `index..len` up whole, with what pairs
    /// with each key, in one copy, then writes the key. The caller writes
    /// what pairs with it.
    ///
    /// # Safety
    ///
    /// As for [`Self::insert`], and as for [`Slots::shift_strides_up`] for
    /// the keys: the strides of keys `index..len + 1`, their entries, lie in
    /// the node.
    #[inline(always)]
    pub(super) unsafe fn insert_shifting_entries(
        self,
        index: usize,
        len: usize,
        key: WindowedKey<K, S>,
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
    /// As for [`Slots::remove`].
    #[inline(always)]
    pub(super) unsafe fn remove(self, index: usize, len: usize) -> WindowedKey<K, S> {
        // SAFETY: forwarded; the columns have the keys' capacity and are
        // initialized like them (type invariant).
        unsafe {
            let window = self.window(index);
            let key = self.keys.remove(index, len);
            self.columns.remove(index, len);
            WindowedKey::from_parts(key, window)
        }
    }

    /// [`Self::remove`] in a node of entries whose first fields are the
    /// keys (D29). It takes key `index` out, then shifts entries
    /// `index + 1..len` down whole, with what pairs with each key, in one
    /// copy. The caller has taken out what pairs with key `index` first.
    ///
    /// # Safety
    ///
    /// As for [`Self::remove`], and as for [`Slots::shift_strides_down`] for
    /// the keys:
    /// - the strides of keys `index..len`, their entries, lie in the node;
    /// - what pairs with key `index` holds nothing that needs dropping.
    #[inline(always)]
    pub(super) unsafe fn remove_shifting_entries(
        self,
        index: usize,
        len: usize,
    ) -> WindowedKey<K, S> {
        // SAFETY: forwarded; the columns have the keys' capacity and are
        // initialized like them (type invariant).
        unsafe {
            let window = self.window(index);
            let key = self.keys.slot(index).read();
            self.columns.remove(index, len);
            self.keys.shift_strides_down(index, len);
            WindowedKey::from_parts(key, window)
        }
    }

    /// See [`Slots::move_run_to`]: the keys and their cells, into `dest`, a
    /// node of the same map and kind.
    ///
    /// # Safety
    ///
    /// As for [`Slots::move_run_to`]. `dest` has this node's prefix length
    /// (debug-checked), since the inline keys move unchanged; a wrong one
    /// gives wrong answers, never undefined behavior.
    #[inline(always)]
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

/// An internal node's separators (D17), for a map with strategy `S`. Every
/// operation mirrors the [`KeySlots`] operation of its name, on
/// [`Separator`]s, with the same contract.
///
/// Invariant: the variant is the plan's: [`Self::Keys`] in a map without
/// inline keys, [`Self::Windows`] in a map with them.
pub(super) enum SeparatorSlots<K, V, S> {
    /// The separator keys themselves, which the node keeps.
    Keys(KeySlots<K, S, Internal<K, V, S>>),
    /// The separators' windows at the node's prefix length, in its inline
    /// keys; the keys themselves are leaf heads (D49).
    Windows {
        columns: KeyColumns,
        prefix_len: usize,
    },
}

impl<K, V, S> Clone for SeparatorSlots<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for SeparatorSlots<K, V, S> {}

/// Every method is `#[inline(always)]`, as [`KeyColumns`]' are: which
/// variant a node's separators are is a constant of its map only where the
/// method is inlined.
impl<K, V, S: InlineKeyStrategy<K>> SeparatorSlots<K, V, S> {
    /// Separator `index`'s window at the node's prefix length, as kept:
    /// [`Window::NONE`] in a node without inline keys.
    ///
    /// # Safety
    ///
    /// As for [`KeySlots::window`].
    #[inline(always)]
    pub(super) unsafe fn window(self, index: usize) -> Window {
        // SAFETY: forwarded; the inline keys have the separators' capacity.
        unsafe {
            match self {
                Self::Keys(keys) => keys.window(index),
                Self::Windows {
                    columns,
                    prefix_len,
                } => {
                    let inline_keys = columns.inline_keys().expect("windows of inline keys");
                    Window::new(prefix_len, inline_keys.read(index))
                }
            }
        }
    }

    /// What the columns hold for `window`, which must be at this node's
    /// prefix length.
    #[inline(always)]
    fn cells_of(prefix_len: usize, window: Window) -> KeyCells {
        debug_assert_eq!(
            window.prefix_len(),
            prefix_len,
            "a separator at another prefix length"
        );
        KeyCells {
            inline_key: window.inline_key(),
            hash_tag: 0,
        }
    }

    /// The key a separator going into a node that keeps keys is.
    #[inline(always)]
    fn key_of(separator: Separator<K, S>) -> WindowedKey<K, S> {
        separator
            .into_windowed_key()
            .expect("a separator into a node that keeps keys is a key")
    }

    /// See [`KeySlots::read`].
    ///
    /// # Safety
    ///
    /// As for [`KeySlots::read`].
    #[inline(always)]
    pub(super) unsafe fn read(self, index: usize) -> Separator<K, S> {
        // SAFETY: forwarded.
        unsafe {
            match self {
                Self::Keys(keys) => Separator::with_key(keys.read(index)),
                Self::Windows { .. } => Separator::without_key(self.window(index)),
            }
        }
    }

    /// See [`KeySlots::write`].
    ///
    /// # Safety
    ///
    /// As for [`KeySlots::write`].
    #[inline(always)]
    pub(super) unsafe fn write(self, index: usize, separator: Separator<K, S>) {
        // SAFETY: forwarded.
        unsafe {
            match self {
                Self::Keys(keys) => keys.write(index, Self::key_of(separator)),
                Self::Windows {
                    columns,
                    prefix_len,
                } => columns.write(index, Self::cells_of(prefix_len, separator.window())),
            }
        }
    }

    /// See [`KeySlots::replace`].
    ///
    /// # Safety
    ///
    /// As for [`KeySlots::replace`].
    #[inline(always)]
    pub(super) unsafe fn replace(
        self,
        index: usize,
        separator: Separator<K, S>,
    ) -> Separator<K, S> {
        // SAFETY: forwarded: slot `index` is read out, then written again.
        unsafe {
            let old = self.read(index);
            self.write(index, separator);
            old
        }
    }

    /// See [`KeySlots::insert`].
    ///
    /// # Safety
    ///
    /// As for [`KeySlots::insert`].
    #[inline(always)]
    pub(super) unsafe fn insert(self, index: usize, len: usize, separator: Separator<K, S>) {
        // SAFETY: forwarded.
        unsafe {
            match self {
                Self::Keys(keys) => keys.insert(index, len, Self::key_of(separator)),
                Self::Windows {
                    columns,
                    prefix_len,
                } => columns.insert(index, len, Self::cells_of(prefix_len, separator.window())),
            }
        }
    }

    /// See [`KeySlots::remove`].
    ///
    /// # Safety
    ///
    /// As for [`KeySlots::remove`].
    #[inline(always)]
    pub(super) unsafe fn remove(self, index: usize, len: usize) -> Separator<K, S> {
        // SAFETY: forwarded.
        unsafe {
            match self {
                Self::Keys(keys) => Separator::with_key(keys.remove(index, len)),
                Self::Windows { columns, .. } => {
                    let window = self.window(index);
                    columns.remove(index, len);
                    Separator::without_key(window)
                }
            }
        }
    }

    /// See [`KeySlots::move_run_to`]: `dest` is another internal node of the
    /// same map, whose separators are of the same variant.
    ///
    /// # Safety
    ///
    /// As for [`KeySlots::move_run_to`].
    #[inline(always)]
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
                (Self::Keys(keys), Self::Keys(dest)) => {
                    keys.move_run_to(start, count, dest, dest_start);
                }
                (
                    Self::Windows {
                        columns,
                        prefix_len,
                    },
                    Self::Windows {
                        columns: dest,
                        prefix_len: dest_prefix_len,
                    },
                ) => {
                    debug_assert_eq!(
                        prefix_len, dest_prefix_len,
                        "a run moves between nodes of different prefix lengths"
                    );
                    columns.move_run_to(start, count, dest, dest_start);
                }
                _ => unreachable!("the separators of one map are of one variant"),
            }
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
                let prefix = unsafe { self.slots.slice_ptr(len).as_ref() };
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
