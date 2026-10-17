The blog post is along with the visualization.

Title: Fastest ordered map.

1. we introduct bbtree, and bctree (its concurrent version), the fastest ordered map in the world.
Then we say btree is the most important data structure in the world "qoute jim gray".
it is zero-dependency, no_std compatible, and it is a drop-in replacement for std::collections::BTreeMap.

2. then we immediately show a performance comparsion.
This comparsion has a few tabs.
First tab shows the single thread bbtree compare with std Btree and skip list (different series).
the x-axis is differnt workloads, u64, uuid, wiki titles, json-str.
y axis is the latency, show p50, p99, and pmax.

Second tab shows the bctree.
Compare with concurrent-map, crossbeam skip list, std btree with rw lock, and bctree.
x-axis is the thread count.
within each, there's a way to switch the workloads.
We should probably show memory consumption as well.

Thrid tab shows the performance on real aws servers, x86 instance, and arm instance.

Fourth tab shows the integrations. SlateDB, Turso, SpacetimeDB(?), fjall, and maybe others. Each tab show a before and after performance comparsion.

the tabs are like cards, it will automatically switch if not clicked.

3. then we say how it is made.

There's two parts: the inline key tricks, and the batched interleaved search tricks.

And they are in two different sections, guided by the visualization we already have.


## Status and decisions (2026-10-16, from the runs)

Added by the session that built the post; the plan above is the author's.

- **Where it lives.** The post is `vis/src/post.rs` at `/post/`. Its cards
  (`performance_cards.rs`) and charts (`single_thread_chart.rs`,
  `concurrent_chart.rs`, `end_to_end_chart.rs`, `memory_chart.rs`,
  `architectures_chart.rs`; the grouped bars in `baseline_bars.rs`)
  compile in the CSVs of `vis/data/<machine>/` (`measurements.rs`). `bench/tools/blog-runs.sh` runs
  every chart's cells on a machine into the ignored `bench/results/blog/`,
  and `bench/tools/blog-collect.sh` merges them into `vis/data/`; a cell
  rerun (`reruns/`) replaces the first run of it. AWS reruns are those two
  commands.
- **Harness** (`bbtree-scaling`): `uuid` (`[u8; 16]`, version 4) and
  `wiki-titles` keys; plain `BBTreeMap`, `BTreeMap` and `HashMap` owned by
  one thread (`Owned`, at one thread only); `RwLock<BTreeMap>`;
  lookups half of which miss (`get-half`, `get-half-batch`); `update`;
  a 90/10 lookup/update mix (`mixed`) and a 90/10 scan/update mix
  (`scan-mixed`, 100-entry scans, the ordered maps only); batched inserts
  (`insert-batch`, `insert_many` 16 at a time); scc batches under one
  epoch guard for the batch; the heap's peak while a map is built.
- **Best use of each map.** bbtree and bctree run with inline keys
  (`Auto` recognizes all four key types) and their batch APIs where they
  have them (`get_many`, `read_many`, `insert_many`). artmap (ART) was
  measured, in both its forms, and left out of the post as not a fair
  comparison; its numbers are in the session's notes, not in
  `vis/data/`. `RwLock<BBTreeMap>` and DashMap were measured and are not
  shown.
