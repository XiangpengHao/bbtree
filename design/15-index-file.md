# 15 — File maps: a map in a file

Status: built (2026-10-19), D58: fixed-size keys and values in the
leaves, and keys and values of any length in each leaf's tail.

- **bbtree**, behind the `file` feature: the file map, its header and its
  check in `bbtree/src/tree/file.rs`; the API (`FileWriter`, `FileMap`,
  `Storable`, `PlainBytes`, `FileBuf`) in `bbtree/src/file.rs`; var-length keys
  and values (`VarBytes`, `VarBytesBuf`) in `bbtree/src/file/var_bytes.rs`.
- **bbtree-disk** (`bbtree-disk/`), generic: `BBTreeMapFile`, a typed map in a
  file, each key and value type keeping its own storage (`DiskType`);
  written, mapped or read, whole or as a range of a larger file.
- **bbtree-datafusion-parquet** (`bbtree-datafusion-parquet/`), bbtree-disk as
  indexes of Parquet files (`ParquetIndexBuilder`), which DataFusion's
  Parquet scans look their predicates' rows up in (`with_parquet_index`). The
  DataFusion, Arrow and Parquet dependencies live here alone.

Terms: a **file map** is a map compacted into one byte string, which a
reader uses where it lies; a **file** alone is the operating system's. A
key or value is **plain** (`PlainBytes`), kept in its slot, or
**var-length** (`VarBytes`), kept in the **tail** of its leaf, the bytes
after it, its slot holding a **span**, `{len, offset}`. **OS pages** are
the operating system's, 4 KiB or 2 MiB, never a node's page.

## Problem

An index over a Parquet file (99-brainstorm) is written once and used
many times, by processes that start cold. It has to live in a file, in the
Parquet file itself where possible, as pq-vector keeps its index, and be
usable the moment the file is opened:

- **Opening costs nothing that grows with the index.** Rebuilding a map
  from its entries costs a pass over all of them: 2 ns an entry at best
  (D25), 0.2 s for 100M, after reading 1.6 GB.
- **Lookups take a map's time.** The point of bbtree is its search; an
  index that searches slower is a different structure.
- **The file is compact**: nodes full, nothing but the map.

## Design

A file map is the unsync tree's own pages, compacted and laid end to end,
with every pointer replaced by an offset. A reader maps the file and runs
the map's code on the pages where they lie.

```text
offset 0      header: magic, version, layout fingerprint, entries, root
              root page            (root node and root area, D50, D52)
              internal nodes       level 1, left to right
              internal nodes       level 2 ...
              leaves               in key order, each followed by its tail
```

- **The tree's own pages.** Each page is copied byte for byte
  (`NodePtr::copy_page_to`), so a file map has the map's layouts, inline
  keys, hash tags and prefixes, and its lookups run the map's searches.
  Nothing about the search is reimplemented, so nothing about it can lag.
- **Compact.** `FileWriter` builds a fresh tree from the entries in key
  order (`TreeBuilder`, D25): every node full but those along the right
  border. A map of random inserts, two-thirds full, compacts the same as
  one built in order: a file map depends on the entries alone (tested).
- **Level order.** The root page, then each level of internal nodes, then
  the leaves in key order. The levels a lookup crosses most lie together at
  the front, a few MB for 100M entries; a range scan reads leaves front to
  back. Each page starts at a multiple of its alignment, so a file map aligned
  to the largest (1 KiB for most key types) has every page aligned.
- **Edges are offsets from their node.** An internal node's edge holds its
  child's offset from the node's own page, tagged with the child's kind as
  a pointer is (D52). Following one adds it to the node's address, which
  the descent holds already. A base address would have to travel with
  every node handle, or live in a global. Children lie after their parents,
  so every offset is positive. A file map is position-independent: it can sit
  at any aligned offset of a file, and be mapped anywhere, unchanged.
- **The strategy says so.** A tree in a file map has strategy `InFile<S>`,
  `S`'s inline keys with `IN_FILE` set, as `Latched<S>` sets `LATCHED`
  (D54). `Internal::edge_unchecked`, the one place an edge is read, branches
  on that constant. Maps never see the branch: their machine code is
  unchanged (Measurements).
- **Keys and values are storable** (`Storable`): a `PlainBytes` type,
  kept in its slot as it is, or `VarBytes`, bytes of any length kept in
  the leaf's tail ("Keys and values of any length"). A plain type has no
  pointers and no padding, and every bit pattern is a value: the integers,
  floats, `()` and arrays of them. Either way the file map holds the whole
  map, and a reader may take whatever bytes it finds.
