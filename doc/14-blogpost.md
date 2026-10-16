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

- **Where it lives.** The post is `vis/src/post.rs` at `/post/`; its charts
  (`performance_cards.rs`, `one_thread_chart.rs`, `scaling_chart.rs`,
  `integration_chart.rs`) read the CSVs that `bench/tools/blog-runs.sh`
  writes and `bench/tools/blog-collect.sh` merges
  (`bench/results/blog/<machine>/`), compiled in (`measurements.rs`). One
  command reruns every chart's numbers on another machine (AWS later).
- **Harness.** `bbtree-scaling` gained what the tabs need: `uuid`
  (`[u8; 16]`, version 4) and `wiki-titles` keys, plain `BBTreeMap` and
  `BTreeMap` owned by one thread (`Owned`, at one thread only),
  `RwLock<BTreeMap>`, and artmap's arena map.
- **Latency is fenced.** A bare `rdtsc` read the end before a lookup's
  misses returned (bbtree's median get read 10 ns). Fenced, an operation
  timed alone takes what it takes, plus 20–25 ns of fences, and can't
  overlap the next. Back to back, independent lookups overlap, and bbtree's
  branchless search lets them overlap most: 10M `u64` gets, bbtree 240 ns
  alone and 109 ns back to back, std 281 and 277. So tab 1 has both views,
  "one at a time" (p50, p99, p99.9; max in the hover) and "back to back"
  (the mean, batched lookups included).
- **ART.** artmap's arena map beats bbtree's single lookups and inserts on
  `u64` and UUIDs (gets 84 vs 109 ns, 86 vs 134) and misses by 2.7×;
  bbtree's batched lookups beat it (60 vs 81, 71 vs 87). The title's claim
  holds for batches and strings, not for single fixed-size lookups.
- **pmax is noise**: an interrupt or a page fault, 10–40 µs for every map;
  it is in the hover, not on the chart.
- **Integrations** run outside the repository, in
  `~/coding/bbtree-integrations/{fjall,slatedb,turso}` (rules in its
  README). SpacetimeDB was dropped: an earlier evaluation found its hash
  index 3.4× faster than batched bbtree.
- **Open.** The Jim Gray quote's wording and source; the cloud tab waits
  for AWS runs.
