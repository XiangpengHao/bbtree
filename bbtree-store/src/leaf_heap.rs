//! [`LeafHeap`]: each leaf's annex in a store's map (bbtree's [`LeafAnnex`]):
//! the chain of heap pages its records go into (`heap_page.rs`), and its
//! generation. [`RecordHeap`] runs what a store does to a leaf's records,
//! while the map holds the leaf exclusively.
//!
//! - **A record** goes after the last one in the leaf's newest page, the
//!   chain's head; a page goes in front when the head lacks room.
//! - **A split** moves no record: the new leaf starts with no page, and its
//!   records lie in the old leaf's chain. Both leaves take the next
//!   generation.
//! - **A merge** splices the leaving leaf's chain in behind the head.
//! - **A record that dies** leaves a hole. Once a page of the leaf's own
//!   chain is down to half its bytes live, and the chain too, or a record
//!   finds no room in a chain a quarter holes, the leaf compacts, in place:
//!   in
//!   each page where only its own records live, it slides them together;
//!   it frees the pages where none of its records live, or orphans them
//!   while other leaves' do. A page where other leaves' records live keeps
//!   its holes until they go. Compacting takes no page, so a store whose
//!   allocator refuses pages still takes its holes back.
//! - **A record two generations behind its leaf** that lies in another
//!   leaf's chain moves into its own leaf's head, once a generation: the
//!   leaf's key range split twice since it was written, so its page holds
//!   the records of leaves further and further off. One generation behind,
//!   it shares a page with a neighbour's.
//!
//! Only a thread holding a leaf exclusively moves, changes or kills its
//! records, and reads or changes its chain: a record's own leaf, that is,
//! the leaf of the key that points to it, which may lie in another leaf's
//! chain. Pages hold a count of their live bytes for that (`heap_page.rs`).

use core::marker::PhantomData;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};

use bbtree::{
    AllocError, Allocator, AnnexedLeaf, Configured, LeafAnnex, NodeConfig, WithInlineKey,
    WithLeafAnnex,
};

use crate::heap_page::HeapPage;
use crate::record::{Record, StoreKey};

/// The strategy of a store's map: byte strings' inline keys, the node
/// config `C`, and a [`LeafHeap`] in each leaf.
pub(crate) type StoreStrategy<C> = WithLeafAnnex<Configured<WithInlineKey, C>, LeafHeap<C>>;

/// A leaf of a store's map, held exclusively.
pub(crate) type StoreLeaf<'a, C> = AnnexedLeaf<'a, StoreKey, (), StoreStrategy<C>>;

/// A leaf's chain of heap pages, and its generation.
pub(crate) struct LeafHeap<C> {
    /// The chain's newest page, where records go; `None` without one.
    head: Option<HeapPage<C>>,
    /// How many times the leaf split, with the leaves it split from,
    /// wrapping.
    generation: u8,
    /// The generation the leaf last pulled its records in at.
    pulled: u8,
}

// SAFETY: the pages are the store's allocator's; only the thread holding
// the leaf reaches them through the annex.
unsafe impl<C> Send for LeafHeap<C> {}

// SAFETY: as for `Send`.
unsafe impl<C> Sync for LeafHeap<C> {}

impl<C> Default for LeafHeap<C> {
    fn default() -> Self {
        Self {
            head: None,
            generation: 0,
            pulled: 0,
        }
    }
}

impl<C: NodeConfig> LeafAnnex for LeafHeap<C> {
    fn split(&mut self, right: &mut Self) {
        self.generation = self.generation.wrapping_add(1);
        right.generation = self.generation;
        right.pulled = self.pulled;
    }

    fn merge(&mut self, right: Self) {
        let Some(right_head) = right.head else {
            return;
        };
        let Some(head) = self.head else {
            self.head = Some(right_head);
            return;
        };
        let mut right_tail = right_head;
        while let Some(next) = right_tail.next() {
            right_tail = next;
        }
        right_tail.set_next(head.next());
        head.set_next(Some(right_head));
    }

    fn release(self, alloc: &impl Allocator) {
        let mut page = self.head;
        while let Some(this) = page {
            page = this.next();
            // SAFETY: the map goes: no thread reads a record again, and the
            // page is in this chain alone.
            unsafe { this.deallocate(alloc) };
        }
    }
}

impl<C: NodeConfig> LeafHeap<C> {
    /// `size` bytes of room, live, in the chain's head page; else in a page
    /// of the chain that has them, which becomes the head.
    fn room(&mut self, size: usize) -> Option<NonNull<u8>> {
        let (mut before, mut page) = (None::<HeapPage<C>>, self.head);
        while let Some(this) = page {
            if this.room() >= size {
                if let Some(before) = before {
                    before.set_next(this.next());
                    this.set_next(self.head);
                    self.head = Some(this);
                }
                return Some(this.append(size));
            }
            (before, page) = (Some(this), this.next());
        }
        None
    }

