# 06 — The race against C++ concurrent maps

Status: built (2026-10-25, 2026-10-26), run on one machine. Upstream's
README race (`OG-design/assets/readme-race.svg`) times the Rust maps on
lookups of random UUID keys; this is the same race for C++ (D32), where
the reference is no longer the Rust crate (02-rust-parity.md) but the
maps a C++ program would otherwise pick.

## The workload

`bench/race/race_bench.cpp`, `race_bench [entries] [threads] [seconds]
[maps] [hit_percent]`:

- **Keys:** 10M random UUIDs (16 bytes), values `u64`. The UUID hash
  declares `is_avalanching` (D30).
- **Lookups:** every thread looks up its own run of the probes, 64 at a
  time, for 2 seconds a map; `hit_percent` of them (50 by default) find
  their key.
- **Building:** a shared map is built by every thread; a locked map by one
  thread, its writers taking turns. Every map must answer the same first
  65,536 lookups right before it races.
- **Best use:** each map gets its fastest lookup, as upstream's harness
  gives each: shoal's bulk `get_many_copied` (64 keys a call) and its
  one-key `get_copied`; Boost's bulk `cvisit` over 64 keys, which overlaps
  memory waits as shoal's bulk reads do (it took Boost from 200 to 390
  M/s, one key at a time to bulk).

## Two heats

Upstream's chart draws DashMap, a hash map, under a dashed line as the
unordered reference. With hash maps of its own (D25), shoal races in both
heats, each fastest first after shoal's:

- **Tree maps (ordered):** shoal's `TreeMap`; oneTBB's `concurrent_map`
  and folly's `ConcurrentSkipList` (skip lists); `absl::btree_map` and
  `std::map` behind a `std::shared_mutex`.
- **Hash maps (unordered):** shoal's `HashMap`; Boost's
  `concurrent_flat_map`; folly's `ConcurrentHashMap`; oneTBB's
  `concurrent_hash_map` and `concurrent_unordered_map`;
  `absl::flat_hash_map` and `std::unordered_map` behind a
  `std::shared_mutex`.

The competitors are dependencies (oneTBB, folly, Abseil, Boost, e.g. from
Homebrew), so the race builds only with `-DSHOAL_RACE_BENCH=ON` and the
library keeps zero (D1). `race_svg.py` draws a CSV as upstream's
bouncing-ball chart; the README shows three runs, their CSVs in
`bench/race/`.

## Results

MacBook Pro M4 Pro (10 performance and 4 efficiency cores), one clean run
each (no swapping), millions of lookups per second.

| | 14 threads, 50% hit | 1 thread, 50% hit | 14 threads, 90% hit |
|---|--:|--:|--:|
| shoal `HashMap`, bulk | **469.7** | **43.1** | **416.2** |
| shoal `HashMap`, one key | **306.1** | 25.2 | **225.4** |
| Boost `concurrent_flat_map`, bulk | 427.1 | 42.3 | 291.8 |
| oneTBB `concurrent_hash_map` | 144.1 | 10.5 | 125.1 |
| folly `ConcurrentHashMap` | 135.1 | 12.1 | 122.4 |
| oneTBB `concurrent_unordered_map` | 65.0 | 4.4 | 66.1 |
| `absl::flat_hash_map` + lock | 6.4 | 17.8 | 5.7 |
| `std::unordered_map` + lock | 3.8 | 6.2 | 3.8 |
| shoal `TreeMap`, bulk | **158.9** | **13.6** | **157.6** |
| shoal `TreeMap`, one key | **84.5** | **7.3** | **77.5** |
| oneTBB `concurrent_map` | 8.7 | 0.7 | 6.7 |
| folly `ConcurrentSkipList` | 8.4 | 0.6 | 7.9 |
| `absl::btree_map` + lock | 2.2 | 2.4 | 1.9 |
| `std::map` + lock | 1.5 | 0.8 | 1.4 |

The full rows are in `bench/race/m4-pro-14-threads.csv`,
`m4-pro-1-thread.csv` and `m4-pro-14-threads-90-hit.csv`.

- **Readers that write nothing scale.** Both shoal maps read without
  writing shared memory (OG-D54, D26): at 14 threads the tree map leads
  its heat 10–18×. On one thread nothing contends and a lock costs
  little: Abseil's locked maps move up (its hash map to third, its B-tree
  to the tree map's nearest rival, 5.7× behind), and the skip lists fall
  to the bottom.
- **Boost and the hit rate.** From 50% to 90% hits Boost's bulk read drops
  and shoal's barely moves. Boost's `unprotected_bulk_visit`
  (`boost/unordered/detail/foa/concurrent_table.hpp`) works in chunks of
  16 keys, and takes the group's lock shared for each key whose group
  holds a candidate: every hit writes shared memory, the write that sank
  shoal's own map behind `shared_mutex`. More hits, more writes: −12% on
  one thread (each write's barrier), −25% at 14 (the lock's line pulled
  between cores). shoal's readers write nothing for a hit or a miss: −2%
  at 14 threads, and +10% on one, where hits and misses run the same
  instructions (D28) and the extra slot reads overlap.

## Reading these numbers (D31)

- **Ratios, not absolutes, across runs.** The same maps measured on
  another day under a load of 5 ran 7–13% lower (409 against 470 for
  shoal's bulk hash map), both maps alike. "427 → 292" for Boost compares
  two runs and two binaries (the 50% run predates the `hit_percent`
  option, whose generator picks hits differently): back to back, the drop
  is 25%, not 32%, and shoal leads by 40% at 90% hits (400 against 285).
- **A quiet machine.** Runs under memory pressure took 9 and 16 minutes
  instead of one, with rows at 0.0 M/s; a run is kept only with no swap
  activity over it, the load average low.

## Open

- **Cached workloads:** with every key in cache (100K keys) on one thread,
  Boost's bulk read is faster (107 against 90 M/s): shoal's sharding and
  its version checked twice (05-hash-maps.md).
- **One machine:** no x86 runs, no MSVC runs.
