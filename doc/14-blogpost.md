The blog post is along with the visualization.

## Blog content

The post's text as the site shows it (`vis/src/post.rs`). Bracketed lines
mark where a figure or the charts sit.

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

#### Prefix key to improve comparisons 

[Figure: one lookup's way down]

[Figure: the leaf]

#### AVX512/NEON SIMD to improve search speed

#### Batching to improve throughput

[Figure: four lookups on one thread]

#### Careful layouts to improve cache locality

#### Optimistic latch coupling to improve concurrency

#### Formal verification to ensure correctness


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
