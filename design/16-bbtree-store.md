# 16 — bbtree-store: keys and values of any length, in heap pages

Status: milestone 1 built (2026-10-21): a map from byte strings to byte
strings that threads share, whose every byte lies in pages from its
allocator, so that a cache can stay within a memory budget.

- **bbtree** gives a map's leaves an annex (D60): data of the caller's in
  each leaf, which the map keeps with its leaf as it splits, merges and
  goes, and hands to the closures of `update_or_insert_with_leaf` and
  `remove_with_leaf` while they hold the leaf (`bbtree/src/leaf_annex.rs`,
  `bbtree/src/map/with_leaf.rs`, `bbtree/src/tree/node/annexed_leaf.rs`).
  Maps keep `()`, which takes no room: their layouts and code are as
  before.
- **bbtree-store** (`bbtree-store/`) is the rest: the store
  (`store.rs`), records and the keys that point to them (`record.rs`), heap
  pages (`heap_page.rs`), each leaf's chain of them, its annex
  (`leaf_heap.rs`), and copied ranges (`range.rs`). A budget is the
  caller's allocator, which refuses pages past it.
- **Milestone 1** (built): each leaf's records in its chain of heap pages,
  records of other leaves among them after splits; a record larger than a
  heap page is refused; lookups, inserts, removals, scans and copied
  ranges.
- **Milestone 2:** overflow chains, for values larger than a heap page.

## Problem

bbtree keeps keys and values in two ways, and handles only the first:

- **Fixed size:** every type whose size is known at compile time, kept in
  its leaf slot, with no indirection. `String` is one of them: to the tree
  it is 24 bytes. Its bytes come from the global allocator, which the map
  neither sees nor counts.
