The blog post is along with the visualization.

## Blog content

The post's text as the site shows it (`vis/src/post.rs`,
`vis/src/how_it_works.rs`). Bracketed lines mark where a figure or the
charts sit; each figure of "How it works" shows one idea and nothing else,
given here as what it draws, a sketch, and its caption.

Title: bbtree: fastest ordered map in the world

> B-trees are by far the most important access path structure in database and file systems. — [Jim Gray](https://en.wikipedia.org/wiki/Jim_Gray_(computer_scientist))

**bbtree** is by far the fastest btree (ordered map):
1. 5x faster than std's `BTreeMap` at single thread, 7x faster than `crossbeam-skiplist` at 12 threads.
2. 30% lower memory usage, 5x lower p99 latency.
3. Zero dependencies, `no_std` compatible.
4. `unsync::BBTreeMap` is a **drop-in replacement** for std's `BTreeMap`/`BTreeSet`.
5. Production-ready by formal verification, model checking, fuzzing, and lots of testing.
6. Brought to you by decades of database research and careful curation by [Xiangpeng Hao](https://xiangpeng.systems).

[Charts: Single thread, Concurrent, End to End, Memory, Architectures]

### How it works

A lookup walks from the root to a leaf: 5 nodes at 10M entries.
At each node, it does two things:

1. **Search**: pick the child to go to.
2. **Fetch**: bring that child into cache, usually a miss to DRAM.

Every B-tree pays both, at every level.
bbtree makes the search cheaper (prefix keys, SIMD), makes the fetch smaller (layout), and overlaps the fetches of different lookups (batching).

#### Prefix keys to improve comparisons

Comparing two `String`s means following two pointers into the heap.
A binary search over a node of `String`s is a chain of heap misses: each probe waits for the one before.

The fix is old database wisdom[^graefe], and the same trick as German strings[^umbra]: keep a short, fixed-size prefix of each key in the node, and compare that instead.
bbtree keeps up to 8 bytes per key, in a dense array next to the keys.
Two keys whose prefixes differ are ordered by their prefixes alone; only a tie reads the heap.

But real keys share prefixes.
In 10M Wikipedia titles, 65% of neighbors share their first 8 bytes, and then the prefix decides nothing.

So bbtree skips what a node's keys share.
Every key in a node lies between the node's two fences (the separators beside it in its parents), so every key starts with whatever the fences share.
The node stores only that length, a `u16`, and keeps the 8 bytes *after* it.
No fence key is stored.

[Figure 1: four Wikipedia titles as the benchmarks store them, each with
the bytes they share dimmed; their first 8 bytes; and the 8 bytes past
what they share (`prefix_keys_figure.rs`).]

```text
key                              first 8 bytes   8 bytes past the shared 20

List_of_bridges_in_Afghanistan   List_of_        fghanist
List_of_bridges_in_Alabama       List_of_        labama··
List_of_bridges_in_Alaska        List_of_        laska···
List_of_bridges_in_Albania       List_of_        lbania··

                                 all tie:        all differ:
                                 compare in full decided in a register
```

> Four Wikipedia titles. Their first 8 bytes are the same, so a plain prefix decides nothing. They share their first 20 bytes, `List_of_bridges_in_A`: a node whose keys all share them skips them, and keeps the next 8 bytes, which tell the keys apart.

When we added prefix keys, Wikipedia lookups went from 1,073 to 704 ns and loaded half as many cache lines from DRAM.
Skipping shared prefixes made lookups of Linux paths another 1.5× faster (693 → 457 ns at 1M).

#### AVX-512/NEON SIMD to improve search speed

With prefix keys, a node search is a search over a small array of integers: 2 bytes a key for integers and UUIDs, 4 to 8 for strings, which is enough once the shared prefix is gone.

Binary search is the textbook answer, and the wrong one here.
Each step waits for the one before, and its branch is a coin flip for the CPU.

bbtree doesn't search; it counts.
The child to follow is the number of keys smaller than the target.
One AVX-512 instruction compares the target with 32 two-byte keys at once, and a popcount of the result is the answer.
A whole internal node is two 64-byte vectors: no loop, no branch, every load issued at once.
Empty slots hold the largest value, so they never count.

[Figure 2: one count. Eight slots, the last empty, the target under each,
the mask, the count (`key_count_figure.rs`).]

```text
prefix keys      3  11  27  40  58  71  90  max
target          45  45  45  45  45  45  45   45
key < target     1   1   1   1   0   0   0    0

                4 ones: go to child 4
```

> One compare checks every slot against the target; counting the ones gives the child to follow. Binary search would take 3 steps here, each waiting for the last, and 6 in a full node of 49 children.

On ARM, the same count runs in NEON.
On an M1 Pro, it took a 10M `u64` lookup from 390 to 174 ns, and from 1,678 instructions to 407.

AVX-512 needs `-C target-cpu=native` (Rust 1.89 or later); without it, bbtree counts with SSE2.

#### Careful layouts to improve cache locality

At 10M entries, the tree is many times larger than the cache, so most fetches go to DRAM.
What a fetch costs is the number of 64-byte cache lines it touches.
bbtree lays out each node so that a lookup touches as few as possible:

1. **1 KiB nodes.** Disk B-trees use big pages because a disk read is expensive. In memory, the unit is a cache line, and every insert shifts half a node. In our sweep, 1 KiB beat everything from 512 B to 8 KiB on random gets and inserts; 4 KiB nodes made random gets 1.4× slower.
2. **Internal nodes keep no keys**, only prefix keys and child pointers. A search reads the prefix keys, then one pointer; full keys in between took cache lines for nothing. 49 children fit in 512 bytes. On a tie, the search reads the full key from the leaf, where it lives anyway.
3. **Leaves keep each key next to its value**, aligned so that a pair never straddles two lines: the line that confirms the key brings the value with it.

[Figure 3: the leaf of one `u64` lookup in the site's 1M-key tree, a cell
a 64-byte line, the lines it read lit, from bbtree's page map and trace
(`cache_lines_figure.rs`). Only the leaf: the site runs bbtree built for
wasm32, whose 4-byte pointers give internal nodes another layout than a
64-bit machine's; a leaf of `u64` keys and values is the same on both.]

```text
leaf, 1 KiB: 3 of 16 lines read
██ ██ ░░ ░░ ░░ ░░ ██ ░░ ░░ ░░ ░░ ░░ ░░ ░░ ░░ ░░
└───┘ └───────────────────────────────────────┘
prefix          keys and values
keys
```

> The leaf of one `u64` lookup, a cell a 64-byte line. The lookup reads the lines lit: the prefix keys, then the one line that holds the key and its value. An internal node reads the same way: its prefix keys, then one child pointer.

And no software prefetch.
Prefetch hides a bad layout instead of fixing it: when we tried it, misses got 12–33% slower, and DRAM traffic went up 4–6×.

#### Batching to improve throughput

A single lookup can't beat its chain of misses: the CPU can't fetch a child before the parent's search picks it.

But a caller often has many keys at hand: a join probing an index, a bulk load.
Their lookups are independent, so their misses can overlap.

`get_many_copied` looks up an array of keys at once:

```rust
let values = map.get_many_copied([&a, &b, &c, &d]);
```

Each key walks the tree in its own *lane*: it searches a node, prefetches the child it picked, and steps aside.
While it waits for memory, the other lanes search.

[Figure 4: four `u64` keys looked up together in the site's 1M-key tree,
a row each: each search a block and, from each prefetch to that key's
next search, the line on its way; time counted in searches, from
bbtree's trace of the batch (`batch_lanes_figure.rs`).]

```text
131072   ■━━━━■━━━━■━━━━■━━━━■━━━━■━━━━■
500000    ■━━━━■━━━━■━━━━■━━━━■━━━━■━━━━■━━━■
851191     ■━━━━■━━━━■━━━━■━━━━■━━━━■━━━━■━━━■
999000      ■━━━━■━━━━■━━━━■━━━━■━━━━■━━━━■━━━■

■ search   ━ line on its way, from the key's prefetch to its next search
```

> Four `u64` keys looked up together on one thread, a row each, time counted in searches. After each search, a key prefetches the next node it reads; while that line is on its way, the other keys search. Up to 3 lines are on their way at once; one key alone has one.

On one thread, batching takes a `u64` get from 109 to 74 ns, and a JSON-key get from 162 to 102 ns.
At 96 threads, UUID gets go from 591 to 795 M/s with 64 keys a call.

Each lane is an `async` block that reads like the plain lookup, and a tiny executor polls the lanes in turn: no allocation, no runtime.
It is also the only place bbtree prefetches: the one line each lane reads next.

#### Optimistic latch coupling to improve concurrency

The usual way to share a B-tree is a reader-writer latch on every node.
But taking a read latch writes to the latch, and every lookup takes the root's.
At 96 cores, the root's cache line bounces between cores on every lookup: readers never block each other, yet they still queue for one line.

bbtree's readers write nothing.
Each node carries a version number:

1. A reader reads the version, reads the node, and checks the version again. Unchanged: what it read is consistent. Changed: start over.
2. A writer latches the node with a compare-and-swap, changes it, and bumps the version as it lets go.

Lookups read their leaf the same way: `get_copied` copies the value out and checks the version after, like a seqlock.
A lookup, hit or miss, writes nothing to shared memory.

[Figure 5: one node's version, a writer, and two readers, a column each,
time running down (`versions_figure.rs`).]

```text
version     writer      reader 1      reader 2
   7                    read v7
   7                    read node     read v7
   7                    check v7 ✓    read node
   7        latch                     read node
   7        change                    read node
   7        release                   read node
   8                                  check v8 ✗
```

> Time runs down. Readers only read. Reader 1 finished before the writer changed anything, so its read stands. Reader 2 overlapped the change, finds version 8, and starts over.

A reader may still be reading a node that a writer just freed.
So a freed node never goes back to the allocator while the map lives: it waits on a free list for the next node of its kind.
A late reader reads a stale node, fails its version check, and retries.
No epochs, no guards to hold.

At 96 threads, bbtree's lookups run at 86–94% of 96 times its one-thread speed.
On UUID gets, `RwLock<BTreeMap>` does 19 M/s; bbtree 591 M/s, 795 batched.
Bf-tree[^bftree] latches its inner nodes the same way[^olc].

#### Model checking and fuzzing to ensure correctness

A faster B-tree is worthless if it loses your data.
bbtree is checked four ways:

1. **Differential fuzzing against std.** Random operations run on bbtree and std's `BTreeMap` side by side; every answer must match, and the tree's invariants are checked after every operation, under AddressSanitizer. Seven targets, every week and before every release.
2. **Model checking with Shuttle.** Shuttle runs the threads one at a time and switches between them at every atomic they share: latch words, the root pointer, free lists. It reaches interleavings that stress tests rarely hit, and found two bugs that our threaded tests and a 12-thread stress run had missed. One: after a merge, a lookup of a separator key could run its closure on a stale copy of an entry, which another thread could then remove and free.
3. **Miri**, with tree borrows and strict provenance, on a selected set of tests.
4. **Hostile user code.** An `Ord`, `Clone` or `Drop` that panics or lies can give wrong answers, but never undefined behavior. A fuzz target checks exactly that.

### Try it

```bash
cargo add bbtree
```

1. Replacing std's `BTreeMap`? Use `bbtree::unsync::BBTreeMap`; change one `use` line.
2. Sharing it across threads? Use `bbtree::BBTreeMap`.
3. Many keys at once? Use `get_many_copied` and `insert_many`.
4. On x86, build with `-C target-cpu=native` for AVX-512.

If you like bbtree, don't forget to cite it in your paper:

```bibtex
@misc{hao2026bbtree,
  author       = {Xiangpeng Hao},
  title        = {{bbtree}: fastest ordered map in the world},
  year         = {2026},
  howpublished = {\url{https://github.com/XiangpengHao/bbtree}},
}
```

[^graefe]: Goetz Graefe, [More Modern B-Tree Techniques](https://doi.org/10.1561/1900000070) (2024): keep full keys out of the comparison stream with dense, fixed-width codes, and skip the prefix a page shares.

[^umbra]: [Umbra: A Disk-Based System with In-Memory Performance](https://db.in.tum.de/~freitag/papers/p29-neumann-cidr20.pdf) (CIDR 2020); see also my post on [the nature of strings](https://blog.xiangpeng.systems/posts/nature-of-string/).

[^bftree]: [Bf-Tree: A Modern Read-Write-Optimized Concurrent Larger-Than-Memory Range Index](https://github.com/microsoft/bf-tree) (VLDB 2024).

[^olc]: Viktor Leis et al., [The ART of Practical Synchronization](https://db.in.tum.de/~leis/papers/artsync.pdf) (DaMoN 2016).


## Status and decisions (2026-10-17, from the runs)

Added by the session that built the post; the blog content above is the
author's.

- **Where it lives.** The post is `vis/src/post.rs` at `/post/`. Its cards
  (`performance_cards.rs`) and charts (`single_thread_chart.rs`,
  `concurrent_chart.rs`, `end_to_end_chart.rs`, `memory_chart.rs`,
  `architectures_chart.rs`; the grouped bars in `baseline_bars.rs`)
  compile in the CSVs of `vis/data/<machine>/` (`measurements.rs`). The
  runs live in [bbtree-eval](https://github.com/XiangpengHao/bbtree-eval):
  its `blog/runs.sh` runs every chart's cells on a machine into its
  `blog/results/`, and `blog/collect.sh` merges them into this
  repository's `vis/data/`; a cell rerun (`reruns/`) replaces the first run
  of it. On AWS, `blog/aws-runs.sh` runs `blog/runs.sh` on a spot instance
  it launches and terminates.
- **One bbtree map (2026-10-17).** The post shows `bbtree::BBTreeMap`,
  the map threads share, as "bbtree" in every tab, one thread included;
  the single-thread `bbtree::unsync::BBTreeMap` runs in none (doc 03,
  D4). Every cell was re-run for it on 2026-10-17, on every machine and in
  every integration.
- **Harness** (bbtree-eval's `bbtree-scaling`): `uuid` (`[u8; 16]`, version 4) and
  `wiki-titles` keys; std's `BTreeMap` and `HashMap` owned by one thread
  (`Owned`, at one thread only); `RwLock<BTreeMap>`;
  lookups half of which miss (`get-half`, `get-half-batch`); `update`;
  a 90/10 lookup/update mix (`mixed`) and a 90/10 scan/update mix
  (`scan-mixed`, 100-entry scans, the ordered maps only); batched inserts
  (`insert-batch`, `insert_many` 16 at a time); scc batches under one
  epoch guard for the batch; the heap's peak while a map is built. Each
  shared map is built by every CPU, but the `RwLock` map, built by one
  thread, as its writers take turns (`WRITERS_TAKE_TURNS`).
- **Best use of each map.** bbtree runs with inline keys (`Auto`
  recognizes all four key types), copies its `u64` values out
  (`get_copied`), and uses its batch APIs (`get_many_copied`, 64 keys a
  call; `insert_many`, 16). artmap (ART) was measured, in both its forms,
  and left out of the post as not a fair comparison; its numbers are in
  the session's notes, not in `vis/data/`. DashMap, a hash map, is shown
  only on the concurrent card, for reference.
- **Charts.** The cards are "Single thread", "Concurrent", "End to End",
  "Memory" and "Architectures", in that order (the author's names); they
  don't turn by themselves. Above them,
  one sentence states the result, computed from the data (bbtree's gets
  against std's on one thread, plain and batched; the systems end to
  end). The map is "bbtree", lowercase, everywhere. The
  wide charts are drawn at their card's width, a unit a pixel, and lay
  themselves out for it: on a phone the line chart names its lines in a
  key under the plot, and the grouped charts put a group a row (on a
  desk, the groups split into rows as even as they go). bbtree wears the
  site's highlighter (`--marker`), rimmed and lined in its dark gold
  (`--marker-strong`, 3.8:1 on the panel); every other map is a dark
  slate.
  - Single thread: the mean time per operation, operations back to back, a
    bar a map from zero, one scale for the four panels (a panel a kind of
    key) ending at the longest bar; Get (half hits, half misses), Insert,
    Update, Scan (100 entries). The maps: bbtree, std's `BTreeMap`, scc
    TreeIndex and concurrent-map (concurrent maps, used from one thread),
    crossbeam's SkipMap. bbtree's batch API is a hollow bar under
    bbtree's own, "batched" said once in the legend. std's `HashMap` is a
    reference for an operation that needn't be ordered: last, below a
    dashed rule, a pale dashed bar, "unordered, no scans"; under Scan its
    row says it can't.
  - Concurrent: a line a map, a point at each thread count measured,
    thread counts to scale from zero (a map whose throughput grows with
    its threads draws a straight line from the corner), names at the
    lines' ends, a crosshair; Get, Insert,
    Update, 90/10 get/update, 90/10 scan/update. bbtree's batched lookups
    are a solid line of their own, "bbtree (batched)"; other maps'
    batches aren't shown. DashMap, a hash map threads share, is drawn for
    reference (the author's call, 2026-10-17): a faint, dotted line that
    fades in, "DashMap (unordered, hashtable)" in italics, a dotted swatch
    in the phone's key; no scans, so no line under 90/10 scan/update. Its
    cells come from `blog/runs.sh tab2-extra` (DashMap and bbtree's
    batched inserts, the latter not shown) on the same machine type the
    same day, copied into the card's results before `blog/collect.sh`,
    which keeps DashMap's rows. It is the one card not run on the 9900X, whose 12 cores made a
    cramped chart and whose second hardware threads bent its lines: it
    runs on AWS's c8a.metal-24xl (AMD EPYC 9R45, Zen 5, 96 cores, one
    socket, 12 × 32 MiB L3; AWS's AMD instances run a thread a core), at
    1 thread and every 8 to 96 (`THREADS=1,8,…,96 blog/aws-runs.sh
    c8a.metal-24xl,c8a.24xlarge aws-c8a tab2 <keys>`), about $0.90 on
    spot. 10M entries still measure memory there: no map fits the 32 MiB
    L3 of an 8-core group, as on the 9900X.
  - Memory: a bar a map, the heap once built, its keys' allocations
    included, and lighter on to the peak while it was built. In these
    insert-only builds the peak is the end for every map; a churn
    workload would show a peak above it.
  - End to End: one bar chart (`baseline_bars.rs`, `BarScale::Relative`), a group a system (the
    note names the maps they ship), a cluster a workload, one bar each:
    the time as shipped, dark, as 100%, and inside it bbtree's, bright,
    as a share of it, so that no system's times compare with another's;
    the speedup over each bar. Bars close together, a narrow step each,
    long workload names on two lines; groups split into rows only where
    one row can't hold them (on a phone, a system a row). The note names the cells that went
    against bbtree.
  - Architectures (no title, no machines table, by the author): the same chart drawn in nanoseconds (`BarScale::Nanos`),
    two bars a machine, std's BTreeMap and bbtree side by side on one
    axis, each labelled with its time: UUID gets (the author's pick of
    workload), each machine captioned with its instruction set. Each
    machine's bench is built for its own CPU (`target-cpu=native` there,
    or its name when built elsewhere: pittsburgh's `rocketlake`). The
    machines: the Ryzen 9 9900X, pittsburgh's Core i5-11400, the M1 Pro,
    and AWS's C8i (Xeon 6) and C9g (Graviton5); the note names the AWS
    instances. Cells come from `blog/runs.sh arch` (UUIDs unless
    other keys are named; Linux or macOS), merged by `blog/collect.sh`;
    on AWS, `blog/aws-runs.sh <types> <name>` runs them on a
    throwaway spot instance (c8i.large and c9g.large, 2 vCPUs, about a
    cent a run in us-west-2) and terminates it, the instance also
    shutting itself down two hours after boot. A machine joins the card in
    `ArchitectureMachine::ALL`. WebAssembly and the ESP32-P4 are left out:
    WebAssembly until Rust builds wasm32 with SIMD (`simd128`) by default
    (its harness is bbtree-eval's `wasm/`, on keys from `bbtree-scaling
    --dump-keys`).
  - Motion: bars grow from their baseline at one speed, each taking the
    time its length takes on its chart's scale (`LinearScale::growth_ms`,
    at most 0.8 s), so that the faster map's bar lands first, the chart
    acting out the time it measures; a value fades in as its bar lands;
    the concurrent lines draw left to right, as threads are added. It
    waits until the cards are first in view, replays when a toggle
    changes what a chart shows (keyed remounts), and with reduced motion
    bars and lines appear whole while values still fade in.
  - Every chart's note ends with links to the code of the maps and
    systems outside std (Dioxus has no SVG link element, so the labels
    themselves aren't links). Notes are kept short, at most 75
    characters a line.
- **Latency was dropped.** Each operation timed alone, fenced (a bare
  `rdtsc` read the end before a lookup's misses returned), takes 20–25 ns
  of fences more and can't overlap the next: bbtree 240 ns alone against
  109 back to back, std 281 and 277. That measured the timer as much as
  the map; the post shows the mean back to back.
- **What the numbers say (Ryzen 9 9900X, 10M entries, 2026-10-17).**
  - One thread, ns per operation, bbtree against std's `BTreeMap`
    (lookups half missing; batched in brackets):

    | keys | get | insert | update | 100-entry scan |
    |---|--:|--:|--:|--:|
    | `u64` | 109 (74) vs 264 | 155 (113) vs 239 | 117 vs 273 | 619 vs 1,647 |
    | UUID | 120 (85) vs 335 | 172 (130) vs 357 | 144 vs 366 | 773 vs 1,764 |
    | Wikipedia titles | 400 (294) vs 693 | 417 (324) vs 613 | 439 vs 702 | 1,248 vs 2,128 |
    | JSON keys | 162 (102) vs 666 | 244 (157) vs 572 | 197 vs 676 | 1,029 vs 2,101 |

    Gets 1.7–4.1× std's, batched 2.4–6.6×. The single-thread map of the
    first round was 2–16% faster than these on gets and 16–22% on
    batches (09-concurrency.md, Performance): the price of sharing,
    which this round cut (09, Lookups, Batches).
  - Threads (AWS c8a.metal-24xl, 96 cores, 2026-10-17): bbtree leads
    all 260 cells, every key, workload and thread count. At 96 threads
    against scc TreeIndex, the next map in each: lookups half missing
    3.9 / 4.5 / 2.9 / 6.9× (`u64`, UUID, titles, JSON; `u64` 627 M/s
    against 161, batched 846), updates 3.5 / 3.9 / 2.8 / 5.4×, the 90/10
    lookup/update mix 3.3 / 3.8 / 2.6 / 5.2×, the 90/10 scan/update mix
    2.0 / 1.4 / 1.3 / 1.4×, inserts 1.2 / 1.4 / 1.4 / 1.7× (a 10M build
    at 96 threads takes about 70 ms, the least precise cell). bbtree's
    lookups at 96 threads are 86–94% of 96 times its one-thread rate. The
    `RwLock` map's updates fall from 3.0 M/s on one thread to 1.2 at 96.
    On the 9900X at 24 threads, scc passed bbtree on the titles'
    scan/update mix (6.5 against 6.1 M ops/s); on the c8a's 12 memory
    channels bbtree leads it, 34 against 26.
  - Architectures, UUID gets, bbtree against std's BTreeMap: Ryzen 9
    9900X 2.8×; pittsburgh's Core i5-11400 2.8× (8.72 against 3.08 M gets
    a second); Apple M1 Pro 2.3× (5.08 against 2.18); AWS C8i (Xeon 6975P-C)
    2.6× (4.72 against 1.83), C9g (Graviton5, Neoverse V3) 2.0× (6.01
    against 3.01); about a cent for both AWS runs, on spot. This C8i
    instance ran std 25% slower than the first round's, so its absolute
    times don't compare across rounds. All four keys on pittsburgh and the
    M1 too (bbtree-eval's `blog/results/*-20261017`): pittsburgh 2.6/2.8/1.8/4.5×,
    M1 1.8/2.3/1.8/4.4× (`u64`, UUID, titles, JSON). WebAssembly at its
    default features, measured in the first round on the single-thread
    map, is the exception: 0.45/0.66/1.18/1.20×, its fixed-size-key gets
    1.5–2.2× slower than std's, as without SIMD bbtree counts a node's
    inline keys one at a time; with `+simd128` 1.04/1.27/1.82/2.44× (raw
    runs in bbtree-eval's `blog/results/wasm*-pittsburgh-20261016`; neither is in
    the post). A wasm SIMD count, like the NEON one (D56), is the fix to
    consider.
  - Hash maps, for reference: std's `HashMap` takes 23 ns a `u64` get
    (bbtree 109, 74 batched), 43 an insert (155).
  - Not in the post: artmap (ART) tied or beat the first round's single
    lookups and inserts of fixed-size keys at 1.2–2× the memory.
  - Memory, heap bytes per entry: bbtree 26.9 / 39.7 / 80.9 / 68.5 (`u64`,
    UUID, titles, JSON), std 27.1 / 38.9 / 71.6 / 59.1: even on
    fixed-size keys, 13–16% more on strings (inline keys and length tags).
- **Integrations**, single thread only (with more, each system is bound by
  its own locks): bbtree-eval's `integrations/{fjall,slatedb,turso,tonbo,lance}`,
  the shipped skiplist against bbtree, re-run on 2026-10-17 with the
  renamed library, each with its RESULTS.md. Only the map changed, and
  where a system writes a batch, the call that hands it to the map whole
  (`insert_many`: fjall's `WriteBatch::commit`, SlateDB's
  `write_entries_to_memtable`, Lance's put batches). Turso keeps two of
  PR #9267's call-site changes the map needs.
  - fjall: gets 2.47×, mix 2.44×, inserts 1.61×, write batches 3.00×
    (1.5× over single inserts); fjall and Turso at 1M rows.
  - Turso: inserts 2.22×, indexed SELECT 1.90×, point SELECT 1.16×,
    update 1.22×; its values are `Arc`s and its writes single, so none of
    the new APIs apply. Its two allocation-failure tests still fail: no
    fallible insert.
  - SlateDB: gets 1.71×, puts 1.65×, 80/20 mix 1.37×, write batches of 10
    rows 2.36× (2.06× with single inserts). `memtable_seek` 0.91–0.99×,
    from 0.80×: SlateDB's iterators read three rows ahead of a seek, and
    the adapter's first batch of two made every seek scan twice; its
    seek batch is now four rows.
  - Tonbo (crossbeam `SkipMap` memtable, Rust 1.90): 10M-row writes
    6.83×, conflict-checked transactions 4.10×, `write_heavy_no_sst_sweep`
    2.12× at 1M-row memtables (a background compaction lands inside the
    timed window or not; 2.12× is the comparable mode; measured on the
    build before the scan fix below). `insert_many` doesn't help it: each
    row's existence probe has loaded the leaf. Its seal drains with
    `into_iter` now. Not in the chart: point reads that walk the whole
    memtable in scan planning, 10.8–11.8×; this round's library first
    walked it 25% slower, entry loads waiting on each leaf's header, found
    by its integration and fixed in `scan.rs` (09-concurrency.md, Scans);
    `Transaction::get` 0.86× (it copies every row per call).
  - Lance (MemWAL index, its own arena skiplist, PR #7011; Rust 1.98): at
    10M rows, point lookups through its API 2.28×, `lookup_many` 4.03×,
    batched puts 3.94×, updates 3.29×, with its `newest` index a plain
    `u64` word now, read by `get_copied`, and put batches through
    `insert_many`. Lance's own small benches and lookups of absent keys
    (they plan a DataFusion query) are unchanged, 0.97–1.06×.
  - **Toolchain.** Before Rust 1.89 the AVX-512 target features are
    unstable and bbtree's search falls back to a scalar count. Systems
    pinned below 1.89 (Turso, Neon: 1.88) run 1.89 with
    `-C target-cpu=native` on both sides, their pin reported too.
    SlateDB's run.sh builds without `target-cpu=native`; with it its rows
    are the same within 2%.
  - **Neon** (pageserver `InMemoryLayer`, std `BTreeMap` → the unsync
    map): stopped during the first round's final runs, not in the chart;
    not re-run.
- **Open.** The Jim Gray quote's wording and source. Not in the post:
  the unsync map's UUID inserts are slower than the shared map's on the
  same tree code (261 against 167 ns), a bug to find.
