# Decisions

Decisions made so far, with the reason and what follows from each.
To change a decision, add a new entry that supersedes it; don't edit the old one.

## Scope (from 00-init)

**D1. Drop-in replacement for std's `BTreeMap`/`BTreeSet`.**
`BBTreeMap` and `BBTreeSet` keep std's API and behavior. Any deviation needs a
strong reason and is documented in README.md. Design as if it could be merged
into std someday; flag anything that would prevent that.

**D2. Zero dependencies.** `serde` and `rand` are optional features.

**D3. `no_std`.**

**D4. Single-threaded**, like std.

**D5. Differential fuzzing against std's btree.** Formal verification with
Verus is optional but welcome.

## Nodes

**D6. Nodes are 4 KB.** (2026-10-07)
A node is a multiple of 4 KB only when the minimum capacity doesn't fit in
4 KB (huge K/V). There are no smaller nodes: no mini-pages, no size classes.

- Consequence: every non-empty map costs at least 4 KB (std: 192 B for a
  one-entry u64→u64 map). Accepted.

**D7. Nodes are always 4 KB-aligned.** (2026-10-07)

- A node never straddles a page: one TLB entry per node, and hardware
  prefetchers, which stop at page boundaries, can stream the whole node.
- The low 12 bits of every node address are zero, which leaves room to pack an
  index next to the pointer.
- Consequence: per-node aligned allocation from the system allocator is too
  expensive (D8).
- Consequence, to measure: the L1d set index is address bits 6–11, which is
  the page offset, so every node's header and first search probe land in the
  same L1 set. If it hurts, offset the key array per node ("coloring").

**D8. Own allocator trait.** (2026-10-07)
Every allocation is a node, counted, with a size known in advance; no runtime
surprises. System and custom allocators are both supported through our trait.

- Consequence: the default allocator cannot request each node as
  `Layout(4096, 4096)`. Measured with glibc (20k nodes, each fully written,
  RSS delta):

  | Strategy                                   | RSS per 4 KB node |
  |--------------------------------------------|-------------------|
  | `alloc(4096, align 16)` (not page-aligned) | 4120 B            |
  | `alloc(4096, align 4096)` per node         | 8200 B (2×)       |
  | carve from 64 KB chunks                    | 4353 B            |
  | carve from 256 KB chunks                   | 4154 B            |
  | carve from 2 MB chunks                     | 4098 B            |

**D9. Keys grow from the front of the node, values from the back.** (2026-10-07)

```
| len | k0 k1 k2 … →            free            ← … v2 v1 v0 |
0     H                                                     4096
```

- Key `i` sits at `H + i·size_of::<K>()`, value `i` at
  `end − (i+1)·size_of::<V>()`.
- With no parent pointer (D11) and no fences (D10), the header is just
  `len: u16`. Node kind comes from the height, as in std.
- Keys are aligned by rounding `H` up to `align_of::<K>()`. Values are aligned
  because the node end is 4 KB-aligned. The free gap needs no padding.
- An insert is one memmove of the keys after the position and one memmove of
  the values; a split is one memcpy for each half.

**D10. Not taken from bf-tree:** slotted layout, fence keys, prefix
compression, preview bytes, mini-pages. (2026-10-07)
Fixed-size generic K/V needs none of the slotted machinery. Fences would need
`K: Clone`. Prefix compression and preview bytes need byte-comparable keys.

**D12. Internal nodes put the edges at the end.** (2026-10-07)

```
| len | k0 k1 k2 … →      free      ← … v2 v1 v0 | e0 e1 … e_cap |
0     H                                     vals_end            4096
```

- The `cap + 1` edges (8 B each) fill the end of the node. Values grow down
  from `vals_end`, the start of the edge array rounded down to
  `align_of::<V>()`.
- Keys sit at the same offset in leaves and internal nodes, so one search
  routine serves both.
- Capacity is the largest `n` that fits, computed per `(K, V)` at compile time.
  u64→u64: leaf 255, internal 170.

**D13. Every node holds at least 12 entries.** (2026-10-07)
Refines D6: the node size is the smallest multiple of 4 KB at which both a
leaf and an internal node hold ≥ 12 entries, and the node stays 4 KB-aligned
(D7).

- 4 KB is enough while `K + V` ≤ ~332 B (the internal node is the tighter
  of the two). Larger entries get 8 KB, 12 KB, … nodes.
- A type aligned to more than 4 KB raises the node alignment to its own,
  which is still a multiple of 4 KB.
- u64→u64 is unaffected: leaf 255, internal 170, 4 KB.

**D15. Nodes are 1 KB, aligned to their own size.** (2026-10-07)
Supersedes the 4 KB size in D6 and the 4 KB alignment in D7. A node is the
smallest multiple of 1 KB at which both kinds hold ≥ 12 entries (D13), and it
is aligned to max(1 KB, align of K, align of V).

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
- A self-aligned 1 KB node never crosses a 4 KB page, so D7's one-TLB-entry
  argument still holds.