    /// `size` bytes of room, live, in a new head page from `alloc`.
    fn room_in_new_page(
        &mut self,
        size: usize,
        alloc: &impl Allocator,
    ) -> Result<NonNull<u8>, AllocError> {
        let page = HeapPage::allocate(alloc)?;
        page.set_next(self.head);
        self.head = Some(page);
        Ok(page.append(size))
    }

    /// How much of the chain records took, and how much of that is live.
    fn chain_use(&self) -> ChainUse {
        let mut chain_use = ChainUse { used: 0, live: 0 };
        let mut page = self.head;
        while let Some(this) = page {
            chain_use.used += this.used();
            chain_use.live += this.live();
            page = this.next();
        }
        chain_use
    }

    /// Whether the chain holds `page`.
    fn holds(&self, page: HeapPage<C>) -> bool {
        let mut chain = self.head;
        while let Some(this) = chain {
            if this == page {
                return true;
            }
            chain = this.next();
        }
        false
    }
}

/// How much of a chain's pages records took, and how much of that is live.
struct ChainUse {
    used: usize,
    live: usize,
}

impl ChainUse {
    /// Whether at least half of what records took is holes.
    fn is_half_holes(&self) -> bool {
        self.used > 0 && self.live * 2 <= self.used
    }

    /// Whether at least a quarter of what records took is holes: worth a
    /// compaction, which moves at most three live bytes a byte of holes, to
    /// save a page.
    fn is_quarter_holes(&self) -> bool {
        self.used > 0 && self.live * 4 <= self.used * 3
    }
}

/// Where a store's records go: its leaves' heaps, from its allocator.
pub(crate) struct RecordHeap<A, C> {
    alloc: A,
    /// How many orphans hold live records (`heap_page.rs`).
    orphans: AtomicUsize,
    _config: PhantomData<fn() -> C>,
}

impl<A: Allocator, C: NodeConfig> RecordHeap<A, C> {
    pub(crate) fn new(alloc: A) -> Self {
        Self {
            alloc,
            orphans: AtomicUsize::new(0),
            _config: PhantomData,
        }
    }

    /// Whether some orphan holds a live record, which only killing it frees.
    pub(crate) fn has_orphans(&self) -> bool {
        self.orphans.load(Ordering::Acquire) > 0
    }

    /// Writes the record of `key` and `value` into `leaf`'s heap, for a key
    /// of `leaf` to point to; or [`AllocError`] if a page it needs is not to
    /// be had. The leaf's records may move first, for room, or to their own
    /// leaf (module documentation).
    pub(crate) fn write(
        &self,
        leaf: &mut StoreLeaf<'_, C>,
        key: &[u8],
        value: &[u8],
    ) -> Result<Record, AllocError> {
        self.pull_stale(leaf);
        let at = self.room_for(leaf, Record::size_of(key.len(), value.len()))?;
        let generation = leaf.annex().generation;
        // SAFETY: the room is the record's size, in a page of the leaf's,
        // apart from `key` and `value`, which the caller has.
        Ok(unsafe { Record::write(at, key, value, generation) })
    }

    /// Gives `stored`, a key of `leaf`, whose bytes are `key`, `value` in
    /// place of its own: in its record if the new value is no longer, else
    /// in a new record ([`Self::write`]).
    pub(crate) fn overwrite(
        &self,
        leaf: &mut StoreLeaf<'_, C>,
        stored: &StoreKey,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), AllocError> {
        let record = stored.record();
        if value.len() <= record.value().len() {
            let size = record.size();
            // SAFETY: the map holds the record's leaf exclusively.
            unsafe { record.overwrite_value(value) };
            self.kill_in(HeapPage::of_record(record.addr()), size - record.size());
            return Ok(());
        }
        let new = self.write(leaf, key, value)?;
        // SAFETY: the new record has the key's bytes, and the map holds its
        // leaf exclusively.
        let old = unsafe { stored.replace(new) };
        self.kill(leaf, old);
        Ok(())
    }