- **Charts.** The cards are "Single thread", "Concurrent", "End to End",
  "Memory" and "Architectures", in that order (the author's names); they
  don't turn by themselves. Above them,
  one sentence states the result, computed from the data (bbtree's gets
  against std's on one thread, plain and batched; the systems end to
  end). The maps are "bbtree" and "bctree", lowercase, everywhere. The
  wide charts are drawn at their card's width, a unit a pixel, and lay
  themselves out for it: on a phone the line chart names its lines in a
  key under the plot, and the grouped charts put a group a row (on a
  desk, the groups split into rows as even as they go). bbtree's maps
  wear the site's highlighter (`--marker`), rimmed and lined in its dark gold
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
    Update, 90/10 get/update, 90/10 scan/update. bctree's batched lookups
    are a solid line of their own, "bctree (batched)"; other maps'
    batches aren't shown. No hash map: DashMap was measured and left
    out.
  - Memory: a bar a map, the heap once built, its keys' allocations
    included, and lighter on to the peak while it was built. In these
    insert-only builds the peak is the end for every map; a churn
    workload would show a peak above it.
  - End to End: one bar chart (`baseline_bars.rs`, `BarScale::Relative`), a group a system (the
    note names the maps they ship), a cluster a workload, one bar each:
    the time as shipped, dark, as 100%, and inside it bctree's, bright,
    as a share of it, so that no system's times compare with another's;
    the speedup over each bar. Bars close together, a narrow step each,
    long workload names on two lines; groups split into rows only where
    one row can't hold them (on a phone, a system a row). The note names the cells that went
    against bctree.
  - Architectures (no title, no machines table, by the author): the same chart drawn in nanoseconds (`BarScale::Nanos`),
    two bars a machine, std's BTreeMap and bbtree side by side on one
    axis, each labelled with its time: UUID gets (the author's pick of
    workload), each machine captioned with its instruction set. Each
    machine's bench is built for its own CPU (`target-cpu=native` there,
    or its name when built elsewhere: pittsburgh's `rocketlake`). The
    machines: the Ryzen 9 9900X, pittsburgh's Core i5-11400, the M1 Pro,
    and AWS's C8i (Xeon 6) and C9g (Graviton5); the note names the AWS
    instances. Cells come from `blog-runs.sh arch` (UUIDs unless
    other keys are named; Linux or macOS), merged by `blog-collect.sh`;
    on AWS, `bench/tools/aws-arch.sh <type> <name>` runs them on a
    throwaway spot instance (c8i.large and c9g.large, 2 vCPUs, about a
    cent a run in us-west-2) and terminates it, the instance also
    shutting itself down an hour after boot. A machine joins the card in
    `ArchitectureMachine::ALL`. WebAssembly and the ESP32-P4 are left out:
    WebAssembly until Rust builds wasm32 with SIMD (`simd128`) by default
    (its harness stays in `bench/wasm`, on keys from `bbtree-scaling
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
- **What the numbers say (Ryzen 9 9900X, 10M entries).**
  - Ordered maps: bbtree and bctree lead every cell, alone, back to back
    and batched, but one: the 90/10 scan/update mix at 24 threads on
    string keys, where scc TreeIndex passes bctree (JSON keys 6.6 against
    6.1 M ops/s, Wikipedia titles 6.3 against 5.4); bctree leads it at 12
    threads (5.7 against 4.4, 5.1 against 4.0) and on `u64` and UUIDs at
    every count (13.2 against 9.1 at 24). Updates stall the `RwLock` maps'
    scans: 1.7–2.2 M ops/s at 24 threads.
  - Architectures, gets, bbtree against std's BTreeMap (u64, UUID,
    Wikipedia titles, JSON keys): Ryzen 9 9900X 2.4/2.9/1.9/4.7×;
    pittsburgh's Core i5-11400 2.6/3.0/1.9/4.2×; Apple M1 Pro
    1.8/2.5/2.0/4.0×. On AWS, UUIDs only: C8i (Xeon 6975P-C) 3.0×
    (7.35 against 2.44 M gets a second), C9g (Graviton5, Neoverse V3)
    2.1× (6.34 against 3.05); about a cent for both runs, on spot. WebAssembly at its default features, in Node on
    pittsburgh, is the exception: 0.45/0.66/1.18/1.20×, bbtree's
    fixed-size-key gets 1.5–2.2× slower than std's. Without SIMD bbtree
    counts a node's inline keys one at a time, and its nodes hold far
    more keys than std's. With `+simd128` (every major browser has it)
    the same cells are 1.04/1.27/1.82/2.44×. Raw runs of both are in
    `bench/results/blog/wasm*-pittsburgh-20261016`; neither is in the
    post. A wasm SIMD count, like the NEON one (D56), is the fix to
    consider.
  - Hash maps, for reference: std's `HashMap` takes 27 ns a `u64` get
    (bbtree 107, 62 batched), 43 an insert (144), 50 an update (118). Not
    in the post: DashMap reads 209 M `u64` gets a second at 24 threads
    (bctree 98, 122 batched).
  - Not in the post: artmap (ART) tied or beat bbtree's single lookups and
    inserts of fixed-size keys, and bctree's under threads, at 1.2–2× the
    memory; bbtree's batched lookups and inserts beat it.
  - Memory: bbtree matches std on fixed-size keys and takes 16–20% more on
    strings (inline keys and length tags).
  - `RwLock<BBTreeMap>`'s `get_many` out-reads bctree's `read_many` by
    ~30%: bctree's batch has room to improve. bbtree's single insert of
    `[u8; 16]` keys (268 ns) is slower than bctree's (173) on the same tree
    code: a bug to find. bctree's scan has a fixed cost per call (below).
- **Integrations**, single thread only (with more, each system is bound by
  its own locks): `~/coding/bbtree-integrations/{fjall,slatedb,turso,tonbo,lance}`,
  the shipped skiplist against bctree, nothing else changed (Turso keeps two of
  PR #9267's call-site changes the map needs). fjall gets 2.51×, mix 2.45×,
  inserts 1.60×; Turso inserts 2.3×, indexed SELECT 2.0×, range 1.7×;
  SlateDB puts 1.67×, gets 1.71–1.83×, but its `memtable_seek` 0.80×: a
  fixed cost per bctree `scan` call (a 3.2 KB frame, a 928-byte copy), in
  bbtree. Turso's two allocation-failure tests fail: bbtree has no fallible
  insert. SpacetimeDB was dropped (its hash index beat batched bbtree).
  fjall, Turso and SlateDB ran at 1M rows; Tonbo and Lance at 10M.
  - **Toolchain.** Before Rust 1.89 the AVX-512 target features are
    unstable and bbtree's search falls back to a scalar count. Systems
    pinned below 1.89 (Turso, Neon: 1.88) run 1.89 with
    `-C target-cpu=native` on both sides, their pin reported too. Turso
    moved under 3% from its 1.88 numbers.
  - **Tonbo** (crossbeam `SkipMap` memtable, Rust 1.90): 10M-row writes
    7.0×, conflict-checked transactions 4.2×, its own
    `write_heavy_no_sst_sweep` 2.17× at 1M-row memtables. Not in the chart:
    its point reads (11×) time a whole-memtable walk in scan planning, not
    a lookup; `Transaction::get` 0.88× (it copies every row per call).
  - **Lance** (MemWAL index, its own arena skiplist, PR #7011; Rust 1.98):
    at 10M rows, point lookups through its API 2.3×, `lookup_many` 3.8×
    (`read_many`, 16 keys at a time), batched puts 3.2×, updates 2.5×.
    Lance's own small benches (≤100K rows) and lookups of absent keys
    (they plan a DataFusion query) are unchanged, 0.97–1.03×.
  - **Neon** (pageserver `InMemoryLayer`, std `BTreeMap` → bbtree): stopped
    during its final runs, not in the chart. Its single runs on 1.89 +
    jemalloc: 1 GiB random ingest 1.33× (puts 1.72×), 128 MiB random
    1.14×, sequential 1.06–1.09×, hot-set reuse 0.98×.
- **Open.** The Jim Gray quote's wording and source; the cloud tab waits
  for AWS runs.
