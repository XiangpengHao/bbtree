# 02 — Field study: how Rust code uses ordered maps, skip lists and hash maps

Goal: choose the key types, value types, sizes and operations that bbtree's
benchmark must cover, based on what real Rust code does rather than on
`u64 → u64`.

Data collected 2026-10-07 from crates.io (latest stable versions).

## Method

| Corpus | What | Why |
|---|---|---|
| Top 500 crates by all-time downloads | library code only (`tests/`, `benches/`, `examples/` skipped) | what the ecosystem's foundations use |
| 22 large applications and libraries | cargo, cranelift, wasmtime, datafusion, polars, tantivy, sqlparser, bevy_ecs, deno_core, solana-runtime, naga, gix, redb, sled, slatedb, lsm-tree, … | where map performance matters |
| 30 top dependents of `crossbeam-skiplist` | slatedb, lsm-tree (fjall), surrealkv, surrealmx, limbo, tonbo, TinyUFO, … | who uses an ordered concurrent map, and for what |
| Real data | 6 JSON documents across domains; top-500 crate names; 15k source paths; 467 `Cargo.lock` files | how big string keys and small maps actually are |

Every `BTreeMap<K, V>`, `BTreeSet<T>`, `HashMap`, `HashSet`, `IndexMap`,
`SkipMap` and `SkipSet` type in the source was extracted and its key and value
classified (integer, string, bytes, path, identifier, id newtype, struct, …).

**Caveats.**
- These are static mentions, not runtime frequency or runtime size.
- The application sample is hand-picked, so it over-weights databases: slatedb alone contributes most of the `Bytes` keys.
- Counts below give both mentions and the number of distinct crates; the crate count is the more robust number.

## 1. How much each structure is used (top 500 crates)

| | crates using it | mentions |
|---|---|---|
| `BTreeMap` / `BTreeSet` | 78 (16%) | 963 |
| `HashMap` / `HashSet` | 138 (28%) | 3,324 |
| `IndexMap` / `IndexSet` | 17 | — |

- Only **4 of the 78** BTree-using crates call an ordered-only method (`range`,
  `first_key_value`, `last_key_value`, `pop_first`, `pop_last`, …). Most
  users want sorted, deterministic iteration, not ordered queries.
- In the applications, the heavy ordered-API users all key by integers:
  - `regex-automata`: 23 range calls, `StateID` keys;
  - `redb`: 16, `TransactionId`/`PageNumber`;
  - `solana-runtime`: `Slot`;
  - `sled`: `Lsn`/`PageId`.
- `BTreeSet` is **41%** of BTree mentions; `HashSet` is 38% of hash mentions.

## 2. `BTreeMap` / `BTreeSet`: key and value types

Concrete key types (660 mentions in 59 crates; generic `K`/`T` excluded):

| key category | share of mentions | crates | most common |
|---|---|---|---|
| string | 26% | **24** | `String` (123), `InternedString`, `&str`, `PackageName` |
| other struct/enum | 21% | 23 | `FeatureValue`, `WrapSchema`, … |
| id/handle newtype (over an integer) | 15% | 13 | `PackageId`, `StateID`, `TransactionId`, `Ulid`, `Slot` |
| bytes | 12% | 7 | `Bytes` (65, mostly slatedb) |
| identifier/symbol (proc macros) | 10% | 7 | `Ident`, `Lifetime`, `Name` |
| integer | 7% | **17** | `u64`, `usize`, `i32`, `u32`, `u8` |
| path | 7% | 4 | `PathBuf` (43, cargo and foundry) |
| tuple | 2% | 8 | |

Values (355 concrete): struct/enum 42%, `Vec` 12%, integer 12%, bytes 6%,
id newtype 6%, `String` 6%, `Option` 5%, nested collection 5%.

How it's used, by group:
1. **Deterministic output.** cargo (`String`/`PackageId`/`PathBuf` keys,
   274 mentions), foundry (`PathBuf`), serde_with, schemars and bindgen. Maps
   are small to medium, iterated in order, rarely hot.
2. **Data-model types.** `serde_json::Map` and `toml::Map` are
   `BTreeMap<String, Value>` unless `preserve_order` is enabled, so every
   parsed JSON or TOML object is a small `BTreeMap`. This is the biggest
   deployment by far; serde_json has 1.4B downloads.
3. **Proc macros.** `BTreeSet<Ident>`, `BTreeSet<Lifetime>`: tiny sets kept
   for deterministic code generation.
4. **Real ordered workloads with integer keys**, where speed matters:
   automata states, transaction and page IDs, slots, log sequence numbers,
   timers, and `rangemap` (40M downloads, built on `BTreeMap`).

## 3. `HashMap` / `HashSet`, for comparison

Concrete keys (1,654 mentions in 96 crates):

| key category | share | crates | most common |
|---|---|---|---|
| string | 22% | **54** | `String` (257), `&str` (48), `InternedString`, `Atom` |
| other struct/enum | 28% | 39 | |
| id/handle newtype | 22% | 29 | `Pubkey` (94, 32-byte key), `PackageId`, `ItemId`, `SsTableId` |
| integer | 12% | 30 | `usize`, `i32`, `u32`, `u64` |
| path | 5% | 6 | `PathBuf` |
| tuple / bytes / `TypeId` | 3% / 3% / 1% | 12 / 7 / 6 | |

Values: struct/enum 34%, integer 12%, `Vec` 11%, `String` 11%, id newtype 10%.

The key mix is close to `BTreeMap`'s: strings, integers and integer-like IDs
dominate both. A hash-map comparison can use the same key types.

## 4. Skip lists (`crossbeam-skiplist`, 15.4M downloads, 102 dependents)

