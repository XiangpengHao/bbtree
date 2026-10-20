# Decisions

Decisions made so far, with the reason and what follows from each.
Each entry states a decision as it holds now. To change one, edit it in place;
the file is not a history of decisions.

## Scope (from 00-init)

**D1. Drop-in replacement for std's `BTreeMap`/`BTreeSet`.**
`unsync::BBTreeMap` and `unsync::BBTreeSet` keep std's API and behavior. Any
deviation needs a strong reason and is documented in README.md. Design as if
it could be merged into std someday; flag anything that would prevent that.

**D2. Zero dependencies.** `serde` and `rand` support, when added, are
optional features.

**D3. `no_std`.**

**D4. Two maps on one tree: `BBTreeMap`, which threads share, and
`unsync`'s, single-threaded like std's.** (2026-10-17)
`bbtree::BBTreeMap` is the map threads share (D54); `bbtree::unsync::BBTreeMap`
and `unsync::BBTreeSet` are std's drop-in replacements (D1). Both keep the
same nodes, and the concurrent one runs the unsync tree's operations.

- **Why the shared map leads:** the ordered maps that matter for speed are
  shared: the systems surveyed (13-gtm.md) keep theirs behind locks or in
  concurrent skiplists, while most single-threaded `BTreeMap`s are small
  (configuration, sorted output) and nobody times them. One name, bbtree,
  for the map they'd pick.
- **Why keep `unsync`:** it is the only one std's users can switch to by
  changing an import. The shared map can't lend `&V`, iterate by reference
  through `&self`, or take keys without inline keys, and its single-thread
  lookups and batches cost a little more (09-concurrency.md). Its code is
  mostly the map and set APIs over the tree both maps use.
- **Names:** `unsync` after once_cell's `sync`/`unsync`. The shared map's
  methods follow std's words where they mean the same: `get_*` returns a
  value (`get_copied`, `get_cloned`, `get_mut`), `read*` runs a closure on
  the entry in place, as scc's maps do.

**D5. Differential fuzzing against std's btree.** Formal verification with
Verus is optional but welcome.

## Nodes

**D8. Own allocator trait, the map's last type parameter.** (2026-10-14)
Every allocation is a node, counted, with a size known in advance; no runtime
surprises. Two levels: an allocator hands out blocks by size and alignment
alone, and the tree makes each block a node of its kind, a leaf, an internal
node or the root page, with that kind's layout (D21).

- **Trait:** `Allocator` (`allocator.rs`), public: the part of std's unstable
  `core::alloc::Allocator` a map needs, `allocate_zeroed(&self, Layout)` and
  `unsafe deallocate(&self, ptr, Layout)`, with std's names and contract (an
  `unsafe trait`; a clone is the same allocator), so it works on stable.
  `Global`, the global allocator, is the default; `&A` is an allocator too,
  to share one between maps.
- **Zeroed pages:** every page comes zeroed, the allocator's job, which
  some do for free (fresh memory from the OS). The latched tree needs it,
  for latch words and the edges optimistic readers read (09-concurrency.md).
- **Never unwinds:** part of the trait's safety contract, as for
  `GlobalAlloc`. The tree calls the allocator in the middle of splits, and
  promises nothing about an unwind there.
- **In types:** `unsync::BBTreeMap<K, V, S = Auto, A: Allocator + Clone =
  Global>`, and the same for the set (the shared `BBTreeMap` takes any
  `A: Allocator`): the strategy before the allocator, so opting
  into inline keys stays `BBTreeMap<K, V, WithInlineKey>` (D18).
  `new_in(alloc)` and `with_strategy_in(strategy, alloc)` take one; `new`,
  `with_strategy`, `Default`, `FromIterator` and `From` stay `Global`-only,
  as std's do.
- **Ownership:** the map holds the allocator and passes `&A` to every tree
  operation that allocates or frees a page; the tree stores none. The
  operations a latched tree runs take a crate-private `PageAllocator`
  instead, which hands out pages, of any contents, and doesn't fail: the
  map's allocator, or a latched operation's pages taken beforehand. A page's
  type (`FreshPage<P>`) carries its layout, so a page goes back with the
  layout it came with; that it goes back to the allocator it came from is
  a convention of the tree's code, as in std's btree, not a type. What
  outlives a borrow of the map, `IntoIter` and the pieces of `append`,
  holds a clone; `clone` and `split_off` give the new map one.
  `append` moves entries, not pages, so maps with different allocators of
  one type append soundly; the set operators build on the left set's.
- **Failure:** `allocate_zeroed` returns `AllocError`. The unsync map calls
  `handle_alloc_error`, as std's collections do, wherever the page is
  asked for, the middle of a split included. The shared `BBTreeMap` hands
  it to the caller: `insert`, `update_or_insert`, `get_or_insert` and each
  entry of the batches return `InsertError`, with the entry, and leave the map
  unchanged. A split counts the pages it takes from the nodes it holds (a
  leaf's, one per internal node below the one with room, and one for a
  root that moves out of the root page), and takes them, free pages first,
  before it changes anything. `Extend` and `FromIterator` call
  `handle_alloc_error`.
- **Cost of the parameter:** with `Global`, instructions per operation at 1M
  entries (fat-LTO probe, mimalloc): get hit and miss, remove and
  `pop_first` unchanged; insert +0.5 (`u64`) and +1.5 (`String`).
- Consequence: a default allocator should carve aligned pages from large
  chunks (see "Proposed, not decided"). Per-node aligned requests cost
  glibc up to twice the memory; measured with 4 KB nodes (20k nodes, each
  fully written, RSS delta):

  | Strategy                                   | RSS per 4 KB node |
  |--------------------------------------------|-------------------|
  | `alloc(4096, align 16)` (not page-aligned) | 4120 B            |
  | `alloc(4096, align 4096)` per node         | 8200 B (2×)       |
  | carve from 64 KB chunks                    | 4353 B            |
  | carve from 256 KB chunks                   | 4154 B            |
  | carve from 2 MB chunks                     | 4098 B            |

**D9. A node is a header, its keys, and the slots that follow them.** (2026-10-07)

```
leaf      | header | k0 k1 … k_n | head v0 v1 … v_n |
internal  | header | k0 k1 … k_n | e0 e1 … e_n+1     |
```

- Without inline keys, the header is `len: u16` and a leaf's `has_head`
  flag (D17). The keys are an array, and the trailing slots, a leaf's head
  and values or an internal node's edges (D12), an array right after the
  keys' capacity. A node's kind is not in its page: every pointer to it
  carries it, in a tag bit (D52).
- Trailing slot `i + 1` goes with key `i`, so splitting, stealing and
  merging move trailing slots the same way in both kinds.
- An insert is one shift of the keys after the position and one of the
  trailing slots; a split is one copy of each half.
- With inline keys, leaves pair keys with values (D29), a head with its
  separator's key (D49), and internal nodes keep no keys, only inline keys
  and edges (D49); the layouts are in `tree/node/layout.rs`.

**D10. Not taken from bf-tree:** slotted layout, stored fence keys,
mini-pages. (2026-10-07)
Fixed-size generic K/V needs none of the slotted machinery, and stored fences
would need `K: Clone`. Prefix stripping and preview bytes come in another
form: windows of an order-preserving encoding, past what a node's fences
share, with no fence stored (D18, D26).

**D12. An internal node's edges are its trailing slots.** (2026-10-07)

- Edge `i` is trailing slot `i`: the child left of key `i`, and edge
  `len` the last (D9).
- Keys sit at the same offset in leaves and internal nodes without inline
  keys, so one search routine serves both.
- Capacity is the largest `n` that fits, computed per `(K, V)` at compile
  time (`tree/node/layout.rs`).

**D13. Every node holds at least 12 keys.** (2026-10-07)
A node kind's size is the smallest multiple of 1 KB (D15) at which it holds
≥ 12 keys, with a leaf's values and head or an internal node's edges (D21).
A type aligned to more than 1 KB raises the node's alignment, and its size
step, to its own.

**D15. Nodes are 1 KB, aligned to their own size.** (2026-10-07)
A node is the smallest multiple of 1 KB at which it holds ≥ 12 keys (D13),
and it is aligned to max(1 KB, align of K, align of its trailing slots).

- Why: a sweep at 10M u64→u64 entries (mimalloc, branchless search, append
  fast path) put 1 KB ahead of every other size, and ahead of std on all three
  workloads:

  | node size | random insert | sequential insert | random get | RSS |
  |-----------|---------------|-------------------|------------|-----|
  | 512 B     | 231 ns        | 21.8 ns           | 180 ns     | 247 MB |
  | **1 KB**  | **207 ns**    | 16.7 ns           | **177 ns** | 237 MB |
  | 2 KB      | 253 ns        | 15.0 ns           | 216 ns     | 234 MB |
  | 4 KB      | 366 ns        | 14.3 ns           | 249 ns     | 239 MB |
  | 8 KB      | 464 ns        | 13.4 ns           | 245 ns     | 503 MB |
  | std       | 230 ns        | 36.7 ns           | 258 ns     | 314 MB |

  Random inserts get worse with node size because each one shifts about half
  the node: at 4 KB that pulls 2.2× std's cache lines from DRAM per insert.
  The 8 KB memory jump is mimalloc's overhead on large aligned blocks.
- A self-aligned 1 KB node never crosses a 4 KB page: one TLB entry per
  node, and hardware prefetchers, which stop at page boundaries, can stream
  the whole node.
- 1 KB alignment leaves 10 free low address bits, so node capacity is capped
  at 1023.
- A leaf holds 12 entries in 1 KB while `K + V` ≤ ~76 B; larger entries get
  2 KB, 3 KB, … leaves (D21). u64→u64 without inline keys: leaf 63,
  internal 63.
- A one-entry map costs 1 KB (std: 192 B for a one-entry u64→u64 map).
- Internal nodes with inline keys keep no keys (D49) and are 512 B,
  aligned to 512 B: 49, 41 and 31 children at 2-, 4- and 8-byte inline
  keys, as many as 1 KiB held with keys or more, and a search scans every
  inline key slot (06, lesson 1). In 1 KiB they held 100, 84 and 63, and
  10M `hash32` hits ran 232 ns against 160.

**D16. Leaves and internal nodes stay sorted.** (2026-10-07)
Unsorted leaf storage, fully unsorted or as a sorted prefix plus an unsorted
tail, was prototyped and rejected.

- 10M u64→u64, mimalloc, ns/op (scans in ns per entry or per scan):

  | workload | sorted | sorted prefix + tail | unsorted | std |
  |---|---|---|---|---|
  | random insert | 207 | 198 | **176** | 219 |
  | sequential insert | **16.9** | 19.1 | 25.4 | 37.8 |
  | random get | **186** | 207 | 198 | 259 |
  | full scan, random-built (per entry) | **4.3** | 8.7 | 10.6 | 14.3 |
  | 64-entry range scan | **512** | 932 | 1142 | 1219 |

