# 06 — Performance cost model

I think it's good to define a cost model before we start to benchmark or profiling.

Workload:
- U64 and String keys, u64 values
- 1M, 10M, 100M entries in the tree

Parameters:
- Node size: inner node, leaf node -> tree levels

Cost:
- Memory usage.
- Expected number of cacheline loads 
- Expected of chained cacheline loads vs parallel cacheline loads
- Expected number of comparisons, key loads
- Expected instructions
- Expected branches

Mindset:
- I think above metrics can be calculated, not just measured.
- We should use the cost model to guide the design of data structure.

## The model (2026-10-10)

The target is a single-threaded, generic `BTreeMap` replacement at 1M and
10M entries. Lookups, inserts, ordered scans and memory count together.
Software prefetch is off in every measurement.

### What we borrow, and what we can't

From Graefe, *More Modern B-Tree Techniques* (2024, DOI
10.1561/1900000070; printed pages):

- §3.1, pp. 180–181: row layouts favor point lookups and updates, column
  layouts favor scans; hybrids keep the hot fields compact per cache line.
- §3.3, pp. 185–186: the split point can follow a sorted insert stream;
  choosing a split point and choosing node capacity are separate decisions.
- §3.4, pp. 187–188: related searches can reuse traversal state.
- §5.5 and §7.2, pp. 216–217 and 231–234: keep full keys and pointers out
  of the comparison stream with dense fixed-width codes; prefixes shared
  by a page allow random access.

A database page can replace a key with an offset into packed bytes; we
can't. `iter` and `get_key_value` return `&K`, so a real `K` must exist in
the node. For the same reason, separators can't be truncated. A summary
can avoid reading the key, but the key must stay.

### Memory and depth

With leaf and internal capacities `C_l`, `C_i`, node sizes `S_l`, `S_i`
and fill fractions `u_l`, `u_i`, a tree of `N` entries has about:

```text
leaves          L ≈ (N + 1) / (u_l C_l + 1)
fanout          f = u_i C_i + 1
internal nodes  ≈ L / (f - 1)
node bytes      ≈ L S_l + [L / (f - 1)] S_i
```

(A non-root leaf holds one extra head value for the separator above it,
hence the `+ 1`.) This predicts depth, not guarantees it: fill differs
between random, sorted and bulk inserts, and between levels. 70% fill
below is an assumption.

Exact capacities (depth counts the leaf; bytes exclude heap-owned key
contents and allocator overhead; 512-byte leaves are hypothetical):

| Key → `u64` | Leaf / internal bytes | Internal layout | `C_l` / `C_i` | Depth, 1M / 10M | Bytes/entry, 10M |
|---|---:|---|---:|---:|---:|
| `u64` | 1024 / 1024 | paired | 55 / 55 | 4 / 5 | 26.60 |
| `u64` | 1024 / 1024 | separate | 55 / 56 | 4 / 5 | 26.59 |
| `u64` | 1024 / 2048 | separate | 55 / 112 | 4 / 4 | 26.59 |
| `u64` | 2048 / 1024 | separate | 112 / 56 | 4 / 5 | 26.12 |
| `u64` | 512 / 1024 | separate | 27 / 56 | 4 / 5 | 27.04 |
| `String` | 1024 / 1024 | paired | 24 / 24 | 5 / 6 | 60.95 |
| `String` | 1024 / 1024 | separate | 24 / 25 | 5 / 6 | 60.82 |
| `String` | 1024 / 2048 | separate | 24 / 50 | 4 / 5 | 60.82 |
| `String` | 2048 / 1024 | separate | 50 / 25 | 5 / 6 | 58.51 |
| `String` | 512 / 1024 | separate | 11 / 25 | 5 / 6 | 65.58 |
| `u128` | 1024 / 1024 | paired | 28 / 28 | 5 / 6 | 52.25 |
| `u128` | 1024 / 1024 | separate | 28 / 38 | 5 / 5 | 51.58 |
| `u128` | 1024 / 2048 | separate | 28 / 77 | 4 / 5 | 51.55 |

Leaves dominate memory. Internal nodes are small (about 6 MiB for 10M
`u64`, 31 MiB for 10M `String`), but they are what every lookup crosses,
so their layout matters for cache residency, not for total bytes. D43
later raised `String` internal capacity to 28.

### Lookup

