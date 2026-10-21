# 16 — bbtree-store: keys and values of any length, in heap pages

Status: milestone 1 built (2026-10-20): a map from byte strings to byte
strings that threads share, whose every byte lies in pages from its
allocator, so that a cache can stay within a memory budget.

- **bbtree**, behind the `store` feature: the store (`BBTreeStore`,
  `RangeCloned`, `InsertError`) in `bbtree/src/store.rs`; its leaves' heaps
  (`InStore`, `HeapPage`, the leaf's store operations) in
  `bbtree/src/tree/node/store.rs`; record pointers (`RecordPtr`), which file
  maps' writers use too, and an insert's key and value (`RecordParts`), in
  `bbtree/src/record_ptr.rs`.
- **bbtree-store** (`bbtree-store/`): the store, re-exported. A budget is
  the caller's allocator, which refuses pages past it.
- **Milestone 1** (built): each leaf's records in its chain of heap pages;
  a record larger than a heap page is refused; lookups, inserts, removals
  and scans.
- **Milestone 2:** overflow chains, for values larger than a heap page.

## Problem

bbtree keeps keys and values in two ways, and handles only the first:

- **Fixed size:** every type whose size is known at compile time, kept in
  its leaf slot, with no indirection. `String` is one of them: to the tree
  it is 24 bytes. Its bytes come from the global allocator, which the map
  neither sees nor counts.
- **Variable length:** bytes whose length is known only at run time.
  bbtree has no story for them. File maps keep them in each leaf's tail
  (15-index-file.md, "Keys and values of any length"), which is a hack:
  - the tail lies outside the leaf's page and its layout, padded to the
    next leaf's alignment: half a KiB a leaf on average;
  - it works only because a file map is written once: no free space, no
    insert, no compaction;
  - a span counts its offset from its own slot, because a key's `Ord` gets
    `&K` and no page: moving the slot breaks it.

The use case is a cache with a memory budget: items are inserted, and the
memory they take must be counted in full. A map's own pages already come
from its `Allocator`, each one counted (D8), and a budget is an allocator
that refuses pages past it. What is missing is a place in those pages for
bytes of any length.

## Terms

- **Store:** a `bbtree-store` map. Its keys and values are byte strings.
- **Heap page:** a page holding records of one leaf, in no order.
- **Heap chain:** a leaf's heap pages, each linked to the next; the leaf
  points to the first.
- **Record:** one entry's key and value, with their lengths, in a heap
  page.
- **Record pointer:** what a leaf's entry holds: its record's address,
  which names the heap page and the offset in it.
- **Garbage:** the bytes of records no entry points to.
- **Compaction:** sliding a heap page's live records together, so that its
  garbage becomes room.
- **Overflow chain:** the pages holding the part of a value too large for
  a heap page.
- **Budget:** the store's allocator, the caller's, which refuses pages
  past what the cache may use.

## Design

A B-tree and a heap file. Leaves hold fixed-size entries, in key order,
each pointing to its record in a heap page. A leaf stays a fixed-size
leaf: its layout, inline keys, hash tags and search are a map's. Bytes of
any length live in pages of their own, sized apart from the leaf.

### Leaves and heap pages

- **A leaf's entry** is its key's inline key and hash tag, as for a byte
  string, and an 8-byte record pointer (`RecordPtr`), its key, and `()`,
  its value. A record pointer is what a file map's writer keeps for a byte
  string (15): a pointer to bytes, length first, which compares, and has
  inline keys and a hash tag (its length), as its bytes do. `Auto` takes it
  for a byte string, so a `&[u8]` looks a store up.
- **A record pointer is a heap page and an offset.** A heap page is aligned
  to its size, so the page is the pointer rounded down to it, and the
  offset the rest.
- **A heap page belongs to one leaf.** A leaf's records lie in its own
  heap pages, which no other leaf points into, so whatever moves a record
  holds the one leaf that points to it.
- **A leaf's heap pages are a chain.** The leaf keeps one pointer, in its
  header, to its newest heap page, which links to the one before, and so
  on to the oldest. A record goes into the newest page, and a new page
  goes in front: neither walks the chain. A leaf is full when its entries
  are: its heap grows by a page when its records need room, not by
  splitting the leaf. A lookup never walks the chain, as its record
  pointer leads to its page; writers do, to compact.
- **The pointer moves with its leaf.** Raising and lowering the root copy
  a node's page whole (`move_node_to`), header and all.
- **Records lie in no order** in a heap page: a new one goes after the
  last record of the newest page. Entries stay sorted in the leaf.
- **Heap page size:** a node config constant (D59), `HEAP_PAGE_SIZE`,
  `LEAF_PAGE_SIZE` by default; a power of two, as every page size is,
  for the rounding above.
- **The leaf's header holds its chain** (`HeapChain`, `tree/node/page.rs`):
  the newest heap page, the chain's garbage, the bytes of records no entry
  points to, and how many pages it holds. It lies after the node header,
  8-aligned (`Header`, `tree/node/layout.rs`), and pushes the inline keys
  16 bytes further: a store's leaves are four more layout constants.

### Records

```text
heap page | next | top | record | record | … | room |
record    | key length | key | value length | value |
```

- **The header** holds `next`, the chain's next (older) heap page, and
  `top`, where the next record goes: 16 bytes.
- **A record** is its key, length first, then its value, length first: a
  record pointer leads to the key as it leads a file map's writer to its
  bytes, and the value follows. Lengths are `u32`s, unaligned. A value too
  large for a heap page will keep only its last part here, with its
  overflow chain ("Larger than a heap page").
- **A hit reads the leaf, then the record,** whose first line holds the
  key and, for short keys, the value's start.

### Inserting

An insert finds its leaf as a map's does, and splits it only if its
entries are full ("Removing and compaction"). A leaf has room when it has
an entry free; its heap makes room for the record as it needs:

1. **The newest page has room after `top`:** the record goes there.
2. **Otherwise, the chain's garbage would hold it:** compact the chain,
   keeping room for it, then as in 1.
3. **Otherwise, a new heap page** from the allocator, chained in front.

The insert holds the leaf the while, on its fast path: no slow path, no
reservation, as a heap page comes straight from the allocator.

A key already present: a value no longer than the old one is written over
it, in place, and the bytes it no longer needs count as garbage; a longer
one is a new record, and the old record is garbage.

### Removing and compaction

- **A removal marks the record deleted:** the leaf drops the entry,
  shifting those after it as a map's leaf does (D22), and the record's
  bytes count as its chain's garbage. Nothing in the heap page moves.
- **Compaction runs when:**
  - the leaf splits or merges;
  - an insert into the leaf finds no room in its heap chain ("Inserting",
    2).
- **In place:** the leaf's record pointers, sorted by their page's place
  in the chain, then by offset; each record slid down to the end of the
  one before it, into the page before if it fits there, and its pointer
  rewritten. The pages it empties go back to the allocator, but one,
  kept empty, if the record that asked for room needs it. It takes no
  page, so it works with the budget spent. Records don't span pages, so
  the garbage doesn't always make room: then the insert takes a page.
- **A split** copies the records of the entries that move into the new
  leaf's heap chain, then compacts the old leaf's. Before it changes
  anything, it grows the old leaf's chain with the empty pages those
  records, and the new one, fill (`grow_heap_by`); the split hands them to
  the new leaf. If the split can't take its node pages after all, they go
  back.
- **A merge** splices the leaving leaf's chain onto the survivor's, the
  pages changing hands, then compacts it.
- **Store leaves merge, and never steal.** A steal would bring a record
  into a leaf whose heap may lack room, and the remove path allocates
  nothing; a leaf left below its minimum stays so, down to empty, until a
  removal lets it merge. Internal nodes steal as a map's do. The invariant
  check exempts store leaves from their minimum.
- **Removed bytes serve the leaf's own inserts.** Garbage becomes room in
  the leaf's heap; a page goes back to the allocator only when compaction
  empties it, or the leaf merges. A store whose budget is spent takes new
  records into leaves with garbage, and refuses those that need a split
  elsewhere.

### Larger than a heap page

- **Keys are capped.** A key's bytes must lie together: inline keys are
  windows at any offset past what a node's keys share (D26), and ties
  compare from where keys differ (`cmp_from`), in the search's hot path.
  An insert of a longer key fails. Most systems cap keys, B-trees at a
  fraction of their page, so that a node holds several (SlateDB and fjall
  checked in their source, Bigtable and RocksDB in their docs,
  2026-10-20):

  | system | key limit |
  |---|---|
  | PostgreSQL B-tree | about a third of a page: 2,704 B at 8 KiB |
  | InnoDB | 3,072 B at 16 KiB pages, less on smaller pages |
  | LMDB | 511 B by default, at compile time |
  | SQLite | none: keys overflow as values do |
  | SlateDB | `u16::MAX`; `put` panics past it (`slatedb/src/batch.rs`) |
  | fjall | 65,536 B |
  | RocksDB | none; up to 8 MB recommended (its FAQ) |
  | Bigtable | 4 KiB (Cloud Bigtable); 64 KB in the paper |
  | memcached | 250 B |

- **A value too large for a heap page goes to an overflow chain:** pages
  of the heap page's size, from the same allocator, each holding the next
  page, then value bytes. The record keeps the key, the
  value's length, the part of the value that doesn't fill a whole page
  (SQLite's rule, so that every page of the chain is full), and the
  chain's first and last page.
- **A chain, not a list of pages.** A list that outgrows its record needs
  an indirect page, then a tree of them; a chain has no limit but the
  budget. Its dependent loads mostly hide: the next page's address sits in
  the page's first line, so that load issues while the rest of the page is
  copied, about one miss a page, which is about what copying a 2–4 KiB
  page takes (an estimate, not measured). At 1 KiB pages a chain is bound
  by latency. A list would read or change a value at an offset without
  walking it, which a store, reading and writing values whole, doesn't do;
  InnoDB went from a chain to a LOB index in MySQL 8.0 for partial updates
  of JSON documents.
- **Freeing a chain** gives each of its pages back to the allocator.

### When the allocator refuses

An insert that needs a page the allocator refuses (a split, or "Inserting",
3) fails with `AllocError`, the store unchanged, as the shared
`BBTreeMap`'s insert does (D8). Freeing room, by finding cold pages or
cold records, is the cache's policy, to be designed later, above the
store.

### Threads

The shared store is the shared `BBTreeMap`'s tree (D54), with heap pages
beside its leaves.

- **One version covers a leaf and its records.** Whatever changes a record
  (an insert, an overwrite, a compaction, a split or merge) holds the
  record's leaf, whose release bumps its version.
- **A lookup reads as a `String` key's does** (D57): a key that ends
  within its inline key is settled in the leaf, unlatched; otherwise the
  lookup latches the leaf shared, and copies the value out under the latch.
- **Heap pages come from the map's allocator, and go back to it.** A
  record is read only by whoever holds its leaf: a lookup copies under the
  leaf's shared latch, and an unlatched read settles a key from its inline
  key and hash tag, never following a record pointer (D57). So heap pages
  need none of what node pages get from the tree's page pool
  (`tree/latched/pages.rs`): type-stable reuse, latching, freeing only once
  the operation lets go. They are a kind of page of their own
  (`allocate_heap`, `deallocate_heap`, D8), which the pool passes through
  to the allocator; a freed heap page's memory goes back to the
  allocator, and so to the budget, at once.
- **Unlatched reads of records** would need type-stable heap pages again:
  a lookup that follows a record pointer before its leaf's version
  confirms it may read a page that went back to the allocator.
- **A value in an overflow chain is read under the leaf's shared latch:**
  an unlatched walk would follow next pages it can't check, and a long
  copy would retry on any write to its leaf.

### API

```rust
impl<S, A: Allocator> BBTreeStore<S, A> {
    const MAX_KEY_LEN: usize;      // a quarter of a heap page
    const MAX_RECORD_SIZE: usize;  // a heap page, less its header
    fn with_strategy_in(strategy: S, alloc: A) -> Self;
    fn allocator(&self) -> &A;
    /// Appends the value to `buf`, as `Read::read_to_end` does; returns its length.
    fn get_into(&self, key: &[u8], buf: &mut Vec<u8>) -> Option<usize>;
    /// `get_into` a new `Vec`.
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn contains_key(&self, key: &[u8]) -> bool;
    /// Whether a value was replaced.
    fn insert(&self, key: &[u8], value: &[u8]) -> Result<bool, InsertError>;
    fn remove(&self, key: &[u8]) -> bool;
    fn scan<Q, R>(&self, range: impl RangeBounds<Q>,
                  f: impl FnMut(&[u8], &[u8]) -> ControlFlow<R>) -> Option<R>;
    fn scan_rev<Q, R>(/* as `scan` */) -> Option<R>;
    fn range_cloned<Q, R>(&self, range: R) -> RangeCloned<'_, S, Q, R>;
}
```

- **Values are copied out.** There is no `&V`: compaction moves records,
  and a value can span pages.
- **Into the caller's `Vec`, appended.** The value's length is known only
  once its record is read, so the buffer grows within the one read; a
  slice too small would need a second lookup, which another thread may
  race. Appending lets a batch put every value in one buffer.
- **With unlatched reads (planned), a length read is bounded by the
  largest value** before the buffer grows: a torn read must not ask for
  gigabytes. A failed check truncates the buffer back and retries.
- **Inserts take `&[u8]`, and copy it once.** The shared insert's key is
  the leaf's key type, a record pointer; an insert's points to the
  caller's key and value where they are (`RecordParts`), whose first field
  is a length no record has (`u32::MAX`), which tells it from a record.
  The search reads the key through it, and the leaf writes the record into
  its heap as the entry goes in (`Leaf::insert_fit`), or as a held entry
  takes the value. Inserts return whether a value was replaced, not the
  old value, a copy most callers don't want. They fail on a key over the
  cap, a record over a heap page, or the allocator's refusal
  (`InsertError`).
- **Ranges:** `scan` and `scan_rev` run a closure on each entry's key and
  value bytes under its leaf's shared latch, as a map's do;
  `range_cloned` hands out each entry's key and value as `Vec<u8>`s,
  holding no latch between them, as a map's does. A range's bounds are
  byte strings of any kind (`AsRef<[u8]>`).
- **Configuration:** the allocator, which is the budget, and the node
  config, `HEAP_PAGE_SIZE` with the other page sizes (D59).

## What a store shares with the map

The store is the shared `BBTreeMap`'s tree, `LatchedTree<RecordPtr, (),
InStore<S>>`: its descent, latches, splits, merges, page pool, shared
prefix and lookups are the map's. The strategy `InStore<S>` sets a flag,
`IN_STORE`, which `Latched` forwards, as `InFile` and `Latched` set theirs
(D54, D58); every store operation in the node code runs under
`NodePtr::is_in_store()`, which folds away for maps. What bbtree changed
for it:

- **Record pointers** are the file writer's `ArenaBytes`, renamed
  `RecordPtr` and moved to `record_ptr.rs`, recognized by `Auto` with
  either feature.
- **Layouts** take a `Header` (latched, heap chain) where they took a
  `latched` flag.
- **Leaves:** `insert_fit` takes a new entry's record in;
  `make_room_for` makes room in the heap for it, and `replace_record` gives
  a held entry a new value, both fallible, and nothing in a map;
  `split_insert` and `split_around` take the page allocator and move the
  right half's records; `Child::append` and `Siblings::merge` take the page
  allocator, and splice the chains; `remove` and `take_head_and_first_key`
  count the record as garbage.
- **Inserts:** a leaf has room when it has an entry free, as a map's. The
  latched tree's `insert_into` makes the heap room itself, and refuses
  with `Refused::NoPage` if the allocator has no page, as `Refused::Full`
  sends the entry to a split. The append path makes room too. A split
  first grows the leaf's chain with the empty pages its moving records,
  and the new one, fill.
- **Pages:** the allocator has a method pair per kind of page (D8); heap
  pages are one kind, which a latched tree's page pool passes through.
- **Ranges:** the map's `range_cloned` machinery is the latched tree's
  `CopiedRange`, which takes how to copy an entry out of its leaf: a map
  clones it, a store copies its bytes. The bounds check beside the scans
  serves both (`LatchedTree::checked_bounds`).
- **Removal:** `fix_underfull_child` merges store leaves, and never steals.
- **Teardown:** `DyingWalk` gives a store leaf's heap pages back with it.
- **Tests:** `store/tests.rs` (std's `BTreeMap` as the model, heap
  invariants with the tree's, ranges, a limit of pages in use of
  `TestPages`, threads), `store/shuttle_tests.rs` (Shuttle, as the map's,
  sharing their `Exploration`).

Not yet checked: that maps compile to the same machine code with the
`store` feature on and off.

## Open questions

- **The key cap:** a quarter of a heap page, built, which bounds what a
  record leaves unused at the end of a heap page to a quarter page: 256 B
  at 1 KiB, about memcached's 250.
- **Freeing a heap page when its last record is removed,** instead of at
  the leaf's next compaction.
- **File maps:** whether variable-length file maps keep their tails, or
  take heap pages.
- **The budget's policy** ("When the allocator refuses").