- 1 KB alignment leaves 10 free low address bits, so node capacity is capped
  at 1023.
- 12 entries fit in 1 KB while `K + V` ≤ ~76 B (the internal node binds);
  larger entries get 2 KB, 3 KB, … nodes. u64→u64: leaf 63, internal 42.
- A one-entry map costs 1 KB instead of 4 KB.
- To revisit after in-node prefetching and with other key/value types.

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
- Possible later: a branch-free linear count instead of binary search inside
  sorted leaves. It's ~5% faster on inserts, and faster on gets only with
  AVX-512 (167 vs 185 ns); it would be gated on target features.

**D17. B+-tree: values live only in leaves; separators are moved, not copied.** (2026-10-07)
Supersedes the value area of internal nodes in D12, and settles the
B-tree-vs-B+-tree proposal.

```
internal  | len | k0 k1 … →            free              | e0 e1 … e_cap |
leaf      | len | k0 k1 … →     free     ← … v1 v0 | head |
```

- Internal nodes hold keys and edges only. **Separator keys are stored
  physically in internal nodes**, which are the hot part of the tree: an
  internal-node search compares against keys inline in that node and never
  follows a pointer into a leaf.
- Leaves hold keys and values as in D9, plus one key-less **head** value: every
  leaf except the leftmost stores the value of the separator key just to its
  left in key order. That key lives in an internal node: the lowest common
  ancestor of the two leaves.
- A tree with L leaves has exactly L − 1 separators, one per boundary between
  neighbouring leaves, so every leaf but the leftmost has exactly one head.
- **No `K: Clone`.** Each key is stored exactly once, as in std. When a leaf
  splits, the right half's smallest key moves up as the separator and its
  value stays behind as the right leaf's head; a merge moves the key back down
  next to its value. Values never leave leaves.
- **Lookup always ends at a leaf.** A match on an internal separator continues
  down the leftmost path of its right subtree to that leaf's head.
- Why: values only in leaves, as intended from the start (00-init: database
  B-trees). Internal fanout rises (u64→u64: 42 → 63 children per 1 KB), and
  large values never sit in internal nodes. Inner nodes are a tiny fraction of
  the tree, so the extra work on separator hits doesn't matter.
- Alternative considered: keep every key physically in its leaf and let
  internal nodes point at it. Rejected because moves inside leaves (shifts,
  splits, steals) would invalidate those pointers; the structural relation
  above needs no pointer maintenance.

**D18. Inline keys: a defaulted strategy parameter that recognizes std types.** (2026-10-07)
Design in 04-inline-key.md.

- **Two parts:**
  - a public `InlineKey` trait anyone can implement, newtypes included (option
    (b));
  - a default strategy that gives std byte-string keys (`String`, `str`,
    `Vec<u8>`, `Box<str>`, …) inline keys automatically, by compile-time type
    identity (option (d)).
- **In types:** `BBTreeMap<K, V, A = Global, S = Auto>`. Existing code is
  unchanged and gets inline keys for std string keys. Other key types opt in with a
  one-line impl plus `BBTreeMap::with_strategy(WithInlineKey)`; `new()` stays
  `Auto`-only, since a generic `new()` breaks inference. There is no separate
  alias type. Until the allocator parameter lands, `S` is the third parameter.
- **Lint:** node handles carry five type parameters, so `clippy.toml` raises
  `type-complexity-threshold` to 300.
- **Width:** windows are 8 bytes (`InlineWord = u64`), and a key type says
  how many of their first bytes nodes keep, `InlineKey::WIDTH`, 2, 4 or 8
  (2026-10-09): 8 for std byte strings, whose ties read the heap; 2 for
  `u64`, `i64`, `u128` and `i128`, which implement `InlineKey`. At 8 bytes,
  real neighbours tie 24–70% of the time; at 16 bytes, 1–20%
  (04-inline-key.md Q7). The integers first opted in with `WithInlineKey`;
  `Auto` gives them inline keys since D31.
- **The trait gives windows** (2026-10-08): `inline_key_at(offset)`, bytes
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

**D20. Node prefetch.** (2026-10-07)
When a search enters a node, it prefetches the node's whole key area, sized by
capacity, not `len`, so it never waits on the header miss.
- Interleaved A/B on the B+-tree, bbtree only, median ns:
  - u64 10M: get_hit 208 → 163, insert 191 → 167;
  - hash32 1M: get 340 → 277;
  - str-json 1M: get 492 → 474.
- Sizing it by `len` didn't help.
- `String` keys gain little: their cost is the heap reads behind the keys,
  which inline keys address (04-inline-key.md).

**D21. Node size per kind.** (2026-10-07)
- **Internal node size** depends only on the key: the smallest multiple of
  1 KB holding ≥ 12 keys and 13 edges.