The machine is a Ryzen 9 9900X: 48 KiB L1D and 1 MiB L2 per core, and two
separate 32 MiB L3s, so one thread can't treat 64 MiB as its cache. At 10M
entries even the `u64` nodes are several times one L3.

```text
lookup ≈ Σ over levels [read header + search summaries + read edge]
         + final key equality + value read
```

These steps are a chain: a child's address depends on the parent's edge,
and the window to compare depends on the node's prefix length. Inside a
node, vector loads of the summaries overlap; a binary search's probes
don't. Counting comparisons misses all of this.

A 1 KiB `String` internal node with separate arrays holds its header and
summaries in 4 cache lines and its child pointers in 4 more; the full keys
in between are rarely needed. Paired, the child pointers spread over the
whole node, so repeated searches need up to 16 lines cached instead of 8.
A `u64` internal node's header and summaries fit in 2 lines.

A `String` hit still needs a full key comparison unless the summary and
length prove equality. That follows a pointer into the heap. More fanout
can't remove this last read; first remove dependencies and instructions,
then consider bigger nodes.

### Insert and scan

A random insert shifts about half a leaf: at 70% fill, about 308 bytes of
pairs for `u64` and 269 for `String`, plus 39 and 67 bytes of summaries.
Doubling the leaf doubles the shift. A split also allocates, moves half a
node, inserts a separator and may refit prefixes. Internal nodes only
change on splits and merges, so a bigger internal node costs less on
insert than a bigger leaf.

A scan pays the descent once, then crosses a leaf boundary about every 40
`u64` pairs or 18 `String` pairs at 70% fill. Each crossing climbs and
descends the node stack. Bigger leaves mean fewer crossings; batched iteration
(`fold`) means less bookkeeping per entry without bigger leaves.

## What the measurements taught us

The model above was wrong or incomplete in five ways. Later sections show
the evidence.

1. **Charge summary search at capacity, not fill.** The SIMD search loads
   all slots, live or not. A 2 KiB internal node saves a level but scans
   336 summaries per `u64` descent instead of 224.
2. **A saved level is not a saved cache miss.** Upper levels are warm.
   Removing a warm level saves instructions, not DRAM time. Most wins on
   warm levels came from fewer instructions: no node stack bookkeeping, no
   128-byte query copy, no result passed through memory.
3. **Code generation can flip a result.** Adding a second caller made
   LLVM outline a tiny wrapper, and a 26-instruction population count
   became 244 (D36). A plain `#[inline]` hint left `String` results going
   through memory and made JSON hits 9% slower (D41). Read the linked
   assembly before timing.
4. **Fewer cache misses and lower latency don't always go together.**
   Aligned leaf pairs cut DRAM fills with no speedup (D34). Wikipedia
   lookups and ordered path inserts got faster while DRAM fills rose.
5. **Fill varies by level and size.** At 10M `u64`, parents of leaves are
   65% full with 1 KiB internals and 60% with 2 KiB; at 1M, 77% and 86%.
   Use counted per-level fill, not one constant.

## Experiments

Each entry: the idea, what was measured, the verdict. Unless noted,
numbers are 10M entries, native CPU, mimalloc, one run, ns per operation.
"Portable" means baseline x86-64 (SSE2, no POPCNT); those runs mostly
used the 2M-entry System-allocator diagnostic in bbtree-eval's
`diagnostics/portable`, whose numbers can't be compared with the main
suite.

