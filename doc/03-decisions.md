# Decisions

Decisions made so far, with the reason and what follows from each.
Each entry states a decision as it holds now. To change one, edit it in place;
the file is not a history of decisions.

## Scope (from 00-init)

**D1. Drop-in replacement for std's `BTreeMap`/`BTreeSet`.**
`BBTreeMap` and `BBTreeSet` keep std's API and behavior. Any deviation needs a
strong reason and is documented in README.md. Design as if it could be merged
into std someday; flag anything that would prevent that.

**D2. Zero dependencies.** `serde` and `rand` support, when added, are
optional features.

**D3. `no_std`.**

**D4. Single-threaded**, like std.

**D5. Differential fuzzing against std's btree.** Formal verification with
Verus is optional but welcome.

## Nodes

**D8. Own allocator trait.** (2026-10-07)
Every allocation is a node, counted, with a size known in advance; no runtime
surprises. System and custom allocators are both supported through our trait.

- Not built yet: until it is, each node is one global-allocator call
  (`tree/node/page.rs`).
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
  keys' capacity. Node kind comes from the height, as in std.
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
  down the leftmost path of its right subtree to that leaf's head.
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
    `Vec<u8>`, `Box<str>`, byte arrays, …) and the 64- and 128-bit integers
    (D31) inline keys automatically, by compile-time type identity (option
    (d)).
- **In types:** `BBTreeMap<K, V, S = Auto>`; the allocator parameter (D8)
  is to go before `S`. Existing code is unchanged and gets inline keys for
  std string and integer keys. Other key types opt in with a one-line impl
  plus `BBTreeMap::with_strategy(WithInlineKey)`; `new()` stays
  `Auto`-only, since a generic `new()` breaks inference. There is no
  separate alias type.
- **Lint:** node handles carry five type parameters, so `clippy.toml` raises
  `type-complexity-threshold` to 300.
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
  a `String`→u64 leaf of 25. The floor of 4 bounds the height: see the
  last bullet.
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
- Consequence: a non-root internal node has at least 5 children, so a tree
  is at most 23 levels above its leaves: one level more would need
  2·5^23 KB of leaves, more than 2^64 bytes. Every path has a slot per level
  (D24).

**D23. Iteration: a cursor per end, each a path (D24).** (2026-10-08)

- **A position is an edge of a leaf.** Past a leaf's last entry, the next
  one is the separator of the lowest node on the path with a key beyond it,
  and its value is the head of the leftmost leaf right of it (D17).
  Separator keys live only in internal nodes, so crossing a leaf boundary
  visits an ancestor whatever we do: leaf-to-leaf links would not help.
  `next()` never calls `Ord` (D11).
- **Each end is a path** (D24): it goes up its own levels to the separator
  and down to the next leaf. No end walks down from the root again.
- **Lazy:** an end that has not started holds a detached path, which writes
  nothing; it starts at the first or last entry on its first move, so
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
  paths a `ValMut` borrow type, as std's give their handles: the paths
  copy, so both ends hold some, and only read the structure and keys;
  handing out a value as `&mut V` is unsafe. The walks vouch for handing out each value once: their ends move
  toward each other, and stop by a count (whole map) or where they meet
  (range).
- **Owning iteration** (`DyingWalk`, for `IntoIter` and for dropping a
  tree, which is dropping one) takes the tree apart as it goes: each entry
  is moved out of its slot, nothing shifts and nothing rebalances, and an
  end frees each node as it goes up out of it, past its last edge (its
  first, going backward); the nodes on the path where the ends meet go
  last. Paths of the `Dying` borrow type copy, so both ends hold some;
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

**D24. Paths: a node pointer per level from the root, shared by every operation.** (2026-10-08)

- **One type** (`tree/path.rs`): the node at each height from the root
  down to a position in one of them, and the index taken at each, as a
  `u16`. It stands for a borrow of the whole tree, which its borrow type
  says: shared, value-mutating and dying paths copy, and an exclusive path
  (`PathMut`) changes the tree through its bottom node, from which nothing
  leads up, and holds the `&mut` of the root to grow a new one. The nodes
  it hands out are plain handles (D14).
- **Who uses it:**
  - an insert that reaches a full leaf searches down one (D37), inserts at
    the leaf, carries splits up the path, then goes back down through the
    halves that took the new entry, so the path ends at it;
  - a removal goes up the path from where the entry came out, fixing each
    underfull child (D22) and stopping at the first level that loses no
    key; a separator's successor comes up from the leftmost leaf right of
    it;
  - an entry keeps the path its search found: reading or changing it goes
    straight to the bottom node, removing or inserting goes up the path;
  - each end of a walk is one (D23), and a range's bounds are searched
    with one;
  - point lookups and inserts into a leaf with room hold only the current
    node (D32, D37).