- **Leaf node size** depends on key and value: ≥ 12 entries plus the head.
- Supersedes the shared node size. Under D17 internal nodes hold no values, so
  a shared size made internal nodes of large-value maps huge (u64 →
  `[u8; 1000]`: 13 KB, ~800 keys).

**D22. Removal: physical, rebalanced bottom-up, nodes at least 1/8 full.** (2026-10-08)
- **No tombstones.** `remove_entry` and `pop_first`/`pop_last` hand the key
  back by value, a generic `K` has no sentinel, and a tombstoned key would
  stay alive, heap memory included, until some compaction, unlike in std.
  Removing costs one shift in the leaf, as inserting does (D16).
- **Underfull** means fewer than `max(capacity / 8, 4)` keys, for a node below
  the root, by its kind's capacity: 7 keys for a u64→u64 leaf of 63, 4 for
  a `String`→u64 leaf of 25. The floor of 4 (2 until the path change of
  D24) bounds the height: see the last bullet.
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
  (D24). With the earlier floor of 2, a 3-child minimum allowed 34 levels,
  and paths of 35 slots, about 370 bytes.

**D23. Iteration: a cursor per end, each a path (D24).** (2026-10-08)
Supersedes the path-stack frame and `MAX_HEIGHT` bullets under "Proposed,
not decided".

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
- **Sizes:** `Iter` 520 B, `Range` 512 B (std: 72 B, 48 B). Before D24, an
  end kept its leaf, the leaf's parent and an index per level, and walked
  down from the root when the parent ran out: `Iter` 232 B.
- **Mutable walks** (`IterMut`, `ValuesMut`, `RangeMut`) use a `ValMut`
  borrow type, as std's do: its handles copy, so both ends hold some, and
  only read the structure and keys; handing out a value as `&mut V` is
  unsafe. The walks vouch for handing out each value once: their ends move
  toward each other, and stop by a count (whole map) or where they meet
  (range). That is the one `unsafe` in `node/walk.rs`.
- **Owning iteration** (`DyingWalk`, for `IntoIter` and for dropping a
  tree, which is dropping one) takes the tree apart as it goes: each entry
  is moved out of its slot, nothing shifts and nothing rebalances, and an
  end frees each node as it goes up out of it, past its last edge (its
  first, going backward); the nodes on the path where the ends meet go
  last. A `Dying` borrow type's handles copy, so both ends hold some;
  moving an entry out and freeing a node are unsafe, vouched for by the
  walk. Entries without drop glue are skipped a leaf at a time when the map
  is dropped. Before, `IntoIter` popped from either end, rebalancing as it
  went, and dropping a tree was a separate teardown. 1M entries, one run
  (ns per entry): consuming `for` loop, u64→u64 15.1 → 7.5 (std 8.9),
  `String`→u64 91.9 → 74.7 (std 74.4); dropping, u64→u64 2.1 → 2.4 (std
  6.6), `String`→u64 64 → 67–72 (std 71), where the teardown dropped a
  leaf's entries in one loop and the walk steps a cursor per entry.
- Measured with D24, one run each, mimalloc, Ryzen 9 9900X (ns per entry;
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
  entry), as before D24. A range is copied after both ends are positioned
  (`take` moves it, for one), at about 15 forwarding conflicts per scan; it
  can't wait to position them, having no type for the bounds' key to keep
  them in.

**D24. Paths: a node pointer per level from the root, shared by every operation.** (2026-10-08)

- **One type** (`node/path.rs`): the node at each height from the root
  down to a position in one of them, and the index taken at each, as a
  `u16`. It stands for a borrow of the whole tree: shared or value-mutating
  paths copy handles out, and an exclusive path (`PathMut`) lends one handle
  at a time, to its bottom node, from which nothing leads up, and holds the
  `&mut` of the tree to grow a new root.
- **Who uses it:**
  - lookups search down one;
  - an insert searches down one, inserts at the leaf, carries splits up the
    path, then goes back down through the halves that took the new entry,
    so the path ends at it;
  - a removal goes up the path from where the entry came out, fixing each
    underfull child (D22) and stopping at the first level that loses no
    key; a separator's successor comes up from the leftmost leaf right of
    it;
  - an entry keeps the path its search found: reading or changing it goes
    straight to the bottom node, removing or inserting goes up the path;
  - each end of a walk is one (D23).
- **Why pointers, not indexes.** An index path is valid under the same
  condition as a pointer path, that nothing else changes the tree, which
  `&mut` already gives; it only turns a broken condition into a panic, and
  costs a walk down from the root at each use. Before, an entry walked down
  on every `get`, and insert and removal were recursions generic over how
  to find their way, with an insert recording where the new entry went
  (`Position`, `InsertionTarget`, `Placement`, `RemovalTarget`): all gone.