| Experiment | Verdict | Decision |
|---|---|---|
| Separate internal keys and children | accepted | D32 |
| 2 KiB internal nodes | rejected | |
| 512-byte leaves | rejected | |
| 64-byte node alignment | rejected | |
| 2-byte inline keys for fixed arrays | accepted | D32 |
| End-biased splits | accepted | D32 |
| Short fitting-insert path | accepted | D33 |
| Range construction without the extra copy | not adopted | |
| SIMD scan of full `u64` leaf keys | rejected | |
| Shared prefix where fences are missing | accepted | D50 |
| Bulk compare of long suffixes | rejected | |
| Cached range leaf run | accepted | D33 |
| Aligned leaf pairs | accepted | D34 |
| String hash tags | accepted | D36 |
| Whole-map iterators reuse the leaf run | accepted | D35 |
| Insert without a node stack | accepted | D37 |
| One fixed-array candidate: equality only | accepted | D38 |
| Two hash-tag candidates; zero/one handling | accepted | D36, D38 |
| Redistribution before a split | deferred (model) | |
| 4-byte string leaf summaries | rejected (model) | |
| Children before keys for fixed arrays | accepted, moot since internal nodes keep no keys | D49 |
| Equality-only leaf masks | native 8-byte only | D41 |
| Portable packed 2-byte masks, 33–64 slots | accepted | D40 |
| Grouped local-prefix guards | accepted with D41 | D41 |
| Slotted leaves | deferred (model) | |
| 4-byte string internal summaries | accepted | D43 |
| Byte-query view and inlined search adapter | accepted | D41 |
| Pointer-based scan runs | accepted | D42 |
| Portable 4-byte masks: one stage, binary above 32 | accepted | D43 |
| Children before keys for `u128` | deferred | |
| Exact guard bytes inside the node | deferred (model) | |
| Re-splitting summary loads at cache lines | rejected (codegen) | |
| Last-leaf append cache | accepted | D45 |
| Value updates keep the last-leaf cache | accepted | D45 |
| Another short-range boundary shortcut | deferred (model) | |
| 2 KiB upper levels only | deferred (model) | |
| Sign-biased portable summaries | rejected (codegen) | |
| Map-owned page arena | deferred (model) | |
| Min/max node prefixes instead of fences | rejected | |

### Node size and layout

**Separate internal keys and children (D32).** Against paired internals:
`u64` hit 134.0 → 131.0, miss 99.4 → 97.4; JSON (1M) hit 266.6 → 258.5;
path (1M) insert 454.6 → 422.1. Small gains everywhere. Accepted.

**2 KiB internal nodes.** Depth does drop (10M `u64`: 5 → 4 levels), but
hits get slower (131.0 → 164.0) and misses much slower (97.4 → 160.7).
DRAM fills are unchanged (3.87 → 3.90 per hit); instructions (260 → 296)
and L1 misses (13.5 → 16.7) rise. The removed level was warm; the wider
summary scans are not free. JSON at 1M improved 3–6%. Rejected as a
default. Lesson 1 comes from here.

**512-byte leaves** (for types that still fit 12 keys). Header and
summaries of a `u64` leaf fit one cache line, and random insert shifts
half as much. Random insert −10.5% and DRAM fills fall (3.84 → 2.95 per
hit), but hits only −3%, sequential insert +13.7%, folds +69%, `range64`
+15%, memory +2% (random) and +13% (sequential). Rejected.

**Upper levels only at 2 KiB** (model only). Keeps the bottom levels
small, but needs a third page type, and on the real 2M `u64` tree both
upper levels stay while summary loads rise 6 → 10 per descent. Deferred.

**Per-node adaptive sizes** (model only). Each node would need a size tag
or dispatch, conversion on growth and hysteresis on shrink. The 2 KiB
result already shows that fewer levels alone don't pay. Deferred until a
workload shows a benefit.

**Slotted leaves** (model only). Keeping pairs in arrival order behind a
sorted 1-byte slot directory would cut insert movement (`u64`: about 349
→ 58 bytes shifted per insert). But every payload read then waits on the
directory, scans stop being linear, capacity drops (`u64` 55 → 53,
`String` 24 → 23), and every slot/iterator proof changes. Deferred.

**Aligned leaf pairs (D34).** DRAM fills per hit fall (`u64` 3.86 →
3.68, JSON 9.18 → 8.80) with identical instructions, but latency barely
moves (`u64` hit 131.5 → 129.6, JSON hit 348.7 → 353.5). Accepted for
the traffic and the 48 bytes it frees for D36.

**Children before keys for fixed arrays.** `[u8; 32]` keys stop
crossing cache lines and summaries plus children shrink by one line.
`hash32` hits 240.9 → 225.1, misses 190.2 → 168.4, inserts 206.2 → 188.1,
same instructions, fewer L1 and DRAM misses. Accepted; moot since internal
nodes with inline keys keep no keys (D49).

**Children before keys for `u128`.** Native random operations improve
2–7%, but portable random hit and insert lose 1–2% and native dense misses
lose 28% (23.3 → 29.8, cause unknown). Deferred. Diagnostic in
bbtree-eval's `diagnostics/integer-edge`.