- **Every byte is written.** Pages come zeroed from the allocator, and
  keys and values have no padding; the one byte left uninitialized
  is the node header's padding after `has_head`, which the copy writes as
  0. Making it a field instead cost the map's insert split its inlining
  (`insert_splitting`, 910 → 1,089 instructions), so the copy does it.
- **The header** holds the magic, the format version, the layout's
  fingerprint, the number of entries, the root's tagged offset and the
  file map's length. The fingerprint hashes every size, offset and choice the
  node code reads pages with, the key and value types' sizes, pointer
  width and byte order: a file map of another layout is refused, never
  misread. The version covers what the fingerprint can't: what an edge
  holds, how inline keys encode keys. A file map is a cache of data that can
  be indexed again; it doesn't outlive a layout change.

### Reading a file map

- **`FileMap::open`** checks every page the tree reaches: in the file map,
  aligned for its kind, no more keys than its kind holds, its head flag a
  `bool`; every edge leading past every page reached before it, so that
  the walk visits each page once and ends; leaves at one depth, within a
  node stack's; the shared prefix within its area; and the leaves' entries
  the header's count. Keys, values, inline keys and prefix lengths may hold
  anything: they steer searches, never out of a node (`tree/node`,
  invariants 4 and 5). Any bytes that pass give answers, maybe wrong, or a
  panic, never undefined behavior. It reads the whole file map once.
- **`FileMap::open_unchecked`** checks the header alone, for file maps the
  caller trusts: a file mapped from disk, which must stay unchanged while
  mapped anyway. It reads the first page and nothing else.
- **OS pages** (`OsPages`). A mapping in the system's small pages
  reads a few KiB from disk for each page a cold lookup reaches; once in
  memory, its lookups miss the TLB more than a map's and take 6% longer.
  Advised huge pages (`MADV_HUGEPAGE`, Linux) come in 2 MiB folios read
  through the mapping, mapped whole: a hot file map looks up at a map's speed,
  but each cold page costs 2 MiB of reading. Pages already in the page
  cache, as those of a file just written, keep their size.
- **Batches.** `FileMap::get_many` runs the map's lanes (D53), above the
  map's threshold: a lane follows an edge through `edge_unchecked`, as
  `get` does. Lanes overlap waits for memory, not for the disk: a
  prefetch of a page not yet mapped does nothing, and the lane's read of
  it faults, as `get`'s does.

## Keys and values of any length

`VarBytes`: each leaf keeps an 8-byte span for each, and their bytes lie in
the leaf's tail, right after the leaf.

```text
leaf      | header | inline keys | hash tags | (key span, value span) … |
tail      | key 0's bytes, value 0's bytes, key 1's bytes, value 1's bytes, … |
          zeros to the next leaf's alignment
leaf      | …
```

- **Spans** are `{len: u32, offset: u32}`, aligned to 8: a key or value of
  at most 4 GiB.
- **Offsets count from the span's own slot.** Edges count from their node
  because node code follows them: `edge_unchecked` holds the node's
  address. Keys are followed by `K`'s own `Ord` and `InlineKey`, which
  bbtree calls with `&K` alone: in the leaf search, `cmp_from`, a
  separator tie's walk to its head key (D49), the walks. From the slot,
  `&K` is all a key needs; from the node, each of those calls would need
  the page passed in, through the search's hot path. A span's bytes lie
  outside the span itself, which a reference may reach under Tree
  Borrows, which Miri checks, and not under Stacked Borrows.
- **Each leaf's tail lies right after it**, entry by entry in key order,
  the head's first, each key's bytes beside its value's. A hit reads the
  leaf and one line of its tail; a cold lookup reads one place of the
  file; a range scan reads it front to back. The cost is the zeros to the
  next leaf's alignment, half a KiB a leaf on average.
- **Building.** `TreeBuilder` builds the compact tree in memory over
  record pointers (`RecordPtr`, which stores share, D60): 8-byte pointers
  to length-prefixed bytes in an arena of the writer's (`ByteArena`),
  which read their bytes alone, as inline keys and prefixes need, and
  stay valid as entries move. They have the span's
  size, alignment and inline keys, so the same layout (checked as the
  writer starts): pages still copy byte for byte, and the writer rewrites
  each key and value slot, the head's included, into a span, as it
  rewrites edges, writing the bytes into the leaf's tail. Slots past a
  leaf's entries become zeros, not stale pointers.