- **24 slots** (`MAX_HEIGHT` 23, from D22's floor of 4), 248 B. The slots are
  uninitialized until written; a path is made detached and started where it
  stays, and is never moved once written inside the tree code: moving one
  copies it with wide loads, which wait on the narrow stores that wrote it
  (D23). Passing the path by value to `remove`, wrapping it in a search
  result, or returning it from a constructor each did: the first version
  took 31 ns per `get` on a 1k map (HEAD 10.9), and `pop_first` was 1.6×.
  An entry and an iterator still move when returned.
- **Not done:** 8 levels in place and a `Vec` for the rest, which made a path
  112 B (`Iter` 248 B). Choosing the storage at every level cost 50–70
  instructions per operation, and `get` +13% at 10M, where fewer
  instructions let more independent lookups overlap.
- Measured, one run each, against the index paths and recursions before
  (ns per call):

  | | 10M u64→u64 before | after | std | 1k u64→u64 before | after | std |
  |---|--:|--:|--:|--:|--:|--:|
  | insert_random | 171 | 164 | 227 | 15.9 | 16.6 | 10.2 |
  | get_hit | 161 | 164 | 272 | 10.9 | 12.8 | 26.3 |
  | remove_random | 227 | 178 | 274 | 28.4 | 30.1 | 39.9 |
  | entry_upsert | 170 | 151 | 240 | 31.5 | 28.1 | 31.7 |
  | pop_first | 27.2 | 16.3 | 21.0 | 14.0 | 9.6 | 10.8 |

- **Open:** a lookup records its path, about 36 instructions more than the
  loop it replaced, which shows on small maps only (+17% at 1k).

**D25. Bulk changes: a builder of full nodes, fed by draining in order.** (2026-10-08)

- **`TreeBuilder`** (`node/build.rs`) builds a tree from entries in
  ascending key order, comparing no keys: each node fills before the next
  starts beside it, and `finish` tops up the right border to the minimum
  (D22) from the full left siblings. The tree is well formed throughout but
  for that border, so a panic part-way takes it apart as any other.
- **`collect` and `from`** sort, keep the last of equal keys (as std), and
  build. 10M u64→u64, one run: 2.02 vs std's 2.86 ns per entry, 16.3 vs
  18.2 bytes per entry; inserting the sorted entries one by one, as before,
  took 18.6 ns and 33.0 bytes (half-full nodes).
- **`append`, `retain` and `extract_if`** drain entries in order by value
  from the front (D23's owning walk; no `Ord`), and rebuild with the
  builder, keeping std's
  observable behavior: what the predicate sees and when, which key and
  value survive, drop order, and what is left after a panic. Removing
  while walking, as std's `extract_if` does in place, would need the walk's
  position tracked through every merge and steal, which std's parent
  pointers give for free.
- **`split_off` cuts the tree along the search path, as std's does**
  (`map/split_off.rs`): from the leaf up, each node on the path keeps what
  lies left of it, and what lies right goes into a new node, after the part
  cut off below; then both sides of the cut are brought back to the
  minimum fill from the top down (`node/border.rs`, which the builder's
  right border shares): each node on the border gets one key more than its
  minimum, by a merge with its sibling or keys moved over from it, so that
  a merge below leaves it at its minimum. A separator at the start of the
  right tree comes down into its first leaf that way. The lengths come
  from counting the shorter tree's entries a node at a time, as std's do.
  1M u64→u64, one cut (µs): at the median 12 000 by popping before → 95
  (std 320); at 1%, 2 400 → 3 (std 5).
- **Cost, for now:** `append` O(n + m) as std's; `retain` O(n) as std's, but
  moving the entries it keeps; `extract_if` O(n) wherever its range is,
  where std's is O(log n) plus the part it touches; `split_off` O(log n),
  plus the shorter tree's nodes to count. A drained entry costs about a
  step of a walk.

**D26. Per-node prefix stripping: a node's inline keys skip what its fences share.** (2026-10-08, exact since 2026-10-09)
Design and measurements in 05-fence-key.md.

- **What a node stores:** `prefix_len`, exactly what its fences (the
  separators beside it in its ancestors) share, 0 along the tree's borders,
  and its keys' windows past it as their inline keys. No fence key is stored
  (D10).
- **For every key with inline keys:** the trait gives windows of an encoding
  (D18), so newtypes get it as std byte strings do.
- **Only a separator change moves a fence,** never an insert into a node.
  The change itself reads no key, so that a panicking `InlineKey` can't
  stop it halfway: a node that must shrink takes its parent's prefix, by a
  shift filled from the parent's separator. Once the entry is counted, the
  nodes whose fences changed fit them, reading their keys.
- **Exact, not "at most":** the first version kept prefixes at most what
  the fences share, grown only where inline keys tied. Exact needs no gate
  and no test-only growth rule, and rebalanced nodes keep their prefixes,
  for 4–23% on inserts, gets and removals of keys that rarely share
  prefixes.
- **Measured:** 24–34% faster at 1M on keys with shared prefixes (paths,
  addresses), 9–12% on Wikipedia titles' inserts and gets; on random strings,
  inserts about 25%, gets 5–10% and removals 21–23% slower than without
  stripping.

**D27. Prefetch behind a default `prefetch` feature; benchmarks time without it.** (2026-10-09)
Refines D20.

- **Library:** the `prefetch` feature, on by default, gates the one
  software prefetch, `prefetch_search_area` (`node/handle.rs`). Without it
  a search reads the node as it goes.
- **Benchmarks:** `bench` depends on bbtree without default features, so a
  run times the code's own memory access unless built with
  `--features prefetch`. The report header records which.
- Why: prefetch hides layout and access-pattern effects, and how much it
  helps depends on how many lines it asks for. In the A/B below, the same
  layout moved by ±5–10% depending only on what was prefetched, so a
  layout change can't be judged with prefetch on.
- The A/B that prompted it: prefetching whole leaves (keys and values) in
  inline-key layouts, a stand-in for interleaving keys and values, made
  10M `str-json` gets 7–9% and inserts 6–7% faster but `u64` gets with
  inline keys 7% slower; prefetching inline keys only was slower on every
  String-like key (`hash32` gets +33%).
- Measured with the feature off (10M `get_hit`, one run): `u64` 166 →
  199 ns, `str-json` 339 → 427 ns, `u64` with inline keys 135 → 133 ns.

**D28. Branch early, loop late: a node's kind is decided once per step.** (2026-10-09)

- **The rule:** a traversal decides whether a node is a leaf or internal
  once, at the top of each step (`NodeRef::force`, or the height a loop
  already knows), and the code after it is for that kind. Likewise a choice
  fixed for a whole loop, such as the end a border is on, is made before
  the loop, as a type.
- **Enforced by the types:** what depends on the kind, a node's capacity
  and where its slots are, is only for `PageKind` handles (`Leaf`,
  `Internal`); a `LeafOrInternal` handle can only be told its kind. Before,
  `NodeKind::is_leaf` answered at run time for `LeafOrInternal`, so every
  such method also compiled on untyped handles and decided the kind inside:
  a descent decided it three to six times per level, a steal or merge five
  to eight times.
- **What it took:** the descent forces each level once and searches it as
  that kind; `TrailingSlots` (a leaf's head and values, an internal node's
  edges) makes splitting, stealing and merging one code for both kinds;
  `BalancingContext` is typed by its children's kind, which
  `has_leaf_children` decides once per node; a walk's cursor reads its leaf
  edge without deciding (its invariant); prefix refits take the kind as a
  type (`PrefixGrowth`); `NodeEnd` is a type (`FirstEdge`, `LastEdge`);
  `append` builds through one `extend`.
- **Why:** the inline-key count of a node with 8-byte inline keys was one
  `filter().count()` in two arms, a leaf's and an internal node's; where
  their capacities differed, LLVM summed the compares' bits one at a time.
- **Measured** against the commit before, prefetch off, medians of three
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

**D29. Nodes with inline keys pair each key with its trailing slot.** (2026-10-09)
Supersedes D9's layout and D12's edges at the end of the node.

- **Layout** (`layout.rs`): a node's trailing slots are a leaf's head and
  values, or an internal node's edges, slot `i + 1` beside key `i`. With
  inline keys, keys and trailing slots are `(key, slot)` entries growing up
  after the inline keys, slot 0 alone in an entry before entry 0, its key's
  place unused. Without inline keys, the keys are an array, and the
  trailing slots one right after the keys' capacity; nothing grows down.
- **Why:** a search runs over the inline keys, and in a leaf it then reads
  the key it ends at, which brings its value along.
- **Cost:** an entry pads its key to its slot's alignment, and the extra
  entry takes one key's room: `String` → `u64` holds 24 keys instead of 25,
  `u64` with inline keys 55 instead of 56, `u128` with inline keys 28
  instead of 38.
- **Measured** against D28, prefetch off, medians of three interleaved
  runs (ns per op), with leaves alone paired as an experiment, and both
  kinds as built:

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

- **Open:** pairing internal nodes costs lookups what pairing leaves gains
  (`hash32` DRAM fills per lookup: 11.4 separate, 8.3 leaves paired, 10.9
  both). An internal node usually reads only an edge after its inline
  keys, and edges side by side may keep each node's few edge lines in
  cache; not profiled yet. Inserts are slower in every paired layout.

**D30. Inserts search like lookups: no append fast path.** (2026-10-09)

- **Before:** `insert` and `entry` searched each node expecting the key to
  go after its last: a key greater than the node's last key went straight
  down the last edge, skipping the node's search. With inline keys, that
  check built the key's window at the node's prefix length and compared it
  with the last inline key, and the full key only on a tie.
- **Why removed:** a shortcut for one workload, ascending keys, that every
  other insert pays for: with inline keys, 41 instructions more per random
  insert at 10M. And it was all of what looked like inline keys' loss on
  sequential inserts: its check, not the search.
- **Measured** on the commit before, prefetch off, one run, ns per insert,
  instructions in brackets:

  | cell | with the check | without |
  |---|--:|--:|
  | `u64`, 10M `insert_sequential` | 8.2 [185] | 44.5 [473] |
  | `u64` with inline keys, 10M `insert_sequential` | 13.3 [330] | 40.1 [481] |
  | `u64`, 1k `insert_sequential` | 4.9 [146] | 14.4 [256] |
  | `u64` with inline keys, 1k `insert_sequential` | 7.8 [233] | 11.6 [249] |
  | `u64`, 10M `insert_random` | 179 [442] | 169 [441] |
  | `u64` with inline keys, 10M `insert_random` | 132 [415] | 131 [374] |

- **Open:** sequential inserts are 3–5× slower than with the check. With
  inline keys and D29's layout, a 10M sequential insert takes 41.6 ns
  [506], std's `BTreeMap` 38.7 ns [751]; at 1k, 11.9 and 12.2 ns.

**D31. `Auto` gives the 64- and 128-bit integers inline keys.** (2026-10-09)
Supersedes D18's integers opting in.

- **What:** `Auto` recognizes `u64`, `i64`, `u128` and `i128` by their type,
  as it does std byte strings, and uses their own `InlineKey` impls:
  2-byte inline keys, as `WithInlineKey` gives them. Other integers have
  no impl and get none; a newtype around one of the four gets none either,
  unless it implements `InlineKey` and the map uses `WithInlineKey`.
- **Why now:** sequential inserts were the loss that kept them opt-in, and
  that was the append check (D30). The count over 2-byte slots runs in
  explicit AVX-512 vectors whatever the capacity, and since D28 no count
  is compiled for two node kinds at once.
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

  Dense keys (`u64-seq`, 0 to N) gain less: 10M `get_hit` 26%,
  `insert_random` 13%, 1M `get_hit` 10%. Nodes on the tree's borders, the
  root among them, have no prefix (D26), so their inline keys are the top
  bytes of the integers, all zero for small ones.
- **Cost:** 2 bytes more per entry, 3 at the leaves' fill: 13% more for
  `u64` → `u64`, 26% for a `u64` set. A node holds 55 `u64` keys
  instead of 63.

**D32. Internal nodes split keys from children; nodes align to 64 bytes.** (2026-10-10)
Updates D29's internal layout, D15's alignment, D22's split point, D23's
folds and D24's point lookups. The experiments behind D32–D45 are in
[06-cost-model.md](06-cost-model.md), the combined results in
[07-large-map-optimization.md](07-large-map-optimization.md).

- **Internal nodes keep keys and child pointers in separate arrays.**
  Leaves with inline keys still pair each key with its value (D29). An
  internal search ends at a child pointer and rarely reads a full key, so
  child pointers should share cache lines with each other, not with keys.
  A leaf search reads the key it ends at and then its value, so pairing
  still pays there. Settles D29's open question.
- **Nodes align to 64 bytes, not 1 KiB:** `max(64, align_of K, align_of
  trailing slot)`. Paths keep indexes in their own array, so nothing uses
  a node pointer's low bits and 1 KiB alignment bought nothing.
  - **Gain:** with the System allocator, 100,000 touched 1 KiB blocks take
    110 MB of RSS instead of 206 MB (−47%).
  - **Cost:** a node can now straddle a 4 KiB page (15 of 64 offsets).
    Timings moved about 1%, except 1M JSON `get_miss`: +7.6%, same
    instructions, more DRAM fills.
- **Point lookups keep no path.** `get` and `get_mut` hold only the
  current node. Inserts, entries, removals and range bounds keep paths.
- **Consuming `fold`/`rfold` run one counted loop per leaf** and update
  the cursor once per leaf; `next` still crosses leaves through the
  boundary helper. An unbounded range end is set up only when first used;
  explicit bounds are still searched when the range is made.
- **A split at a node's end leaves the arriving side
  `min(2 × minimum, capacity / 2)` entries;** the old node keeps the rest.
  A split in the middle stays even. Sorted inserts now leave full nodes
  behind, not half-full ones, and the headroom above the minimum keeps one
  removal from undoing the split. Inserts that fit pay nothing.
  At 10M `u64`: sequential insert 37.9 → 23.8 bytes/entry (−37%), random
  insert 27.0 → 27.2 (+0.7%).
- **Fixed byte arrays get 2-byte inline keys under `Auto`, down from 8.**
  `[u8; 16]`, `[u8; 20]` and `[u8; 32]` hold their bytes in the node, so
  a tie costs a load from the same node, not a heap read; narrower
  summaries mean fewer cache lines per search. Heap strings keep 8 (D43
  narrows their internal nodes to 4); `WithInlineKey` keeps each impl's
  width. 10M `hash32`: hits 318.1 → 300.4 ns, misses 282.1 → 207.4,
  inserts 336.0 → 310.9; DRAM fills per hit 10.7 → 6.1.
- **Search details.** A byte-string query's windows are read from its own
  bytes, not from a 128-byte zero-padded copy. On x86-64 without POPCNT, a
  sorted SSE2 mask that fits one 64-bit word gives both bounds directly,
  with no software population count. AVX-512 and POPCNT targets are
  unchanged.

**D33. Cheaper fitting inserts, a local search prefix, cached range runs.** (2026-10-10)

- **A fitting insert has its own short path.** Allocation and split
  propagation move to an outlined function with its own stack frame
  (integer insert frame: 2,216 → 360 bytes). Growing a node keeps the
  max-valued summary padding already there, and an insert at the end skips
  the empty tail move. Replacing a value borrows it in place instead of
  copying the path. 10M ordered `u64` insert: 36.0 → 34.0 ns; random
  insert unchanged.
- **An internal byte-string node can keep a longer local search prefix.**
  - **When:** all of the node's 8-byte windows tie and its keys share at
    least 8 more bytes past the fence prefix (05-fence-key.md). The longer
    offset goes in 2 spare header bytes.
  - **Search:** a query first checks that it has those extra bytes; if not,
    it goes to the first or last child.
  - **Mutation:** the fence prefix itself never changes, so child prefixes
    and moved windows still use it. Before keys move, the node restores its
    ordinary windows; afterwards it rebuilds the local ones. Windows handed
    out by split, merge and clone are always the ordinary ones.
  - **Why:** paths share long prefixes; the root has no fences, so its
    prefix is 0 and all its windows tie. On 1M paths, full-key comparisons
    per query fall 7.7 → 3.6. Only seven internal nodes qualify; JSON has
    none.
  - **Measured:** 10M paths 3–4% faster for insert, hit and miss; 1M path
    hits 492 → 455 ns.
- **A range iterator caches a checked leaf interval.** Repeated
  `next`/`next_back` step inside it; switching direction, crossing a leaf
  or folding first syncs the path. `Range`/`RangeMut` grow 512 → 552
  bytes. 10M `range64`: `u64` 736 → 685 ns (−7%), JSON 1302 → 1204 ns.

**D34. Align leaf pairs so a key and its value share a cache line.** (2026-10-10)

- **What:** when a leaf entry (key and value) is a power of two up to 64
  bytes, entries start at a multiple of its size. `u64` → `u64` pairs
  (16 B) now start at byte 144, not 136; `String` → `u64` (32 B) at 256,
  not 232.
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
- **Cost:** `Iter`, `IterMut` and `Keys` grow 520 → 560 bytes.
- **Measured,** manual `next()`, 2M entries, baseline x86-64: `u64`
  4.50 → 3.99 ns/entry (−11%), `hash32` 9.36 → 8.73 (−7%). The native
  10M fold is unchanged (2.21 ns/entry).

**D36. String leaves keep 2-byte length tags in spare padding.** (2026-10-10)

- **What:** for recognized heap byte strings, a leaf stores each key's
  length mod 65,536 in 2 bytes, only where existing padding holds the whole
  tag array before both the head value and the first key. Capacity and
  allocation size don't change. `KeySlots` moves tags with their keys.
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
- **Pitfall:** the first version made JSON hits 25% slower. A second
  caller made LLVM outline a tiny summary-view wrapper, and its population
  count grew from 26 to 244 instructions. The wrapper is now forced inline.

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

**D39. Fixed-array internal nodes put children before keys.** (2026-10-10)

- **What:** for `Auto`'s 16-, 20- and 32-byte arrays, an internal node's
  child array comes right after the summaries, then the full keys. Page
  size and capacity don't change; a layout that would lose capacity keeps
  the old order.
- **Why:** with 32-byte keys, every key now sits inside one cache line
  (half crossed one before), and summaries plus children take one line
  fewer.
- **Not for heap strings:** their local prefix check (D33) reads the first
  key right after the summaries. Custom strategies keep their layout.
- **Measured** at 10M `hash32`: hits 240.9 → 225.1 ns, misses
  190.2 → 168.4, inserts 206.2 → 188.1. Same instructions; fewer L1 and
  DRAM misses.

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
  - The thin `NodeRef::search` adapter, which only re-attaches the
    caller's node and height to the found index, is `#[inline(always)]`.
    The search body itself may still be outlined.
  - Local-prefix guards (D33) compare 32 bytes at a time; a cold helper
    orders a mismatch.
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
  1 KiB. Before a key moves from an internal node into a leaf, its full
  8-byte window is recomputed from the key. Custom strategies and fixed
  arrays keep their width.
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
Supersedes D30's rejection of an append fast path; D30's per-node check
stays rejected.

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
- **Safety:** the comparison and inline key are computed before anything
  changes. The check hands back a private token, and only consuming it
  moves the key and value in, so a panicking `Ord` or `InlineKey` leaves
  the map unchanged and the pointer cleared.
- **Why this is not D30:** D30's check ran in every node of every insert
  (+41 instructions per random insert). This one is a single `Option`
  test per insert while unset. For random keys it can only be set at a
  new maximum: about 17 times over 10M inserts. No counters, no tuned
  thresholds.
- **Cost:** the map is 32 bytes instead of 24 (std's is 24). JSON random
  insert +2.3% in one run; other random inserts within ±0.5%. Descending
  and middle-ordered runs get no shortcut.
- **Measured,** 10M ordered inserts: `u64` 31.1 → 5.3 ns (std 39.5), JSON
  121.7 → 45.3 (std 198.9), paths 185.9 → 63.1 (std 256.0).

**D46. No software prefetch.** (2026-10-10)
Supersedes D20 and D27.

- **What:** the library issues no prefetch instructions, and the `prefetch`
  feature is gone. Hardware prefetchers work as before. AGENTS.md forbids
  explicit prefetching anywhere, including benchmarks and prototypes.
- **Why:**
  - Prefetch hides what the layout does with memory, so layout changes
    can't be judged with it on (D27). The benchmarks already ran without
    it, which made the default build one that no decision was measured on.
  - It fetched every key line up to capacity, at every level, whatever the
    search needed. After D32's separate internal arrays, `u64` and
    `String` internal nodes prefetched their full keys, which a search
    rarely reads, and not their child pointers, which it always does.
  - Memory latency is a layout problem: put what an operation needs in
    the lines it already loads.
- **Measured** on D45's working copy, 10M, prefetch off → on, one run each
  (the off and on runs were about 9 hours apart):
  - Misses got slower: `u64` 92.9 → 116.5 ns (+25%), JSON 235.9 → 263.7
    (+12%), `hash32` 138.4 → 184.5 (+33%). `hash32` hits too: 183.8 →
    195.6 (+6%).
  - DRAM fills per lookup rose 4–6×: `u64` hit 3.8 → 14.9, `hash32` miss
    3.9 → 25.5.
- **Cost:** the default build gives up what prefetch did help: random
  inserts (`u64` 137.1 → 105.0, JSON 276.7 → 240.3, `hash32` 197.9 →
  158.4), `u64` `range64` (659.3 → 504.9) and some hits (`u64` 130.2 →
  116.9, JSON 305.2 → 285.0, paths 563.0 → 469.3). These are the
  prefetch-on numbers the default build no longer reaches.

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

**D14. Unsafe lives behind zero-overhead typed abstractions.** (2026-10-07)
Every runtime fact the unsafe code needs is a check the B-tree algorithm
already makes for its own reasons (is the node full? does the merge fit? is
this a leaf?). That check returns a token, and the unchecked operation
requires the token: one check, no duplicates, nothing on the hot path.

- **One memory primitive:** `Slots<T, Dir>`, a fixed-capacity array in a page
  that grows up (keys, edges) or down (values). Insert, remove, move a run to
  another node, view as a slice. Written and tested once.
- **Positions are proofs:** `Edge` (index ≤ len) and `Kv` (index < len).
  They come from our own search (in bounds for any `Ord`), first/last
  positions, or checked constructors that are off the hot path.
- **Room and merge are tokens:** the insert-or-split decision yields the token
  `insert_fit` needs; the should-we-merge check yields the token `merge` needs.
- **Handles carry the height,** so `force` (leaf or internal) and `descend`
  are safe; no caller ever asserts a node's kind.
- **Edges come only from owned subtrees:** `Subtree { ptr, height }` goes in,
  and removing an edge gives one back. The height match is asserted on
  split/merge, off the hot path.
- **Paths lend one mutable node at a time** (built with the tree layer).
- **Enforced:** `#![forbid(unsafe_code)]` in every module except the slots,
  handle and page modules of `node/` and the tree's path module.

**D44. A child's prefix is never shorter than its parent's, even with a lying encoding.** (2026-10-10)

- **What:** when a refit computes a child's prefix, or the border beside a
  replaced separator shrinks, the target is floored at the parent's
  current prefix. A valid `InlineKey` already guarantees this; the floor
  keeps it when a custom encoding breaks its ordering contract. No user
  callback runs while keys are half moved.
- **Why:** while testing D45, a reversed encoding asked for a child prefix
  of 6 under a parent prefix of 7, and split promotion hit a debug
  assertion. The assertion stays; the missing invariant is now enforced at
  refit. This fix stands whether or not D45 is kept.
- **Test:** the adversarial-encoding test checks every parent/child pair
  after each operation, without calling the lying encoder, and keeps its
  exactly-once drop ledger.

## Proposed, not decided

- **Path-stack frame** = one word: node address | index in the low 10 bits
  (D15), using strict-provenance `map_addr`. Caps node capacity at 1023.
- **Stack depth `MAX_HEIGHT`:** D13 fixes it. Capacity ≥ 12 means a
  non-root internal node has ≥ 6 children, so the height is ≤ 21. That gives
  ~170 B per stack with one-word entries.
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
- **Fill factor:** right-biased split for appends, redistribution to a sibling
  before splitting. (The full-node bulk build for `FromIterator`/`append`
  is D25.)
