//! A store's leaves (`design/16-bbtree-store.md`): each keeps its entries'
//! records, key and value, in a chain of heap pages of its own, and its
//! keys are pointers to them ([`RecordPtr`]).
//!
//! - **The chain** is the leaf's. Its header ([`HeapChain`]) points to the
//!   newest heap page, which links to the one before, down to the oldest. A
//!   record goes after the last one in the newest page; a new page goes in
//!   front. No other leaf points into a leaf's heap pages, so whatever moves
//!   a record holds the one leaf that points to it.
//! - **A record** is its key, length first, then its value, length first
//!   ([`RecordPtr`]). Records lie in no order; the leaf's entries are sorted.
//! - **An entry dropped** leaves its record where it is, counted as
//!   garbage. Compaction slides the live records together, in place, and
//!   gives back the pages it empties: when the leaf splits or merges, or
//!   when a record needs room that the garbage makes.
//! - **A split** copies the records of the entries that move into the new
//!   leaf's chain, onto the empty pages in front of the old leaf's chain,
//!   then compacts the old one. **A merge** splices the leaving leaf's chain
//!   onto the survivor's, then compacts.
//! - **Heap pages** are a kind of page of their own ([`PageKind::Heap`]),
//!   which the allocator hands out and takes back by its heap methods: only
//!   their leaf's holder reads them, so a tree's page pool passes them
//!   through, with none of the type-stable reuse node pages get
//!   (`tree/latched/pages.rs`). An operation that must not fail midway takes
//!   them first, by growing the leaf's chain with empty pages.
//!
//! The node code calls these where it adds, drops or moves a store leaf's
//! entries, under [`NodePtr::is_in_store`], which folds away for maps.
//! [`InStore`] is the strategy that says so; it takes `RecordPtr` keys
//! alone, so a store leaf's keys are record pointers.

use alloc::vec::Vec;
use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem::size_of;
use core::ptr::{self, NonNull};

use super::inline_keys::WindowedKey;
use super::leaf::Leaf;
use super::page::{HeapChain, NodePtr, PageAllocator, PageType};
use crate::allocator::{AllocError, PageKind};
use crate::inline_key::InlineKeyStrategy;
use crate::inline_key::sealed::{InlineKeySource, Sealed};
use crate::node_config::NodeConfig;
use crate::record_ptr::RecordPtr;

/// The strategy of a store's tree: `S`'s inline keys, and leaves that keep
/// their records in heap pages. Its keys are record pointers. Nothing
/// public names it: a store with strategy `S` keeps its tree with this one.
pub(crate) struct InStore<S>(PhantomData<fn() -> S>);

impl<S: InlineKeyStrategy<RecordPtr>> Sealed<RecordPtr> for InStore<S> {
    const SOURCE: InlineKeySource<RecordPtr> = S::SOURCE;
    type Config = S::Config;
    const IN_STORE: bool = true;
}

impl<S: InlineKeyStrategy<RecordPtr>> InlineKeyStrategy<RecordPtr> for InStore<S> {}

impl<S: InlineKeyStrategy<RecordPtr>> InStore<S> {
    /// The longest key: a quarter of a heap page.
    pub(crate) const MAX_KEY_LEN: usize = HeapPage::<S::Config>::SIZE / 4;

    /// The largest record: what a heap page holds after its header.
    pub(crate) const MAX_RECORD_SIZE: usize = HeapPage::<S::Config>::MAX_RECORD_SIZE;
}

/// A store's heap page, of node config `C`: records of one leaf, after a
/// header, in no order.
///
/// ```text
/// | next | top | records … | room |
/// ```
///
/// `next` is the chain's next page, the one before this one; null in the
/// oldest. `top` is where the next record goes.
pub(in crate::tree) struct HeapPage<C> {
    page: NonNull<u8>,
    _config: PhantomData<fn() -> C>,
}

// By hand: derives would bound `C`, which the marker doesn't hold.

impl<C> Clone for HeapPage<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for HeapPage<C> {}

// SAFETY: a heap page is used with this layout only.
unsafe impl<C: NodeConfig> PageType for HeapPage<C> {
    const KIND: PageKind = PageKind::Heap;

    fn page_layout() -> Layout {
        Layout::from_size_align(Self::SIZE, Self::SIZE).expect("a heap page's layout is valid")
    }
}