- Unsorted leaves only win random inserts. A linear scan of a cold leaf pulls
  every key line from DRAM, so it loses gets, and sorting on read is branchy,
  so it loses scans.
- Blocker for D1: std's `iter`, `keys`, `values`, `IntoIterator`, `Debug`,
  `Hash`, `PartialEq`/`PartialOrd` and `Drop` have no `K: Ord` bound, so they
  could not produce key order from unsorted leaves without storing a
  comparator in the map (one more word, plus user code in `Drop`/`Debug`) or
  a per-leaf permutation (the slotted layout D10 rejected).

**D17. B+-tree: values live only in leaves; separators are moved, not copied.** (2026-10-07)

- Internal nodes hold separators and edges only. Without inline keys, **the
  separator keys are stored physically in internal nodes**, which are the
  hot part of the tree: an internal-node search compares against keys in
  that node. With inline keys, they are leaf heads (D49).
- Leaves hold keys and values (D9), plus one **head**: every leaf except the
  leftmost stores the value of the separator key just to its left in key
  order. That key lives in an internal node, the lowest common ancestor of
  the two leaves, or with inline keys, in the head beside its value (D49).
- A tree with L leaves has exactly L − 1 separators, one per boundary between
  neighbouring leaves, so every leaf but the leftmost has exactly one head.
- **No `K: Clone`.** Each key is stored exactly once, as in std. When a leaf
  splits, the right half's smallest key becomes the separator and its value
  stays behind as the right leaf's head; the key moves up, or with inline
  keys stays in the head (D49); a merge brings both back together as an
  entry. Values never leave leaves.
- **Lookup always ends at a leaf.** A match on an internal separator continues
  down the left border of its right subtree to that leaf's head.
- Why: values only in leaves, as intended from the start (00-init: database
  B-trees). Internal fanout is higher (u64→u64: 64 children per 1 KB, 43
  with values in internal nodes), and large values never sit in internal
  nodes. Inner nodes are a tiny fraction of the tree, so the extra work on
  separator hits doesn't matter.
- Alternative considered: keep every key physically in its leaf and let
  internal nodes point at it. Rejected because moves inside leaves (shifts,
  splits, steals) would invalidate those pointers. D49 keeps the keys in
  leaves without pointers: a separator's key is its head's, which the tree's
  shape finds.

**D18. Inline keys: a defaulted strategy parameter that recognizes std types.** (2026-10-07)
Design in 04-inline-key.md.

- **Two parts:**
  - a public `InlineKey` trait anyone can implement, newtypes included (option
    (b));
  - a default strategy that gives std byte-string keys (`String`, `str`,
    `Vec<u8>`, `Box<str>`, byte arrays, …), a file map's `VarBytes` (D58)
    and the 64- and 128-bit integers (D31) inline keys automatically, by
    compile-time type identity (option (d)).
- **In types:** `BBTreeMap<K, V, S = Auto, A = Global>`, the strategy
  before the allocator (D8). Existing code is unchanged and gets inline
  keys for std string and integer keys. Other key types opt in with a
  one-line impl plus `BBTreeMap::with_strategy(WithInlineKey)`; `new()`
  stays `Auto`-only, since a generic `new()` breaks inference. There is no
  separate alias type.
- **Width:** windows are 8 bytes (`InlineWord = u64`), and nodes keep the
  first `InlineKey::WIDTH` bytes of them, 2, 4 or 8. Under `Auto`: heap
  byte strings, whose ties read the heap, 8 in leaves and 4 in internal
  nodes (D43); byte arrays 2 (D32); `u64`, `i64`, `u128` and `i128` 2
  (D31). At 8 bytes, real neighbours tie 24–70% of the time; at 16 bytes,
  1–20% (04-inline-key.md Q7).
- **The trait gives windows:** `inline_key_at(offset)`, bytes
  `offset..offset + 8` of an encoding that orders like the key, so that a
  node can skip the bytes its keys share (05-fence-key.md, "The trait").
  Windows stay 8 bytes for every key type; nodes keep `WIDTH` of them.
  It gives a hash tag too, if `HAS_HASH_TAG` (D36), compares ties from
  where keys differ, if `HAS_CMP_FROM` (D43), and compares itself against
  what every key shares (`cmp_prefix`, D50).
- **Why not a plain trait with a default:** "default for every type, override for
  `String`" needs specialization, which is nightly-only and years from stable.
- **TODO:** when specialization is stable, switch to it, and drop the std-type
  list and `WithInlineKey` (tracked in 04-inline-key.md).

**D19. In-node search depends on the key type.** (2026-10-07)
- **Keys with no drop glue and ≤ 16 bytes** (integers, `char`, small tuples):
  branchless binary search.
- **Everything else** (`String`, `Vec`, `Box`, large arrays): plain binary
  search that stops early. No linear scan for them.
- Chosen at compile time (`needs_drop`, `size_of`), at no run-time cost. Known
  misfire: `&str` and `&[u8]` look like plain data.
- Why: on keys whose comparison reads the heap, the branchless chain serializes
  every heap miss. Plain binary search was 10–35% faster than branchless on
  them, with integers unchanged.

**D21. Node size per kind.** (2026-10-07)
- **Internal node size** depends only on the key: the smallest multiple of
  1 KB holding ≥ 12 keys and 13 edges.
- **Leaf node size** depends on key and value: ≥ 12 entries plus the head.
- Why: internal nodes hold no values (D17), so one size for both kinds would
  make internal nodes of large-value maps huge (u64 → `[u8; 1000]`: 13 KB,
  ~800 keys).

**D22. Removal: physical, rebalanced bottom-up, nodes at least 1/8 full.** (2026-10-08)
- **No tombstones.** `remove_entry` and `pop_first`/`pop_last` hand the key
  back by value, a generic `K` has no sentinel, and a tombstoned key would
  stay alive, heap memory included, until some compaction, unlike in std.
  Removing costs one shift in the leaf, as inserting does (D16).
- **Underfull** means fewer than `max(capacity / 8, 4)` keys, for a node below
  the root, by its kind's capacity: 7 keys for a u64→u64 leaf of 63, 4 for
  a `String`→u64 leaf of 25. The floor of 4 bounds how deep the leaves
  lie: see the last bullet.
- **Bottom-up, like insert (D11).** On the way back up, each level fixes the
  child it went into if that child is underfull:
  - it merges the child with a sibling (the left one if there is one) when
    both fit in one node with their separator;
  - otherwise it moves one entry over from the sibling. A sibling too full
    to merge with holds more than `capacity - min_len` keys, so one entry
    restores the minimum, and after at most `min_len` such steals in a row
    the two fit. Moving more at once, to even the two out, is not worth its
    code at this threshold.
  A merge can leave the parent underfull in turn. An internal root left
  without keys is popped; an empty leaf root stays allocated, as in std.
- **A separator is replaced by its successor (D17).** The first key of the
  leftmost leaf right of it moves up, and that key's value becomes the leaf's
  head, so values never leave their leaf.
- Why not std's ~1/2: too strict. At 1/2, a merge yields an almost full node
  and a split leaves halves right at the threshold, so churn at a node
  boundary splits and merges over and over; at 1/8, a split half takes about
  3/8 of a node's worth of removals to underflow.
- Cost: after adversarial removals, nodes may be 1/8 full, up to 4× the
  memory per entry of a half-full guarantee.
- Consequence: a non-root internal node has at least 5 children, so the
  leaves lie at most 23 levels below the root: one level more would need
  2·5^23 KB of leaves, more than 2^64 bytes. Every node stack has a slot per
  level (D24).

**D23. Iteration: a cursor per end, each a node stack (D24).** (2026-10-08)

- **A position is an edge of a leaf.** Past a leaf's last entry, the next
  one is the separator of the lowest node on the node stack with a key beyond
  it, and its value is the head of the leftmost leaf right of it (D17).
  Separator keys live only in internal nodes, so crossing a leaf boundary
  visits an ancestor whatever we do: leaf-to-leaf links would not help.
  `next()` never calls `Ord` (D11).
- **Each end is a node stack** (D24): it goes up its own levels to the
  separator and down to the next leaf. No end walks down from the root again.
- **Lazy:** an end that has not started holds a detached node stack, which
  writes nothing; it starts at the first or last entry on its first move, so
  iterating from one end never descends the other. A range positions both
  ends when it is made, for std's panics on bounds out of order.
- **In place:** a cursor is updated field by field, never rebuilt and copied
  over. A copy reads it with 64-byte loads right after narrow stores, which
  can't be forwarded: the load waits for the stores to retire, and so for
  every older cache miss. Over many small maps, that serialized one map's
  misses behind the previous one's: one forwarding conflict per map,
  28.2 → 11.5 ns per entry for 1M 4-entry maps once removed.