**64-byte node alignment.** With glibc's malloc, a 1 KiB-aligned 1 KiB
block can't sit next to another, so every other slot is skipped:
100,000 blocks span 204.8 MB at 1 KiB alignment and 108.8 MB at 64.
mimalloc puts 1 KiB blocks on 1 KiB boundaries at either alignment, and
an A/B showed the same instructions and RSS, with timings within
run-to-run noise. Rejected: nodes stay 1 KiB-aligned (D15).

**Map-owned page arena** (model only). mimalloc already packs 1 KiB
blocks. Pages allocated in time order aren't neighbors in key order after
random splits, so an arena alone gives no scan locality. A shared arena
would have to outlive both halves of `split_off`, and one live page could
pin a whole chunk. At 0.027 pages per insert, saving 10% of insert time
would need about 510 ns saved per page. Deferred.

### Splits and fill

**End-biased splits (D32).** First try: leave only the minimum on the side
that gets the new key. Sequential insert memory fell 45%, but one removal
could then undo the split. Accepted form: that side gets `min(2 ×
minimum, capacity / 2)`. Sequential `u64` insert memory 37.9 → 23.8
bytes/entry (−37%) and time 42.3 → 36.0 ns; random insert memory +0.7%.

**Redistribution before a split** (model only). Moving entries to a
sibling with enough room (keeping `ceil(capacity / 8)` free on both
sides, to avoid one-entry ping-pong) would cut node memory 9–12%, but move
16–23% more data and more than double full-leaf events, which D37 pays
for with a second search. Deferred as a memory option.

### Lookup

**2-byte inline keys for fixed arrays (D32).** A `[u8; 32]` key's bytes
sit in the node, so a tie on a short summary costs no heap read, and 2-byte
summaries take a quarter of the cache lines of 8-byte ones. `hash32` hits
318.1 → 300.4, misses 282.1 → 207.4, inserts 336.0 → 310.9; DRAM fills per
hit 10.7 → 6.1. Accepted.

**SIMD scan of full `u64` leaf keys.** Compare every live key instead of
summaries to save one dependent load. Hits 131.3 → 360.7, misses 96.1 →
315.7; DRAM fills 3.8 → 16.1 per lookup. Rejected: summaries are the
better filter.

**Shared prefix where fences are missing (D50).** A node whose windows
all tie at its prefix searches its keys whole: on 1M paths, 7.7 full-key
comparisons a query when the root had prefix 0. Dense integers tie the same
way at the root and borders, UUIDv7-like arrays wherever their timestamp is
shared. Two ways to skip those bytes:

- **Per node,** a local prefix kept where the windows tie, checked against
  the node's first key. Built and measured (2026-10-10): dense `u64` hits
  −8.5%, inserts −5%; 1M paths −2.7%; UUIDv7-like `[u8; 16]` 366 → 270
  instructions at 10M, 27.7 → 7.5 ns at 1k. Every internal search paid a
  header field and a branch, 16–28 instructions a lookup on random keys;
  every key move restored and rebuilt the windows; and once separator keys
  became leaf heads (D49), each check walked to a leaf.
- **Per tree,** the bytes every key shares, beside the root, which the root
  and the borders, the nodes without a fence, take as their prefix. A
  query is checked once, at the top, against bytes in one line next to the
  root pointer; one without them reads no node. Model: one compare a
  lookup, a null test where keys share nothing; a shrink touches the two
  borders, O(log n) nodes, at most once per byte. Measured against D49:
  dense `u64` lookups −46 instructions, 10M paths −123, random keys +1.

Accepted per tree. Inside the tree, a node's keys fill the range between
its fences, so the fences give it nearly all its keys share.
Code generation decided most of the per-node work: a call anywhere in the
descent loop, even a cold one, kept the loop's state in stack slots (+30
to +100 instructions); comparing 64 shared path bytes a window at a time
took 300 more instructions than in 32-byte chunks, which the per-tree check
keeps. Node searches are forced inline for fixed-width keys (outlined,
`hash32` lookups took 442 instructions instead of 271) and left to LLVM for
heap strings (forced inline, the benchmark's `String` lookups took 449
instead of 433).
Carrying a parent's matched prefix down instead is unsound: it says
nothing about an outer child's other side.