- **Strategy: `Auto`**, which takes `VarBytes` for a byte string, as it
  takes `String`, by its type (D18): its inline keys are windows of its
  bytes, its hash tag its length, as a heap string's, and a `&[u8]` looks
  it up, compared in place, as a `&str` does `String` keys. Its writer's
  `RecordPtr` is recognized alike, so the writer's pages have the
  reader's inline keys. `WithInlineKey` works too, through `VarBytes`'s
  `InlineKey` impl, which reads its bytes as `String`'s does. Var-length
  keys need inline keys: without them internal nodes would keep keys,
  which no tail backs. Both strategies give them, and the fingerprint
  tells their file maps apart, and plain from var-length slots of one size.
- **Queries under `WithInlineKey` are spans** (`VarBytesBuf`): a lookup
  uses inline keys only when it looks up with `K` itself. A query
  buffer holds a span and the bytes after it, keys of up to 64 bytes in
  place. Written right before the lookup, it must not move: a moved
  buffer is read back by wide loads that can't take their data from the
  narrow stores that wrote it, and wait for them to retire, and with them
  for the previous lookup's cache misses. Lookups then no longer overlap:
  var-length `u64` keys, a buffer a lookup, took 347 ns a hit so, 264 ns
  written in place (`VarBytesBuf::empty`, then `set`).
- **Spans aren't `Copy`.** A copy would follow its offset from wherever it
  was copied to. Their fields are private, and nothing hands one out by
  value. `Storable`, sealed, takes `PlainBytes` and `VarBytes` alone.
- **Checking.** `FileMap::open` also checks that the bytes of each live
  span lie in the file map: it reads every leaf, and no tail.
- **Fixed-size keys stay plain.** Var-length, a `u64` hit reads one more
  line, the key's bytes, and the file map takes 49 bytes an entry against
  18.5: 216 ns against 127 a hit at 10M (Measurements).

## Typed maps in files (bbtree-disk)

- **`BBTreeMapFile<K, V>`** is a map in a file: written once from a map's
  entries, in their order; mapped or read, whole or from a range of a
  larger file (`write_at`, `read_range`), as the Parquet index is; looked
  up with a borrowed form of the key, as a map is, one key or a batch
  (`get_many`). Lookups hand back views of the file. Mapping is a builder,
  `mmap(path)`, whose OS pages and range are optional: small pages and the
  whole file unless set.
- **Each type picks its storage** (`DiskType`): its stored type, plain or
  `VarBytes`; its keys' strategy; and its view. Integers, floats and byte
  arrays are plain, in their slots, under `Auto`, so a `u64` map in a file
  is the plain file map, at a map's speed. `String` and `Vec<u8>` are
  `VarBytes`, under `Auto` too, and come back as `&str` and `&[u8]`.
  `plain_disk_type!` makes a caller's `PlainBytes` type one, under `Auto`
  or, with its own `InlineKey` impl, `WithInlineKey`. Rust's coherence rules
  forbid a blanket impl over `PlainBytes` beside `String`'s, so each plain
  type opts in.
- **Why not one layout of bytes for every type:** integers as `VarBytes`
  read the tail on every hit and take 2.6 times the bytes (Measurements).
- **Queries** (`DiskQuery`) are the key itself for a plain type, and its
  bytes for text and bytes, `&[u8]`: neither is copied.
- **Inputs** (`DiskInput`): what the writer takes, owned or borrowed:
  `&map` of a map, `&mut map` of the shared map, by its owner,
  `range_cloned(..)` of one threads still change, and `Cow` bytes, which
  pass to the writer without a copy.

## Parquet indexes (bbtree-datafusion-parquet)

- **A file's indexes** (`ParquetIndexBuilder`): any number, each of one
  or more columns, in order.
  `ParquetIndexBuilder::source(path).index(&["UserID"]).index(&["CounterID", "EventDate"]).build()`
  checks every index's columns before it touches the file; then, index by
  index, reads the columns in file order, by the footer it read first,
  adds each row to its key's rows in an unsync `BBTreeMap`, and writes the
  map as a file map after the file's data, past the last row group or the
  index before it; then writes the footer once, after them. It returns
  what it built of each (`ParquetIndexStats`): rows, distinct keys, bytes,
  time. The footer's key-value entry `bbtree.indexes` lists every index,
  as JSON: its schema, each column's name and the kind of its values, and
  the file map's byte range. Data pages,
  page indexes and bloom filters keep their offsets (tested), and other
  readers read the file as before. If writing fails, the old footer is put
  back; a crash meanwhile can leave the file without one.