impl<C: NodeConfig> HeapPage<C> {
    /// A heap page's size and alignment, the node config's: a power of two,
    /// at least 1 KiB.
    const SIZE: usize = {
        let size = C::HEAP_PAGE_SIZE;
        assert!(
            size.is_power_of_two() && size >= 1024,
            "a NodeConfig's HEAP_PAGE_SIZE is a power of two, at least 1024"
        );
        size
    };

    /// Where `top` is, a `u32`, after `next`.
    const TOP_OFFSET: usize = size_of::<*mut u8>();

    /// Where the records start.
    const RECORDS_OFFSET: usize = (Self::TOP_OFFSET + size_of::<u32>()).next_multiple_of(8);

    /// The largest record a heap page holds.
    const MAX_RECORD_SIZE: usize = Self::SIZE - Self::RECORDS_OFFSET;

    /// The heap page at `page`, if it is not null.
    fn at(page: *mut u8) -> Option<Self> {
        NonNull::new(page).map(|page| Self {
            page,
            _config: PhantomData,
        })
    }

    /// The heap page that holds the record at `record`: heap pages are
    /// aligned to their size.
    fn of_record(record: RecordPtr) -> Self {
        let page = (record.record().as_ptr()).map_addr(|addr| addr & !(Self::SIZE - 1));
        Self::at(page).expect("a record lies in a heap page")
    }

    /// A new, empty heap page from `alloc`, in front of `next`; or, if
    /// `alloc` has none to give, the layout it was asked for.
    fn try_allocate(alloc: &impl PageAllocator, next: *mut u8) -> Result<Self, Layout> {
        let layout = Self::page_layout();
        let page = (alloc.try_allocate_page(Self::KIND, layout)).map_err(|AllocError| layout)?;
        Ok(Self::init(page, next))
    }

    /// [`Self::try_allocate`], ending the program as std's collections do if
    /// `alloc` has no page to give.
    fn allocate(alloc: &impl PageAllocator, next: *mut u8) -> Self {
        Self::init(alloc.allocate_page(Self::KIND, Self::page_layout()), next)
    }

    /// The heap page at `page`, new and empty, in front of `next`.
    fn init(page: NonNull<u8>, next: *mut u8) -> Self {
        let heap_page = Self {
            page,
            _config: PhantomData,
        };
        heap_page.set_next(next);
        heap_page.set_top(Self::RECORDS_OFFSET);
        heap_page
    }

    /// Gives the page back to `alloc`, which it came from.
    ///
    /// # Safety
    ///
    /// The page came from `alloc` ([`Self::allocate`]), and nothing reads it
    /// again.
    unsafe fn deallocate(self, alloc: &impl PageAllocator) {
        // SAFETY: as the caller promises, for a heap page's kind and layout.
        unsafe { alloc.deallocate_page(Self::KIND, self.page, Self::page_layout()) };
    }

    /// The chain's next page, the one before this one; null in the oldest.
    fn next(self) -> *mut u8 {
        // SAFETY: `next` is an aligned pointer at the page's start, which
        // `allocate` wrote.
        unsafe { self.page.cast::<*mut u8>().read() }
    }

    fn set_next(self, next: *mut u8) {
        // SAFETY: as in `next`; the caller's leaf has the page alone.
        unsafe { self.page.cast::<*mut u8>().write(next) };
    }

    /// Where the next record goes.
    fn top(self) -> usize {
        // SAFETY: `top` is a `u32` in the page, which `allocate` wrote.
        unsafe {
            self.page
                .byte_add(Self::TOP_OFFSET)
                .cast::<u32>()
                .read_unaligned() as usize
        }
    }

    fn set_top(self, top: usize) {
        debug_assert!((Self::RECORDS_OFFSET..=Self::SIZE).contains(&top));
        let top = u32::try_from(top).expect("a heap page's offsets fit in a u32");
        // SAFETY: as in `top`; the caller's leaf has the page alone.
        unsafe {
            self.page
                .byte_add(Self::TOP_OFFSET)
                .cast::<u32>()
                .write_unaligned(top);
        }
    }

    /// The bytes free after the last record.
    fn room(self) -> usize {
        Self::SIZE - self.top()
    }

    /// Whether the page holds no record.
    fn is_empty(self) -> bool {
        self.top() == Self::RECORDS_OFFSET
    }

    /// Whether the record at `record` lies in this page.
    fn holds(self, record: RecordPtr) -> bool {
        Self::of_record(record).page == self.page
    }