- **Sizes:** `Iter` 560 B, `Range` 552 B (D33, D35; std's `Iter`: 72 B).
- **Mutable walks** (`IterMut`, `ValuesMut`, `RangeMut`) give their
  node stacks a `ValMut` borrow type, as std's give their handles: the node
  stacks copy, so both ends hold some, and only read the structure and keys;
  handing out a value as `&mut V` is unsafe. The walks vouch for handing out each value once: their ends move
  toward each other, and stop by a count (whole map) or where they meet
  (range).
- **Owning iteration** (`DyingWalk`, for `IntoIter` and for dropping a
  tree, which is dropping one) takes the tree apart as it goes: each entry
  is moved out of its slot, nothing shifts and nothing rebalances, and an
  end frees each node as it goes up out of it, past its last edge (its
  first, going backward); the nodes on the node stack where the ends meet go
  last. Node stacks of the `Dying` borrow type copy, so both ends hold some;
  moving an entry out and freeing a node are unsafe, vouched for by the
  walk. Entries without drop glue are skipped a leaf at a time when the map
  is dropped. 1M entries, one run (ns per entry): consuming `for` loop,
  u64→u64 7.5 (std 8.9), `String`→u64 74.7 (std 74.4); dropping, u64→u64
  2.4 (std 6.6), `String`→u64 67–72 (std 71). Popping from either end
  instead, rebalancing as it went, took 15.1 and 91.9.
- Measured, one run each, mimalloc, Ryzen 9 9900X (ns per entry;
  per scan for `range64`):

  | workload | bbtree | std |
  |---|--:|--:|
  | iterate, 10M u64→u64 | 9.49 | 14.4 |
  | range64, 10M u64→u64 | 843 | 1174 |
  | iterate, 1k u64→u64 | 2.91 | 0.56 |
  | range64, 1k u64→u64 | 211 | 68 |
  | iterate, 1M maps × 4 (`str-json`→`struct32`) | 14.3 | 4.7 |
  | iterate, 100k maps × 32 | 13.1 | 6.9 |
  | iterate, 1M real JSON objects | 17.9 | 6.0 |

- **Open:** small maps are 3–5× std, from instruction count (about 72 per
  entry). A range is copied after both ends are positioned
  (`take` moves it, for one), at about 15 forwarding conflicts per scan; it
  can't wait to position them, having no type for the bounds' key to keep
  them in.

**D24. Node stacks: a node pointer per level from the root, shared by every operation.** (2026-10-08)

- **One type** (`tree/node_stack.rs`): a stack of the nodes from the root down
  to a position in one of them, the root first, and the index taken at
  each, as a `u16`. It stands for a borrow of the whole tree, which its borrow type
  says: shared, value-mutating and dying node stacks copy, and an exclusive
  node stack (`NodeStackMut`) changes the tree through its bottom node, from
  which nothing leads up, and holds the `&mut` of the root to raise or lower
  the tree (D52). It keeps each node as its edge does, tagged with its kind;
  the nodes it hands out are plain handles (D14).
- **Who uses it:**
  - an insert that reaches a full leaf searches down one (D37), inserts at
    the leaf, carries splits up the node stack, then goes back down through
    the halves that took the new entry, so the node stack ends at it;
  - a removal goes up the node stack from where the entry came out, fixing
    each underfull child (D22) and stopping at the first level that loses no
    key; a separator's successor comes up from the leftmost leaf right of
    it;
  - an entry keeps the node stack its search found: reading or changing it
    goes straight to the bottom node, removing or inserting goes up the node
    stack;
  - each end of a walk is one (D23), and a range's bounds are searched
    with one;
  - point lookups and inserts into a leaf with room hold only the current
    node (D32, D37).
- **Depth, counted by the node stack.** Nothing records how tall a tree is. A
  node stack knows how deep it went, and a node's kind comes from its tag
  (D52). A node above a node stack's bottom has the edge down, so it is
  internal: going up returns it as one, reading no tag, and going up from the
  root raises the tree first, the one way inserts, joins and builds grow it
  (`NodeStackMut::ascend_raising`). Work that goes back up counts from where it
  started: an insert's split numbers the levels it climbs from the leaf,
  which a new root above leaves alone, and a removal's fixing goes by
  depth, which merges keep until the root lowers, after them. Clearing
  prefixes before a cut reads the nodes on the node stack, and a bulk build
  fits the nodes a new leaf closed with the node stack that hung it (D25).
  Only a cut counts how deep each part's leaves lie, to count the shorter
  part's entries (`Tree::leaf_depth`). A slot read
  checks its depth against the levels on the node stack, at most 24, which
  also keeps it in bounds. Measured against D52 at 10M, one run each, in
  instructions per operation: lookups and inserts unchanged, removal 689 →
  693, `range64` 1,669 → 1,632, iteration 6.2 → 5.6 per entry; times within
  run-to-run noise.
- **Why pointers, not indexes.** A node stack of indexes is valid under
  the same condition as one of pointers, that nothing else changes the
  tree, which `&mut` already gives; it only turns a broken condition into
  a panic, and costs a walk down from the root at each use.
- **24 slots** (`MAX_DEPTH` 23, from D22's floor of 4), 248 B. The slots are
  uninitialized until written; a node stack is made detached and started where
  it stays, and is never moved once written inside the tree code: moving one
  copies it with wide loads, which wait on the narrow stores that wrote it
  (D23). Passing the node stack by value to `remove`, wrapping it in a search
  result, or returning it from a constructor each did, and made a 1k `get`
  take 31 ns instead of 10.9, and `pop_first` 1.6× as long. An entry and
  an iterator still move when returned.
- **Rejected:** 8 levels in place and a `Vec` for the rest, which made a node
  stack 112 B (`Iter` 248 B). Choosing the storage at every level cost 50–70
  instructions per operation, and `get` +13% at 10M, where fewer
  instructions let more independent lookups overlap.
- Measured, one run each, against node stacks of indexes and recursive
  insert and removal (ns per call):

  | | 10M u64→u64 before | after | std | 1k u64→u64 before | after | std |
  |---|--:|--:|--:|--:|--:|--:|
  | insert_random | 171 | 164 | 227 | 15.9 | 16.6 | 10.2 |
  | get_hit | 161 | 164 | 272 | 10.9 | 12.8 | 26.3 |
  | remove_random | 227 | 178 | 274 | 28.4 | 30.1 | 39.9 |
  | entry_upsert | 170 | 151 | 240 | 31.5 | 28.1 | 31.7 |
  | pop_first | 27.2 | 16.3 | 21.0 | 14.0 | 9.6 | 10.8 |


**D25. Bulk changes: a builder of full nodes, fed by draining in order.** (2026-10-08)

- **`TreeBuilder`** (`tree/build.rs`) builds a tree from entries in
  ascending key order, comparing no keys: each node fills before the next
  starts beside it, and `finish` tops up the right border to the minimum
  (D22) from the full left siblings. The tree is well formed throughout but
  for that border, so a panic part-way takes it apart as any other.
- **`collect` and `from`** sort, keep the last of equal keys (as std), and
  build. 10M u64→u64, one run: 2.02 vs std's 2.86 ns per entry, 16.3 vs
  18.2 bytes per entry.
- **`append`** drains entries in order by value from the front (D23's
  owning walk; no `Ord`), and rebuilds with the builder, keeping std's
  observable behavior: which key and value survive, drop order, and what
  is left after a panic.
- **`retain` and `extract_if`** remove in place, as std's do: the walk is a
  node stack at the next entry, and each removal tracks where that entry
  goes through its merges and steals, as the slot it lands in
  (`NodeStackMut::remove_and_go_to_next`, `tree/remove.rs`), which std's
  parent pointers give for free.
- **`split_off` cuts the tree where its search went, as std's does**
  (`tree/cut.rs`): from the leaf up, each node on the node stack keeps what
  lies left of it, and what lies right goes into a new node, after the part
  cut off below; then both sides of the cut are brought back to the
  minimum fill from the top down (`tree/cut.rs`, which the builder's
  right border shares): each node on the border gets one key more than its
  minimum, by a merge with its sibling or keys moved over from it, so that
  a merge below leaves it at its minimum. A separator at the start of the
  right tree comes down into its first leaf that way. The lengths come
  from counting the shorter tree's entries a node at a time, as std's do.
  1M u64→u64, one cut (µs): at the median 12 000 by popping before → 95
  (std 320); at 1%, 2 400 → 3 (std 5).
- **Cost:** `append` O(n + m) as std's; `retain` O(n) and `extract_if`
  O(log n) plus the part it touches, as std's; `split_off` O(log n), plus
  the shorter tree's nodes to count. A drained entry costs about a step of
  a walk.

**D26. Per-node prefix stripping: a node's inline keys skip what its fences share.** (2026-10-08)
Design and measurements in 05-fence-key.md.

- **What a node stores:** `prefix_len`, exactly what its fences (the
  separators beside it in its ancestors) share, along the tree's borders
  what every key shares (D50), and its keys' windows past it as their
  inline keys. No fence key is stored
  (D10).
- **For every key with inline keys:** the trait gives windows of an encoding
  (D18), so newtypes get it as std byte strings do.
- **Only a separator change moves a fence,** never an insert into a node.
  The change itself reads no key: a node that must shrink takes its
  parent's prefix, by a shift filled from the parent's separator. Once the
  entry is counted, the nodes whose fences changed fit them, reading their
  keys.
- **Exact, not "at most":** a prefix is a function of the fences, so there
  is no gate and no growth rule, and rebalanced nodes keep their prefixes.
  Against prefixes grown only where inline keys tie, that costs 4–23% on
  inserts, gets and removals of keys that rarely share prefixes.
- **Where a fence is missing,** on the root and the borders, the tree's
  shared prefix stands in for it (D50).
- **Measured:** 24–34% faster at 1M on keys with shared prefixes (paths,
  addresses), 9–12% on Wikipedia titles' inserts and gets; on random strings,
  inserts about 25%, gets 5–10% and removals 21–23% slower than without
  stripping.

**D28. Branch early, loop late: a node's kind is decided once per step.** (2026-10-09)

- **The rule:** a traversal decides whether a node is a leaf or internal
  once, where it reads the node's page from an edge, a node stack or the root
  (`Node::at`, the one cast, from the pointer's tag, D52), and the code
  after it is for that kind. Likewise a choice fixed for a whole loop, such as the end a
  border is on, is made before the loop, as a type.
- **Enforced by the types:** each kind is its own type (D51), and what
  depends on the kind, a node's capacity and where its slots are, is in
  that type's code alone. Deciding inside each method instead had a descent
  decide it three to six times per level, and a steal or merge five to
  eight times.
- **What it takes:** a descent decides the root's kind once, then loops
  over internal nodes, the leaf's search after the loop; going down an
  edge, its tag tells the child's kind. Code for
  either kind of children (`Siblings`, prefix fits) goes through `Child`,
  decided once per node (`has_leaf_children`); a walk's cursor reads its
  leaf edge without deciding (its invariant); `NodeEnd` is a type
  (`FirstEdge`, `LastEdge`); `append` builds through one `extend`.
- **Why:** the inline-key count of a node with 8-byte inline keys was one
  `filter().count()` in two arms, a leaf's and an internal node's; where
  their capacities differed, LLVM summed the compares' bits one at a time.
- **Measured** against kinds decided inside each method, medians of three
  interleaved runs (ns per op; per entry for `iterate`, per scan for
  `range64`), instructions per op in brackets:

  | cell | before | after |
  |---|--:|--:|
  | memtable16 → bytes100, 1M `get_hit` | 355 [1,024] | 300 [593] |
  | `str-json` set, 1M `get_hit` | 305 [1,244] | 284 [702] |
  | `u64`, 10M `iterate` | 5.7 [37] | 5.2 [23] |
  | `u64`, 10M `range64` | 740 [3,056] | 707 [2,434] |
  | `u64` with inline keys, 10M `insert_random` | 142 [435] | 134 [415] |
  | `u64` with inline keys, 10M `remove_random` | 269 [640] | 254 [596] |

  The other headline, key and real-key lookups and inserts stayed within
  2%, with as many or fewer instructions.
- **Open:** a single 8-byte count over 28 to 31 slots, as a `String` set's
  31-slot leaf has, still sums the bits one at a time (31 slots: 222
  instructions, 25: 34).

**D29. Leaves with inline keys pair each key with its value.** (2026-10-09)

- **Layout** (`layout.rs`): with inline keys, a leaf's keys and values are
  `(key, value)` entries after the inline keys, the head in an entry before
  entry 0, its key's place the separator's key (D49). Internal nodes keep no
  keys (D49).
- **Why:** a search runs over the inline keys, and in a leaf it then reads
  the key it ends at, which brings its value along.
- **Cost:** an entry pads its key to its value's alignment, and the extra
  entry takes one key's room: `String` → `u64` holds 24 keys instead of 25,
  `u64` with inline keys 55 instead of 56, `u128` with inline keys 28
  instead of 38.
- **Measured** on D28's tree, medians of three interleaved runs (ns per
  op):

  | cell | separate | leaves paired | both paired |
  |---|--:|--:|--:|
  | `hash32`, 10M `get_hit` | 312 | 298 | 316 |
  | `str-json`, 10M `get_hit` | 426 | 418 | 444 |
  | `u64` with inline keys, 10M `get_hit` | 139 | 128 | 134 |
  | `u64` with inline keys, 10M `insert_random` | 133 | 150 | 145 |
  | `u64` with inline keys, 10M `remove_random` | 252 | 246 | 260 |
  | `u64` with inline keys, 10M `range64` | 715 | 742 | 739 |
  | `u64`, no inline keys, 10M `get_hit` | 199 | 179 | 181 |
  | `u64`, no inline keys, 10M `insert_random` | 165 | 180 | 180 |

- **Internal nodes are not paired:** it cost lookups what pairing leaves
  gains (`hash32` DRAM fills per lookup: 11.4 separate, 8.3 leaves paired,
  10.9 both). An internal search reads only an edge after its inline keys
  (D32). Inserts are slower in every paired layout.

**D30. The insert descent has no per-node append check.** (2026-10-09)

- **What:** `insert` and `entry` search each node as a lookup does. No node
  first checks whether the key goes after its last key and then straight
  down its last edge. Ascending inserts take D45's last-leaf shortcut
  instead.
- **Why:** a shortcut for one workload, ascending keys, that every other
  insert pays for: with inline keys, the check took the key's window at
  each node's prefix and compared it with the last inline key, 41
  instructions more per random insert at 10M. It was also all of what
  looked like inline keys' loss on sequential inserts.
- **Measured** with prefetch off, one run, ns per insert, instructions in
  brackets:

  | cell | with the check | without |
  |---|--:|--:|
  | `u64`, 10M `insert_sequential` | 8.2 [185] | 44.5 [473] |
  | `u64` with inline keys, 10M `insert_sequential` | 13.3 [330] | 40.1 [481] |
  | `u64`, 1k `insert_sequential` | 4.9 [146] | 14.4 [256] |
  | `u64` with inline keys, 1k `insert_sequential` | 7.8 [233] | 11.6 [249] |
  | `u64`, 10M `insert_random` | 179 [442] | 169 [441] |
  | `u64` with inline keys, 10M `insert_random` | 132 [415] | 131 [374] |

**D31. `Auto` gives the 64- and 128-bit integers inline keys.** (2026-10-09)

- **What:** `Auto` recognizes `u64`, `i64`, `u128` and `i128` by their type,
  as it does std byte strings, and uses their own `InlineKey` impls:
  2-byte inline keys, as `WithInlineKey` gives them. Other integers have
  no impl and get none; a newtype around one of the four gets none either,
  unless it implements `InlineKey` and the map uses `WithInlineKey`.
- **Why:** the count over 2-byte slots runs in explicit AVX-512 vectors
  whatever the capacity, and since D28 no count is compiled for two node
  kinds at once. Without D30's append check, sequential inserts gain too.
- **Measured,** `u64` → `u64` without and with inline keys, on D28's
  layout, `insert_sequential` with D30 applied, prefetch off, ns per op,
  one run (three for the 10M `remove_random`, `pop_first` and `range64`):

  | cell | none | 2 B |
  |---|--:|--:|
  | 10M `get_hit` / `get_miss` | 198 / 180 | 138 / 96 |
  | 10M `insert_random` / `insert_sequential` | 183 / 44.5 | 133 / 40.1 |
  | 10M `remove_random` / `pop_first` / `range64` | 261 / 33.1 / 708 | 252 / 34.1 / 728 |
  | 1k `get_hit` / `get_miss` | 14.2 / 12.7 | 5.9 / 4.1 |
  | 1k `insert_random` / `remove_random` | 16.3 / 46.1 | 14.5 / 43.2 |
  | bytes per entry, map / set | 23.4 / 11.8 | 26.5 / 14.9 |

  Dense keys (`u64-seq`, 0 to N) share their top bytes, all zero for
  small ones: the tree keeps them, and the root and the borders skip them
  (D50), where they have no fences (D26).
- **Cost:** 2 bytes more per entry, 3 at the leaves' fill: 13% more for
  `u64` → `u64`, 26% for a `u64` set. A node holds 55 `u64` keys
  instead of 63.

**D32. Internal nodes split keys from children.** (2026-10-10)
The experiments behind D32–D45 are in [06-cost-model.md](06-cost-model.md).

- **Internal nodes keep keys and child pointers in separate arrays,** where
  they keep keys at all: with inline keys they keep none (D49). Leaves with
  inline keys still pair each key with its value (D29). An internal search
  ends at a child pointer and rarely reads a full key, so child pointers
  should share cache lines with each other, not with keys. A leaf search
  reads the key it ends at and then its value, so pairing still pays there.
- **Nodes stay aligned to 1 KiB (D15).** Tried 64 bytes: it only saves
  memory under glibc's malloc. mimalloc puts 1 KiB blocks on 1 KiB
  boundaries either way, and an A/B showed the same instructions and RSS,
  with timing differences within run-to-run noise.
- **Point lookups keep no node stack.** `get` and `get_mut` hold only the
  current node. Inserts, entries, removals and range bounds keep node stacks.
- **Consuming `fold`/`rfold` run one counted loop per leaf** and update
  the cursor once per leaf; `next` still crosses leaves through the
  boundary helper. An unbounded range end is set up only when first used;
  explicit bounds are still searched when the range is made.
- **A split at a node's end leaves the arriving side
  `min(2 × minimum, capacity / 2)` entries;** the old node keeps the rest.
  A split in the middle stays even. Sorted inserts leave full nodes
  behind, not half-full ones, and the headroom above the minimum keeps one
  removal from undoing the split. Inserts that fit pay nothing.
  At 10M `u64`: sequential insert 37.9 → 23.8 bytes/entry (−37%), random
  insert 27.0 → 27.2 (+0.7%).
- **Fixed byte arrays get 2-byte inline keys under `Auto`.**
  `[u8; 16]`, `[u8; 20]` and `[u8; 32]` hold their bytes in the node, so
  a tie costs a load from the same node, not a heap read; narrower
  summaries mean fewer cache lines per search. Heap strings keep 8 (D43
  narrows their internal nodes to 4); `WithInlineKey` keeps each impl's
  width. 10M `hash32`, 8 → 2 bytes: hits 318.1 → 300.4 ns, misses 282.1 →
  207.4, inserts 336.0 → 310.9; DRAM fills per hit 10.7 → 6.1.
- **Search details.** A byte-string query's windows are read from its own
  bytes, not from a 128-byte zero-padded copy. On x86-64 without POPCNT, a
  sorted SSE2 mask that fits one 64-bit word gives both bounds directly,
  with no software population count. AVX-512 and POPCNT targets are
  unchanged.

**D33. Cheaper fitting inserts and cached range runs.** (2026-10-10)

- **A fitting insert has its own short path.** Allocation and split
  propagation move to an outlined function with its own stack frame
  (integer insert frame: 2,216 → 360 bytes). Growing a node keeps the
  max-valued summary padding already there, and an insert at the end skips
  the empty tail move. Replacing a value borrows it in place instead of
  copying the node stack. 10M ordered `u64` insert: 36.0 → 34.0 ns; random
  insert unchanged.
- **A range iterator caches a checked leaf interval.** Repeated
  `next`/`next_back` step inside it; switching direction, crossing a leaf
  or folding first syncs the node stack; it costs `Range`/`RangeMut` 40 bytes.
  10M `range64`: `u64` 736 → 685 ns (−7%), JSON 1302 → 1204 ns.

**D34. Align leaf pairs so a key and its value share a cache line.** (2026-10-10)

- **What:** when a leaf entry (key and value) is a power of two up to 64
  bytes, entries start at a multiple of its size: `u64` → `u64` pairs
  (16 B) at byte 144, not 136; `String` → `u64` (32 B) at 256, not 232.
- **Why:** a quarter of `u64` pairs and half of `String` pairs crossed a
  cache line, so a hit could load one more line just for the value.
- **Cost:** none in capacity or allocation. The page size and
  `(capacity + 1) × entry size` are multiples of the entry size, so
  rounding the start up only uses tail slack.
- **Measured** at 10M: DRAM fills per hit 3.86 → 3.68 (`u64`) and
  9.18 → 8.80 (JSON), same instructions. Latency barely moved: `u64` hits
  131.5 → 129.6 ns, JSON hits 348.7 → 353.5 ns.
- **Also:** frees 48 bytes in a `String` → `u64` leaf, which D36 uses.

**D35. Whole-map iterators reuse the range iterator's leaf run.** (2026-10-10)

- **What:** borrowed whole-map iterators wrap D33's range walk plus an
  exact remaining count. `next`/`next_back` step inside the cached leaf
  run; a consuming fold hands over to D32's counted per-leaf loop.
  Owning iteration and drop are unchanged.
- **Cost:** 40 bytes in `Iter`, `IterMut` and `Keys`.
- **Measured,** manual `next()`, 2M entries, baseline x86-64: `u64`
  4.50 → 3.99 ns/entry (−11%), `hash32` 9.36 → 8.73 (−7%). The native
  10M fold is unchanged (2.21 ns/entry).

**D36. Leaves keep 1-byte hash tags in spare padding.** (2026-10-10)

- **What:** a hash tag is a byte per key that equal keys share, so that
  keys whose tags differ are different. A leaf stores its keys' tags where
  existing padding holds the whole tag array before the head's key (D49),
  as in `String` → `u64`. Where the head's key took the padding's end, the
  leaf gives up an entry to keep them: `String` → `String` and `String` →
  `u128` 17 → 16, a latched `String` → `u64` (D54) 24 → 23. Where tags
  never fit, as in `String` → `()` or `&str` → `u64`, it has none, and its
  plan says so. Allocation size doesn't change. `KeySlots` moves tags with
  their keys.
- **Whose tags:** a recognized heap byte string's is its length,
  saturated at 255 (`[u8]`'s `InlineKey::hash_tag`). Under
  `WithInlineKey`, a key's impl gives its own, at any width, if it sets
  `HAS_HASH_TAG`; std byte strings' impls give their lengths, so a newtype
  can take its string's. Integers and byte arrays have none: their ties
  are cheap.
- **User code while keys move:** a leaf computes a key's tag as it writes
  the key, while it moves keys too (a head rejoining the entries in a
  merge). An impl's `hash_tag` runs there behind an abort-on-unwind guard
  (D48). A tag that breaks the contract gives wrong answers, never
  undefined behavior.
- **Why a byte:** keys rarely reach 255 bytes, so a byte tells as many
  lengths apart as two did, and half the array fits where two bytes
  didn't: with 2-byte tags, `String` → `u64` gave up an entry (24 → 23),
  and `String` → `String` and a latched `String` → `u64` kept none.
  Saturating, not wrapping (`len % 256`), keeps every byte string's tag
  below 255 an exact length, which D57's unlatched copy relies on.
- **Search:** tags are read only when the inline keys leave more than one
  candidate. No tag matches: absent. One: compare that full key. Two:
  compare the upper key, then the lower only if the query sorts below it.
  Three or more: the old binary search, so keys of one tag never cause a
  linear scan. The search confirms each candidate a tag picks against
  the full key. D57's unlatched copy alone takes an equal tag for an equal
  length, for a key that ends within its inline key, and only below 255:
  a saturated tag tells nothing of length.
- **Why:** path keys tie in their 8-byte windows inside leaves. On 1M
  paths, heap key reads per lookup fall from 1.98 to about 1.0 for hits
  and 2.18 to 0.20 for misses.
- **Cost:** inserts maintain a third array: 10M path insert +2.0%. JSON
  leaves never have ambiguous runs, so JSON pays upkeep and gains nothing
  on lookups; its timings stayed flat.
- **Measured** at 10M paths: hits −10.7%, misses −31.2%. From 2-byte to
  1-byte tags, 10M, ns per lookup, one run: paths → `u64` hits 503 → 497,
  misses 398 → 378; JSON → `u64` hits 236 → 234, misses 155 → 159; a
  latched paths → `u64`, now tagged, hits 646 → 524, misses 618 → 446;
  paths → `String`, now tagged, hits 578 → 524, misses 546 → 415. A
  key's own tags, 10M paths as `String` under `WithInlineKey`, untagged
  → tagged: hits 695 → 606, misses 679 → 462; keys without tags run the
  same instructions as before (`memtable24`: 470.7 per hit).
- **Pitfall:** the summary-view wrapper is forced inline. Outlined, as
  LLVM did once it had a second caller, its population count grew from 26
  to 244 instructions, and JSON hits got 25% slower.

**D37. Inserts record a node stack only when the leaf is full.** (2026-10-10)

- **What:** an insert first descends without a node stack, like a lookup, and
  either replaces an existing value or inserts into a leaf with room. If
  the leaf is full, an outlined function searches again, this time
  recording the node stack, and splits.
- **Why:** most inserts fit; at 1M, 2.6% of `u64` inserts and 5.8% of
  string inserts reach a full leaf. The rest shouldn't record parents and
  indexes they never use.
- **Safety:** the second search uses its own result, not the first one's:
  an inconsistent `Ord` may find an existing key or another leaf the second
  time. All comparisons happen before anything changes, and the key is
  still dropped before the value is replaced.
- **Measured** at 10M: random insert `u64` −4.1%, JSON −19.4%, paths
  −4.8%; ordered `u64` −8.8%. DRAM fills unchanged: the saving is
  instructions (JSON 824 → 649), mostly from helpers the old path called
  at every level.

**D38. A leaf lookup handles zero and one candidates directly.** (2026-10-10)

- **What:** when the summaries leave no candidate, the lookup returns
  absent at once. One candidate needs only equality, not an ordered binary
  search. A fixed byte array compares all its bytes together (`[u8; 32]`:
  one 32-byte load, XOR and test, no `memcmp`); a heap string skips the
  known prefix. A borrowed query must still match the array's full length,
  trailing zeros included. Custom strategies keep their comparison.
- **Also:** extends D36 from one to two matching tags.
- **Measured** at 10M `hash32`: hits 271.4 → 245.6 ns (instructions
  527 → 431); misses 193.0 → 195.6 ns.

**D40. Baseline x86-64 packs 33–64 two-byte summaries into one mask word.** (2026-10-10)

- **What:** without POPCNT or AVX-512, nodes with 33–64 two-byte summaries
  pack each pair of SSE2 comparison vectors into one bit per slot. Both
  bounds then come from trailing-one counts on one 64-bit word. Other
  widths, capacities and targets keep their code.
- **Why:** above 32 slots, LLVM emitted two software population counts,
  each with a multiply.
- **Measured,** 2M entries, baseline x86-64, System allocator: `u64` hits
  94.3 → 67.1 ns, misses 55.6 → 41.5, inserts 171.9 → 142.5; `u128` hits
  148.3 → 109.1, misses 74.9 → 55.7, inserts 251.0 → 217.9.

**D41. Byte queries are known at compile time; the search adapter is always inlined.** (2026-10-10)

- **What:**
  - A recognized byte-string query holds a plain slice view instead of an
    `Option`; the type checks that recognize it also guard its accessor.
    Unknown borrowed query types still use `Ord`.
  - The thin `search` adapter of each node kind, which only picks the
    search body by the plan, is `#[inline(always)]`. The search body is
    inlined for fixed-width keys and left to LLVM for heap strings
    (`NodePlan::inlines_node_search`), except in an insert's search, which
    always inlines its leaf's (`Leaf::search_always_inlined`): out of line, it cost
    a random JSON insert 40 instructions more (D51).
  - A lookup that ends at a separator reads its value out of line
    (`Found::separator_key_value`): inlined, the walk down to the head sat
    in every descent's loop (D51).
  - The shared-prefix check (D50) on heap strings compares 32 bytes at a
    time, inline.
  - Native 8-byte leaf summaries use an equality mask.
- **Why:** with the `Option`, an outlined helper re-tested for null and
  kept a generic `memcmp` fallback. Without forced inlining, every internal
  level returned its search's state through memory: about 14 memory
  operations per level for `String`.
- **Measured** at 10M, all four together: JSON hits 343.8 → 334.4 ns,
  misses 282.3 → 249.4; path hits 601.0 → 585.0, misses 450.4 → 432.9;
  `hash32` hits 224.8 → 185.9, misses 169.4 → 140.9. The binary's code is
  20 KB smaller.
- **Pitfall:** with a plain `#[inline]` hint instead, the same change made
  JSON hits 9% slower (374.4 ns).

**D42. Cached scan runs hold key pointers instead of indexes.** (2026-10-10)

- **What:** in a paired leaf with nonzero stride, a cached run (D33, D35)
  keeps a current and an end key pointer, in the space the redundant leaf
  handle used. Each step advances one pointer: no leaf load, no index
  arithmetic. Plain and zero-size layouts keep indexes. The node stack still
  holds the leaf, so the index can be recovered at a boundary or a
  direction change. Iterator sizes don't change.
- **Safety:** only the key end serves as the end marker. In a full `u64`
  leaf it is one past the page, which is allowed; the value end would be 8
  bytes past it, which isn't. A value pointer is formed only once the run
  proves an entry is left. Mutable iterators keep the same interval
  argument, so each `&mut V` is handed out once.
- **Measured:** 10M `u64` `range64` 688.2 → 664.5 ns (the per-item loop
  goes from 18 to 15 instructions). Manual `next()`, 2M, baseline x86-64:
  `u64` 3.92 → 3.52 ns, `hash32` 8.74 → 7.52.

**D43. Heap-string internal nodes use 4-byte summaries.** (2026-10-10)

- **What:** under `Auto`, heap byte strings keep 8-byte summaries in
  leaves and use 4-byte ones in internal nodes. Internal capacity rises
  25 → 28 for `String` and 31 → 36 for `&str`/`Box<str>`; pages stay
  1 KiB. A window an internal node hands out, which may go into a leaf, is
  read again from its key, the head below (D49), for all 8 bytes. Fixed
  arrays keep their width.
- **Keys with their own impl:** under `WithInlineKey`, an 8-byte impl that
  sets `HAS_CMP_FROM` gets the same 8 / 4, and its ties compare with its
  `InlineKey::cmp_from`, past the bytes known equal, as std strings' do
  (`SearchBytes::cmp_from`, which std byte strings' impls share). The
  windows its internal nodes read again call its `inline_key_at` while
  keys move, behind an abort-on-unwind guard (D48). An impl without it
  keeps its width in both kinds and compares ties with `Ord`.
- **Why:** about 94% of lookups pass through an internal node without
  ending there, so the extra ties from 4 bytes cost little (JSON: +0.02
  heap reads per hit), while the summaries shrink from 4 cache lines to 2.
  In leaves, 4 bytes would more than double JSON's heap comparisons, so
  leaves stay at 8. The ties are cheap only because they compare from
  where keys differ: from byte 0, each reads a key's first lines, which
  path keys share. So 4 bytes go with the comparison, never without it.
- **Search on baseline x86-64:** 17–32 four-byte summaries use one SSE2
  packing step and trailing-one counts; above 32, the compiler's count
  (04 Q6). A second packing step for 33–64 cut instructions but was slower
  on borrowed JSON, so it was dropped.
- **Measured,** 10M native: JSON inserts 314.1 → 282.5 ns, hits
  343.8 → 320.4, misses 282.3 → 235.4; paths about 1.3% faster. Baseline
  x86-64, owned `String`: JSON hits 408.7 → 390.6, misses 335.5 → 297.3.
  On 10M paths, with hash tags (D36), ns per hit / miss and DRAM lines per
  hit, one run each: 4 bytes inside and ties from where keys differ
  481 / 385, 17.2 lines; 8 bytes inside 565 / 427, 18.6; 4 bytes with
  ties from byte 0 661 / 482, 28.0. Paths as `String` under
  `WithInlineKey`, from hash tags only (8 bytes inside, `memcmp` from byte
  0) to `cmp_from` and 8 / 4: 589 / 459 → 575 / 457 ns, 905 → 812
  instructions and 21.6 → 19.0 lines per hit; with `cmp_prefix` too
  (D50), 486–491 / 376–389 ns against `Auto`'s 481–489 / 375–383.
- **Cost:** on baseline x86-64 with borrowed keys (2M diagnostic), path
  misses are 2.2% slower; the other borrowed cells improved about 2%.

**D45. Remember the last leaf after a split, so ascending inserts can append.** (2026-10-10)

- **What:** the map owner keeps an optional pointer to the tree's last
  leaf.
  - **Set** only by a plain insert that split, when its node stack shows the
    new key went into the last slot at every level.
  - **Used** while set: an insert first checks that the leaf has room and
    that the key is strictly greater than the leaf's last key, then
    appends with no descent.
  - **Cleared** by anything else: an equal or smaller key, a full leaf,
    or any `&mut` access that can change the tree's structure. The next
    qualifying split sets it again. `Entry` handles never set it; clones,
    `split_off` outputs and new maps start without it.
- **Safety:** the check hands back a private token, and only consuming it
  moves the key and value in.
- **Why not D30's check:** it ran in every node of every insert
  (+41 instructions per random insert). This one is a single `Option`
  test per insert while unset. For random keys it can only be set at a
  new maximum: about 17 times over 10M inserts. No counters, no tuned
  thresholds.
- **Cost:** a word in the map (std's is 24 bytes; bbtree's 40, with
  D50's shared prefix). JSON random insert +2.3% in one run; other random
  inserts within ±0.5%. Descending and middle-ordered runs get no
  shortcut.
- **Measured,** 10M ordered inserts: `u64` 31.1 → 5.3 ns (std 39.5), JSON
  121.7 → 45.3 (std 198.9), paths 185.9 → 63.1 (std 256.0).

**D46. No software prefetch.** (2026-10-10)

- **What:** the library issues no prefetch instructions but in the batch
  lanes (D53), which prefetch, through one wrapper in `tree/batch.rs`,
  only the line each lane reads next; single-key operations never do.
  There is no `prefetch` feature. Hardware prefetchers work as they do.
  AGENTS.md forbids explicit prefetching anywhere else, benchmarks and
  prototypes included.
- **Why:**
  - Prefetch hides what the layout does with memory: the same layout moved
    by ±5–10% depending only on what was prefetched, so layout changes
    can't be judged with it on, and the build users run must be the one
    measured.
  - It fetched every key line up to capacity, at every level, whatever the
    search needed. After D32's separate internal arrays, `u64` and
    `String` internal nodes prefetched their full keys, which a search
    rarely reads, and not their child pointers, which it always does.
  - Memory latency is a layout problem: put what an operation needs in
    the lines it already loads.
- **Measured** on D45's tree, 10M, prefetch off → on, one run each (the
  off and on runs were about 9 hours apart):
  - Misses got slower: `u64` 92.9 → 116.5 ns (+25%), JSON 235.9 → 263.7
    (+12%), `hash32` 138.4 → 184.5 (+33%). `hash32` hits too: 183.8 →
    195.6 (+6%).
  - DRAM fills per lookup rose 4–6×: `u64` hit 3.8 → 14.9, `hash32` miss
    3.9 → 25.5.
- **Cost:** what prefetch helps, given up: random inserts (`u64` 137.1 →
  105.0, JSON 276.7 → 240.3, `hash32` 197.9 → 158.4), `u64` `range64`
  (659.3 → 504.9) and some hits (`u64` 130.2 → 116.9, JSON 305.2 → 285.0,
  paths 563.0 → 469.3), off → on.

**D47. Node plans: every per-key-type choice in one place.** (2026-10-11)
Design and measurements in 07-node-plan.md.

- **What:** `NodePlan::of::<S, Kind>()` (`tree/node/node_plan.rs`) decides, per
  key type and node kind, everything the nodes specialize on: summary width,
  hash tags, rehydrated windows, full and tied searches,
  whole-key equality, and whether separator keys are heads (D49). One constructor per row of the table,
  each with its reason. Layouts, slots, searches and prefix upkeep read it;
  none asks what the key type is. Per-key arrays (inline keys, hash tags)
  are `KeyColumns`, which every key move moves alike.
- **Why:** each specialization was spelled where it was used, as a stand-in
  for type identity (`ByteString && width() == Two` for fixed arrays), in 26
  places, and lived in three files.
- **Zero overhead, by rule:** code derives the plan from type parameters
  where it uses it and never passes or stores it; dispatchers over
  plan-derived values are `#[inline(always)]`; `KeyInliner` and
  `NodePlan::of` stay small, as a third variant or one more arm changed
  LLVM's inlining by 9–142 instructions per operation.
- **Measured** against the tree before, instructions per operation, 33
  cells: identical on 26; `hash32` lookups −88 (LLVM now inlines the
  internal search); the rest within ±9, of which set lookups +2 (an LLVM
  speculation drops `inbounds` and keeps one null test). Debug tests 86 → 75 s, from
  narrowing type recognition before comparing type identities.

**D48. A panic in user code is not reconciled.** (2026-10-12)

- **What:** when user-provided code panics (`Ord`, `InlineKey`, `Clone`,
  `Drop`), the map does whatever is simplest. Calls into user code are not
  ordered around changes, and a panic need not leave a valid map behind.
- **Why:** after a panic in user code there is no state the map can
  gracefully reconcile, and designing around it constrains every
  structural operation.
- **Still sound:** no undefined behavior, whatever user code does (D14).
  User code runs between structural changes, not inside one, so unwinding
  never leaves what unchecked code trusts (lengths, node minimums) half
  done (D26, D37). A change that must call it inside one goes behind an
  abort-on-unwind guard, as a key's hash tag (D36), and the window an
  internal node reads again from its key (D43), do. The allocator, called
  inside splits, must not unwind at all (D8).
- **Tests:** a lying `Ord` or `InlineKey` must stay memory-safe. A
  panicking one need not leave a valid map; the tests that still check one
  (`unsync/map/tests/drops.rs`, `unsync/map/tests/inline_key.rs`,
  `bbtree/fuzz/src/user_code_panics.rs`) go when a change breaks them.

**D49. With inline keys, separator keys are leaf heads, and internal nodes keep no keys.** (2026-10-13)
Design, census and measurements in 11-separator-key.md.

- **What:** in a map whose keys have inline keys, each separator's key is
  the head of the leftmost leaf right of it, beside its value (D17), in
  entry 0's key place (D29). Internal nodes keep only inline keys and
  edges, in 512 B pages (D21). A search compares inline keys, and on a tie
  reads a separator's key by walking down its right edge and first edges to
  that head. Without inline keys, internal nodes keep their keys (D17).
- **Why:** inline keys decide most internal searches (0.06–0.26 ties per
  lookup on integers, `hash32` and JSON), so internal nodes held keys that
  lookups rarely read. Without them, they hold as many children in half
  the bytes.
- **Moves:** a head's key moves with its value in splits, moves between
  siblings, merges and removals; internal nodes move only windows and
  edges, and no key moves between levels. `SeparatorSlots` holds an
  internal node's separators, keys or windows. A node keeps its separators'
  keys only without inline keys (`Internal::keys`); with them, a search reads
  them as the leaves' head keys (`HeadKeys`), walked to, and other code as
  `Internal::separator_key`.
- **Measured** against the tree before, 10M (paths 1M), ns per operation,
  one run each:

  | cell | before | after |
  |---|--:|--:|
  | `u64` hit / miss | 128 / 90 | 124 / 94 |
  | `u64` dense hit / miss | 158 / 88 | 148 / 98 |
  | `hash32` hit / miss | 181 / 135 | 160 / 145 |
  | JSON hit / miss | 311 / 249 | 276 / 179 |
  | paths hit / miss | 371 / 252 | 426 / 312 |
  | `u64`, `hash32`, JSON random insert | 146, 193, 289 | 136, 171, 241 |
  | `u64` random remove, `range64` | 249, 652 | 249, 630 |

- **Pitfall:** the tie search over walked keys doesn't inline; called on
  every search, even with nothing tied, it made `u64` lookups run 135 more
  instructions, and JSON's 77 once the internal search inlined into `get`
  (D50). A search whose inline keys leave no candidate returns before the
  call, in its caller.
- **Open:** ties walk: `u64` and `hash32` misses, and paths, which tie
  about once per lookup.

**D50. The root keeps what every key shares as its fences; border nodes take them.** (2026-10-13)
Design in 05-fence-key.md, "The shared prefix".

- **What:** the root has no parent to give it fences, so the tree keeps
  them with it, in the root page (D51, D52): the leading bytes of the
  encoding every key shares (D18), up to 64, and how many
  (`SharedPrefix`, `tree/shared_prefix.rs`). The root, and every node with no
  separator above it on a side, along the tree's borders, take its length
  as their prefix, where they had 0 (D26). Every key with inline
  keys gets it, `InlineKey` impls included.
- **Search:** a lookup, an entry, a range bound or an insert checks the
  query against those bytes once, before the root. A query without them
  sorts before or after every key and reads no node: a lookup misses, a
  range or an entry starts at the first or last leaf edge. Below, every
  node skips its prefix unchecked. Std byte strings compare 32 bytes at a
  time. A key with an `InlineKey` impl compares by its
  `InlineKey::cmp_prefix`: by default a window at a time against the
  shared bytes read as words; the std byte strings' impls 32 bytes at a
  time, as `Auto` does, which a newtype takes from its string.
- **Why the trait has `cmp_prefix`:** on 10M paths, which share 64 bytes,
  the check was all that kept `String` under `WithInlineKey`, with every
  other hook (D36, D43), from `Auto`'s speed: 540 / 423 ns per hit / miss
  with windows against `Auto`'s 481–489 / 375–391, and 486 / 376 without
  the check. The demand reads from DRAM were `Auto`'s (15.5 lines per
  hit); the 8 ascending windows of the query's first line drew 1.7 more
  lines per hit of hardware-prefetch fills past the key, which two 32-byte
  loads don't, unrolled or not. With the strings' `cmp_prefix`: 486–491 /
  376–389 ns, and 610 instructions per hit to `Auto`'s 626.
- **Upkeep, reading no key:** an insert whose key shares less shrinks the
  bytes first, and the border nodes with them, by D26's window shift; an
  empty tree starts from its first key's. Removals leave them: the rest
  share at least as much, so the prefix is correct, not exact. A cut gives
  both trees the bytes; a build checks each key as it appends; a clone
  copies them.
- **Why not per-node local prefixes,** which internal nodes of std keys
  kept where fences share too little, checking each query against their
  first key: that read a key on every search of such a node, a walk to a
  leaf head with D49, cost every internal search a header field, and made
  every key move restore and rebuild windows. One check at the top
  establishes the same for the root and the borders, where fences are
  missing, and the internal search, without the per-node check, is small
  enough for LLVM to inline into `get`.
- **Cost:** 65 bytes of the root page, none of the map (D52). Each insert
  checks its key against the bytes, inline: a length test when nothing is
  shared; starting and shrinking are out of line. The insert's append shortcut (D45) checks
  after it clears its hint, so no user code runs with the hint set.
- **Measured** against D49, 10M, one run each; instructions per operation
  repeat exactly between runs, times vary by about 10 ns (the same binary
  ran `u64` random inserts in 146 and 135 ns):

  | cell | instructions, D49 → D50 | ns, D49 → D50 |
  |---|--:|--:|
  | `u64` hit / miss | 239 / 214 → 240 / 215 | 124 / 94 → 129 / 94 |
  | `u64` dense hit / miss | 340 / 316 → 294 / 270 | 148 / 98 → 138 / 96 |
  | `hash32` hit / miss | 366 / 336 → 264 / 232 | 160 / 145 → 153 / 108 |
  | JSON hit / miss | 462 / 425 → 385 / 347 | 276 / 179 → 270 / 174 |
  | paths hit / miss | 764 / 697 → 641 / 575 | 547 / 418 → 495 / 405 |
  | paths 1M hit / miss | 935 / 867 → 642 / 575 | 426 / 312 → 375 / 325 |
  | `u64`, `hash32`, JSON random insert | 348, 427, 610 → 347, 394, 603 | 136, 171, 241 → 145, 167, 238 |
  | `u64` random remove, `range64` | 657, 1598 → 651, 1605 | 249, 630 → 249, 613 |

  Not measured again: UUIDv7-like `[u8; 16]` at 1k, 27.7 → 7.5 ns with
  local prefixes, whose root's local prefix was what every key shares.

**D51. A root, internal nodes and leaves, each its own type.** (2026-10-13)

- **What:** `Leaf` and `Internal` are page handles with the operations of
  their kind, and `Tree` owns the tree: its root page, with the root node
  and the fences as the shared prefix (D50, D52), and the rightmost leaf
  (D45). `Node`, a leaf or an internal node, as the tag of the pointer to
  it says (D52), is what an edge, a node stack or the root leads to, and the
  one place a kind is decided (D28). What both kinds share is composition
  (pages, slots, inline-key column) and `Child`, for code that works at either
  level: `Siblings` (merges and moves between children), joins and prefix
  fits.
- **Why:** leaves and internal nodes diverged (D29, D36, D49), but a handle
  generic over kind pushed both through one interface, and the root's
  state sat on a type every split half also was. With one type per role,
  each kind's layout and search change without the other's.
- **Measured** against D50, one run each (ns per op; instructions in
  brackets), 10M but paths at 1M:

  | cell | before | after |
  |---|--:|--:|
  | `u64` hit / miss | 128 / 92 [240 / 215] | 125 / 90 [211 / 187] |
  | `u64` dense hit / miss | 134 / 94 [294 / 270] | 133 / 89 [263 / 240] |
  | `hash32` hit / miss | 148 / 104 [264 / 232] | 152 / 108 [258 / 228] |
  | JSON hit / miss | 240 / 191 [385 / 347] | 238 / 187 [374 / 338] |
  | paths 1M hit / miss | 376 / 322 [642 / 575] | 375 / 292 [607 / 545] |
  | `u64`, dense `u64`, `hash32` random insert | 140, 150, 171 [347, 437, 394] | 147, 148, 174 [343, 438, 399] |
  | JSON, paths 1M random insert | 240, 421 [603, 1,187] | 248, 371 [609, 1,201] |
  | `u64` random remove, `range64` | 253, 630 [651, 1,593] | 250, 625 [649, 1,588] |

  Times vary by about 10 ns between runs of one binary. JSON inserts ran
  3% slower in each of three runs, with 6 more branches each.
- **Pitfalls:** LLVM inlined differently once one function served what
  per-borrow-type copies had: lookups went 4 instructions up until the
  walk to a found separator's head moved out of line (D41), and JSON
  inserts 40 until their leaf search was forced inline (D41).

**D52. Pointers carry a node's kind; the root page stays the root.** (2026-10-13)

- **What:** every pointer to a node, an edge, a node stack's level, a node
  handle or the tree's to its root page, says whether the node is a leaf,
  in its lowest bit, which pages aligned to 512 B or more leave 0: a
  leaf's pointer carries the tag. It is the only record of the kind: the
  node's page does not keep it, and there is one pointer type, `NodePtr`,
  which gets its tag where a page becomes a node of a kind and masks it
  off to reach the page. Following an edge decides the child's kind
  before the child is read (D28). The root node
  lives in the root page: room for a node of either kind, and after it the
  root area, the tree's shared prefix (D50); nothing records how tall the
  tree is (D24). A tree keeps
  its root page for life. Raising it moves the root node to a fresh page of
  its kind and makes the root internal above it; lowering it moves the
  root's only child up and frees the child's page; either returns the
  root page's pointer tagged with the root's new kind, which `Tree` keeps
  in place of the old one. `Tree` holds the page once the map has a tree,
  which its first entry allocates (D1).
- **Why:** a node's kind travels with every pointer to it, not as a count
  the code threads down from the root, which a latch taken hand over hand
  would have to keep right too; with the kind on the pointer, a latch knows what it latches
  before it touches the node, as bf-tree's parent flag
  (`children_is_leaf`) tells it, per edge here. The shared prefix is the
  root's fences, so it lives with the root node, not behind a pointer of
  its own. A root that never moves needs no root pointer to swap and read
  again, which bf-tree's new roots need (01-bftree.md). One record of the
  kind cannot disagree with another, and one pointer type leaves no
  untagged copy of an address to mix up with a tagged one: untagging is
  one `and` on the way to the page.
- **Cost:** a raise or lower copies a node, a page at most, and retags
  the root's pointer. Every access through a pointer masks off its tag
  first. A join moves
  the shorter tree's root to a plain page first; a cut's new part and a
  copy move their top into a root page. The root page is the larger
  kind's page and 65 B. The map is 24 bytes, std's size, down from 40.
- **Measured** against D51, 10M, one run each (ns per op; instructions in
  brackets):

  | cell | before | after |
  |---|--:|--:|
  | `u64` hit / miss | 126 / 94 [211 / 187] | 130 / 89 [218 / 192] |
  | `u64` random insert, remove | 135, 245 [343, 649] | 137, 247 [354, 689] |
  | `u64` `range64` | 626 [1,588] | 629 [1,657] |
  | JSON hit / miss | 233 / 173 [374 / 338] | 246 / 174 [383 / 345] |
  | JSON random insert | 247 [609] | 237 [622] |

  Times vary by about 10 ns between runs of one binary. Instructions rose
  by 2 to 6%: the root's kind is read from its header, a leaf's pointer
  loses its tag, and balancing reads a child's tag where D51 kept the
  level in a register.

**D53. Batches descend in async lanes, each prefetching what it reads next.** (2026-10-15)

- **What:** map `get_many` and `insert_many`, and set `contains_many` and
  `insert_many`, take fixed arrays and return one result per key in input
  order. Each key descends in a lane, an async block that prefetches the
  line it reads next (D46's exception) and yields; a private executor
  polls up to 64 lanes by a live-bit mask, more in `Option` slots, with a
  no-op waker. Nothing async is public. `extend` batches sixteen entries at
  a time; set operations that look a small set up in a large one batch 32
  lookups ahead of their iterator. Below an estimated L2 of leaf pages,
  everything goes one key at a time. Doc 12.
- **Why:** a lookup is a chain of dependent misses, and one operation's
  chain doesn't overlap the next one's. Lanes overlap them without
  allocation. A lane reads like the single-key search, so lanes extend to
  inserts and set operations without a new schedule.
- **Correctness:** inserts apply in input order and shrink the shared
  prefix just before each entry. A saved leaf length detects insertions
  that fit; a split drops that leaf's destinations and every separator;
  an insertion from the root drops them all. Trees match individual
  inserts node by node. Panics are outside the contract (D48).
- **Cost:** at 10M, 1.4–2.5× faster than one key a call, but 10–15 ns per
  lookup slower (24–30 on wiki titles) than level-by-level lanes, which
  search every lane at one depth and then follow: they stay the measured
  ceiling. Async keeps fewer misses in flight at the leaves. Single-key paths keep their compilation through
  a call-site marker and `inline(always)` on four insert helpers; doc 12
  lists what each saved.

**D54. The shared `BBTreeMap`: the unsync tree, shared through optimistic lock coupling; its tree operations are the unsync tree's own, run on the nodes it holds.** (2026-10-15)
Design, protocols and measurements in 09-concurrency.md.

- **What:** `BBTreeMap` keeps its tree with strategy `Latched<S>`, which
  gives `S`'s inline keys and a 16-byte header with a latch word
  (`node/latch.rs`). Internal nodes are read optimistically, in place,
  through raw pointers, and changed in place by writers holding them (OLC,
  `node/optimistic.rs`); leaves are latched to change, and to run a
  closure on an entry; lookups read them optimistically first (D57). Keys
  need inline keys (D49).
- **Same tree, same operations:** the shared prefix (D50), prefixes fit to
  their fences, 4-byte windows of heap strings (D43), appends to the last
  leaf (D45), a root that grows and shrinks in its page. A split, a
  rebalancing removal or a shrinking of the shared prefix takes, without
  waiting, every node bbtree's operation will change or read a separator
  of, then runs that operation along a node stack from the topmost one
  (`tree/latched`). The shared map's own code is only what sharing needs:
  the descent, which nodes to take, type-stable pages, the append pointer.
- **Why not a tree layer of its own:** the first version had one, and left
  out the shared prefix, prefix fits after removals, lowering to a leaf,
  4-byte windows and appends. Each cut needed a special case elsewhere, in
  the node plan, the invariant check or the root, or made another path
  hot: dense integers tied at the root, 205 ns a `u64-seq` hit.
- **Holding a node holds its separators' keys:** a head key changes only
  while the node holding its separator is held, so bbtree's operations
  read keys where they are. A reader that holds nothing latches the head's
  leaf shared: a removed key's memory may be freed at once.
- **Why the strategy:** every node type already carries it, so no type
  gains a parameter, and the layouts pick their constants from it as they do
  from the plan.
- **Why OLC, no copies:** racing reads go through raw pointers and form no
  reference, so only the version check decides what a read was worth. A
  first version read through atomic copies and wrote through private copies
  of each node: about 30 instructions more per internal level for lookups.
- **Type-stable nodes, not epochs:** a freed node's page goes on the
  tree's free list for its kind, keeps its latch word, whose version only
  grows, and goes back to the allocator when the map drops. A racing
  reader always reads a node of the kind its pointer says, and its check
  fails; nothing per operation, no guards. The root page alone changes
  kind, and a reader checks a node's version before following an edge.
- **Cost to bbtree:** the inline-key searches read through a raw `Slots`
  view; a node stack can start along a kept path; a removal's fixing reads
  no node above the last it changes. Atomic header accessors and a zeroing
  branch in `FreshPage::allocate`, tried first, changed bbtree's insert
  paths (an `inline(always)` on `len` moved a `u32` split by 195
  instructions); racing reads have functions of their own instead.

**D55. The shared map's batches hold no latch across a yield.** (2026-10-16)
Design and measurements in 09-concurrency.md, "Batches".

- **What:** `read_many`, `get_many_copied`, `get_many_cloned` and
  `insert_many` run one lane a key on the unsync tree's executor and
  `Prefetcher` (D53). A lane reads internal nodes and its leaf's inline
  keys optimistically, prefetching each line first. A lookup's lane copies
  its entry unlatched where the leaf settles the key (D57); otherwise it
  latches the leaf, searches it, runs the closure and lets go within one
  poll. An insert's lane only finds its leaf and the version it read; the
  inserts then go in input order, each taking its leaf by a
  compare-and-swap at that version, moved on by the batch's own inserts
  there, and otherwise inserting as a single insert does. Batches go one
  key at a time on trees with fewer than three internal levels, about an
  L2 of leaves.
- **Why:** a latch held across a yield would make lanes wait for one
  another, and a writer for a whole batch. Prefetching before latching
  keeps the latch to the poll that needs it.
- **Ties walk synchronously:** a lane that prefetched and yielded along a
  tie's walk to its head, as a nested future or written flat in the lane,
  ran 10M `u64` batched lookups at 75–80 ns against 71: the suspension
  points cost more than the overlapped walks saved (2026-10-17).

**D56. AArch64 counts inline keys in NEON, relying on invariant 4.** (2026-10-16)

- **What:** on AArch64, every width of inline key counts in explicit NEON
  vectors (`NeonCount` in `vector_count.rs`): per 16 bytes of slots, one
  `cmhi` and one `cmeq` against the broadcast target, their all-ones lanes
  subtracted into counters of the slots' width, one `addv` each at the end;
  a capacity that isn't a multiple of the vector ends with an overlapping
  load, its counted lanes masked out. The exact-key equality mask (D41) and
  the hash-tag filter (D36) stay x86-only: the count already tells none
  from one from many, and hash tags only filter ties.
- **Why:** the compiler's own count there widened each compared lane to 64
  bits, one at a time, to add it to a `usize`. A 10M `u64` hit on an M1 Pro
  ran 1,678 instructions, against 228 on Rocket Lake with AVX-512, and
  bbtree was 1.06× std's speed.
- **Slots past `len` count:** the counts are exact where those hold the
  greatest inline key (invariant 4), as the compiler's count already
  required, and clipped to `0..len` for any contents. x86's masks ignore
  them; masking them per vector on NEON cost 8–18% of a lookup.
- **Measured** (M1 Pro, 10M entries, probe bbtree-eval's
  `diagnostics/portable`, one run each), ns before → after: `u64` hit 389.5 → 173.5, miss 308.1 →
  137.3; `hash32` hit 398.7 → 185.2, miss 270.7 → 119.9; borrowed JSON hit
  428.7 → 267.3, miss 306.8 → 198.2. Instructions per `u64` hit 1,678 →
  407.

**D57. Lookups read the leaf optimistically; a `Copy` value is copied out unlatched.** (2026-10-17)
Design and measurements in 09-concurrency.md, "Lookups".

- **What:** a lookup's descent reads its leaf at a version too, without
  the latch, and searches its inline keys in place. A key they rule out is
  absent once the version, unchanged, confirms the read: a miss writes
  nothing. `get_copied` and `get_many_copied`, for `V: Copy`, copy the
  candidate's value out and check the version after, as a sequence lock
  reads, where the leaf settles the key without following a pointer: a key
  ordered by its own bytes (std integers, byte arrays) is copied and
  compared, and a byte string that ends within its 8-byte inline key is
  the candidate of the same length, where the leaf's hash tags tell it
  exactly (D36). Otherwise the lookup latches the
  same leaf at the same version and searches it; a leaf that changed sends
  it the latched way, which waits rather than race again.
- **Why `Copy`:** a closure, or a clone of a value that owns memory, can't
  run on what an unlatched read saw: a writer may free that memory before
  the check. A `Copy` value has no `Drop` and no interior mutability, so a
  torn copy is thrown away unused. Closures (`read`, `update`, scans) keep
  the latch.
- **Measured** (9900X, 10M `u64`, bbtree-eval's `bbtree-scaling`, against
  the latched lookups of 2026-10-16): get-half at 24 threads 98.0 → 113.6
  M/s, batched 121.5 → 145.9; one thread within 4%. A hit no longer writes its leaf's
  latch word, a line every reader of the leaf took in turn.

**D58. File maps: a map's pages, compacted and laid end to end, edges as offsets from their node, read where they lie.** (2026-10-19)
Design and measurements in 15-index-file.md.

- **What:** `FileWriter` builds a full-node tree from a map's entries
  (D25) and writes its pages byte for byte, in level order, after a header
  that fingerprints the layout. Each edge holds its child's offset from
  the edge's own node, tagged as a pointer is (D52). `FileMap` reads the
  file map in place, a tree of strategy `InFile<S>`, which only sets
  `IN_FILE`: `Internal::edge_unchecked` adds the offset to the node's
  address, for `get` and the batch lanes (D53) alike. Keys and values are
  `PlainBytes`, kept in their slots, or `VarBytes`, of any length, kept in
  each leaf's tail, the slot holding a span, `{len, offset}` from the slot
  itself. `Auto` takes `VarBytes` for a byte string, as it does `String`:
  a `&[u8]` looks a key up, compared in place. bbtree-disk writes, maps
  and reads typed maps in files (`BBTreeMapFile`), each key and value type
  picking plain or `VarBytes` storage (`DiskType`);
  bbtree-datafusion-parquet embeds indexes of one or more columns in a
  Parquet file, one entry per distinct key, and DataFusion's Parquet scans
  look their predicates' rows up in them.
- **Why the tree's own pages:** lookups run the map's code, so they take
  its time: at 10M `u64`, 125.9 ns a hit read into memory, 126.4 ns mapped
  in huge pages, against the map's 125.4. A format of its own would search
  slower, or repeat the search.
- **Why offsets from the node, not from the file map's start:** the
  descent holds the node's address already; a base would travel with every
  node handle. A file map is position-independent: any aligned offset of a
  file, mapped anywhere, unchanged, nothing written to it.
- **Zero cost to maps:** `IN_FILE` is a constant of the strategy, as
  `LATCHED` is. Every probed map operation compiles to the same machine
  code as before. The header's padding byte, which no write initializes,
  is zeroed in the copy, not made a field: a field cost the insert split
  its inlining.
- **Two ways to open:** `open` checks every page for what keeps reads in
  bounds, so that any bytes give answers, wrong or not, or a panic, never
  undefined behavior; `open_unchecked`, for a trusted file, checks the
  header alone and reads nothing else.
- **Span offsets from the slot, not the node:** bbtree hands a key's `Ord`
  and `InlineKey` only `&K`; from the slot, that is all a span needs. The
  writer builds over `ArenaBytes`, 8-byte pointers of the span's layout,
  and rewrites them into spans as it copies each leaf.
- **Fixed-size keys stay plain:** var-length, a 10M `u64` hit took 216 ns
  against 127, the file map 49 bytes an entry against 18.5.
- **Names:** "file map" for the format; "file" alone for the operating
  system's; "tail", "span" and "var-length" for the bytes of any length,
  as "heap" already means the allocator's and "handle" a node's; "OS
  pages" for the operating system's, as "page" means a node's.

## Navigation

**D11. No parent pointers; iterators use a node stack.** (2026-10-07)

- A split or merge never touches the moved children. std rewrites the parent
  link of every moved or shifted child (`node.rs:1045`), which would be ~85
  scattered writes per leaf split at our fanout.
- Iterators carry the root-to-leaf node stack inline, with no allocation.
  Unlike re-descending from the root (as bf-tree does), this never calls `Ord`
  inside `next()`, matching std.

## Version policy

- Everything that std btree has, bbtree should have, and is always v1.0;
- All the additional features follow the semantic versioning.
- MSRV is 3 months after the last release, bumped every 3 months.

## Safety

**D14. Types make safe what is stable; explored code keeps plain contracts.** (2026-10-07)
The node code, its layouts, prefixes and searches, keeps changing (D26–D50),
so a node handle is a page pointer with no access of its own, whose
contracts, access, positions and room, the code that holds it keeps by hand
(`tree/node/mod.rs`); what has stopped changing keeps its types (D51).

- **One memory primitive:** `Slots<T>`, a fixed-capacity array of slots in a
  page, side by side or a stride apart (keys, values, edges, inline keys).
  Insert, remove, move a run to another node, view as a slice. Written and
  tested once (`tree/node/slots.rs`).
- **Pages are owned until a node holds them:** `FreshPage`, typed by what
  it is for, a kind of node or the root (D52), is the only way to make a
  node and what a node turns back into.
- **Node stacks and walks carry a borrow type** (`Immut`, `Mut`, `ValMut`,
  `Dying`): it makes them `Send` and `Sync` as the borrow they stand for,
  and the walks hand out each entry once by construction (D23).
- **Checks the algorithm makes anyway, once:** room before an insert,
  whether siblings fit before a merge, a node's kind from its pointer's
  tag (D52). A
  method on the hot path checks positions and room in debug builds only;
  one off it checks them always.
- **Enforced:** `#![forbid(unsafe_code)]` in every module except the node
  modules that touch pages (`node/mod.rs`, `page.rs`, `slots.rs`,
  `leaf.rs`, `internal.rs`, `root_page.rs`, `siblings.rs`,
  `inline_key_search.rs`, `vector_count.rs`), the tree's `root.rs`, `node_stack.rs`, `search.rs` and
  `walk.rs`, and file maps' `tree/file.rs`, `file.rs` and
  `file/var_bytes.rs` (D58).
- **Was:** after std's `node.rs`, one handle generic over access and kind,
  positions and room as proof types, and edges as owned subtrees; it froze
  the structure the proofs were about, and pushed leaves and internal nodes
  through one interface. Which contracts get types
  again is phase 2's question, once the design settles.

**D44. A child's prefix is never shorter than its parent's, even with a lying encoding.** (2026-10-10)

- **What:** when a refit computes a child's prefix, or the border beside a
  replaced separator shrinks, the target is floored at the parent's
  current prefix. A valid `InlineKey` already guarantees this; the floor
  keeps it when a custom encoding breaks its ordering contract.
- **Why:** a reversed encoding asked for a child prefix of 6 under a
  parent prefix of 7, and split promotion hit a debug assertion. The
  assertion stays, and the invariant is enforced at refit.
- **Test:** the adversarial-encoding test checks every parent/child pair
  after each operation, without calling the lying encoder, and keeps its
  exactly-once drop ledger.

## Proposed, not decided

- **Default allocator:** carve pages from large chunks (≥ 256 KB). Open:
  chunks per map or per allocator instance; when chunks go back to the OS.
- **Stable-first:** the library builds on stable; nightly only for fuzzing and
  Miri. Without `#[may_dangle]`, our dropck is stricter than std's (a README
  deviation); an optional `nightly` feature restores it.
- **`alloc`:** whether core code depends on `alloc` or only the default
  allocator does.
- **Fill factor:** redistribution to a sibling before splitting (deferred in
  06-cost-model.md). End-biased splits are D32; the full-node bulk build for
  `FromIterator`/`append` is D25.