**Min/max node prefixes instead of fences.** One prefix per node, what its
first and last keys share, would drop fences. Built, optimized and
measured (2026-10-12): JSON hits 309 → 446 ns, Wikipedia 423 → 527,
`hash32` 175 → 189; Codex's tuning got JSON to 409. Why, by adding each
cost to the fence tree alone: a prefix the fences don't guarantee must be
checked against a key at about two levels a lookup, and a leaf hit
confirmed from the key's bytes; for strings both read heap lines (+1.3
DRAM lines a lookup), and the bookkeeping adds about 120 instructions.
Rejected; fences stay, and the tree's shared prefix covers where they are
missing.

**Bulk compare of long suffixes.** About 100 fewer instructions per path
operation, but more branch misses: paths 1–2.5% slower, JSON 1–2% slower.
Rejected.

**String hash tags (D36).** On 1M paths, leaf heap reads fall from 1.98
to 1.20 per hit and 2.18 to 0.25 per miss. First version: JSON hits 25%
slower because LLVM outlined a tiny wrapper (lesson 3). Fixed version:
path hits −10.7%, misses −31.2%, inserts +2.0%; JSON flat. Accepted. A
tail-plus-length fingerprint would save only 0.3 more reads per lookup
and touch the query's tail; not tried.

**Two tag candidates; zero/one candidates handled directly (D36, D38).**
Allowing two matching tags (compare upper, then lower) cuts path heap
reads per hit 1.21 → 1.02. Alone it made JSON hits 3% slower (code
layout). With explicit empty/one-candidate handling: path hits 621.5 →
605.4; JSON within 1%. Accepted together.

**One fixed-array candidate: equality only (D38).** A `[u8; 32]`
candidate is checked with one load, XOR and test instead of an ordered
comparison. `hash32` hits 271.4 → 245.6 (instructions 527 → 431), misses
193.0 → 195.6. Portable hits −16%. Accepted.

**4-byte string leaf summaries** (model only). Would raise `String` leaf
capacity 24 → 26, but JSON heap comparisons on hits more than double
(23,653 → 51,869 per 100k). Rejected.

**4-byte string internal summaries (D43).** On internal nodes, the extra
ties cost little (JSON +0.02 heap reads per hit), and capacity rises 25 →
28. Native JSON insert 314.1 → 282.5, hit 343.8 → 320.4, miss 282.3 →
235.4, with fewer L1 misses; paths about 1.3% faster. Accepted (see the
portable search policy below).

**Equality-only leaf masks.** For lookups, only the matching positions
are needed. Native `u64` hit/miss improve 3–7% but with more
instructions, JSON misses −6%, path lookups +1%. Only the native 8-byte
case is kept (D41).

**Grouped local-prefix guards (D41).** Compare guard bytes 32 at a time.
Alone: paths about 1% faster, JSON 3% slower (an outlined helper spilled
a register). Kept only as part of D41's composition.

**Byte-query view and inlined search adapter (D41).** First composition
(query view + grouped guards + 8-byte masks): JSON hits 343.8 → 374.4,
rejected. Forcing the thin search adapter inline fixed it: JSON hit 343.8
→ 334.4, miss 282.3 → 249.4; path hit 601.0 → 585.0, miss 450.4 → 432.9;
`hash32` hit 224.8 → 185.9, miss 169.4 → 140.9. Binary 20 KB smaller.
Accepted.

**Exact guard bytes inside the node** (model only). Would remove one
dependent heap read at an active guard, but 64 reserved bytes cost
capacity (28 → 26), and storing them in unused slots needs its own
invalidation rules. Active guards are almost all at the warm root.
Deferred.

**Re-splitting summary loads at cache lines** (codegen only). A 56-slot
search's first load crosses a cache line; splitting 28 + 28 avoids it but
adds 8–10 instructions per node. Rejected before timing.

**Wikipedia titles.** A real-data check, 10M of 19.3M titles
(`enwiki-20260901-titles.txt`, SHA256
`27997e84a87a3916795548922776dfaa8443cf64a38c6dc7263af2ac933ab63f`).
Before → after: insert 458.6 → 357.3, hit 524.8 → 423.1, miss 406.1 →
334.7; std 594.6 / 783.3 / 814.2. DRAM fills rose 4–13% and branch
misses 29–58% while latency fell (lesson 4).

### Portable search (baseline x86-64)

Without POPCNT, LLVM counts mask bits in software, with multiplies.