    /// `removed`, a key of `leaf`, is about to go. If its record lies in
    /// the leaf's chain, and the page, and the chain, are half holes once it
    /// dies, the leaf compacts, the record left as it is: the map reads the
    /// key until the removal returns, and the record dies only then
    /// ([`Self::kill_removed`]). Its page counts it live meanwhile, so no
    /// compaction slides over it or frees it.
    pub(crate) fn before_removal(&self, leaf: &mut StoreLeaf<'_, C>, removed: &StoreKey) {
        let record = removed.record();
        let (page, size) = (HeapPage::<C>::of_record(record.addr()), record.size());
        let annex = leaf.annex();
        if !annex.holds(page) || (page.live() - size) * 2 > page.used() {
            return;
        }
        let chain_use = annex.chain_use();
        let after = ChainUse {
            used: chain_use.used,
            live: chain_use.live - size,
        };
        if after.is_half_holes() {
            self.compact(leaf, Some(removed));
        }
    }

    /// The record of a key whose removal returned dies: no key points to it,
    /// and no thread reads it. Its page may be any leaf's then, or an orphan.
    pub(crate) fn kill_removed(&self, record: Record) {
        self.kill_in(HeapPage::of_record(record.addr()), record.size());
    }

    /// `record`, of a key of `leaf` that points elsewhere now, dies. If it
    /// lay in the leaf's own chain, the leaf compacts once half the page,
    /// and half the chain, are holes.
    fn kill(&self, leaf: &mut StoreLeaf<'_, C>, record: Record) {
        let page = HeapPage::<C>::of_record(record.addr());
        // Before the kill: once it is done, another leaf's page may go.
        let own = leaf.annex().holds(page);
        if self.kill_in(page, record.size()) || !own {
            return;
        }
        if page.live() * 2 <= page.used() && leaf.annex().chain_use().is_half_holes() {
            self.compact(leaf, None);
        }
    }

    /// `size` bytes of `page`'s records died; whether it was an orphan they
    /// emptied, which goes back to the allocator then.
    fn kill_in(&self, page: HeapPage<C>, size: usize) -> bool {
        if !page.kill(size) {
            return false;
        }
        self.orphans.fetch_sub(1, Ordering::AcqRel);
        // SAFETY: an orphan that a kill emptied is that kill's alone.
        unsafe { page.deallocate(&self.alloc) };
        true
    }

    /// `size` bytes of room in `leaf`'s chain, live: in its head page, or
    /// another page with room, which becomes the head; else, once a
    /// compaction slid the records together if a quarter of the chain is
    /// holes, there; else in a new head page; else, if the allocator has
    /// none to give, once a compaction took back whatever holes there are.
    fn room_for(
        &self,
        leaf: &mut StoreLeaf<'_, C>,
        size: usize,
    ) -> Result<NonNull<u8>, AllocError> {
        if let Some(at) = leaf.annex().room(size) {
            return Ok(at);
        }
        if leaf.annex().chain_use().is_quarter_holes() {
            self.compact(leaf, None);
            if let Some(at) = leaf.annex().room(size) {
                return Ok(at);
            }
        }
        match leaf.annex().room_in_new_page(size, &self.alloc) {
            Ok(at) => Ok(at),
            Err(error) => {
                let chain_use = leaf.annex().chain_use();
                if chain_use.live < chain_use.used {
                    self.compact(leaf, None);
                }
                leaf.annex().room(size).ok_or(error)
            }
        }
    }