- **Variable length:** bytes whose length is known only at run time.
  bbtree had no story for them; file maps kept them in each leaf's tail,
  outside the leaf's page and its layout. Now both the store and file
  maps keep them in heap pages beside their leaves, bbtree knowing only
  the hooks that let them (the leaf annex here; for file maps, a slot
  type's check and what follows a leaf, 15-index-file.md).

The use case is a cache with a memory budget: items are inserted, and the
memory they take must be counted in full. A map's own pages already come
from its `Allocator`, each one counted (D8), and a budget is an allocator
that refuses pages past it. What is missing is a place in those pages for
bytes of any length.

## Terms

- **Store:** a `bbtree-store` map. Its keys and values are byte strings.
- **Record:** one entry's key and value, with their lengths and a
  generation, in a heap page.
- **Heap page:** a page of records, from the allocator's heap method.
- **Chain:** a leaf's heap pages, its annex pointing to the newest, the
  head, each linked to the one before.
- **A record's leaf:** the leaf of the key that points to it. A record may
  lie in another leaf's chain: a stray.
- **Hole:** the bytes of a record no key points to.
- **Orphan:** a heap page no chain holds, where other leaves' records live.
- **Compaction:** sliding a leaf's records together, in place, over the
  holes.
- **Generation:** a leaf's count of splits, its own and the leaves' it
  split from, wrapping at 256.

## Design

A B-tree and a heap file. The store is bbtree's shared map,
`BBTreeMap<StoreKey, (), WithLeafAnnex<Configured<WithInlineKey, C>,
LeafHeap<C>>>`: leaves hold fixed-size entries, in key order, each key a
pointer to its record; each leaf's annex chains the heap pages its records
go into. A leaf stays a map's leaf: its layout, inline keys, hash tags,
search and latches are the map's, its annex 16 bytes after its header.

### Keys and records

```text
heap page | next | top | live | record | record | … | room |
record    | key length | value length | generation | key | value |
```

- **A key (`StoreKey`) is an atomic pointer to its record.** It orders,
  and has inline keys and a hash tag (its length), as its key bytes do,
  through `WithInlineKey`. A store swaps the pointer, while it holds the
  key's leaf exclusively, as the record moves or the value changes.
- **A lookup's key points to the caller's bytes** (`QueryKey`), behind a
  length no record has (`u32::MAX`), so that the map looks it up with
  inline keys. A new key is one until its record is written: the store
  repoints it before it goes in.
- **A record** is both lengths, `u32`s, then its generation, a byte, then
  the key and the value, unaligned. A hit reads the leaf, then the record,
  whose first line holds the key and, for short keys, the value's start.
- **A heap page** is aligned to its size (`HEAP_PAGE_SIZE`, a node config
  constant, D59, the leaves' by default), so a record's page is its
  address rounded down. Its header holds `next`, the chain's link, and
  `top`, where the next record goes, both its leaf's alone, and `live`, the
  bytes of its live records, an atomic, with a bit for an orphan.

### Chains

- **A record goes into its leaf's chain:** after the last record of the
  head; else into another page of the chain with room, which becomes the
  head; else, once a compaction made room ("Holes"); else into a new page,
  in front.
- **A split moves no record.** The new leaf starts with no page; its
  records lie in the old leaf's chain, strays. Both leaves take the next
  generation (`LeafAnnex::split`).
- **A merge splices the leaving leaf's chain** in behind the head
  (`LeafAnnex::merge`). A steal moves entries and no record: strays again.
- **Strays come home two generations on.** Once a leaf's generation
  changed, its next write reads its records that lie outside its chain,
  and moves each written two generations or more before into its chain:
  its key range split twice since, so its page holds the records of leaves
  further and further off. One generation behind, it shares a page with a
  neighbour's records, which a range scan over both reads anyway. A leaf
  that compacts takes all its strays in.
- **Dropping the store** frees the chains with their leaves
  (`LeafAnnex::release`), and kills every record first while orphans are
  out, which free themselves as they empty.

### Holes

- **A record dies** when its key goes, or points to a new record: a hole.
  A value no longer than the old one goes in place instead, the bytes it no
  longer needs a hole.
- **A leaf compacts** when a removal leaves a page of its chain, and the
  chain, half holes; or when a record finds no room and a quarter of the
  chain is holes (worth it: a compaction moves at most three live bytes a
  byte of holes, and saves a page); or when the allocator refuses a page
  and the chain has holes at all.
- **In place, no page taken:** in the pages of the chain where only the
  leaf's records live, its records slide together as one run, in place,
  from the first such page on; the pages the run empties go back to the
  allocator, and the one it ends in becomes the head. Pages where none of
  its records live go too, or become orphans while others' do. A store
  whose budget is spent takes its holes back.
- **A page where another leaf's records live keeps its holes** until they
  go: only their own leaf moves them, holding it. Whether one does is in
  the page's live count: live bytes the leaf's own records don't account
  for are others'.
- **An orphan frees itself:** whichever thread empties it, by killing its
  last record or by orphaning it empty, frees it, the orphan bit and the
  live count one atomic.
- **A removed record dies after the removal returns.** The map reads the
  key until then: removing a separator refits prefixes with its key. The
  record's page counts it live meanwhile, so no compaction slides over it
  or frees it.

### Larger than a heap page

- **Keys are capped** at a quarter of a heap page: a key's bytes must lie
  together, as inline keys are windows at any offset (D26) and ties
  compare from where keys differ, in the search's hot path. Most systems
  cap keys, B-trees at a fraction of their page (SlateDB and fjall checked
  in their source, Bigtable and RocksDB in their docs, 2026-10-20):

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

- **A record larger than a heap page is refused,** until milestone 2:
  overflow chains, pages of the heap page's size each holding the next
  page, then value bytes, the record keeping the key, the value's length,
  the part that doesn't fill a whole page (SQLite's rule) and the chain's
  ends. A chain, not a list of pages: a list that outgrows its record
  needs an indirect page, a chain has no limit but the budget, and a store
  reads and writes values whole.

### When the allocator refuses

An insert that needs a page the allocator refuses, a node or a heap page,
fails with `AllocError`, the store holding what it held, as the shared
map's insert does (D8). A refused heap page compacts first. The room a
leaf's chain frees stays that leaf's: a store whose budget is spent takes
new records into leaves with holes, and refuses others. Freeing room, by
finding cold records, is the cache's policy, above the store.

### Threads

- **Only a thread holding a record's leaf exclusively moves, changes or
  kills it**; a lookup or a scan reads it holding the leaf shared, under
  the map's closures. The map's unlatched reads settle a key from its
  inline key and hash tag, and never read its bytes (D57: a `StoreKey`
  doesn't order by its own bytes), and a descent that ties reads a
  separator's key holding its leaf shared.
- **A page's chain fields are its leaf's;** `live` is any record's leaf's,
  atomically, as a stray dies in another leaf's page.
- **No page goes while a record lives in it,** so no record goes while a
  thread reads it: a chain's page goes by its leaf, once none of its
  records live; an orphan by the thread that empties it.

### API

```rust
impl<A: Allocator + Clone, C: NodeConfig> BBTreeStore<A, C> {
    fn with_config_in(config: C, alloc: A) -> Self;
    fn max_key_len(&self) -> usize;      // a quarter of a heap page
    fn max_record_size(&self) -> usize;  // a heap page, less its header
    /// Appends the value to `buf`, as `Read::read_to_end` does; returns its length.
    fn get_into(&self, key: &[u8], buf: &mut Vec<u8>) -> Option<usize>;
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn contains_key(&self, key: &[u8]) -> bool;
    /// Whether a value was replaced.
    fn insert(&self, key: &[u8], value: &[u8]) -> Result<bool, InsertError>;
    fn remove(&self, key: &[u8]) -> bool;
    fn scan<Q, R>(&self, range: impl RangeBounds<Q>,
                  f: impl FnMut(&[u8], &[u8]) -> ControlFlow<R>) -> Option<R>;
    fn scan_rev<Q, R>(/* as `scan` */) -> Option<R>;
    fn range<Q, R>(&self, range: R) -> RangeCopies<'_, A, C>;
}
```

- **Values are copied out,** appended to the caller's `Vec`: there is no
  `&V`, as compaction moves records. The value's length is known only once
  its record is read, so the buffer grows within the one read.
- **Inserts take `&[u8]`, and copy it once,** into the record. They return
  whether a value was replaced, and fail on a key over the cap, a record
  over a heap page, or the allocator's refusal (`InsertError`).
- **Ranges:** `scan` and `scan_rev` run a closure on each entry's bytes
  under its leaf's shared latch; `range` hands out copies, key and value,
  from either end, sixteen at a time, by scans that start past what each
  end handed out last, holding no latch between them.

## What bbtree gives a store

One generic mechanism, the leaf annex (D60), in the core; everything about
heap pages is the store's.

- **`LeafAnnex`**, a trait: `Default`, `split(&mut self, right: &mut
  Self)`, `merge(&mut self, right: Self)`, `release(self, alloc)`. `()`
  implements it, taking no room.
- **`WithLeafAnnex<S, L>`**, a strategy: `S`'s inline keys and node
  config, and annex `L`. `InlineKeyStrategy` has `type LeafAnnex`, `()` but
  there; `Configured`, `Latched` and `InFile` forward it. Only a
  `BBTreeMap`'s leaves keep an annex: a latched tree's; a map for one
  thread or a file map with one fails to compile.
- **Layout:** `NodeLayout<K, V, C, L>` puts `L` after a latched leaf's
  header (`Header`), aligned; of size 0, every layout is as before.
- **Node code:** a new leaf gets `Default`'s annex (`NodePtr::initialize`);
  a leaf's split and merge call the annex's (`Leaf::split_around`,
  `Child::append`); the dying walk releases it (`DyingWalk`); raising and
  lowering the root copy it with the node (`move_node_to`).
- **`AnnexedLeaf<'a, K, V, S>`:** a leaf an operation holds exclusively,
  its annex (`annex()`) and keys (`keys()`), which
  `update_or_insert_with_leaf` hands to its closures, `occupied` with the
  held entry, `vacant` with the new one right before it goes in, either
  free to refuse for want of a page (the entry comes back, the map
  unchanged); and `remove_with_leaf` to its closure right before the entry
  comes out. A refused split gives its reserved node pages back
  (`NodePages::unreserve`).

A map's insert, get and remove run the same instructions as before: 400.8,
244.7 and 475.1 a `u64` operation at 1M keys before and 400.6, 244.7 and
474.7 after (`instructions:u`, 2026-10-21); get compiles to the same
machine code.

## Measurements

10M entries, 20-byte keys and 32-byte values, keys hashed so that insert
order is unrelated to key order, mimalloc, 1 KiB leaves and heap pages
(`bbtree-store/examples/bench.rs`, 2026-10-21), against the designs this
one replaced: a store with every record in its own leaf's chain, records
moved at every split (the first milestone 1), and a store on the map whose
records went into size classes shared by all leaves (a prototype).

| | this store | records in own leaf | size classes |
|---|---|---|---|
| insert, 1 thread | 419 ns | 393 ns | 274 ns |
| get | 334 ns | 354 ns | 348 ns |
| scan | 17.2 ns an entry | 13.3 ns | 23.9 ns |
| bytes an entry, nodes + heap | 25.0 + 82.9 = 107.8 | 25.0 + 76.9 = 101.9 | 25.3 + 68.3 = 93.6 |
| insert, 8 threads | 71 ns | 69 ns | 191 ns |
| get, 8 threads | 49 ns | 51 ns | 55 ns |

- **Under churn,** values replaced with ones of 16 to 64 bytes and a
  quarter of the entries removed and put back, the heap holds 1.54 bytes a
  live record byte once steady, 1.85 when a leaf compacted only at half
  holes.
- **Strays two generations behind coming home** cost, at 5M inserts, 38 ns
  an insert (394 against 356) and a quarter more heap pages (0.095 a
  record against 0.075, as moved records leave holes), for scans 11%
  faster (17.5 against 19.6 ns an entry).

## Open questions

- **The key cap:** a quarter of a heap page, which bounds what a record
  leaves unused at the end of a page to a quarter page: 256 B at 1 KiB,
  about memcached's 250.
- **The two thresholds:** a quarter of the chain holes before a page is
  taken, half on a removal; and whether strays should come home two
  generations on, or only as their leaf compacts.
- **The budget's policy** ("When the allocator refuses").
