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
bouncing-ball chart, each heat on its own clock (its fastest ball crosses
in 0.6 s), so the trees don't crawl beside the hash maps; the README
shows four runs, their CSVs in `bench/race/`.

## Results

MacBook Pro M4 Pro (10 performance and 4 efficiency cores), one clean run
each (no swapping out, a load of 3.4 from other apps before the first),
millions of lookups per second, after D28's bulk read took only the keys
that may be there past their tags. Bold, each heat's fastest.

| | 14 threads, 50% hit | 1 thread, 50% hit | 14 threads, 90% hit | 14 threads, 10% hit |
|---|--:|--:|--:|--:|
| shoal `HashMap`, bulk | **551.2** | **55.5** | **510.4** | 808.1 |
| shoal `HashMap`, one key | 300.3 | 26.8 | 242.4 | 428.8 |
| Boost `concurrent_flat_map`, bulk | 396.5 | 43.9 | 314.9 | **927.4** |
| oneTBB `concurrent_hash_map` | 130.2 | 10.6 | 132.5 | 165.9 |
| folly `ConcurrentHashMap` | 125.0 | 12.1 | 121.4 | 140.9 |
| oneTBB `concurrent_unordered_map` | 57.8 | 4.4 | 69.9 | 69.3 |
| `absl::flat_hash_map` + lock | 7.2 | 18.0 | 6.6 | 9.3 |
| `std::unordered_map` + lock | 4.0 | 6.3 | 3.9 | 4.5 |
| shoal `TreeMap`, bulk | **147.3** | **13.8** | **162.0** | **195.5** |
| shoal `TreeMap`, one key | 78.8 | 7.9 | 73.6 | 95.8 |
| oneTBB `concurrent_map` | 8.2 | 0.7 | 7.7 | 8.1 |
| folly `ConcurrentSkipList` | 7.3 | 0.7 | 8.2 | 7.6 |
| `absl::btree_map` + lock | 1.9 | 2.6 | 2.0 | 2.1 |
| `std::map` + lock | 1.3 | 0.9 | 1.4 | 1.4 |

The full rows are in `bench/race/m4-pro-14-threads.csv`,
`m4-pro-1-thread.csv`, `m4-pro-14-threads-90-hit.csv` and
`m4-pro-14-threads-10-hit.csv`.

- **Readers that write nothing scale.** Both shoal maps read without
  writing shared memory (OG-D54, D26): at 14 threads the tree map leads
  its heat 18–24×. On one thread nothing contends and a lock costs
  little: Abseil's locked maps move up (its hash map past every
  concurrent map but Boost's, its B-tree to the tree map's nearest rival,
  5.3× behind), and the skip lists fall to the bottom.
- **Boost and the hit rate.** From 50% to 90% hits Boost's bulk read drops
  21% and shoal's 7%. Boost's `unprotected_bulk_visit`
  (`boost/unordered/detail/foa/concurrent_table.hpp`) works in chunks of
  16 keys, and takes the group's lock shared for each key whose group
  holds a candidate: every hit writes shared memory, the write that sank
  shoal's own map behind `shared_mutex`. More hits, more writes, back to
  back: −14% on one thread (each write's barrier), −26% at 14 (the lock's
  line pulled between cores). shoal's readers write nothing for a hit or
  a miss; more hits only send more keys past their tags to read their
  slots (D28).
- **Boost leads when most keys miss.** At 10% hits Boost's bulk read
  runs 15% ahead of shoal's (927 against 808 M/s; 830 against 811 back
  to back). Before D28 listed the keys that may be there, it ran 1.5×
  (870 against 585, back to back): every miss ran a hit's steps. Counted
  on one thread (D31), a lookup took 133 instructions and 74 cycles in
  shoal, against Boost's 56 and 48; listed, 112 and 55. What remains is
  how a miss ends: Boost's stops at its group's overflow byte, one line
  read and no lock taken, and branches on whether its group holds a
  candidate, which predicts well when most keys miss. A shoal miss ends
  only at an empty tag: in a shard at 60% occupancy, 9% of misses find
  none among their eight tags and 1.6% match another key's tag, each
  taking the slow path (a mispredicted branch, a serial probe); 11% of
  tag words straddle two lines.
- **Cached workloads.** With every key in cache (100K keys, one thread)
  instructions decide, and shoal's bulk read leads Boost's only at 50%
  hits: 122 against 105 M/s; 143 against 156 at 90%, 177 against 276 at
  10%. It trailed at 50% (80 against 99) until the select that skips a
  miss's slot read compiled to a conditional move rather than a branch
  (D28). Listing the keys that may be there cost it 11% at 90% (161 to
  143), where nearly every key goes on, and gained 14% at 10%.

## Reading these numbers (D31)

- **Ratios, not absolutes, across runs.** The same maps measured on
  another day under a load of 5 ran 7–13% lower (409 against 470 for
  shoal's bulk hash map), both maps alike. Boost's bulk read moves more
  from run to run than shoal's: over four runs of one binary at 14
  threads and 50% hits, Boost 403–433, shoal 448–474, shoal ahead by
  5–17% (before D28 listed the keys that may be there). The table's
  "397 → 315" for Boost compares two runs; back to back, its drop
  measured 26%.
- **A quiet machine.** Runs under memory pressure took 9 and 16 minutes
  instead of one, with rows at 0.0 M/s; a run is kept only with no swap
  activity over it, the load average low.

## Open

- **One machine:** no x86 runs, no MSVC runs.
- **Mostly misses:** Boost's 15% lead at 10% hits, in how a shoal miss
  ends (a wider tag read would leave 2% of misses without an empty tag,
  against 9%).