    /// Slides the records of `leaf`'s keys, but `gone`'s, which stays where
    /// it is, together in the pages of its chain where no other leaf's
    /// record lives, in place, as one run from the first such page on;
    /// frees those pages it empties, and the pages where none of its
    /// records live, or orphans them while other leaves' do
    /// (`heap_page.rs`). The page the run ends in becomes the head. Then the leaf's records in other leaves' pages move in,
    /// where there is room, or pages to be had: those pages' leaves can
    /// slide them then, and orphans empty.
    ///
    /// A page's live bytes that its leaf's records don't account for are
    /// other leaves': a page whose live bytes they all account for has no
    /// other live record, and gets none, as only its leaf writes it.
    fn compact(&self, leaf: &mut StoreLeaf<'_, C>, gone: Option<&StoreKey>) {
        let mut chain = Vec::new();
        let mut page = leaf.annex().head;
        while let Some(this) = page {
            chain.push(this);
            page = this.next();
        }
        // The leaf's records in its chain, by address: each page's together.
        let mut own: Vec<(Record, &StoreKey)> = (leaf.keys())
            .filter(|&key| gone.is_none_or(|gone| !ptr::eq(key, gone)))
            .map(|key| (key.record(), key))
            .filter(|&(record, _)| chain.contains(&HeapPage::of_record(record.addr())))
            .collect();
        own.sort_unstable_by_key(|&(record, _)| record.addr());
        let records_in = |page: HeapPage<C>| {
            let first = own.partition_point(|&(record, _)| record.addr() < page.records_start());
            let count = (own[first..].iter())
                .take_while(|&&(record, _)| page.holds(record.addr()))
                .count();
            &own[first..first + count]
        };
        // The pages the run slides over, and those others' records keep.
        let (mut run, mut kept) = (Vec::new(), Vec::new());
        for &this in &chain {
            let own_bytes: usize = records_in(this)
                .iter()
                .map(|&(record, _)| record.size())
                .sum();
            if own_bytes == 0 {
                self.let_go(this);
            } else if this.live() == own_bytes {
                run.push(this);
            } else {
                kept.push(this);
            }
        }
        // The writer, a page of the run and where in it, never passes the
        // record it moves next: a record that doesn't fit after the writer
        // in its page lies in a later page.
        let (mut to_page, mut used) = (0, 0);
        for &from_page in &run {
            for &(record, key) in records_in(from_page) {
                let size = record.size();
                if used + size > HeapPage::<C>::ROOM {
                    run[to_page].set_used(used);
                    (to_page, used) = (to_page + 1, 0);
                }
                let to = run[to_page];
                // SAFETY: the record, and its new place, which only records
                // moved already held, lie in pages where only the leaf's
                // records live; the map holds the leaf exclusively, so no
                // thread reads them.
                unsafe {
                    let at = to.records_start().add(used);
                    ptr::copy(record.addr().as_ptr(), at.as_ptr(), size);
                    key.replace(Record::at(at));
                }
                from_page.move_live(to, size);
                used += size;
            }
        }
        let annex = leaf.annex();
        annex.head = None;
        for &this in kept.iter().rev() {
            this.set_next(annex.head);
            annex.head = Some(this);
        }
        if let Some(&last) = run.get(to_page) {
            last.set_used(used);
            for &empty in &run[to_page + 1..] {
                // SAFETY: every record of the page moved out, and no chain
                // holds it any more.
                unsafe { empty.deallocate(&self.alloc) };
            }
            for &this in &run[..=to_page] {
                this.set_next(annex.head);
                annex.head = Some(this);
            }
        }
        for key in leaf.keys() {
            let record = key.record();
            let elsewhere = !chain.contains(&HeapPage::of_record(record.addr()));
            if elsewhere && gone.is_none_or(|gone| !ptr::eq(key, gone)) && !self.pull(leaf, key) {
                return;
            }
        }
    }

    /// Lets `page` go from its chain: frees it if no record lives in it,
    /// else orphans it.
    fn let_go(&self, page: HeapPage<C>) {
        if page.live() > 0 {
            self.orphans.fetch_add(1, Ordering::AcqRel);
            if !page.orphan() {
                return;
            }
            // Another leaf killed its last record meanwhile.
            self.orphans.fetch_sub(1, Ordering::AcqRel);
        }
        // SAFETY: no record lives in the page, and no chain holds it.
        unsafe { page.deallocate(&self.alloc) };
    }

    /// Moves the records of `leaf`'s keys that lie in another leaf's chain
    /// and are two generations or more behind the leaf into its head, once
    /// a generation. If a page is not to be had, stops, to try again.
    fn pull_stale(&self, leaf: &mut StoreLeaf<'_, C>) {
        let annex = leaf.annex();
        let generation = annex.generation;
        if annex.pulled == generation {
            return;
        }
        for key in leaf.keys() {
            // The chain first, from the record's address: only a record
            // elsewhere is read.
            let record = key.record();
            if leaf.annex().holds(HeapPage::of_record(record.addr()))
                || generation.wrapping_sub(record.generation()) < 2
            {
                continue;
            }
            if !self.pull(leaf, key) {
                return;
            }
        }
        leaf.annex().pulled = generation;
    }

    /// Moves the record of `key`, of `leaf`, which lies in another leaf's
    /// page, into a page of `leaf`'s chain with room, or a new one; `false`
    /// if no page is to be had.
    fn pull(&self, leaf: &mut StoreLeaf<'_, C>, key: &StoreKey) -> bool {
        let record = key.record();
        let size = record.size();
        let annex = leaf.annex();
        let at = match annex.room(size) {
            Some(at) => at,
            None => match annex.room_in_new_page(size, &self.alloc) {
                Ok(at) => at,
                Err(AllocError) => return false,
            },
        };
        let generation = annex.generation;
        // SAFETY: the room is the record's size, in a page of the leaf's;
        // the map holds the key's leaf exclusively.
        unsafe { key.replace(record.copy_to(at, generation)) };
        self.kill_in(HeapPage::of_record(record.addr()), size);
        true
    }

    /// Kills every record of the map the store is dropping, so that the
    /// orphans that hold some go back: the chains go with their leaves
    /// ([`LeafHeap::release`]).
    pub(crate) fn kill_at_drop(&self, record: Record) {
        self.kill_in(HeapPage::of_record(record.addr()), record.size());
    }
}
