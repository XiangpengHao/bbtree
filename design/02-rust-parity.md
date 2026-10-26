# 02 — Parity with the Rust crate

Status: met (2026-10-25). The C++ maps against the Rust crate on the same
workloads, and how each gap was closed. The Rust reference left with the
Rust code (D4); its harness, `bench/reference`, is in git history before
`d0e4f24`.

## Method

`bench/*_bench.cpp` against `bench/reference`: the Rust maps, the same keys
(`bench/workload.hpp`'s generator, mirrored), the same order of
operations, M4 Pro, system allocator, `-mcpu=native`. One run each; a range
where runs on a quiet machine spread.

A gap was chased in this order (D31):

1. **Count, don't time.** Instructions and cycles per operation by
   `/usr/bin/time -l` over every thread, which hold still while other load
   moves wall-clock throughput by ±15%. `GET_ROUNDS` and `BATCH_ROUNDS`
   run each phase a set number of times, so that the difference of two
   runs counts one phase alone.
2. **Same paths?** Both maps visit the same nodes, take the same ties and
   fallbacks, prefetch the same lines: if not, the gap is behaviour, and
   the C++ is wrong. In every case here they matched, so the gap was code
   generation.
3. **Disassemble** the hot loop on both sides: values that go through
   memory in C++ and stay in registers in Rust (D18), calls that LLVM
   inlined and clang didn't (D17).
4. **Attribute exactly** when the disassembly doesn't say: single-step
   about 90 operations of each map under lldb, every instruction counted
   against its inlined function and line. (Instruments' Processor Trace
   would do the same; it wouldn't record on this machine.)

## Unsync map

ns per operation, lower is better.

| cell | Rust | C++ |
|---|--:|--:|
| 10M u64 insert_random | 246.6 | 224.8 |
| 10M u64 get_hit | 136.3 | 125.6 |
| 10M u64 get_miss | 122.7 | 114.7 |
| 10M u64 remove_random | 279.0 | 220.7 |
| 10M u64 insert_sequential | 8.6 | 8.8 |
| 1M path-string get_hit, instructions / cycles per lookup | 939 / 941 | 766 / 980 |
| 1M u64 iterate by `next` / folded | 3.0 / 1.6 | 2.2 / 1.7 |
| 10M u64 iterate by `next` / folded | 3.7 / 2.5 | 2.9 / 2.5 |
| 1M u64 range64 (per scan) | 249 | 195 |
| 10M u64 range64 (per scan) | 481 | 407 |
| 10M u64 batch insert / hit / miss, 64 a call (ns per key) | 92.1 / 49.4 / 42.3 | 87.5 / 46.1 / 42.3 |

The path-string `get_hit` row predates D17: with the lookup chain inlined
it runs 185 ns against Rust's 276 at 1M (one run each).

- **Scans (D14):** with the walk's crossing out of line, as Rust's, clang
  kept the walk's index in memory, and 64-entry scans took 1,092 cycles
  against Rust's 949. `Range::next` itself, once it contained the
  crossing, stopped being inlined, and whole-map iteration went from 2.1
  to 4.2 ns an entry. Both force-inlined.

## Unsync sets

u64, each operation folded into a count, ns per element: two 1M sets
sharing half their elements, walked side by side; a 27K set, every 37th
element of one, looked up in it a batch at a time.

| cell | Rust | C++ |
|---|--:|--:|
| 1M difference / intersection / union | 7.1 / 6.8 / 6.6 | 6.8 / 6.8 / 6.9 |
| 27K into 1M intersection / `is_subset` | 29.3 / 26.9 | 22.1 / 19.5 |

- **Folds (D16):** walking both sets first ran 1.4–1.9× Rust's time: the
  fold passed its runs by reference to a function that wasn't inlined,
  which kept them in memory. Returned by value: parity.

## Shared map

Millions of operations per second, higher is better. These throughput
rows predate D17–D21.

| cell | Rust | C++ |
|---|--:|--:|
| 10M u64 insert at 1 / 4 / 8 threads | 4.4 / 17.0–17.3 / 31.3–31.6 | 4.4 / 16.1–16.4 / 29.1–29.7 |
| 10M u64 get_copied at 1 / 4 / 8 threads | 8.4 / 33.6–34.3 / 61.7–66.0 | 8.4–8.8 / 34.1–34.8 / 53.5–63.3 |
| 10M u64 batch_get_hit, 1 thread | 17.5–17.7 | 18.0 |
| 10M u64 batch_get_hit, 8 threads | 117.2–134.7 | 113.1–126.0 |
| 10M u64 batch_get_hit, instructions / cycles per key, 1 thread | 752 / 221 | 734 / 221 |

Counted per insert over every thread, key setup (31–35 instructions)
taken out, after D17–D19:

| instructions / cycles per insert | Rust | C++ |
|---|--:|--:|
| 1 thread | 641 / 915 | 659 / 927 |
| 4 threads | 647 / 922 | 665 / 949 |
| 8 threads | 660 / 1029 | 683 / 1042 |

After D18's unboxed outcome and D20: 685 against 676 per insert at 10M,
key setup included in both, at parity in cycles. A shared get: 341
instructions against Rust's 357 (2M keys, by the `GET_ROUNDS`
difference).

How each gap closed:

- **Batched lookups, 9% behind on one thread (D18).** 36 more
  instructions a key on the same paths (4 levels, the same ties and
  fallbacks). The 32-byte `std::optional<Routed>` came back from the tie
  search on the stack, and so did the untied result it merges with; the
  tie's window went by pointer, so every route spilled it; the fence in
  each version check made clang reload every lane field read after it.
  `Routed` is 12 bytes, the window goes by value, the lane's fields are
  read before the fence, the level loop keeps its count and index in
  locals: 52 fewer instructions and 20 fewer cycles a key.
- **Inserts 5–7% behind at 4 and 8 threads (D19).** The spin wait was
  `yield`, which waits for nothing on Apple cores; Rust's binary has `isb`
  (14 of them, no `yield`). With `isb`, contention costs C++ no more than
  Rust; the latch, append check, free lists, 128-byte padding and splits
  were compared line by line and matched.
- **62 more instructions per insert, on one thread as on eight (D17,
  D18).** Not contention, a per-insert cost, which the lldb attribution
  split up: `TreeMap::insert` out of line, saving and restoring 13
  registers and checking the stack protector (24); `LatchedTree::insert`
  out of line (6); the separator flag converted from `bool` on each level
  (9); the descent's `std::optional<Descent>` moved out and destroyed (5).
- **Gets, 16 more instructions (D17, D21).** Clang left
  `TreeMap::get_copied`, `Leaf::find`, `search_candidates` and the byte
  windows' compares out of line; and lookups loaded the root three times
  where Rust loads it twice. Inlined, the root loaded once: 341 against
  357.
- **Three optional layers per insert (D18).** `insert_into`, `try_append`
  and `settle` wrapped the policy's output in a second `std::optional`,
  where Rust's policy returns one `Result`: 43 fewer instructions per
  insert unboxed (6,889M to 6,846M over 10M).
- **A fence after each latch (D20)**, and **a 65-byte copy on each
  descent (D21)**, which Rust also pays: one fence and one copy fewer.

## Open

- Shared inserts run about 9 more instructions than Rust's, at parity in
  cycles. What remains is diffuse across the descent and leaf lines both
  maps share, not one call or copy.
- Wall-clock throughput of shared inserts at 4 and 8 threads was never
  timed on a quiet machine; the counts above show no contention gap.
- Bulk build against Rust, any x86 timing, and MSVC performance were not
  measured.