- **2-byte summaries, 33–64 slots (D40):** pack two SSE2 comparisons into
  one bit per slot in one 64-bit word; bounds come from trailing-one
  counts. `u64` hits 94.3 → 67.1, misses 55.6 → 41.5, inserts 171.9 →
  142.5; `u128` hits 148.3 → 109.1. Accepted.
- **4-byte summaries, 17–32 slots (D43):** one packing step, same idea.
  Owned `String`, 10M main suite: JSON hit 408.7 → 390.6, miss 335.5 →
  297.3, despite 140–166 more instructions per operation.
- **4-byte summaries, 33–64 slots** (borrowed `&str`, capacity 36): a
  second packing step cut the reduction from 130 to 94 instructions, but
  borrowed JSON stayed slower than with 8-byte internals. Rejected; the
  compiler's count is used (04 Q6).
- **Sign-biased summary storage** (codegen only): would remove a few XORs
  per search, but the cached-insert path grows 43 instructions. Rejected
  before timing.

### Insert

**Short fitting-insert path (D33).** The old insert reserved split
scratch space every time (a 2,216-byte frame), called `memmove` even for
an append, and copied a 256-byte node stack to replace a value. After: a
360-byte frame. Ordered `u64` insert 36.0 → 34.0 (405 → 362
instructions); random insert unchanged. Accepted.

**Insert without a node stack (D37).** Full leaves are rare (2.6% of `u64`,
5.8% of string inserts at 1M), so recording the node stack only then, with a
second search, saves work on the rest. Random `u64` −4.1%, ordered `u64`
−8.8%, JSON −19.4% (824 → 649 instructions), paths −4.8%; DRAM fills
unchanged. The bigger string gain comes from no longer calling outlined
helpers at each level. Accepted.

**Last-leaf append cache (D45).** Ordered `u64` insert took 31.1 ns and
363 instructions with almost no cache misses: the cost is warm descent.
Remembering the last leaf after an end split and appending larger keys
directly gives: ordered `u64` 31.1 → 5.3, JSON 121.7 → 45.3, paths
185.9 → 63.1. Random inserts pay one `Option` test: `u64` −0.4%, `hash32`
+0.0%, paths +0.5%, JSON +2.3%. The first version returned the owned key
and value on failure, adding a 32-byte copy for `hash32` and 12
instructions for JSON; the final version checks a borrowed key and
returns a token that moves the key only on success. Baseline x86-64:
ordered `u64` 43.5 → 5.5, JSON 143.7 → 47.9; random +0.9% / +0.2%.
Accepted (D45); D30's per-node check stays rejected.

**Value updates keep the last-leaf append cache (D45).** A `get_mut`
cleared the remembered leaf, so ascending inserts alternating with value
updates descended the tree every time. Keeping the leaf: 10M `u64`
inserts each followed by `*map.get_mut(&(key / 2)) += 1` took about 103 →
54 ns per insert+update pair, while ascending inserts alone stayed at
about 18 ns; comparisons per insert, counted through `Ord`, went 23.55 →
2.41, the same as ascending inserts alone. A test asserts that equality,
so a regression in the cache shows as a test failure, not just a timing
change. Accepted (D45).

### Scans

**Cached range leaf run (D33).** `range(start..).take(64)` steps with
`next()`, which rechecked attachment, the other end and the leaf header
on every item. Caching the checked interval: `u64` `range64` 736.0 →
685.2 (instructions 2062 → 1735), JSON 1302 → 1204; DRAM fills flat.
Accepted.

**Range construction without the extra copy.** Removing one of two
512-byte copies of the range state: 736.0 → 723.9, with slightly more
instructions. Too small to claim; not adopted.

**Whole-map iterators reuse the run (D35).** Portable manual `next()`:
`u64` 4.50 → 3.99 ns/entry, `hash32` 9.36 → 8.73. Native fold unchanged.
Accepted.

**Pointer-based scan runs (D42).** Step a key pointer instead of
reloading the leaf and scaling an index: the per-item loop goes from 18
to 15 instructions. `u64` `range64` 688.2 → 664.5; portable `next()`
`u64` 3.92 → 3.52, `hash32` 8.74 → 7.52. Accepted.

**Another short-range boundary shortcut** (model only). The item loop is
already 15 instructions; a direct sibling step at leaf crossings might
save 60–80 of about 1,590 instructions per scan (4–5%) and no memory
fetch. Deferred.