- **Why pointers, not indexes.** An index path is valid under the same
  condition as a pointer path, that nothing else changes the tree, which
  `&mut` already gives; it only turns a broken condition into a panic, and
  costs a walk down from the root at each use.
- **24 slots** (`MAX_HEIGHT` 23, from D22's floor of 4), 248 B. The slots are
  uninitialized until written; a path is made detached and started where it
  stays, and is never moved once written inside the tree code: moving one
  copies it with wide loads, which wait on the narrow stores that wrote it
  (D23). Passing the path by value to `remove`, wrapping it in a search
  result, or returning it from a constructor each did, and made a 1k `get`
  take 31 ns instead of 10.9, and `pop_first` 1.6× as long. An entry and
  an iterator still move when returned.
- **Rejected:** 8 levels in place and a `Vec` for the rest, which made a path
  112 B (`Iter` 248 B). Choosing the storage at every level cost 50–70
  instructions per operation, and `get` +13% at 10M, where fewer
  instructions let more independent lookups overlap.
- Measured, one run each, against index paths and recursive insert and
  removal (ns per call):

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
- **`append`, `retain` and `extract_if`** drain entries in order by value
  from the front (D23's owning walk; no `Ord`), and rebuild with the
  builder, keeping std's
  observable behavior: what the predicate sees and when, which key and
  value survive, drop order, and what is left after a panic. Removing
  while walking, as std's `extract_if` does in place, would need the walk's
  position tracked through every merge and steal, which std's parent
  pointers give for free.
- **`split_off` cuts the tree along the search path, as std's does**
  (`tree/cut.rs`): from the leaf up, each node on the path keeps what
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
- **Cost:** `append` O(n + m) as std's; `retain` O(n) as std's, but
  moving the entries it keeps; `extract_if` O(n) wherever its range is,
  where std's is O(log n) plus the part it touches; `split_off` O(log n),
  plus the shorter tree's nodes to count. A drained entry costs about a
  step of a walk.

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
  once, where it reads the node's page from an edge, a path or the root
  (`Node::at`, the one cast, from the height), and the code after it is for
  that kind. Likewise a choice fixed for a whole loop, such as the end a
  border is on, is made before the loop, as a type.
- **Enforced by the types:** each kind is its own type (D51), and what
  depends on the kind, a node's capacity and where its slots are, is in
  that type's code alone. Deciding inside each method instead had a descent
  decide it three to six times per level, and a steal or merge five to
  eight times.
- **What it takes:** a descent decides the root's kind once, then loops
  over internal nodes, the leaf's search after the loop; going down an
  edge, an internal node's height tells its children's kind. Code for
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
- **Point lookups keep no path.** `get` and `get_mut` hold only the
  current node. Inserts, entries, removals and range bounds keep paths.
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
  copying the path. 10M ordered `u64` insert: 36.0 → 34.0 ns; random
  insert unchanged.
- **A range iterator caches a checked leaf interval.** Repeated
  `next`/`next_back` step inside it; switching direction, crossing a leaf
  or folding first syncs the path; it costs `Range`/`RangeMut` 40 bytes.
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

**D36. String leaves keep 2-byte length tags in spare padding.** (2026-10-10)

- **What:** for recognized heap byte strings, a leaf stores each key's
  length mod 65,536 in 2 bytes, only where existing padding holds the whole
  tag array before the head's key (D49). Where the head's key took the
  padding's end, as in `String` → `u64`, the leaf gives up an entry to keep
  them (24 → 23); where tags never fit, as in `String` → `String`, it has
  none. Allocation size doesn't change. `KeySlots` moves tags with their
  keys.
- **Search:** tags are read only when the 8-byte summaries leave more than
  one candidate. No tag matches: absent. One: compare that full key. Two:
  compare the upper key, then the lower only if the query sorts below it.
  Three or more: the old binary search, so equal-length keys never cause
  a linear scan. A tag never decides equality on its own: it's truncated.
- **Why:** path keys tie in their 8-byte windows inside leaves. On 1M
  paths, heap key reads per lookup fall from 1.98 to about 1.0 for hits
  and 2.18 to 0.20 for misses.
- **Cost:** inserts maintain a third array: 10M path insert +2.0%. JSON
  leaves never have ambiguous runs, so JSON pays upkeep and gains nothing
  on lookups; its timings stayed flat.
- **Measured** at 10M paths: hits −10.7%, misses −31.2%.
- **Pitfall:** the summary-view wrapper is forced inline. Outlined, as
  LLVM did once it had a second caller, its population count grew from 26
  to 244 instructions, and JSON hits got 25% slower.

**D37. Inserts record a path only when the leaf is full.** (2026-10-10)

- **What:** an insert first descends without a path, like a lookup, and
  either replaces an existing value or inserts into a leaf with room. If
  the leaf is full, an outlined function searches again, this time
  recording the path, and splits.
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
    always inlines its leaf's (`Leaf::search_inlined`): out of line, it cost
    a random JSON insert 40 instructions more (D51).
  - A lookup that ends at a separator reads its value out of line
    (`Found::separator_key_value`): inlined, the walk down to the head made
    LLVM keep a counter for it in every descent's loop (D51).
  - The shared-prefix check (D50) on heap strings compares 32 bytes at a
    time, inline.
  - Native 8-byte leaf summaries use an equality mask.
- **Why:** with the `Option`, an outlined helper re-tested for null and
  kept a generic `memcmp` fallback. Without forced inlining, every internal
  level returned node, height and index through memory: about 14 memory
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
  arithmetic. Plain and zero-size layouts keep indexes. The path still
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
  read again from its key, the head below (D49), for all 8 bytes. Custom
  strategies and fixed arrays keep their width.
- **Why:** about 94% of lookups pass through an internal node without
  ending there, so the extra ties from 4 bytes cost little (JSON: +0.02
  heap reads per hit), while the summaries shrink from 4 cache lines to 2.
  In leaves, 4 bytes would more than double JSON's heap comparisons, so
  leaves stay at 8.
- **Search on baseline x86-64:** 17–32 four-byte summaries use one SSE2
  packing step and trailing-one counts; above 32, a branchless binary
  search. A second packing step for 33–64 cut instructions but was slower
  on borrowed JSON, so it was dropped.
- **Measured,** 10M native: JSON inserts 314.1 → 282.5 ns, hits
  343.8 → 320.4, misses 282.3 → 235.4; paths about 1.3% faster. Baseline
  x86-64, owned `String`: JSON hits 408.7 → 390.6, misses 335.5 → 297.3.
- **Cost:** on baseline x86-64 with borrowed keys (2M diagnostic), path
  misses are 2.2% slower; the other borrowed cells improved about 2%.

**D45. Remember the last leaf after a split, so ascending inserts can append.** (2026-10-10)

- **What:** the map owner keeps an optional pointer to the tree's last
  leaf.
  - **Set** only by a plain insert that split, when its path shows the new
    key went into the last slot at every level.
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

- **What:** the library issues no prefetch instructions and has no
  `prefetch` feature. Hardware prefetchers work as they do. AGENTS.md
  forbids explicit prefetching anywhere, including benchmarks and
  prototypes.
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
  length tags, rehydrated windows, full and tied searches,
  whole-key equality, and whether separator keys are heads (D49). One constructor per row of the table,
  each with its reason. Layouts, slots, searches and prefix upkeep read it;
  none asks what the key type is. Per-key arrays (inline keys, length tags)
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
- **Rejected:** a public `KeyBytes` trait and `WithKeyBytes` strategy for
  byte-ordered newtypes: no measured gain over `WithInlineKey` (311 vs
  315 ns per lookup on path newtypes).

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
  abort-on-unwind guard.
- **Tests:** a lying `Ord` or `InlineKey` must stay memory-safe. A
  panicking one need not leave a valid map; the tests that still check one
  (`map/tests/drops.rs`, `map/tests/inline_key.rs`) go when a change
  breaks them.

**D49. With inline keys, separator keys are leaf heads, and internal nodes keep no keys.** (2026-10-13)
Design, census and measurements in 11-separator-key.md.

- **What:** in a map whose keys have inline keys, each separator's key is
  the head of the leftmost leaf right of it, beside its value (D17), in
  entry 0's key place (D29). Internal nodes keep only inline keys and
  edges, in 512 B pages (D15). A search compares inline keys, and on a tie
  reads a separator's key by walking down its right edge and first edges to
  that head. Without inline keys, internal nodes keep their keys (D17).
- **Why:** inline keys decide most internal searches (0.06–0.26 ties per
  lookup on integers, `hash32` and JSON), so internal nodes held keys that
  lookups rarely read. Without them, they hold as many children in half
  the bytes.
- **Moves:** a head's key moves with its value in splits, moves between
  siblings, merges and removals; internal nodes move only windows and
  edges, and no key moves between levels. `SeparatorSlots` holds an
  internal node's separators, keys or windows; `SeparatorKeys` lends their
  keys, in place or walked to.
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
  them with it (`Root`, D51): the leading bytes of the encoding every key
  shares (D18), up to 64 (`SharedPrefix`, `tree/shared_prefix.rs`), one
  pointer, null while nothing is shared. The root, and every node with no
  separator above it on a side, along the tree's borders, take its length
  as their prefix, where they had 0 (D26). Every key with inline
  keys gets it, `InlineKey` impls included.
- **Search:** a lookup, an entry, a range bound or an insert checks the
  query against those bytes once, before the root. A query without them
  sorts before or after every key and reads no node: a lookup misses, a
  range or an entry starts at the first or last leaf edge. Below, every
  node skips its prefix unchecked.
- **Upkeep, reading no key:** an insert whose key shares less shrinks the
  bytes first, and the border nodes with them, by D26's window shift; an
  empty tree starts from its first key's. Removals leave them: the rest
  share at least as much, so the prefix is correct, not exact. A cut gives
  both trees the bytes; a join shrinks both to what they share with the
  separator; a build checks each key as it appends; a clone copies them.
- **Why not per-node local prefixes,** which internal nodes of std keys
  kept where fences share too little, checking each query against their
  first key: that read a key on every search of such a node, a walk to a
  leaf head with D49, cost every internal search a header field, and made
  every key move restore and rebuild windows. One check at the top
  establishes the same for the root and the borders, where fences are
  missing, and the internal search, without the per-node check, is small
  enough for LLVM to inline into `get`.
- **Cost:** the map is 40 bytes, 8 more. Each insert checks its key
  against the bytes, inline: a null test when nothing is shared; starting
  and shrinking are out of line. The insert's append shortcut (D45) checks
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
Design in 12-tree-structure.md.

- **What:** `Leaf` and `Internal` are page handles with the operations of
  their kind, and `Root` owns the tree: the top node and its height, its
  fences as the shared prefix (D50), and the append hint (D45). `Node`, a
  leaf or an internal node by its height, is what an edge, a path or the
  root leads to, and the one place a kind is decided (D28). What both
  kinds share is composition (pages, slots, inline-key column) and `Child`,
  for code that works at either level: `Siblings` (merges and moves
  between children), joins and prefix fits.
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

## Navigation

**D11. No parent pointers; iterators use a path stack.** (2026-10-07)

- A split or merge never touches the moved children. std rewrites the parent
  link of every moved or shifted child (`node.rs:1045`), which would be ~85
  scattered writes per leaf split at our fanout.
- Iterators carry the root-to-leaf path inline, with no allocation. Unlike
  re-descending from the root (as bf-tree does), this never calls `Ord` inside
  `next()`, matching std.

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
- **Pages are owned until a node holds them:** `FreshPage`, typed by the
  kind of node it is for, is the only way to make a node and what a node
  turns back into.
- **Paths and walks carry a borrow type** (`Immut`, `Mut`, `ValMut`,
  `Dying`): it makes them `Send` and `Sync` as the borrow they stand for,
  and the walks hand out each entry once by construction (D23).
- **Checks the algorithm makes anyway, once:** room before an insert,
  whether siblings fit before a merge, a node's kind from its height. A
  method on the hot path checks positions and room in debug builds only;
  one off it checks them always.
- **Enforced:** `#![forbid(unsafe_code)]` in every module except the node
  modules that touch pages (`node/mod.rs`, `page.rs`, `slots.rs`,
  `leaf.rs`, `internal.rs`, `siblings.rs`, `inline_key_search.rs`,
  `vector_count.rs`), and the tree's `root.rs`, `path.rs`, `search.rs` and
  `walk.rs`.
- **Was:** after std's `node.rs`, one handle generic over access and kind,
  positions and room as proof types, and edges as owned subtrees; it froze
  the structure the proofs were about, and pushed leaves and internal nodes
  through one interface (12-tree-structure.md). Which contracts get types
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

- **Exact allocation count before mutating:** insert counts the full nodes on
  its path from the leaf upward, allocates that many first, and so can fail
  without changing the tree. Name of the fallible insert (`try_insert` is
  taken in std).
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
