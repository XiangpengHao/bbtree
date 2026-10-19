# 13 — Go-to-market: where an ordered map is on the critical path

Status: survey, 2026-10-15. Six research agents read the code, PRs and
issues linked below; company and funding facts are unverified. Not yet
re-checked by hand.

## Question

bbtree needs a system where an ordered in-memory map is heavy and
data-intensive: on the critical path of a CPU-bound workload, at a million
entries or more (or rebuilt at a high rate), and a large share of the
profile. A demo works only there (Amdahl). Most famous Rust apps fail this:
Codex, Bun, DataFusion, Polars, Turso, SlateDB, Qdrant's search path and
most blockchain clients keep their ordered maps off the hot path, or use
hash-then-sort, sorted runs or concurrent skip lists instead.

Candidates are split by two tests:

- **Fit:** written in Rust, and the map has one writer at a time (a lock,
  per-thread state, or left-right copies), so bbtree's `&mut self` API
  works without a concurrent variant.
- **Evidence:** a profile, issue, PR or benchmark shows the map is hot.

Tier 1 passes both. Tier 2 passes fit but has no evidence yet. Systems
that are hot but need concurrent writers, snapshots or rank (SurrealDB,
Lance, Weaviate, Reth, Valkey, Zed) are out of scope here.

Before integrating with any of them: profile the target workload and add
up the map's self-time, which is the ceiling of any speedup.

## Tier 1: fits, and evidence the map is hot

### SpacetimeDB (Clockwork Labs)

An in-memory relational database whose table indexes are std `BTreeMap`s.

