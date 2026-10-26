# 01 — The port: Rust to C++17

Status: done (2026-10-24, 2026-10-25), every module in scope (D3). How the
Rust crate `bbtree/` at upstream's `8adae97` became the headers under
`src/shoal/`, and how the port was checked.

## Method

The port went bottom up, one Rust module to one C++ header, each layer
tested before the next was written:

1. **Nodes:** layouts first, as `constexpr` arithmetic, checked against
   the numbers in the Rust crate's layout tests; then windows, separators
   and search keys, the SIMD summary counts, pages and latches, the leaf
   and the internal node.
2. **The unsync tree:** node stacks, insert, remove with rebalancing,
   prefix upkeep, teardown, and the invariant checker, which the
   differential tests run after every step.
3. **An early benchmark** of point operations against the Rust crate,
   before any more API: a gap found on a small surface is cheap to
   attribute (02-rust-parity.md).
4. **Walks, ranges and iterators**, then **the latched tree** (page pool,
   held nodes, descent, inserts with splits, appends, removals, scans) and
   the shared map, then both **batch** APIs.
5. **The rest of the API** once the core met Rust (D3): bulk build, clone,
   `split_off`, `retain`, `extend`, entries, `extract_if`, `append`,
   iteration by value, sets and set algebra, leaf annexes.

Where Rust and C++ differ in what they can say, the port picks the C++
idiom that keeps Rust's behaviour and cost; each such choice is a decision
(D6–D16). Where clang compiled the same algorithm to more work than LLVM
made of Rust's, the fix went into how the C++ is written, not into the
algorithm (D17–D21).

## Verification

- **Differential tests** (`tests/`, the zero-dependency harness in
  `tests/test_harness.hpp`): random operations against `std::map`,
  `std::set` and the `std::set_*` algorithms, with the tree's invariants
  checked as they go, over several key types, including strings that
  share long prefixes.
- **Same tree, node by node:** batched inserts and `extend` build exactly
  the nodes single inserts build, through a test hook on the map, as
  Rust's tests check.
- **Sanitizers:** ASan+UBSan on every test, Linux's leak checker on the
  pieces that move raw pages (clone, `split_off`, `IntoIter`, annexes),
  TSan on the shared maps (D12).
- **Mutation checks:** a fix or a hook removed on purpose (the batched set
  lookups, the annex release, a version check) must fail a test.
- **Other architectures:** x86_64 under Rosetta at the SSE2 baseline and
  with AVX2+BMI2+POPCNT before CI existed; then CI on every path (D24).

## Module map

Rust paths are under `bbtree/src/` of upstream at `8adae97`.

| Module | Rust source | C++ | State |
|---|---|---|---|
| layouts, plans | `tree/node/layout.rs`, `tree/node/node_plan.rs` | `detail/node/layout.hpp`, `plan.hpp` | ported; layouts match Rust's tests |
| inline keys, slots, SIMD | `inline_key.rs`, `tree/node/{inline_key_search,inline_keys,slots,vector_count,hash_tags}.rs` | `inline_key.hpp`, `detail/node/*.hpp` | ported; NEON, SSE2, AVX2+BMI2+POPCNT and AVX-512 tested in CI |
| leaves, internal nodes, root page, siblings | `tree/node/*.rs` | `detail/node/*.hpp` | ported |
| node stacks, insert, remove, prefixes, teardown | `tree/{node_stack,search,insert,remove,prefix,shared_prefix,root,teardown,invariants}.rs` | `detail/tree/*.hpp` | ported; differential tests vs `std::map` |
| walks, ranges, iterators | `tree/walk.rs`, `unsync/map/{iter,iter_mut}.rs` | `detail/tree/walk.hpp`, `unsync/range.hpp` | ported |
| unsync batches | `tree/batch.rs` | `detail/tree/batch.hpp` | ported; same nodes as single inserts |
| bulk build, clone, `split_off`, `retain`, `extend` | `tree/{build,clone,cut}.rs`, `unsync/map/{from_entries,mod}.rs` | `detail/tree/build.hpp`, `unsync/map.hpp` | ported; differential tests, ASan clean |
| latched tree, OLC, scans, batches | `tree/latched/*.rs`, `tree/node/optimistic.rs`, `map/{mod,batch}.rs` | `detail/latched/*.hpp`, `map.hpp` | ported; TSan clean (racing reads excluded, D12) |
| entry API, `extract_if`, `append`, `into_iter` | `unsync/map/{entry,extract_if,append,into_iter}.rs` | `unsync/{entry,extract_if,into_iter}.hpp`, `unsync/map.hpp` | ported; differential tests, ASan and leak checks clean |
| sets and set algebra | `unsync/set/*.rs` | `unsync/{set,set_algebra}.hpp` | ported, every strategy; differential tests against `std::set` algorithms |
| leaf annexes, shared map's `with_leaf` | `leaf_annex.rs`, `tree/node/annexed_leaf.rs`, `map/with_leaf.rs` | `leaf_annex.hpp`, `map.hpp` | ported; leak-checked through splits, merges, refusals and threads (TSan) |
| owned maps (`get_mut`, `iter_mut`, `clear`) | `map/owner.rs` | `map.hpp` | ported |
| file maps, mmap, store, vis, fuzz, Verus proofs | | | out of scope (D3) |

The hash maps have no Rust source (05-hash-maps.md).

## Layout

```
src/shoal/               the library, one header per feature
  map.hpp                shoal::TreeMap, the concurrent tree map (latched nodes)
  hash_map.hpp           shoal::HashMap, the concurrent hash map (inline slots, sharded)
  hash.hpp               shoal::Hash, the hash maps' default policy
  unsync/hash_map.hpp    shoal::unsync::HashMap, the single-threaded hash map (paged)
  unsync/hash_index.hpp  shoal::unsync::HashIndex, an index over rows stored elsewhere
  unsync/map.hpp         shoal::unsync::TreeMap, the single-threaded map
  unsync/range.hpp       its ranges and iterators
  unsync/entry.hpp       its entries (Entry, OccupiedEntry, VacantEntry)
  unsync/extract_if.hpp  its extractions, and retain's walk
  unsync/into_iter.hpp   its iteration by value
  unsync/set.hpp         shoal::unsync::TreeSet
  unsync/set_algebra.hpp operations on two sets: difference, intersection, union, ...
  leaf_annex.hpp         AnnexedLeaf, and what a shared map's leaf annex provides
  bound.hpp              Bound<Q> for ranges
  insert_error.hpp       InsertError, InsertOutcome (the shared map's failures)
  allocator.hpp          Allocator concept, Global, Layout, PageKind
  node_config.hpp        NodeConfig (page sizes)
  inline_key.hpp         InlineKey<K> traits: windows, hash tags, cmp_from
  relocatable.hpp        is_trivially_relocatable<T>
  detail/target.hpp      what the target provides: SIMD macros, builtins (D23)
  detail/node/           nodes: layouts, inline keys, SIMD search, leaves, internal nodes
  detail/tree/           the unsync tree: stacks, insert, remove, walks, batches, build
  detail/latched/        the latched tree: pages, racing reads, held nodes, batches
  detail/hash/           the hash tables: paged (owned map), inline slots (shared map)
tests/                   differential and unit tests (CTest)
bench/                   benchmarks matching upstream bbtree/bench workloads
bench/race/              the race against other C++ maps (opt-in: SHOAL_RACE_BENCH)
design/                  these notes
```

Organized by feature, as upstream's crate is, not by technical bucket: a
reviewer of one feature reads one header. Headers name the Rust module
they port, where there is one, so that the two can be read side by side.
