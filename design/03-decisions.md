# Decisions

Decisions made so far, with the reason and what follows from each. Each
entry states a decision as it holds now. To change one, edit it in place;
the file is not a history of decisions.

`D<n>` in this repository names an entry of this file. `OG-D<n>` names a
decision of upstream bbtree's
[`design/03-decisions.md` at `8adae97`](https://github.com/XiangpengHao/bbtree/blob/8adae97a0ed97e5c98fa2790312eb0aec9719846/design/03-decisions.md),
the commit this fork branched from, and `OG-design/NN-*.md` or "OG-doc NN"
one of upstream's [design notes](https://github.com/XiangpengHao/bbtree/tree/8adae97a0ed97e5c98fa2790312eb0aec9719846/design)
there ("OG" for the original). The tree maps keep upstream's decisions
unless an entry here departs from one, and says why.

## Scope (from 00-init)

**D1. Header-only C++17, zero dependencies, for DuckDB.** (2026-10-24)
The maps are headers under `src/shoal/`, standard C++17 apart from one
header (D23), with no dependency but the standard library. A map's memory
comes from an allocator, its last template parameter (OG-D8).

- **Why C++17:** the target the port was asked for (00-init.md). It brings
  `if constexpr`, `std::optional` and inline variables, which the node
  plans (OG-D47) lean on, and nothing later is needed.
- **Why header-only:** DuckDB vendors its third-party code; headers vendor
  by copying a directory, and the map's hot paths inline into the
  embedder's code (D17), compiled with its flags (D22).
- **Build:** CMake, for the tests and benchmarks only. Warnings are errors
  (`-Wall -Wextra -Werror`, MSVC `/W4 /WX /permissive-`), so that an
  embedder's warning flags find nothing. Dependencies the benchmarks race
  against are opt-in (D32).

**D2. Upstream is the reference for the tree maps.** (2026-10-24)
The tree maps keep the Rust crate's data structure, page layouts and
algorithms, line for line where C++ allows: the same nodes, the same
splits and merges, the same descent, the same batch lanes. A layout's
numbers are checked against the Rust crate's tests. Where the C++ code
departs from the Rust code it says why in a decision here (D6–D21).

- **Why:** upstream measured and reasoned its way to its design
  (OG-design/06-cost-model.md); a port that keeps it inherits that work,
  and a gap against the Rust map is then a C++ problem, not a design one,
  which narrows where to look (01-port.md).
- **How it was checked:** the Rust crate stayed in the repository until
  the C++ map met it on the same workloads: benchmarks with the same keys
  in the same order on both sides (02-rust-parity.md), and the shared
  map's batched inserts checked to build the same tree, node by node, as
  single inserts. The tests are differential against `std::map`,
  `std::set` and `std::unordered_map`.

**D3. The whole map API, not the file maps.** (2026-10-24, 2026-10-25)
Ported: the unsync map and set, the shared map, their batches, entries,
`extract_if`, `retain`, `append`, iteration by value, bulk build, clone,
`split_off`, `extend`, set algebra, leaf annexes and `with_leaf`. Not
ported: file maps, `bbtree-mmap`, `bbtree-store`, the visualizer, fuzzing
and the Verus proofs.

- **Why:** DuckDB wants in-memory maps; it has its own storage. The rest of
  the map API was first left out to reach performance parity on the core
  maps sooner (a smaller surface to profile); it was ported once the core
  matched Rust, since an entry API and sets are what a drop-in map is
  judged on (OG-D1).

**D4. A C++-only fork.** (2026-10-25)
Once the C++ maps met the Rust crate's performance, the Rust crates,
their tooling (Cargo, the Nix flake, Miri, fuzzing, Verus, shuttle) and
upstream's design notes left the repository. The C++ code moved to the
root: `src/shoal/`, `tests/`, `bench/`. This folder records the fork's
own design; upstream's notes are cited as OG-.

- **Why:** two implementations in one repository is two to keep in step,
  and the Rust one is maintained upstream. What the Rust reference
  measured stays in 02-rust-parity.md and in git history before
  `d0e4f24`.

**D5. The name: shoal.** (2026-10-26)
The library is `shoal` (namespace `shoal`, `SHOAL_` macros and options,
`#include <shoal/...>`), its maps `TreeMap`, `TreeSet`, `HashMap` and
`HashIndex`, each under `shoal::unsync` where single-threaded.

- **Why not bbtree:** bbtree is the Blessed B-tree; with hash maps beside
  the tree (D25) the library is no longer one B-tree, and borrowing
  upstream's name, or its "BB", would claim its identity. A shoal is many
  fish moving together without colliding: shards, readers that write
  nothing, bulk reads with 64 keys in flight.
- **What keeps the old name:** "bbtree" means the upstream Rust crate,
  wherever it appears. LICENSE says shoal derives from bbtree and keeps its
  copyright line.

## Translating Rust

**D6. Strategies become traits.** (2026-10-24)
Rust's `Auto` and `WithInlineKey` strategies exist because Rust lacks
specialization (OG-D18). C++ has it: `shoal::InlineKey<K>` is a traits
template whose primary template says "no inline keys"; the standard byte
strings and the 64- and 128-bit integers are specialized as Rust's `Auto`
recognizes them, and a user key opts in by specializing it. The strategy
parameter keeps only what is left: node config, leaf annex, latched.

- **Why:** keys are generic (any type with `operator<`), and DuckDB picks
  its key types; a trait it specializes for its own string type costs one
  specialization, not a strategy type threaded through every map.

**D7. Keys and values are trivially relocatable.** (2026-10-24, 2026-10-25)
The node code moves keys, values and whole entries as bytes (shifts,
splits, page moves), as Rust moves everything (OG-D9, OG-D14).
`shoal::is_trivially_relocatable<T>` defaults to
`std::is_trivially_copyable<T>` and is specialized for types known to
move as their bytes: `std::vector`, `std::unique_ptr`, `std::shared_ptr`,
and the `std::string` of libc++ and of MSVC, whose short strings keep
their bytes in place with no pointer to them. A map `static_assert`s it.

- **Not relocatable:** libstdc++'s `std::string`, whose short-string
  pointer points into itself; and any container under checked iterators
  (MSVC's `_ITERATOR_DEBUG_LEVEL` above 0, libstdc++'s debug mode), which
  registers its address with its iterators. A map of those fails to
  compile, rather than corrupting memory. Use `std::vector<uint8_t>`, or a
  `std::string_view` into stable storage.
- **Why not fix up after each move:** every byte move in the node code
  would need a pass over what it moved; the hash maps, which construct
  entries in place, take any type instead (D25).
- **Layout tests check `std::string` itself,** not a stand-in shaped like
  Rust's `String`: the numbers worked out for Rust's 24-byte string are
  checked where `std::string` is 24 bytes (libc++); elsewhere (MSVC, 32
  bytes) the choices that hold at either size are checked, and the layout
  code checks its own invariants at compile time for every shape.

**D8. Ordering by `operator<`.** (2026-10-24)
Keys order by `operator<`; the three-way compare a search needs is
`shoal::detail::Order`, and byte strings compare by their bytes. A
byte-string key accepts any byte-string query (`std::string_view` for
`std::string`); other keys need the key type, or a type comparable with
it by `operator<`, then without inline keys.

**D9. User code must not throw while keys move.** (2026-10-24)
Comparisons and `InlineKey` hooks run in `noexcept` functions: a throw
terminates the program, never leaves a node half moved. Rust aborts on an
unwind there (OG-D48).

**D10. Allocation failure: the unsync map ends the program, the shared
map refuses.** (2026-10-24)
The unsync map ends the program, as Rust's `handle_alloc_error` does: a
split asks for pages midway and can't undo what it changed. The shared map
returns `InsertError`, the map unchanged, as Rust's does: it takes its
pages before changing anything (OG-D8). DuckDB should embed the shared map,
or give the unsync map an allocator that never fails.

**D11. Batch lanes are state machines, stepped level by level.**
(2026-10-24)
Rust's batch lanes are async blocks polled by a private executor (OG-D53).
C++17 has no coroutines: a lane is a struct of its state, and both C++
batches step every live lane through one stage, then the next, each stage
a force-inlined function, the live lanes compacted once per level. A
shared-tree lane that another thread's change sends back, or puts at
another depth, goes alone after the batch. Lanes prefetch the line each
reads next, only through `Prefetcher` (AGENTS.md).

- **Why level by level:** upstream's notes (OG-doc 12) measure a
  level-by-level scheduler as the faster ceiling, and without async it is
  also the simpler code. Dispatching stages through one switch cost the
  shared lookups 168 instructions a key; compacting after every stage, 36
  more.

**D12. Racing reads.** (2026-10-24, 2026-10-25)
Optimistic readers read header fields with relaxed atomic loads, and slots
with plain loads in functions excluded from ThreadSanitizer
(`SHOAL_RACING_READ`), as the Rust code reads raw pointers. The version
check decides what a read was worth (OG-D54). The hash maps' optimistic
readers follow the same rule (D26).

- **A fresh page's latch:** a reader can reach a page a writer just
  allocated, through an edge it read racing, and load that page's latch
  word while the allocator's zeroing of it is unordered with the load. The
  compare-and-swap that follows reads the word's latest value (the
  writer's lock), and a version read there is thrown away by the parent's
  check. Rust's reads are the same. TSan ignores races inside `NodeLatch`
  (`tests/tsan.supp`), which only ever touches latch words, atomically;
  without it `shared_map_test` failed 2 of 15 runs under TSan, with it 0.

**D13. Iterators keep the node stack in place.** (2026-10-24)
Cursors keep the node stack (OG-D24) where they are; `end()` is a
sentinel type, so range-for never copies a cursor.

**D14. Walks cross leaves inline.** (2026-10-24)
Rust keeps a walk's leaf crossing out of line. In C++ that call, which may
change the walk, kept clang from holding the walk's index in a register:
every entry's index went through a store and a load. 64-entry scans of a
1M map took 1,092 cycles at 1,375 instructions, against Rust's 949 at
1,684. Inlined: 736 cycles.

**D15. A consuming iterator frees its pages at the end.** (2026-10-25)
Rust's `IntoIter` takes the tree apart as it goes, freeing each node once
both ends are past it. C++'s walks go one way, so `IntoIter` runs one walk
from each end, moves entries out until the count left reaches zero, and
gives every page back when it is destroyed. The work is the same; the
pages stay allocated until then.

**D16. Set operations look ahead without copying a walk.** (2026-10-25)
Rust's batched lookups clone the iterator to read 32 elements ahead. C++
walks are never copied (OG-D23): the cursor hands the next 32 to a buffer
of pointers, which one batch looks up in place (`PointedKeys`), then out
one at a time. A fold's runs come back from the refill by value: passed by
reference they lived in memory, and every step loaded and stored them (32
instructions a merge step against Rust's 18; by value, parity).

## Code generation: matching what LLVM makes of Rust

Each of these closed a gap that profiling found between the C++ and the
Rust map on the same path, with the same nodes visited, the same ties and
fallbacks taken (02-rust-parity.md).

**D17. Hot paths inline into their callers, as LLVM inlines Rust's.**
(2026-10-25)
`SHOAL_ALWAYS_INLINE` is Rust's `#[inline(always)]` (OG-D41, OG-D47), and
goes also where clang left out of line what LLVM inlines in Rust: the
shared map's inserts and lookups and `LatchedTree::insert`, the exact
lookup chain (`Leaf::find`, `search_candidates`, the byte windows'
`cmp_from` and `cmp_prefix`), and the walk's `next` with its crossing
(D14).

- **Why:** out of line, `TreeMap::insert` saved and restored 13 registers
  and checked the stack protector on every insert, 24 of the 62
  instructions an insert ran over Rust's; `LatchedTree::insert`, 6 more.
  The lookup chain inlined took a shared get from 372 to 341 instructions
  (Rust 357), and the path-string `get_hit` from 4% more cycles than Rust
  to faster.
- **Cost:** the insert path, about 490 instructions, is compiled into
  every caller, as Rust's compiler does.

**D18. Results come back in registers.** (2026-10-25)
A hot path returns nothing that must go through memory: `Routed` is 12
bytes, so that `std::optional<Routed>` returns in registers; its
`at_separator` is a byte the descent ORs into its own, where a `bool` was
converted back on every level; the tie window goes by value, not by
pointer; a descent fills the caller's `Descent` instead of returning an
optional one; the leaf returns the insert policy's `LeafOutcome` itself,
not wrapped in a second `std::optional`; and a shared `HeldLeaf` holds
nothing, as Rust's `LatchMode::Held` (`LeafFind` 40 bytes, not 48).

- **Why:** each was found in the disassembly as values spilled to the
  stack: the batch's route result and window (with lane fields reloaded
  after each version fence, now read before it), 52 instructions a key;
  the descent's optional, 5 an insert; the flag, 9; the boxed outcome,
  43M instructions over 10M inserts.

**D19. Spin waits are `isb` on Arm.** (2026-10-24)
A spinning thread waits with `pause` on x86 and `isb` on Arm, as Rust's
`core::hint::spin_loop`. `yield` waits for nothing on Apple cores: a
thread waiting for a latch read its word back to back, and C++ inserts ran
5–7% behind Rust's at 4 and 8 threads, at parity on one. Since, going from
1 to 8 threads adds about as many instructions per insert in C++ as in
Rust (+24 against +19).

**D20. A latch is taken with one acquire-release CAS.** (2026-10-25)
`try_lock_at` is one acquire-release compare-and-swap, where Rust pairs an
acquire CAS with a release fence: the acquire half keeps writes made under
the latch from being seen before the latch is taken, and on AArch64 it is
one `casal` with no `dmb` after it. The version check keeps its fence: an
acquire load would not order the racing reads before it.

**D21. A latched descent reads the root once and the shared prefix in
place.** (2026-10-25)
The descent loads the root pointer itself, and answers a null root as
"outside", which lookups read as absent: they loaded it to ask
`has_root`, then again in the descent (Rust loads it twice, the port
three times). And it asks `place_outside_racing` of the root page's
shared prefix in place, where Rust copies its 65 bytes first; the version
check vouches for either. The batch start keeps the copy, which its lanes
hold across stages.

- **Tried and reverted:** passing the descent's root-leaf fact to
  `prefixed` in place of its `is_root` load cost 15 instructions an
  insert, the flag round-tripping through the stack where the load had
  been merged with the descent's own.

**D22. The stack protector stays the embedder's choice.** (2026-10-25)
The build sets no `-fno-stack-protector`, and the code opts no function
out. Benchmarks run with the toolchain's default (on for Apple clang and
Ubuntu's GCC; Rust has none).

- **Why:** the library is header-only: its code compiles with DuckDB's
  flags, not ours, and a flag here would only make the benchmarks measure
  a build nobody ships. The check aborts a stack overwrite in production,
  for instance a corrupted tree driving the 24-level node stack past its
  end, which the sanitizers, run at test time on tested paths, can't vouch
  for. Inlining the inserts (D17) moved the check from every insert to its
  caller, which took most of its cost (11 instructions an insert) away.

## Portability

**D23. One header says what the target provides.** (2026-10-25)
`detail/target.hpp` holds everything that differs between compilers: the
architecture and SIMD macros the node search picks its code with
(OG-D41), and the few instructions C++17 has no word for (bit counts,
byte swaps, the spin wait, racing loads), spelled as GCC/clang builtins or
MSVC intrinsics. Everything else is standard C++17. The prefetch stays in
`Prefetcher` (D11). MSVC's `/arch:AVX2` brings BMI2, `/arch:AVX` POPCNT.

- **Why:** DuckDB builds with GCC, clang and MSVC; one header to review
  per new target, not every header that counts bits.

**D24. CI runs every SIMD path on every OS.** (2026-10-25)
`.github/workflows/cpp.yml` runs the tests with clang and libc++ on Linux
x86_64 (SSE2, AVX2+BMI2+POPCNT, AVX-512), Linux aarch64, macOS arm64 and
macOS x86_64; with MSVC on Windows x64 (SSE2, AVX2) and Windows arm64, and
builds MSVC's AVX-512; and ASan+UBSan and TSan on both Linux
architectures. Code under `#if` for one instruction set must compile on
the others.

- **No GCC/libstdc++ job:** the library compiles with GCC 13 and
  `-Werror`, but the tests store `std::string`, which libstdc++ can't move
  as bytes (D7).
- **Known sanitizer exceptions:** ASan's alloc-dealloc check is off on
  Linux, which Ubuntu's libc++ trips inside its own exception classes
  whenever a test throws; MSVC's C4702 ("unreachable code" after an
  inlined throw) and C4324 (`CachePadded` pads on purpose) are off.

