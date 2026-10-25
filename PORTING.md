# bbtree C++17 port

The Rust crate `bbtree/` of [XiangpengHao/bbtree](https://github.com/XiangpengHao/bbtree)
is the reference: the C++ port keeps its data structure, layouts and
algorithms. This fork branched from upstream at `8adae97`; the port was
checked against the Rust crate there by side-by-side benchmarks, then the
Rust code left this repository once the C++ port met its performance on
the same workloads. The tests here are differential against the standard
containers.

The design notes stay upstream, in [`design/` at `8adae97`](https://github.com/XiangpengHao/bbtree/tree/8adae97a0ed97e5c98fa2790312eb0aec9719846/design):
`design/NN-*.md` and "doc NN" in this repository name those files, and
`D<n>` the decisions of [`design/03-decisions.md`](https://github.com/XiangpengHao/bbtree/blob/8adae97a0ed97e5c98fa2790312eb0aec9719846/design/03-decisions.md).

Target: header-only C++17, zero dependencies, embeddable in DuckDB.

## Layout

```
src/bbtree/              the library, one header per feature
  map.hpp                bbtree::BBTreeMap, the concurrent map (latched nodes)
  unsync/map.hpp         bbtree::unsync::BBTreeMap, the single-threaded map
  unsync/range.hpp       its ranges and iterators
  unsync/entry.hpp       its entries (Entry, OccupiedEntry, VacantEntry)
  unsync/extract_if.hpp  its extractions, and retain's walk
  unsync/into_iter.hpp   its iteration by value
  unsync/set.hpp         bbtree::unsync::BBTreeSet
  unsync/set_algebra.hpp operations on two sets: difference, intersection, union, ...
  leaf_annex.hpp         AnnexedLeaf, and what a shared map's leaf annex provides
  bound.hpp              Bound<Q> for ranges
  insert_error.hpp       InsertError, InsertOutcome (the shared map's failures)
  allocator.hpp          Allocator concept, Global, Layout, PageKind
  node_config.hpp        NodeConfig (page sizes)
  inline_key.hpp         InlineKey<K> traits: windows, hash tags, cmp_from
  relocatable.hpp        is_trivially_relocatable<T>
  detail/node/           nodes: layouts, inline keys, SIMD search, leaves, internal nodes
  detail/tree/           the unsync tree: stacks, insert, remove, walks, batches, build
  detail/latched/        the latched tree: pages, racing reads, held nodes, batches
tests/                   differential and unit tests (CTest)
bench/                   benchmarks matching upstream bbtree/bench workloads
```

## Decisions specific to the port

- **P1. Strategies become traits.** Rust's `Auto`/`WithInlineKey` exist
  because Rust lacks specialization. C++ has it: `bbtree::InlineKey<K>` is a
  traits template; the primary template says "no inline keys", and the std
  byte strings and 64/128-bit integers are specialized as Rust's `Auto`
  recognizes them. A user key opts in by specializing it. The strategy
  parameter keeps only what is left: node config, leaf annex, latched.
- **P2. Keys and values are trivially relocatable.** The node code moves
  keys, values and whole entries as bytes (shifts, splits, page moves), as
  Rust moves everything. `bbtree::is_trivially_relocatable<T>` defaults to
  `std::is_trivially_copyable<T>` and is specialized for types known to be
  relocatable (`std::vector`, `std::unique_ptr`, `std::shared_ptr`, libc++'s
  `std::string`). A map static_asserts it. libstdc++'s `std::string` (SSO
  pointer into itself) is not relocatable: use `std::vector<uint8_t>`
  or a `std::string_view` into stable storage.
- **P3. Ordering.** Keys order by `operator<`. The three-way compare a
  search needs is `bbtree::detail::Order`; byte strings compare by bytes.
  Heterogeneous lookups: a byte-string key accepts any byte-string query
  (`std::string_view` for `std::string`), other keys need the key type or a
  type comparable with it by `operator<` (then without inline keys).
- **P4. User code must not throw while keys move** (Rust's D48 aborts on
  unwind there). Comparisons and `InlineKey` hooks run in `noexcept`
  functions: a throw terminates, never corrupts memory silently.
- **P5. Allocation failure.** The unsync map ends the program, as Rust's
  `handle_alloc_error` does: a split asks for pages midway and can't undo
  what it changed. The shared map returns `InsertError`, leaving the map
  unchanged, as Rust's does: it takes its pages before changing anything.
  DuckDB should embed the shared map, or an allocator that never fails.
- **P6. Batches without async.** Rust's batch lanes are async blocks polled
  by a private executor (D53). C++17 has no coroutines: lanes are explicit
  state machines, stepped level by level (P10), prefetching the line each
  reads next, only through `Prefetcher` (AGENTS.md).
- **P7. Racing reads.** Optimistic readers read header fields with relaxed
  atomic loads, and slots with plain loads in functions excluded from
  ThreadSanitizer, as the Rust code reads raw pointers. The version check
  decides what a read was worth (D54). A reader can also reach a page a
  writer just allocated, through an edge it read racing, and load that
  page's latch word while the allocator's zeroing of it is unordered with
  the load: the compare-and-swap that follows reads the word's latest value
  (the writer's lock), and a version read there is thrown away by the
  parent's check. Rust's reads are the same; TSan ignores races inside
  `NodeLatch`, which only ever touches latch words, atomically.
- **P8. Iterators.** Cursors keep the node stack (D24) in place; `end()` is
  a sentinel type, so range-for never copies a cursor.
- **P9. Walks cross leaves inline.** Rust keeps a walk's leaf crossing out of
  line. In C++ that call, which may change the walk, kept clang from holding
  the walk's index in a register: every entry's index went through a store
  and a load. 64-entry scans of a 1M map took 1,092 cycles at 1,375
  instructions, against Rust's 949 at 1,684. Inlined: 736 cycles.
- **P10. Batches go level by level.** Rust's batch lanes are async blocks;
  its design notes (doc 12) measure a level-by-level scheduler as the
  faster ceiling. Both C++ batches step every live lane through one stage,
  then the next, each stage a force-inlined function, the live lanes
  compacted once per level. A shared-tree lane that another thread's change
  sends back, or puts at another depth, goes alone after the batch. Stage
  dispatch through one switch cost the shared lookups 168 instructions per
  key; compacting after every stage, 36 more.
- **P11. A consuming iterator frees its pages at the end.** Rust's `IntoIter`
  takes the tree apart as it goes, freeing each node once both ends are
  past it. C++'s walks go one way, so `IntoIter` runs one walk from each
  end, moves entries out until the count left reaches zero, and gives every
  page back when it is destroyed. The work is the same; the pages stay
  allocated until then.
- **P12. Set operations look ahead without copying a walk.** Rust's batched
  lookups clone the iterator to read 32 elements ahead. C++ walks are never
  copied (D23): the cursor hands the next 32 to a buffer of pointers, which
  one batch looks up in place (`PointedKeys`), then out one at a time. A
  fold's runs come back from the refill by value: passed by reference, they
  lived in memory, and every step loaded and stored them (32 instructions a
  merge step against Rust's 18; by value, parity).

## Status

Rust paths are under `bbtree/src/` of upstream at `8adae97`.

| Module | Rust source | C++ | State |
|---|---|---|---|
| layouts, plans | `tree/node/layout.rs`, `tree/node/node_plan.rs` | `detail/node/layout.hpp`, `plan.hpp` | ported; layouts match Rust's tests |
| inline keys, slots, SIMD | `inline_key.rs`, `tree/node/{inline_key_search,inline_keys,slots,vector_count,hash_tags}.rs` | `inline_key.hpp`, `detail/node/*.hpp` | ported; NEON tested natively, SSE2 and AVX2+BMI2+POPCNT tested as x86_64 under Rosetta, AVX-512 compile-only |
| leaves, internal nodes, root page, siblings | `tree/node/*.rs` | `detail/node/*.hpp` | ported |
| node stacks, insert, remove, prefixes, teardown | `tree/{node_stack,search,insert,remove,prefix,shared_prefix,root,teardown,invariants}.rs` | `detail/tree/*.hpp` | ported; differential tests vs `std::map` |
| walks, ranges, iterators | `tree/walk.rs`, `unsync/map/{iter,iter_mut}.rs` | `detail/tree/walk.hpp`, `unsync/range.hpp` | ported |
| unsync batches | `tree/batch.rs` | `detail/tree/batch.hpp` | ported; same nodes as single inserts |
| bulk build, clone, `split_off`, `retain`, `extend` | `tree/{build,clone,cut}.rs`, `unsync/map/{from_entries,mod}.rs` | `detail/tree/build.hpp`, `unsync/map.hpp` | ported; differential tests, ASan clean |
| latched tree, OLC, scans, batches | `tree/latched/*.rs`, `tree/node/optimistic.rs`, `map/{mod,batch}.rs` | `detail/latched/*.hpp`, `map.hpp` | ported; TSan clean (racing reads excluded, P7) |
| entry API, `extract_if`, `append`, `into_iter` | `unsync/map/{entry,extract_if,append,into_iter}.rs` | `unsync/{entry,extract_if,into_iter}.hpp`, `unsync/map.hpp` | ported; differential tests, ASan and leak checks clean |
| sets and set algebra | `unsync/set/*.rs` | `unsync/{set,set_algebra}.hpp` | ported, every strategy; differential tests against `std::set` algorithms |
| leaf annexes, shared map's `with_leaf` | `leaf_annex.rs`, `tree/node/annexed_leaf.rs`, `map/with_leaf.rs` | `leaf_annex.hpp`, `map.hpp` | ported; leak-checked through splits, merges, refusals and threads (TSan) |
| owned maps (`get_mut`, `iter_mut`, `clear`) | `map/owner.rs` | `map.hpp` | ported |
| file maps, mmap, store, vis, fuzz, Verus proofs | | | out of scope |

## Hash maps

The C++ port also has an initial DuckDB-inspired hash map implementation.
`bbtree::unsync::BBHashMap` owns a single-threaded table; `bbtree::BBHashMap`
shares the same table implementation in shards, each behind the tree's
latch, read without it ("Shared reads" below).
Both are header-only C++17 with no external dependencies. This is a new
implementation, with no Rust reference or established performance claims.

```cpp
#include "bbtree/unsync/hash_map.hpp"
#include "bbtree/hash_map.hpp"

bbtree::unsync::BBHashMap<std::string, int> owned;
owned.insert("one", 1);                       // optional previous value
const int* value = owned.get("one");          // borrowed query, no string allocation
owned.reserve(10000);                        // value's address stays valid

bbtree::BBHashMap<std::string, int> shared;
auto inserted = shared.insert("one", 1);      // InsertOutcome, as the shared tree
auto copied = shared.get_copied("one");
shared.update("one", [](const auto&, int& value) { ++value; });
```

### Layout and costs

The inspiration is DuckDB's [two-part aggregate hash table](https://duckdb.org/2022/03/07/aggregate-hashtable):
a compact probe index filters candidates before following references into
payload storage, and cached full hashes allow index growth without hashing
keys again. Our descriptors use a 16-bit fingerprint and a 48-bit entry
identifier, without encoding pointers. Empty and deleted identifiers are
distinct; equality always checks the real key. Contiguous linear probing
stops only at an empty descriptor.

Entries contain the full mixed hash, `K`, and `V`. Pages contain at most 64
entries and aim for 4 KiB when entry size permits. Oversized entries get one
slot per page; over-alignment is preserved. Page bitmaps support iteration,
and deleted slots form a free list. Every allocation, including the page
directory and probe index, goes through the supplied allocator's
`allocate_zeroed`/`deallocate` interface.

The index grows at 75% occupied-plus-deleted capacity. At that threshold,
if live occupancy is below 50%, it rebuilds at the same capacity instead,
in place. An index array and the page directory each begin with their
capacity (a 64-byte header line, a word), for optimistic readers.
Index rebuilding is linear in allocated entry slots plus index capacity,
but moves no keys or values and calls neither hash nor equality. Payload
pages and holes remain allocated until `clear` or destruction.

With index occupancy `alpha`, payload occupancy `beta`, and aligned entry
size `E`, steady-state memory is approximately `8 / alpha + E / beta`
bytes per live entry, plus page headers, the directory and alignment.
Immediately after doubling, index occupancy falls to approximately 37.5%.
For `u64 -> u64`, `E = 24`: at 75% index occupancy and full payload pages
the estimate is 34.7 bytes per entry before bookkeeping. Small pairs pay a
substantial overhead relative to flat hash maps. A hit also depends on
descriptor, page pointer, entry, and any heap-owned key bytes; batching
overlaps independent queries but does not remove those dependencies.

### API and ownership

Template parameters are `K, V, Hasher, Equal, Allocator`; the shared map
additionally accepts a power-of-two shard count (default 64, maximum 65536).
Keys and values must have non-throwing move constructors and destructors.
Unlike the tree, byte relocatability is unnecessary: entries are constructed
in place, so ordinary `std::string` works on either standard library.
Replacement retains the original key and returns the previous value.

Hashing defaults to `bbtree::Hash<K>` (based on `std::hash`); owning byte
strings hash their borrowed string view. Both maps avalanche the policy's
result before extracting placement/fingerprint bits. This is not a keyed
hash or a collision-attack defense. Supply a suitable hasher for hostile
input. Equivalent keys and heterogeneous queries must hash identically;
hashing and equality must stay consistent while entries are stored, and
must not re-enter the map.

The owned map supports lookup, mutable lookup, key/value lookup, insertion,
removal, reservation, copying/moving, iteration, and batches. `clear_retain_capacity`
destroys entries while retaining the index and pages for another build. Fresh
slots use a bump cursor; erased slots use the free list. Insert/reserve
preserve existing references. Erase invalidates the removed entry; clear
and assignment invalidate all entries. Iteration order is unspecified;
structural mutation invalidates iteration. Empty maps allocate nothing.
`reserve` reserves index capacity, with payload pages still allocated lazily.
Allocation failure throws `std::bad_alloc` and preserves the entries, though
capacity may have changed. Hash/equality exceptions also preserve entries.
Copy assignment constructs the replacement before changing the target.

The shared map offers copied/cloned reads, scoped `read`/`update`, insertion,
removal and conditional removal, traversal, clear, and batches. Hash and
equality policies must allow simultaneous const calls; allocator copies
must allow calls from different shards. Read callbacks run on a copy of the
entry when keys and values are trivially copyable, and with the shard held
shared otherwise; updates hold it exclusively. Callbacks must neither
re-enter the map nor retain entry references. Direct pointer/reference
callback results are rejected. Callback exceptions release the shard;
changes an update made before throwing remain. Shared maps are neither
copyable nor movable.

Shared insertion returns an `InsertOutcome`, preserving the input entry on
the allocator's refusal. Exceptions from user policies propagate, including
`std::bad_alloc` thrown by a policy. Shard selection uses mixed bits 32–47,
leaving the fingerprint and low 32 placement bits untouched. Growth pauses
operations on its shard for a linear rebuild. Counts, traversal, clear and
invariant checks hold one shard at a time, and are not map-wide snapshots.
Exclude concurrent writers if `clear` must leave the entire map empty.
`clear` keeps a shard's memory for later entries; the map gives it back
when destroyed.

Owned `get_many` advances bounded tiles of 128 queries through descriptor
discovery and key comparison, writing pointers in input order. Completed
queries are removed from further rounds. `get_or_insert_many` hashes each
borrowed query once, creates missing owned keys with an initializer, and
returns stable mutable value pointers. Duplicate keys share a pointer and
invoke the initializer only once. Cached empty slots can be published directly
when there are no tombstones, the slot remains empty and enough capacity has
been reserved for the tile; otherwise it searches again. Earlier inserts
remain committed if initialization or allocation throws. `update_or_insert_many`
shares this discovery/publication logic and fuses value updates with resolution:
`initialize(row, query)` constructs a missing value; `update(row, query, value)`
changes an existing one. Callbacks run in input order, including duplicates,
and must not re-enter the map. Completed changes and modifications made by a
throwing update remain. `for_each_hashed` walks dense page runs and exports the
cached mixed hash without hashing again; that hash is local to the map's policy,
not a persistent identifier. Shared batch reads go 64 keys at a time
("Shared reads"). Insert batches process inputs sequentially,
including duplicates, and move from input arrays. Shared batches restore
refused entries to their input slots and report `failed[i]`. Earlier
operations remain committed on a later exception. Batch inputs/outputs
must be valid disjoint arrays; zero count permits null pointers. No hash
map path uses software prefetching.

### Shared reads

Readers of a shared map write nothing shared when keys and values are
trivially copyable, as the latched tree's readers (D54, P7). A shard is
the tree's `NodeLatch`: writers hold it exclusively and bump its version.
A reader reads the version, probes the shard's index and copies the entry
it finds without the latch (`HashTable::racing_find`), checks the version,
and only then runs user code, on its copy; after four changed versions it
holds the shard shared. Keys or values that are not trivially copyable are
always read with the shard held shared: their copies could not be made
from bytes a writer is changing.

Whatever a writer does meanwhile, a racing reader stays in memory it may
read. It learns an index array's or a directory's bounds from the array
itself, never from a separate capacity it might read from another state;
and a shard frees nothing a reader may still be reading while threads
share it: an index array or directory that growth replaces stays with the
shard (`RetainedBlocks`) until the map is destroyed, at most the size of
the ones in use since each growth doubles; a same-capacity rebuild works
in place; `clear` keeps pages. Release fences order a new array's, page's
or directory's contents before the pointer that publishes it.

`get_many_copied` reads 64 keys a call in steps, each step for every key
before the next, so that the keys' waits for memory overlap (no
prefetching): every shard's version by a relaxed load, then one fence; the
descriptor at each key's home slot; the cached hash of the entry each
leads to; the entries copied and keys compared; one fence, then every
version checked. A key whose shard changed is read again on its own. The
versions are loaded relaxed and fenced once because an acquire load per key
held each key up behind the previous key's memory wait.

Measured with `bench/race/race_bench.cpp` (10M random UUID keys, half the
lookups missing, M4 Pro), batched, millions of lookups per second:

| | 1 thread | 14 threads |
|---|--:|--:|
| shard behind `std::shared_mutex` | 8.6 | 13.5 |
| optimistic, versions by acquire loads | 8.5 | 113.2 |
| versions relaxed, one fence | 10.3 | |
| in steps across the batch | 28.5 | 243.4 |
| Boost `concurrent_flat_map`, bulk `cvisit` | 42.4 | 392.7 |

Before the steps, a key cost 117 instructions and 412 cycles (IPC 0.28):
the lookups waited on memory one after another. What separates the map
from Boost's now is the first access: Boost's metadata, a byte a slot
(about 12 MB here), stays in cache, while a miss here reads an eight-byte
descriptor from an index of 128 MB. A compact fingerprint array in front
of the descriptors would take that access out of most misses; it would
change the index the owned map and `BBHashIndex` share.

### Verification

`hash_map_test` differentially checks mixed operations against
`std::unordered_map`, including forced collisions and tombstones. Tests
also cover borrowed strings, stable references, cached hashes, move-only
and nonassignable values, over-aligned oversized entries, object lifetimes,
copy failures, and allocator refusals. `shared_hash_map_test` checks racing
writers/readers, atomic updates, scoped access, batch semantics, allocator
failure/retry, and exception unwinding; optimistic readers against writers
that grow, erase, rebuild and clear, and against writers that rewrite
eight-word values (a reader without its version check sees words from two
writes, and the test fails); and that what shards retain goes back to the
allocator. Both participate in normal CTest
and sanitizer builds. No benchmarks are run as part of tests.

`bbtree::unsync::BBHashIndex<Handle, Allocator>`, in
[`unsync/hash_index.hpp`](src/bbtree/unsync/hash_index.hpp), indexes rows
owned elsewhere: it consumes cached hashes unchanged, stores row handles
directly, and batches key matching and row creation through callbacks. It
shares descriptor storage with the owned map. `hash_index_test` covers it.

The DuckDB experiments built on these maps (the benchmark linking upstream
DuckDB's `GroupedAggregateHashTable`, the native integration, and their
measurements) stay on branch `codex/hashmap` (`f8ef72f`), with their
reports, logs and data.

## Measurements

`bench/*_bench.cpp` against `bench/reference` (the Rust map, same keys, same
order; removed with the Rust code, in git history before this fork dropped
it), M4 Pro, system allocator, `-mcpu=native`, one run each; a range
where runs on a quiet machine spread.

Unsync map, ns per operation (lower is better):

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

Unsync sets of u64 (`set_bench.cpp`), each operation folded into a count, ns
per element: two 1M sets sharing half their elements, walked side by side;
a 27K set, every 37th element of one, looked up in it a batch at a time.

| cell | Rust | C++ |
|---|--:|--:|
| 1M difference / intersection / union | 7.1 / 6.8 / 6.6 | 6.8 / 6.8 / 6.9 |
| 27K into 1M intersection / `is_subset` | 29.3 / 26.9 | 22.1 / 19.5 |

Shared map, millions of operations per second (higher is better):

| cell | Rust | C++ |
|---|--:|--:|
| 10M u64 insert at 1 / 4 / 8 threads | 4.4 / 17.0–17.3 / 31.3–31.6 | 4.4 / 16.1–16.4 / 29.1–29.7 |
| 10M u64 get_copied at 1 / 4 / 8 threads | 8.4 / 33.6–34.3 / 61.7–66.0 | 8.4–8.8 / 34.1–34.8 / 53.5–63.3 |
| 10M u64 batch_get_hit, 1 thread | 17.5–17.7 | 18.0 |
| 10M u64 batch_get_hit, 8 threads | 117.2–134.7 | 113.1–126.0 |
| 10M u64 batch_get_hit, instructions / cycles per key, 1 thread | 752 / 221 | 734 / 221 |
| 10M u64 get_copied, instructions per op, 1 thread | 424 | 443 |

Shared inserts, counted per insert by `/usr/bin/time -l` over every
thread, key setup (31–35 instructions) taken out. These counts hold still
while other load moves wall-clock throughput by ±15%; the cycles move by
about 2% from run to run.

| instructions / cycles per insert | Rust | C++ |
|---|--:|--:|
| 1 thread | 641 / 915 | 659 / 927 |
| 4 threads | 647 / 922 | 665 / 949 |
| 8 threads | 660 / 1029 | 683 / 1042 |

The throughput rows predate the fixes below.

- **Spinning waits with `isb`, as Rust's `spin_loop` does on Arm.** C++
  spun on `yield`, which waits for nothing on Apple cores: a thread waiting
  for a latch read its word back to back. A 4-thread insert run on a loaded
  machine went from 5–7% behind Rust to 4.6%; not yet measured on a quiet
  one.
- **Shared batch lookups keep their lanes in registers.** They ran 9% behind
  Rust on one thread, 36 more instructions per key on the same paths (4
  levels, the same ties and fallbacks, counted). Three things went through
  memory: `std::optional<Routed>`, 32 bytes, came back from the
  out-of-line tie search on the stack, and so did the untied result it
  merges with; the tie's window went by pointer, so every route spilled
  it; and the fence in each version check made clang read again every lane
  field read after it. `Routed` is 12 bytes now, the window goes by value,
  the steps read a lane's fields before the fence, and the level loop keeps
  its count and lane index in locals: 52 fewer instructions per key, 20
  fewer cycles.
- **Shared inserts inline into their caller, as Rust's do.** They ran 62
  more instructions per insert than Rust's, on one thread as on eight: a
  per-insert cost, not contention. Single-stepping 90 inserts of each map
  under lldb, every instruction attributed to its inlined function and
  line, found the excess: `BBTreeMap::insert` called out of line, saving
  and restoring 13 registers and checking the stack protector on every
  insert (24 instructions); the descent's `std::optional<Descent>`, moved
  out and destroyed (5); `LatchedTree::insert` out of line (6); and the
  separator flag, a `bool` read back from the register the out-of-line tie
  search returns it in, converted on each level (9). The map's inserts and
  `LatchedTree::insert` are force-inlined now, the descent fills the
  caller's `Descent`, and `Routed::at_separator` is a byte the descent ORs
  into its own.
- **The exact-lookup chain inlines too.** Clang also left
  `BBTreeMap::get_copied`, `Leaf::find`, `search_candidates` and
  `ByteWindows::cmp_from`/`cmp_prefix` out of line, where LLVM inlines
  Rust's; force-inlined, with the shared map's root loaded once in the
  descent instead of twice before it, a shared get runs 341 instructions
  against Rust's 357 (2M keys, by the `GET_ROUNDS` difference), and the
  path-string `get_hit` that ran 4% more cycles than Rust's at fewer
  instructions runs faster in time (185 against 276 ns at 1M, one run
  each). A shared `HeldLeaf` also holds nothing now, as Rust's
  `LatchMode::Held` does: `LeafFind` is 40 bytes, not 48.
- **A latch is taken with one acquire-release CAS.** `try_lock_at` paired
  an acquire CAS with a release fence; the CAS is acquire-release now,
  whose acquire half says what the fence did, with no `dmb` after it on
  Arm. The version check's fence stays: an acquire load would not order
  the racing reads before it. Plumbing the descent's root-leaf fact to
  `prefixed` in place of its `is_root` load was tried and reverted: 15
  more instructions per insert, the flag round-tripping through the
  stack where the load had been merged with the descent's own.

- **The leaf hands its outcome back unboxed.** `insert_into`,
  `try_append` and `settle` wrapped the insert policy's output in a
  second `std::optional`, and `split` took one by reference: three
  optional layers per insert where Rust's policy returns one `Result`.
  They return `LeafOutcome` directly now; a full leaf is a refusal of
  no layout, told apart from a policy refusal by the `refused`
  out-param, as before. 43 fewer instructions per insert at 10M
  (6,889M to 6,846M); the lldb line profile no longer shows the
  optional-construction line that dominated the old one.

Open:

- Shared inserts run about 9 more instructions than Rust's (685 against
  676 per insert at 10M, key setup included in both, by the same
  difference count at `GET_ROUNDS=0`), at parity in cycles on the runs
  measured. What remains is diffuse across the descent and leaf lines
  both maps share, not one call or copy. Wall-clock throughput at 4
  and 8 threads still wants a quiet machine.