- **One entry per distinct key**: its value the rows that hold it
  (`RowIds`, a disk type of the crate's own), in order, each a `u32`,
  little-endian, in the leaf's tail; files of more rows are refused. Read
  in file order, each key's rows come out in order: nothing is sorted. A
  key of many rows takes little more than its rows; a lookup is one
  search, and the rows lie together. Rows whose first column is null are
  left out: no lookup asks for them.
- **Keys** (`index_key.rs`, `KeyForm`), in the file map's own key type
  where one column allows it:
  - one column of numbers: `u64`, signed numbers with the sign bit
    flipped, floats flipped into IEEE 754's total order, as Arrow compares
    them;
  - one column of text or byte strings: `Vec<u8>`, the bytes;
  - several columns: bytes, as the columns are chosen at run time and no
    Rust type holds any mix of them. Each column's part is a tag byte, 0
    for null and 1 for a value, then the value: a number as its `u64`,
    big-endian; bytes with each 0 byte written `0 0xFF`, and `0 0` after
    them. No part's bytes start another's, so the keys of the rows with
    given values in the first columns, and a range of values in the next,
    are one range of keys. Nulls of later columns stay, ordered first:
    `a = 1` on an index of `(a, b)` finds the rows whose `b` is null.
- **Kinds of values** (`KeyType`): integers, unsigned integers, floats,
  bytes (text and binary of every Arrow type), `Date32`, `Date64`, and
  times and timestamps by unit. Values of one kind encode alike whatever
  their exact types: an index of an `Int32` column serves a table that
  reads it as `Int64`, one of `Utf8` a table of `Utf8View`.
- **Scans** (`with_parquet_index`, on a `SessionStateBuilder`, as
  pq-vector's `with_pq_vector`): a physical optimizer rule, after
  DataFusion's own, so after filter pushdown, wraps the source of each
  Parquet scan with a predicate. As a file is opened, in the morsel API's
  I/O step, its footer is read through the scan's reader factory, whose
  metadata cache the scan then reads it from. The indexes that apply look
  rows up; the rows all of them found become the file's
  `ParquetRowSelection`, DataFusion's input for external indexes: it reads
  their row groups alone, and within them their rows, after its own
  pruning by statistics, page indexes and bloom filters. A file split into
  ranges that several partitions read is looked up once.
- **An index applies** when its columns resolve, by name, in the scan's
  schema, as DataFusion resolves a file's columns, to columns of the kinds
  it indexed, and the predicate constrains its first column: comparisons
  to literals (`=`, `<`, `<=`, `>`, `>=`), `IN` lists, and `OR`s and
  `AND`s of those on one column. Values on its leading columns, then a
  range on the next, make key ranges: `a = 1 AND b IN (2, 3) AND c > 4` is
  two. The scan still filters what it reads, so the rows need only hold
  every row the predicate keeps; what the index can't read of it, a cast
  column, another column, a function, it leaves to the filter.
- **Why in the scan:** every Parquet table gets it, `register_parquet`,
  listing tables, tables of files with and without indexes, with no table
  provider of its own. The rule itself can't look rows up: rules are
  synchronous, and would read every file's footer while planning, even
  files a `LIMIT` never opens. An operator over the scan, as pq-vector's,
  would look every file up before the first row, and hide the scan from
  later rules.
- **Trusted indexes:** an index is mapped from its file and opened
  unchecked, its header alone. The scans trust that indexed files hold
  what the builder wrote, and don't change while read: `with_parquet_index`
  is safe to call, and says so. Checking an index on each query reads it
  whole: 1.2 GB for `UserID` of `hits.parquet`.

## Measurements

9900X, Linux 7.2, mimalloc, one run each, pinned to one core.

**Maps don't change.** The probes of `get` (`u64`, `i64`, `u128`,
`[u8; 32]`, `String`), `range`, `iter`, `insert`, `remove`, the shared
map's `get_copied` and `collect` compile to the same machine code as
before, instruction for instruction, with and without the `file` feature.
A file map's `get` differs from a map's in its descent loop alone:
`add rbx, [rbx + rdx*8 + 0x70]` where the map has
`mov rbx, [r13 + r12*8 + 0x70]`.

**Lookups at 10M random `u64` keys and values** (`bbtree-disk/examples/lookups.rs`,
and `huge_pages.rs` for the mapped rows, mapped from disk; the file map
184.7 MB, 18.47 bytes per entry, written in 60 ms):

| | get hit | get miss |
|---|--:|--:|
| map, bulk built | 125.4 ns | 90.4 ns |
| file map, read into memory | 125.9 ns | 94.3 ns |
| file map, mapped, small pages | 133.9 ns | 96.8 ns |
| file map, mapped, huge pages | 126.4 ns | 91.2 ns |

**Lookups of var-length keys and values at 10M entries**
(`bbtree-disk/examples/var_bytes_lookups.rs`; read into memory; queries
written in place). One run, pinned, while the machine ran about 1.6 times
slower than for the table above (its map took 204 ns a hit, against
125.4): compare rows within this table only.

| | get hit | get miss | bytes an entry |
|---|--:|--:|--:|
| `u64`: map | 203.9 ns | 138.7 ns | |
| `u64`: `BBTreeMapFile<u64, u64>`, in slots | 204.9 ns | 139.9 ns | 18.5 |
| `u64`: file map, var-length, `&[u8]` queries | 326.9 ns | 225.1 ns | 49.1 |
| text, 13 to 45 bytes: map of `String` | 352.6 ns | 347.2 ns | |
| text: `BBTreeMapFile<String, u64>` | 320.7 ns | 317.2 ns | 73.4 |

Text keys under `Auto`, compared in place, took 316.9 ns a hit against
333.8 ns copied into a `VarBytesBuf` under `WithInlineKey`, in two runs of
that day.

The text map's `u64` values in their slots take no fewer bytes than in
the tail did: each leaf's tail still rounds up to the same 1 KiB step. At
1M keys of about 21 bytes, they take 49.1 bytes an entry against 73.4.

**ClickBench `hits_99_zstd.parquet`, 997,497 rows, 109 MB**
(`bbtree-datafusion-parquet/examples/parquet_lookups.rs`; one run each, not
pinned; `SELECT *`, index against scan, each in a session of its own):

- `UserID`, 214,397 keys: index 9.6 MB, built in 55 ms. The 30 rows of
  one value in 12 ms, against 116 ms scanning.
- `URL`, 289,848 keys: 62.6 MB, 259 ms. `EventDate`, 11 keys: 4.0 MB,
  24 ms.
- `(CounterID, EventDate)`, 708 keys: 4.0 MB, 45 ms. The 29,183 rows of
  one `CounterID` and day in 19 ms, against 24 ms; the 131,072 of one
  `CounterID` in 65 ms, against 70 ms.

**ClickBench `hits.parquet`, 99,997,497 rows, 14.8 GB**, sorted by
`CounterID` and `EventDate`, warm in the page cache (same example; one run
each, not pinned; DataFusion's defaults, which don't push filters into the
Parquet decoder):

- `UserID`, 17.6M keys: index 879 MB, built in 10.8 s, 5.0 GB resident at
  most. `SELECT *` of one value's rows (35 to 142): 23–85 ms with the
  index, against 6.0–6.3 s scanning, 74–263 times faster. The scan reads
  every row group: statistics can't prune a column the file isn't sorted
  by.
- `(CounterID, EventDate)`, 22,406 keys: 401 MB, 5.5 s. 2,072 rows of one
  counter and day: 38 ms, against 244 ms; of one counter: 29 ms, against
  200 ms; 738,172 rows of `CounterID = 62` in July: 204 ms, against
  353 ms. The scan prunes row groups by statistics already.

Not yet measured: cold lookups.

## Limits and open problems

- **Read only.** Changing a file map would need its pages to go back to an
  allocator that doesn't own them; an index of new data is a new file map.
- **Local files only:** indexes are mapped from their files; files of
  other object stores are scanned as before. Reading an index's pages
  through the object store as lookups reach them would lift it.
- **Every index that applies is used:** a wide range on a column the file
  isn't sorted by collects and sorts its rows, which lie in every row
  group. No cost model chooses among indexes, or the scan, yet.
- **Static predicates only:** dynamic filters, of a join's build side or a
  TopK, are left to the scan's filter. Looking them up would make index
  joins.