## Hash maps (05-hash-maps.md)

**D25. Two hash maps on two tables.** (2026-10-25, 2026-10-26)
`shoal::unsync::HashMap` keeps a paged table, after DuckDB's two-part
aggregate hash table: a probe index of descriptors in front of entries in
pages that never move, so references stay valid through growth.
`shoal::HashMap` keeps an inline-slot table in each of its shards: the
entries in the slots linear probing visits, a tag a slot in front, one
block. `shoal::unsync::HashIndex` indexes rows owned elsewhere, on the
paged table's descriptors.

- **Why two tables:** the owned map hands out references that must
  outlive growth, so its entries can't move; the shared map hands out none
  that outlives a callback, so its entries may move, and live where the
  probe finds them. The paged layout made the shared map twice the work
  of Boost's: a hit walked shard, version, index header, tag, descriptor,
  page directory, page, entry and version again (160 instructions and 96
  cycles a key cached, against Boost's 85 and 41).
- **Entries are constructed in place,** so keys and values need only
  non-throwing moves and destructors, not relocation (D7): `std::string`
  works on every standard library.

**D26. The shared hash map's readers write nothing shared.**
(2026-10-25)
A shard is the tree's `NodeLatch`. Writers hold it exclusively and bump its
version. A reader of trivially copyable keys and values reads the version,
probes the shard's block and copies the entry it finds without the latch,
checks the version, and only then runs user code, on its copy; after four
changed versions it holds the shard shared. Other types are always read
with the shard held shared.

- **Why:** a `std::shared_mutex` read lock writes the lock's line; with
  14 cores reading, those lines bounce between them. Behind one, the map
  did 13.5 M lookups/s at 14 threads; optimistic, 113.
- **Memory a racing reader may read:** it learns a block's bounds from
  the block's header, never from a separate capacity; a block that growth
  replaces stays with the shard (`RetainedBlocks`) until the map is
  destroyed, less than the block in use since each growth doubles;
  `clear` keeps the block. A release fence orders a new block's contents
  before the pointer that publishes it.

**D27. Tags in front.** (2026-10-26)
Both tables keep a byte a slot, in front of what they probe: empty (and,
in the paged table, deleted), or occupied with the hash's top seven bits.
A lookup reads a descriptor or slot only where the tag matches, so a miss
reads none. The inline table reads its tags eight at a time as one word
(the first seven cloned past the last, so that a group never wraps), and
a zero-byte test finds the first match and the first empty one. In the
paged table descriptors stay the truth and every write of one writes its
tag.

**D28. Bulk reads go in steps, and branch on no outcome.** (2026-10-26)
`get_many_copied` reads 64 keys a call, each step for every key before the
next, so that their waits for memory overlap without prefetching: every
shard's version by a relaxed load, then one fence; the eight tags at each
key's home; which keys go on, those with a match before an empty one or
with neither; for those, the first match's cached hash; the entry copied
and keys compared; one fence, then every version checked. A key whose
shard changed is read again on its own. No step branches on whether a key
is there: the keys that go on are listed by writing every key at the
list's end and moving the end by whether it goes on; a key with eight full
tags and no match reads a zeroed `NoEntry` in its slot's place; a hit is
`candidate & hash equal & keys equal`, and a miss's value is written and
dropped.

- **Why one fence:** an acquire load per key held each key up behind the
  previous key's wait for memory (8.5 to 10.3 M/s on one thread).
- **Why no branch:** with half the race's keys absent, branches on the
  outcome mispredicted half the time, and each misprediction also threw
  away the following keys' loads: 78 cycles a key against Boost's 40.
  Branch-free, 46. Merging steps instead saved instructions and cost time
  (27.4 to 19.8 M/s), for the same reason.
- **Why only the keys that may be there go on:** a key its tags show
  absent cost what a hit did, read `NoEntry`, copied it and compared keys,
  where Boost's miss stops at its group's metadata: 133 instructions and
  74 cycles a lookup against Boost's 56 and 48, at 10% hits (10M keys, one
  thread). Listed, 112 and 55 at 10% hits; 127 and 76 at 50%, from 131 and
  97. That cycles fell 21 at 50% hits, instructions only 4, isn't
  explained yet. At 90% hits, where nearly every key goes on, the list
  costs 14 instructions a lookup (129 to 143) and still saves 7 cycles
  (87 to 80); in cache (100K keys) it costs 11% at 90% hits (161 to 143
  M/s), nothing at 50%, and gains 14% at 10%.