- **Structure:** an enum over typed `BTreeMap`s. Non-unique indexes are
  `BTreeMap<K, SameKeyEntry>`
  ([btree_index.rs#L19](https://github.com/clockworklabs/SpacetimeDB/blob/master/crates/table/src/table_index/btree_index.rs#L19)),
  unique ones `BTreeMap<K, RowPointer>`
  ([unique_btree_index.rs#L13](https://github.com/clockworklabs/SpacetimeDB/blob/master/crates/table/src/table_index/unique_btree_index.rs#L13)).
  Keys are integers up to `u256`, `F32`/`F64`, `Box<str>`, `AlgebraicValue`,
  and 8–128-byte order-preserving encodings of multi-column keys
  ([mod.rs#L810](https://github.com/clockworklabs/SpacetimeDB/blob/master/crates/table/src/table_index/mod.rs#L810)).
- **Operations:** point seeks dominate. Every row write inserts or deletes,
  and every transaction and subscription query seeks.
- **Evidence:** they keep tuning these indexes against hash and dense-array
  ones.
  - [#2221](https://github.com/clockworklabs/SpacetimeDB/pull/2221) added
    direct (dense `Vec`) indexes: a semijoin benchmark −22%; a `seek_point`
    change made point scans about 12% faster.
  - [#4248](https://github.com/clockworklabs/SpacetimeDB/pull/4248): encoding
    composite keys as bytes took a hash-index seek from 111 to 46 ns, for
    their game BitCraft.
  - [#3976](https://github.com/clockworklabs/SpacetimeDB/pull/3976) added
    hash indexes; [#4733](https://github.com/clockworklabs/SpacetimeDB/pull/4733)
    byte-key B-tree indexes; [#4311](https://github.com/clockworklabs/SpacetimeDB/pull/4311)
    calls the index path "very load bearing code".
  - [#3240](https://github.com/clockworklabs/SpacetimeDB/issues/3240): indexes
    with "> 1 million rows with the same key".
- **Fit:** `Arc<RwLock<CommittedState>>`, one writer
  ([datastore.rs#L71](https://github.com/clockworklabs/SpacetimeDB/blob/master/crates/datastore/src/locking_tx_datastore/datastore.rs#L71)).
- **Pitch:** one ordered index that matches their hash index on point
  seeks, instead of maintaining B-tree, hash and direct index types.

### Meilisearch indexer

Search-engine indexing that hashes, then sorts, byte-string keys.

- **Structure:** per-thread hashbrown maps `HashMap<&[u8], DelAddBbbul>`,
  bump-allocated with FxHash, one per bucket. At merge time
  `merge_caches_sorted` sorts each map's entries, probes the other threads'
  maps for each key, and merges with spilled sorters
  ([cache.rs, L446–530](https://github.com/meilisearch/meilisearch/blob/main/crates/milli/src/update/new/extract/cache.rs)).
- **Operations:** `insert_add_u32(key, docid)` for every word and every
  nearby word pair in every document
  ([extract_word_pair_proximity_docids.rs](https://github.com/meilisearch/meilisearch/blob/main/crates/milli/src/update/new/extract/searchable/extract_word_pair_proximity_docids.rs)),
  then one ordered pass. Millions of distinct keys.
- **Evidence:**
  - [#5124](https://github.com/meilisearch/meilisearch/pull/5124) added
    sorting whole maps before the merge: `word_position_docids` merge 988 s
    → 202 s, prefix-position recompute 429 s → 62.5 s.
  - [#5130](https://github.com/meilisearch/meilisearch/pull/5130), "Replace
    the HashMaps caches by BTreeMaps", was closed after benchmarking: "Ok,
    we lose too much time." std `BTreeMap` lost on inserts.
- **Fit:** the maps are per-thread; they use a bump allocator, which
  bbtree's allocator parameter takes.
- **Open question:** bbtree wins only if its insert beats hash insert plus
  the sort. On 10M wiki-titles (2026-10-15, scratch run), `insert_many` 16
  took 303 ns per key against hashbrown's 77 ns, so the sort has to cost
  more than the 226 ns gap. #5130 is a ready-made A/B to measure that.

### Neon pageserver

The in-memory layer that buffers write-ahead-log records before they are
written as layer files.

- **Structure:** `RwLock<BTreeMap<CompactKey /*i128*/, VecMap<Lsn,
  IndexEntry>>>`
  ([inmemory_layer.rs#L78](https://github.com/neondatabase/neon/blob/main/pageserver/src/tenant/storage_layer/inmemory_layer.rs#L78)).
- **Operations:** `entry()` for every WAL record ingested (`put_batch`),
  `range()` for every page read.
- **Evidence:** [#8452](https://github.com/neondatabase/neon/issues/8452),
  "2x ingest performance": the index "grows huge … When profiling, a
  significant fraction of runtime is spent in btreemap::entry (and
  Key::cmp)". [#715](https://github.com/neondatabase/neon/issues/715)
  asked to replace the map over memory overhead. Their fix shrank the key
  to `i128` and kept the structure.
- **Fit:** one writer under the timeline write lock, async readers.
- **Risk:** Databricks acquired Neon in 2025; the repo was last pushed
  2026-08-31.
- **Note:** the competitor is std `BTreeMap` with integer keys. On 10M
  random `u64` (2026-10-15), bbtree's `get` took 148 ns against std's 312.

### Raphtory (Pometry)

A temporal-graph engine; every edge event updates ordered maps.

- **Structure:** two tiers, a sorted `Vec` that becomes a `BTreeMap` past a
  size cutoff.
  - Neighbours: `AdjSet<VID, EID>`, sorted `Vec` up to 1024, then
    `BTreeMap<VID, EID>`
    ([adjset.rs#L6-L24](https://github.com/Pometry/Raphtory/blob/db_v4/raphtory-core/src/entities/nodes/structure/adjset.rs#L6-L24)).
  - Timestamps: `TCell<A>`, sorted vec-map up to 128, then
    `BTreeMap<EventTime, A>` with `EventTime = (i64, usize)`
    ([tcell.rs#L10-L26](https://github.com/Pometry/Raphtory/blob/db_v4/raphtory-core/src/entities/properties/tcell.rs#L10-L26)).
- **Operations:** every edge event does a point lookup, inserts into both
  endpoints' neighbour sets, and inserts a timestamp
  ([segment.rs#L257-L330](https://github.com/Pometry/Raphtory/blob/db_v4/db4-storage/src/segments/node/segment.rs#L257-L330)).
  Windowed views use `range`; `last_before` is a predecessor lookup.
- **Evidence (thin):** [#2473](https://github.com/Pometry/Raphtory/pull/2473),
  "some attempts at perf improvement", force-inlines `Adj::get_edge`, with
  no numbers.
- **Fit:** the segment sits behind a `parking_lot` `RwLock` and is mutated
  through `&mut self`.
- **Pitch:** one bbtree replaces both tiers and the cutoff logic.

## Tier 2: fits, no evidence yet that it is hot

- **Substrate / Polkadot SDK `OverlayedChanges`.** Every runtime storage
  `get` and `set` goes through `BTreeMap<StorageKey(Vec<u8>),
  OverlayedEntry>`, with a `BTreeSet` of dirty keys per nested transaction
  ([changeset.rs#L187](https://github.com/paritytech/polkadot-sdk/blob/3de1a0ebc8/substrate/primitives/state-machine/src/overlayed_changes/changeset.rs#L187)).
  `next_key` is a successor scan; keys share 32-byte pallet prefixes;
  single-threaded `&mut`. No profiling issue found.
- **jj commit index.** The mutable segment holds `BTreeMap<CommitId,
  LocalCommitPosition>` and `BTreeMap<ChangeId, SmallVec>`, with point gets,
  neighbour lookups and prefix ranges
  ([mutable.rs L88–89, L651–672](https://github.com/jj-vcs/jj/blob/main/lib/src/default_index/mutable.rs)).
  20-byte hash keys; a full reindex covers the whole history. No profile.
- **ReadySet `PartialMap`.** A `BTreeMap` plus an interval tree
  ([lib.rs#L46](https://github.com/readysettech/readyset/blob/main/partial-map/src/lib.rs#L46))
  backs reader caches for range queries
  ([inner.rs#L41](https://github.com/readysettech/readyset/blob/main/reader-map/src/inner.rs#L41))
  and dataflow state keyed by `DfValue` tuples
  ([keyed_state.rs#L27](https://github.com/readysettech/readyset/blob/main/dataflow-state/src/keyed_state.rs#L27)).
  Left-right, so each copy takes `&mut`. Hash maps are the default; the
  B-tree serves range indexes only. Their flamegraph in
  [#1618](https://github.com/readysettech/readyset/issues/1618) blames a
  sorted `SmallVec`, not the map.
- **Cozo.** Semi-naive Datalog evaluation does `contains_key` and `insert`
  per derived tuple and `range` per join probe on `BTreeMap<Tuple, bool>`
  ([temp_store.rs#L27-L62](https://github.com/cozodb/cozo/blob/main/cozo-core/src/runtime/temp_store.rs#L27-L62));
  the memory store is `ShardedLock<BTreeMap<Vec<u8>, Vec<u8>>>`
  ([mem.rs#L41](https://github.com/cozodb/cozo/blob/main/cozo-core/src/storage/mem.rs#L41)).
  Inactive since 2024-12-04: a showcase benchmark, not a user.
- **Wasmtime DRC garbage collector free list.** `BTreeMap<u32, u32>`
  (block → length) and `BTreeMap<u32, BTreeSet<u32>>` (length → blocks);
  every free not next to the bump region does a predecessor and successor
  lookup
  ([free_list.rs L28–35, L316–366](https://github.com/bytecodealliance/wasmtime/blob/main/crates/wasmtime/src/runtime/vm/gc/enabled/free_list.rs)).
  [#12969](https://github.com/bytecodealliance/wasmtime/pull/12969) tried a
  cache-friendly `Vec` for 48% fewer instructions on
  [#11141](https://github.com/bytecodealliance/wasmtime/issues/11141)'s
  benchmark; review dropped it over quadratic worst cases ("with enough
  extra btrees we can make this work"), and
  [#12987](https://github.com/bytecodealliance/wasmtime/pull/12987) added
  size buckets. This is bbtree's pitch exactly, but the default collector
  is now the copying one, and Wasmtime needs fallible allocation.
- **Yrs (y-crdt).** Each client's blocks are a sorted `Vec<Block>`:
  `find_index` binary-searches on every ID lookup and `split_block` inserts
  into the middle in O(n)
  ([block_store.rs L17, L49–80, L457–468](https://github.com/y-crdt/y-crdt/blob/main/yrs/src/block_store.rs)).
  Sized by the document (10⁵–10⁶ blocks); no issue pins it.