| use | crates | key → value |
|---|---|---|
| LSM memtable | slatedb, lsm-tree (fjall), bitkv-rs, surrealmx, tonbo | `SequencedKey { Bytes, u64 }` → `RowEntry`; `InternalKey { UserKey (bytes), SeqNo, ValueType }` → `UserValue` (bytes); `Vec<u8>` → log position; `Bytes` → versions; `Timestamped<Key>` → `Option<Record>` |
| MVCC / transaction tables | limbo_core, oxisqlite-core, memodb, surrealmx, skipdb-core, libsql-wal, surrealkv | `RowID`/`TxID`/`u64`/`u32` → `RwLock<Vec<RowVersion>>`, `Arc<Commit>`, … |
| caches | TinyUFO (Pingora), breve | `Key` (a `u64` hash) → bucket |
| networking and routing | memberlist-quic, nodecraft, torrust-tracker, p2p_channel, mpchash | `SocketAddr`, domain, `InfoHash` (20 bytes), ring position (`u64`) |
| misc | async-signals, open-coroutine, godrays | `c_int`, `c_longlong`, `String` |

So skip lists are used where a **concurrent** ordered map is needed. Keys are
either byte strings with a sequence number (memtables) or integers (IDs,
hashes, positions), and values are often behind a lock or an `Arc`.

## 5. Sizes

### String keys

| source | median | mean | p90 | p99 | max |
|---|---|---|---|---|---|
| JSON object keys (19k objects, 6 documents) | 7 B | 8.5 B | 14 B | — | 34 B |
| crate names (top 500) | 10 B | 10.0 B | 16 B | 23 B | 28 B |
| `name version` (PackageId-like) | 16 B | 16.8 B | 23 B | 30 B | 34 B |
| source path, relative to crate | 22 B | 25.0 B | 42 B | 57 B | 102 B |
| source path, absolute (cargo registry) | 103 B | 105.4 B | 122 B | 139 B | 181 B |

Most string keys are short, 8–16 bytes, so their heap buffers are tiny and the
24-byte `String` header is a large part of the key. Paths are the long-key case,
about 100 bytes.

JSON key length per document (median / p90):
- twitter: 11 / 23
- citm_catalog: 8 / 14
- crates.io API: 10 / 17
- npm registry: 6 / 12
- GitHub events: 5 / 13
- recipes: 5 / 9

### Map sizes

| source | median | p90 | p99 | max |
|---|---|---|---|---|
| JSON objects: keys per object (each is one `serde_json::Map`) | **2** | 6 | 31 | 2,651 |
| `Cargo.lock` packages (cargo's per-package maps) | 30 | 136 | 436 | 671 |

- **92%** of JSON objects have ≤ 8 keys. The typical `BTreeMap` in the wild
  is tiny, and there are many of them.
- **Memtables are large.** SlateDB defaults to 64 MiB L0 SSTs
  (`l0_sst_size_bytes`, the memtable flush size) and up to 512 MiB unflushed.
  With ~16 B keys and ~100 B values, a 64 MiB memtable holds roughly 500k
  entries.

### Key/value record sizes used by storage benchmarks

| benchmark | key | value |
|---|---|---|
| RocksDB `db_bench` defaults | 16 B | 100 B |
| YCSB core workloads | ~23 B (`user` + number) | 1 KB (10 fields × 100 B) |
| bf-tree's design point | small records, ~100 B total (secondary indexes) | |

## 6. Benchmark matrix this implies

The default allocator is **mimalloc**.

**Key types**

| key | stands for |
|---|---|
| `u64`, random and sequential | integer IDs, slots, page numbers, sequence numbers |
| `u32` | small IDs (`StateID`, `RowID`, `u32` indices) |
| `String`, lengths from the JSON-key distribution (median 7, p90 14) | serde_json / toml objects, names |
| `String`, ~16 B | crate names, PackageId-like keys |
| `String` / `PathBuf`, ~100 B | path keys (cargo, foundry) |
| `[u8; 32]` | hashes and public keys (`Pubkey`, `InfoHash` is 20 B) |
| `(Bytes, u64)` with 16 B and 24 B user keys | LSM memtable keys (SlateDB, fjall) |

**Value types**

| value | stands for |
|---|---|
| `()` | sets: 41% of BTree mentions |
| `u64` | counters, IDs |
| 32 B struct | `serde_json::Value`-sized enums, small records |
| `String` | string-to-string maps |
| 100 B and 1 KB bytes | KV records (db_bench, YCSB) |

**Sizes**

| size | entries | stands for |
|---|---|---|
| tiny | 1–32 | JSON / TOML objects |
| small | 100–1k | lockfile-sized maps |
| medium | 100k | |
| large | 10M+ | DRAM-resident: memtables, indexes |

For tiny maps, measure *many* maps (e.g. 1M maps of 4 entries), because the
cost that matters is per-map memory and construction.

**Operations**
- insert (random and sequential);
- get, hit and miss;
- full iteration;
- short range scans (64 entries);
- `entry`;
- `remove`;
- `pop_first` (timer and queue use);
- bulk build (`FromIterator`);
- for tiny maps: build, then iterate (the serde pattern).

**Concurrent memtable workload, later.** `(Bytes, u64)` keys, ~100 B values,
concurrent inserts mixed with ordered scans, compared against
`crossbeam-skiplist`.

**Baselines:** std `BTreeMap`, `hashbrown` / std `HashMap`, and for the
concurrent workload `crossbeam-skiplist`.

## Open questions this study can't answer statically

- Runtime map-size distribution in real programs. Measuring it would mean
  instrumenting std's `BTreeMap` (or swapping in a counting wrapper) during,
  for example, a `cargo build` and a serde_json-heavy service.
- How often maps are iterated compared with point-queried at runtime.