- **Why the slot's address comes before the select:** written as
  `candidate ? slot : NoEntry`, clang compiled the select to a branch,
  the slot's address being work in one of its arms (`__builtin_unpredictable`
  alone didn't stop it). Computed first, as an integer, the select is a
  conditional move (`SHOAL_UNPREDICTABLE` says so to the compiler as well):
  cached (100K keys, one thread), 80 to 120 M/s at 50% hits, against
  Boost's 110; 150 to 164 at 90%, against 158.

**D29. Removal shifts back, no tombstones.** (2026-10-26)
An inline table's removal shifts the entries after the hole back into it
(Knuth's Algorithm R): no tombstones, so no rebuild, and a block is only
ever replaced by growth (D26). Growth moves entries by their cached
hashes, running no user code.

**D30. A hasher that says it avalanches is used unmixed.** (2026-10-26)
Both maps mix the policy's hash before taking placement and tag bits,
unless the policy declares `is_avalanching`, as Boost reads it
(`HashBits::of`). The race's UUID hash declares it, and both shoal's and
Boost's maps skip their mix for it.

## Measuring

**D31. Parity is judged by counts, and ratios within one run.**
(2026-10-25, 2026-10-26)
Against Rust, a gap is measured by instructions and cycles counted per
operation (`/usr/bin/time -l` over every thread, the `GET_ROUNDS` and
`BATCH_ROUNDS` differences), and found by attributing them: the same
paths taken on both sides, then disassembly, then single-stepping both
maps under lldb with every instruction attributed to its inlined function
and line. Wall-clock throughput is quoted only from a quiet machine (no
swapping, load low); between maps, only ratios measured in the same run
are compared.

- **Why:** on a shared laptop, back-to-back runs at 8 threads moved by up
  to 15%, more than the gaps chased; the counts held still. A Boost drop
  from 427 to 292 M/s read across two runs and two binaries was 25%, not
  32%, measured back to back (06-race.md).

**D32. The race: upstream's README race, in C++, opt-in.** (2026-10-25)
`bench/race/` races shoal's maps against the best-known C++ maps that
threads can share, on the workload of upstream's README race
(`OG-design/assets/readme-race.svg`): 10M random UUID keys, 64 keys a call, half the lookups missing by default, in
two heats, tree maps and hash maps. Each map gets its best use, Boost's
bulk `cvisit` included. The competitors are dependencies, so the race
builds only with `-DSHOAL_RACE_BENCH=ON`. Many-key reads are called
"bulk", as Boost's API calls them.

**D33. A root page's size is a multiple of its alignment.** (2026-10-26)
The root page is room for the larger node kind, then the shared prefix
(OG-D50). That sum is not a multiple of the page alignment: a `uint64_t`
map's root is 1089 bytes, aligned to 1024. Rust's `Layout` allows a size
that is not a multiple of its alignment. C++17's aligned `operator new`
does not: the call is undefined unless the size is a multiple of the
alignment. `FreshPage`'s root layout rounds the size up
(`NodeLayoutMath::next_multiple_of`); the prefix stays at
`root_area_offset`, and the spare bytes are padding. Deallocation uses
that same layout.

- **Why not pad `SharedPrefix`:** its 65 bytes are the length and the 64
  prefix bytes, and a latched descent reads the length at a fixed offset.
  Padding the page, not the value, leaves that layout alone.