    /// Writes `record`'s, a store's, after the last record, where it fits,
    /// and returns a pointer to it: from another page, or from the caller's
    /// key and value.
    ///
    /// # Safety
    ///
    /// The caller's leaf has the page alone, and `record`'s bytes lie apart
    /// from the page's room.
    unsafe fn append(self, record: RecordPtr) -> RecordPtr {
        let (top, size) = (self.top(), record.size());
        debug_assert!(size <= self.room(), "a record goes where it fits");
        // SAFETY: `size` bytes from `top` lie in the page, past every record
        // in it (caller), apart from the record's own bytes. They are a
        // record once written, which the page keeps.
        unsafe {
            let to = self.page.add(top);
            record.write_record(to);
            self.set_top(top + size);
            RecordPtr::new(to)
        }
    }
}

/// A store leaf's records and their heap chain. Each method is called only
/// for a store's leaves, whose keys are record pointers ([`InStore`]).
impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// The leaf's heap chain, in its header.
    fn heap_chain<'a>(self) -> &'a mut HeapChain {
        debug_assert!(NodePtr::<K, V, S>::is_in_store());
        // SAFETY: a store's leaf keeps its heap chain right after its node
        // header (`layout.rs`, `Header`), initialized with the leaf
        // (`NodePtr::initialize`). Only an operation that has the leaf
        // alone reaches it, for as long as it has it.
        unsafe {
            self.node_ptr()
                .page()
                .byte_add(NodePtr::<K, V, S>::heap_chain_offset())
                .cast::<HeapChain>()
                .as_mut()
        }
    }

    /// The record pointer in key slot `slot`: a store's keys are record
    /// pointers ([`InStore`] takes no other).
    fn record_slot(slot: NonNull<K>) -> NonNull<RecordPtr> {
        debug_assert_eq!(size_of::<K>(), size_of::<RecordPtr>());
        slot.cast()
    }

    /// `key` as the record pointer it is in a store.
    fn record_of(key: &K) -> RecordPtr {
        // SAFETY: a store's keys are record pointers (`record_slot`).
        unsafe { Self::record_slot(NonNull::from(key)).read() }
    }

    /// The slots of the leaf's record pointers: its head's key, if it has a
    /// head, then its keys.
    fn record_slots(self) -> impl Iterator<Item = NonNull<RecordPtr>> {
        let head = self.has_head().then(|| self.head_key_slot());
        // SAFETY: keys `0..len` are initialized.
        let keys = (0..self.len()).map(move |index| unsafe { self.key_slot(index) });
        head.into_iter().chain(keys).map(Self::record_slot)
    }

    /// The slot of entry `index`'s record pointer, or of the head's for
    /// `None`.
    fn entry_record_slot(self, index: Option<usize>) -> NonNull<RecordPtr> {
        Self::record_slot(match index {
            None => self.head_key_slot(),
            Some(index) => {
                debug_assert!(index < self.len());
                // SAFETY: `index < len <= capacity`.
                unsafe { self.key_slot(index) }
            }
        })
    }

    /// The newest heap page, if the leaf has one.
    fn newest_heap_page(self) -> Option<HeapPage<S::Config>> {
        HeapPage::at(self.heap_chain().newest)
    }

    /// The leaf's heap pages, newest first.
    fn heap_pages(self) -> impl Iterator<Item = HeapPage<S::Config>> {
        core::iter::successors(self.newest_heap_page(), |page| HeapPage::at(page.next()))
    }

    /// Whether `record` fits in the leaf's newest heap page.
    fn heap_has_room_for_record(self, record: RecordPtr) -> bool {
        self.newest_heap_page()
            .is_some_and(|page| record.size() <= page.room())
    }

    /// Whether the record `key` points to fits in the leaf's newest heap
    /// page.
    pub(in crate::tree) fn heap_has_room_for(self, key: &K) -> bool {
        self.heap_has_room_for_record(Self::record_of(key))
    }

    /// Counts `size` more bytes of the leaf's heap as garbage.
    fn add_garbage(self, size: usize) {
        let chain = self.heap_chain();
        chain.garbage += u32::try_from(size).expect("a record's size fits in a u32");
    }

    /// Puts `page` in front of the leaf's chain.
    fn push_heap_page(self, page: HeapPage<S::Config>) {
        let chain = self.heap_chain();
        page.set_next(chain.newest);
        chain.newest = page.page.as_ptr();
        chain.pages += 1;
    }

    /// Puts a new, empty heap page from `alloc` in front of the chain; or,
    /// if `alloc` has none to give, the layout it was asked for.
    pub(in crate::tree) fn grow_heap(self, alloc: &impl PageAllocator) -> Result<(), Layout> {
        let page = HeapPage::try_allocate(alloc, ptr::null_mut())?;
        self.push_heap_page(page);
        Ok(())
    }

    /// Puts `count` new, empty heap pages from `alloc` in front of the
    /// chain, for a split to hand to its new leaf ([`Self::take_spare_heap_page`]);
    /// or, if `alloc` has not all to give, none, and the layout it was asked
    /// for.
    pub(in crate::tree) fn grow_heap_by(
        self,
        count: usize,
        alloc: &impl PageAllocator,
    ) -> Result<(), Layout> {
        for grown in 0..count {
            if let Err(layout) = self.grow_heap(alloc) {
                for _ in 0..grown {
                    let page = self.take_spare_heap_page(alloc);
                    // SAFETY: the page came from `alloc` just above, empty.
                    unsafe { page.deallocate(alloc) };
                }
                return Err(layout);
            }
        }
        Ok(())
    }

    /// The empty page in front of the leaf's chain, taken off it, as a split
    /// gives its new leaf; a new one from `alloc` if the chain has none in
    /// front.
    ///
    /// # Panics
    ///
    /// Through `handle_alloc_error`, if `alloc` has none to give.
    fn take_spare_heap_page(self, alloc: &impl PageAllocator) -> HeapPage<S::Config> {
        let chain = self.heap_chain();
        match HeapPage::<S::Config>::at(chain.newest).filter(|page| page.is_empty()) {
            Some(page) => {
                chain.newest = page.next();
                chain.pages -= 1;
                page
            }
            None => HeapPage::allocate(alloc, ptr::null_mut()),
        }
    }

    /// Gives back the empty pages in front of the leaf's chain, which a
    /// split that didn't happen took ([`Self::grow_heap_by`]).
    pub(in crate::tree) fn free_spare_heap_pages(self, alloc: &impl PageAllocator) {
        while self.newest_heap_page().is_some_and(HeapPage::is_empty) {
            let page = self.take_spare_heap_page(alloc);
            // SAFETY: the page came from `alloc`, and holds no record.
            unsafe { page.deallocate(alloc) };
        }
    }

    /// Makes room for `key`'s record in the leaf's newest heap page:
    /// compacts the chain if its garbage would hold the record, and chains
    /// a new page from `alloc` if that doesn't make room; or, if `alloc` has
    /// none to give, returns the layout it was asked for, the leaf's entries
    /// as they were.
    pub(super) fn make_heap_room(self, key: &K, alloc: &impl PageAllocator) -> Result<(), Layout> {
        if self.heap_has_room_for(key) {
            return Ok(());
        }
        let size = Self::record_of(key).size();
        if self.heap_chain().garbage as usize >= size {
            self.compact_heap_keeping(size, alloc);
            if self.heap_has_room_for(key) {
                return Ok(());
            }
        }
        self.grow_heap(alloc)
    }

    /// Makes room for `key`'s record in `half`, this leaf or the one a split
    /// gave its right half: in front of `half`'s chain, the empty page in
    /// front of this leaf's, or a new one from `alloc`.
    pub(super) fn give_heap_room(self, half: Self, key: &K, alloc: &impl PageAllocator) {
        if !half.heap_has_room_for(key) {
            half.push_heap_page(self.take_spare_heap_page(alloc));
        }
    }

    /// `key`, an insert's, pointing to the caller's key and value, with its
    /// record written into the leaf's newest heap page, which has room for
    /// it ([`Self::make_heap_room`]): what the leaf keeps for a new entry.
    pub(super) fn take_record_in(self, key: WindowedKey<K, S>) -> WindowedKey<K, S> {
        let page = self
            .newest_heap_page()
            .expect("an entry comes in once its leaf's heap has room for its record");
        let (mut key, window) = key.into_parts();
        // SAFETY: the leaf has the page alone; the caller's key and value lie
        // outside it. `key` is a record pointer (`record_slot`), which the
        // written record replaces, and needs no drop.
        unsafe {
            let record = page.append(Self::record_of(&key));
            Self::record_slot(NonNull::from(&mut key)).write(record);
        }
        WindowedKey::from_parts(key, window)
    }

    /// Gives entry `index`, the head for `None`, the value of `key`, an
    /// insert's, pointing to the caller's key, which is the entry's, and
    /// value: in place if it is no longer, the bytes it leaves counted as
    /// garbage; otherwise as a new record in the newest heap page, made
    /// room for as [`Self::make_heap_room`] does, the old one garbage. If
    /// `alloc` has no heap page to give, returns the layout it was asked
    /// for, the entry as it was.
    pub(super) fn replace_record_in_heap(
        self,
        index: Option<usize>,
        key: &K,
        alloc: &impl PageAllocator,
    ) -> Result<(), Layout> {
        let new = Self::record_of(key);
        // SAFETY: the slot holds the entry's record pointer.
        let old = unsafe { self.entry_record_slot(index).read() };
        let (old_size, new_size) = (old.size(), new.size());
        if new_size <= old_size {
            // SAFETY: the old record has room for the new value, as their
            // keys are equal; the leaf has its page alone, and the caller's
            // value lies outside it.
            unsafe { old.write_value(new.value()) };
            self.add_garbage(old_size - new_size);
            return Ok(());
        }
        self.make_heap_room(key, alloc)?;
        // The room may have moved the old record: read its slot again.
        let slot = self.entry_record_slot(index);
        let page = self.newest_heap_page().expect("a page with room");
        // SAFETY: the leaf has the page alone; the caller's key and value lie
        // outside it. The slot takes the written record.
        unsafe { slot.write(page.append(new)) };
        self.add_garbage(old_size);
        Ok(())
    }

    /// Counts the record of `key`, an entry the leaf no longer holds, as
    /// garbage.
    pub(super) fn discard_record(self, key: &K) {
        self.add_garbage(Self::record_of(key).size());
    }

    /// Copies the records of `right`'s entries, which the leaf just gave it
    /// in a split, into `right`'s chain, onto the empty pages in front of
    /// the leaf's chain, or new ones from `alloc`. Their old copies are the
    /// leaf's garbage, until the split compacts it.
    pub(super) fn move_records_to(self, right: Self, alloc: &impl PageAllocator) {
        for slot in right.record_slots() {
            // SAFETY: the slot holds a record pointer of one of `right`'s
            // entries, into this leaf's chain.
            let record = unsafe { slot.read() };
            if !right.heap_has_room_for_record(record) {
                right.push_heap_page(self.take_spare_heap_page(alloc));
            }
            let page = right.newest_heap_page().expect("a page with room");
            // SAFETY: `right` has the page alone, and the record lies in this
            // leaf's chain. The slot takes the copy.
            unsafe { slot.write(page.append(record)) };
            self.add_garbage(record.size());
        }
    }

    /// Splices `right`'s chain onto the leaf's, as their entries merge into
    /// the leaf, then compacts it, giving back to `alloc` the pages it
    /// empties. `right` keeps no heap page.
    pub(super) fn take_heap_of(self, right: Self, alloc: &impl PageAllocator) {
        if let Some(oldest) = right.heap_pages().last() {
            let taken = core::mem::replace(right.heap_chain(), HeapChain::EMPTY);
            let chain = self.heap_chain();
            oldest.set_next(chain.newest);
            chain.newest = taken.newest;
            chain.pages += taken.pages;
            chain.garbage += taken.garbage;
        }
        self.compact_heap(alloc);
    }

    /// Slides the live records together, in place, page by page from the
    /// oldest, and gives back to `alloc` the pages it empties. Leaves no
    /// garbage.
    pub(super) fn compact_heap(self, alloc: &impl PageAllocator) {
        self.compact_heap_keeping(0, alloc);
    }

    /// [`Self::compact_heap`], keeping one emptied page, empty, in front of
    /// the chain if the last record leaves less than `room` bytes after it:
    /// room for a record about to go in, which the garbage made.
    ///
    /// The records go in the order they lie, by page from the oldest, then
    /// by offset, each to the end of the one before, or to the start of the
    /// next page if it doesn't fit there. Nothing goes past where it lies,
    /// so each move lands on bytes already moved or garbage.
    fn compact_heap_keeping(self, room: usize, alloc: &impl PageAllocator) {
        let mut pages: Vec<HeapPage<S::Config>> = self.heap_pages().collect();
        pages.reverse();
        let mut live: Vec<(usize, usize, NonNull<RecordPtr>)> = self
            .record_slots()
            .map(|slot| {
                // SAFETY: the slot holds a record pointer of the leaf's.
                let record = unsafe { slot.read() };
                let rank = (pages.iter())
                    .position(|page| page.holds(record))
                    .expect("a leaf's records lie in its own heap pages");
                (rank, record.record().addr().get(), slot)
            })
            .collect();
        live.sort_unstable_by_key(|&(rank, addr, _)| (rank, addr));
        let (mut kept, mut top) = (0, HeapPage::<S::Config>::RECORDS_OFFSET);
        for &(_, _, slot) in &live {
            // SAFETY: as above.
            let record = unsafe { slot.read() };
            let size = record.size();
            if top + size > HeapPage::<S::Config>::SIZE {
                pages[kept].set_top(top);
                kept += 1;
                top = HeapPage::<S::Config>::RECORDS_OFFSET;
            }
            // SAFETY: `size` bytes from `top` lie in page `kept`, at or before
            // where the record lies (the order above), over bytes moved
            // already or garbage; `copy` allows the overlap. The slot takes
            // the moved record, which the leaf's page keeps.
            unsafe {
                let to = pages[kept].page.add(top);
                ptr::copy(record.record().as_ptr(), to.as_ptr(), size);
                slot.write(RecordPtr::new(to));
            }
            top += size;
        }
        let chain = self.heap_chain();
        let freed = if live.is_empty() {
            *chain = HeapChain::EMPTY;
            &pages[..]
        } else {
            pages[kept].set_top(top);
            if HeapPage::<S::Config>::SIZE - top < room && kept + 1 < pages.len() {
                kept += 1;
                pages[kept].set_top(HeapPage::<S::Config>::RECORDS_OFFSET);
            }
            chain.newest = pages[kept].page.as_ptr();
            chain.pages = u32::try_from(kept + 1).expect("a chain's pages fit in a u32");
            chain.garbage = 0;
            &pages[kept + 1..]
        };
        for page in freed {
            // SAFETY: the page came from `alloc`, and no record of the leaf's
            // lies in it any more.
            unsafe { page.deallocate(alloc) };
        }
    }

    /// Gives every heap page of the leaf back to `alloc`, as the leaf goes
    /// with its entries.
    pub(in crate::tree) fn free_heap(self, alloc: &impl PageAllocator) {
        let pages: Vec<HeapPage<S::Config>> = self.heap_pages().collect();
        for page in pages {
            // SAFETY: as in `compact_heap`; nothing reads the leaf's records
            // again.
            unsafe { page.deallocate(alloc) };
        }
        *self.heap_chain() = HeapChain::EMPTY;
    }

    /// How many heap pages a split of the full leaf around key
    /// `separator_index` takes: those the records of the entries that move
    /// fill, copied one after another, and one more for the entry the split
    /// inserts.
    pub(in crate::tree) fn heap_pages_for_split(self, separator_index: usize) -> usize {
        let mut pages = 1;
        let mut room = 0;
        for index in separator_index..self.len() {
            let size = Self::record_of(self.key(index)).size();
            if size > room {
                pages += 1;
                room = HeapPage::<S::Config>::MAX_RECORD_SIZE;
            }
            room -= size;
        }
        pages
    }

    /// Panics unless the leaf's records lie in its own heap pages, apart,
    /// and the chain counts its pages and garbage right: what the heap pages
    /// hold, less the live records, is garbage.
    #[cfg(any(test, fuzzing))]
    pub(in crate::tree) fn check_heap(self) {
        let pages: Vec<HeapPage<S::Config>> = self.heap_pages().collect();
        let chain = self.heap_chain();
        assert_eq!(
            chain.pages as usize,
            pages.len(),
            "a chain counts its heap pages"
        );
        let mut live: Vec<(usize, usize)> = Vec::new();
        for slot in self.record_slots() {
            // SAFETY: the slot holds one of the leaf's record pointers.
            let record = unsafe { slot.read() };
            assert!(
                pages.iter().any(|page| page.holds(record)),
                "a leaf's records lie in its own heap pages"
            );
            live.push((record.record().addr().get(), record.size()));
        }
        live.sort_unstable();
        for pair in live.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0, "records lie apart");
        }
        let used: usize = (pages.iter())
            .map(|page| page.top() - HeapPage::<S::Config>::RECORDS_OFFSET)
            .sum();
        let live: usize = live.iter().map(|&(_, size)| size).sum();
        assert_eq!(
            used - live,
            chain.garbage as usize,
            "a chain counts its garbage"
        );
    }
}
