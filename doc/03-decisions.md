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
- **Width:** inline keys start at 8 bytes (`InlineWord = u64`). The width is
  one constant, to revisit on real keys (04-inline-key.md Q7). At 8 bytes, real
  neighbours tie 24–70% of the time; at 16 bytes, 1–20%.
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
- **Underfull** means fewer than `max(capacity / 8, 2)` keys, for a node below
  the root, by its kind's capacity: 7 keys for a u64→u64 leaf of 63.
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
- Consequence: a non-root internal node may have only 3 children (capacity
  12–23). The path-stack depth bound under "Proposed, not decided" (≥ 6
  children, height ≤ 21) becomes height ≤ 34 for 1 KB nodes in a 64-bit
  address space.

**D23. Iteration: a cursor per end, holding its path as edge indexes, with the leaf and its parent cached.** (2026-10-08)
Supersedes the path-stack frame and `MAX_HEIGHT` bullets under "Proposed,
not decided".

- **A position is an edge of a leaf.** Past a leaf's last entry, the next
  one is the separator of the lowest ancestor with a key beyond the path,
  and its value is the head of the leftmost leaf right of it (D17).
  Separator keys live only in internal nodes, so crossing a leaf boundary
  visits an ancestor whatever we do: leaf-to-leaf links would not help.
  `next()` never calls `Ord` (D11).
- **Each end stores its leaf, the leaf's parent, and the edge index taken at
  every level as a `u16`** (a node holds ≤ 1023 keys). The parent answers
  most boundary crossings; when it runs out, the cursor walks down again
  from the root by its edge indexes, still without `Ord`.
- **35 indexes per end:** with D22's 3-child minimum and 1 KB leaves, a tree
  taller than 34 levels above its leaves would need more than 2^64 bytes.
- **Sizes:** `Iter` 232 B, `Range` 224 B (std: 72 B, 48 B). A pointer per
  level would have made `Iter` ~560 B.
- **Lazy:** an end that has not started starts at the first or last entry
  on its first move, so iterating from one end never descends the other.
- **In place:** a cursor is updated field by field, never rebuilt and copied
  over. A copy reads it with 64-byte loads right after narrow stores, which
  can't be forwarded: the load waits for the stores to retire, and so for
  every older cache miss. Over many small maps, that serialized one map's
  misses behind the previous one's: one forwarding conflict per map,
  28.2 → 11.5 ns per entry for 1M 4-entry maps once removed.
- Measured, one run each, mimalloc, Ryzen 9 9900X (ns per entry; per scan
  for `range64`):

  | workload | bbtree | std |
  |---|--:|--:|
  | iterate, 10M u64→u64 | 5.85 | 14.2 |
  | range64, 10M u64→u64 | 719 | 1163 |
  | iterate, 1M maps × 4 (`str-json`→`struct32`) | 11.5 | 4.9 |
  | iterate, 100k maps × 32 | 10.8 | 7.2 |
  | iterate, 1M real JSON objects | 13.6 | 6.0 |

- **Mutable walks** (`IterMut`, `ValuesMut`, `RangeMut`) use a `ValMut`
  borrow type, as std's do: its handles copy, so both ends hold some, and
  only read the structure and keys; handing out a value as `&mut V` is
  unsafe. The walks vouch for handing out each value once: their ends move
  toward each other, and stop by a count (whole map) or where they meet
  (range). This is the path module D14 allows `unsafe` in, `node/walk.rs`.
- **Owning iteration, for now,** pops from either end, rebalancing as it
  goes (`into_iter.rs`). An owning walk that takes entries out and frees
  each node once both ends are past it is to replace the pops.
- **Open:** small maps are 0.43–0.67× std, from instruction count (57 vs 26
  per entry for 4-entry maps), not stalls. Range scans still take ~18
  forwarding conflicts per scan, from adapters such as `take` moving the
  freshly positioned `Range`.

**D24. Entries remember a position: edge indexes from the root.** (2026-10-08)

- `entry` searches once, calling every `Ord` the entry will need, and
  remembers the path to the key, or to the leaf edge where it goes, as edge
  indexes. Everything an entry does afterward follows that path down again,
  comparing no keys, as std's entries compare none after `entry`.
- **Removing** through an occupied entry runs the usual removal recursion
  (D22) with the position as its target, separators included (D17).
- **Inserting** through a vacant entry runs the insert recursion with the
  position as its target. On the way back up, each level records the index
  leading to the new entry: a split reports the half that took it, and a
  level that took a split child's separator leads to the new entry by the
  edge left of it, or right of it if the entry went to the child's right
  half. The entry then follows the new path down to hand out the value.
  Plain `insert` records nothing: the recording is a generic parameter that
  compiles away for it.
- No `unsafe`: following a path again costs a walk down instead of keeping
  node handles, which would need std's dormant references.
- Measured, one run each (ns per call): `entry(k).or_default()` counting,
  10M u64→u64: 164 vs std's 239; 1k u64→u64: 31.2 vs 31.5; 1k 16-byte
  strings: 42.6 vs 57.5.

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
- **`append`, `split_off`, `retain` and `extract_if`** drain entries in order
  by popping an end (no `Ord`), and rebuild with the builder, keeping std's
  observable behavior: what the predicate sees and when, which key and
  value survive, drop order, and what is left after a panic. Removing
  while walking, as std's `extract_if` does in place, would need the walk's
  position tracked through every merge and steal, which std's parent
  pointers give for free.
- **Cost, for now:** `append` O(n + m) as std's; `retain` O(n) as std's, but
  moving the entries it keeps; `extract_if` O(n) wherever its range is, and
  `split_off` O(the smaller part), where std's are O(log n) plus the part
  they touch. A popped entry costs ~a `pop_first`; an owning walk (D23,
  open) would make the drains cheaper.

## Navigation

**D11. No parent pointers; iterators use a path stack.** (2026-10-07)

- A split or merge never touches the moved children. std rewrites the parent
  link of every moved or shifted child (`node.rs:1045`), which would be ~85
  scattered writes per leaf split at our fanout.
- Iterators carry the root-to-leaf path inline, with no allocation. Unlike
  re-descending from the root (as bf-tree does), this never calls `Ord` inside
  `next()`, matching std.

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
