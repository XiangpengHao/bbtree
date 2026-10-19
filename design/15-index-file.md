# 15 — File maps: a map in a file

Status: built (2026-10-19), D58: fixed-size keys and values in the
leaves, and keys and values of any length in each leaf's tail.

- **bbtree**, behind the `file` feature: the file map, its header and its
  check in `bbtree/src/tree/file.rs`; the API (`FileWriter`, `FileMap`,
  `Storable`, `PlainBytes`, `FileBuf`) in `bbtree/src/file.rs`; var-length keys
  and values (`VarBytes`, `VarBytesBuf`) in `bbtree/src/file/var_bytes.rs`.
- **bbtree-disk** (`bbtree-disk/`), generic: `MappedFile`, a file map in a file,
  mapped or read; `IndexFile`, a typed map of any length, through
  `ToBytes`/`FromBytes`.
- **bbtree-datafusion-parquet** (`bbtree-datafusion-parquet/`), bbtree-disk as an
  index of Parquet files: `ParquetIndex`. The Arrow and Parquet
  dependencies live here alone; DataFusion's, when its integration lands.

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
  `ArenaBytes`: 8-byte pointers to length-prefixed bytes in an arena of
  the writer's (`ByteArena`), which read their bytes alone, as inline keys
  and prefixes need, and stay valid as entries move. They have the span's
  size, alignment and inline keys, so the same layout (checked as the
  writer starts): pages still copy byte for byte, and the writer rewrites
  each key and value slot, the head's included, into a span, as it
  rewrites edges, writing the bytes into the leaf's tail. Slots past a
  leaf's entries become zeros, not stale pointers.
- **Strategy: `WithInlineKey`.** The span's `InlineKey` reads its bytes as
  `String`'s does: 8-byte windows, its length as its hash tag,
  comparisons from where keys differ and against a prefix. Var-length keys
  need inline keys: without them internal nodes would keep keys, which no
  tail backs. A writer refuses to write such a file map, and a reader to
  read one; the fingerprint keeps plain and var-length slots of one size
  apart.
- **Queries are spans too** (`VarBytesBuf`): with `WithInlineKey`, a
  lookup uses inline keys only when it looks up with `K` itself. A query
  buffer holds a span and the bytes after it, keys of up to 64 bytes in
  place. Written right before the lookup, it must not move: a moved
  buffer is read back by wide loads that can't take their data from the
  narrow stores that wrote it, and wait for them to retire, and with them
  for the previous lookup's cache misses. Lookups then no longer overlap:
  `IndexFile<u64, u64>` took 347 ns a hit so, 264 ns written in place
  (`VarBytesBuf::empty`, then `set`).
- **Spans aren't `Copy`.** A copy would follow its offset from wherever it
  was copied to. Their fields are private, and nothing hands one out by
  value. `Storable`, sealed, takes `PlainBytes` and `VarBytes` alone.
- **Checking.** `FileMap::open` also checks that the bytes of each live
  span lie in the file map: it reads every leaf, and no tail.
- **Fixed-size keys stay plain.** Var-length, a `u64` hit reads one more
  line, the key's bytes, and the file map takes 49 bytes an entry against
  18.5: 216 ns against 127 a hit at 10M (Measurements).

## Typed index files (bbtree-disk)

- **`ToBytes`** writes a key or value as bytes. A key's bytes order as the
  key does, since the tree orders keys by their bytes: text as its UTF-8,
  integers big-endian, signed ones with the sign bit flipped.
- **`FromBytes`** reads them back, as a view of the file's bytes where it
  can: `&str` for `String`, `&[u8]` for `Vec<u8>`, the number for an
  integer.
- **`IndexFile<K, V>`** is a map of var-length keys and values in a file:
  written from a map's entries, in their order; mapped or read; looked up
  with a borrowed form of the key, as a map is, one key or a batch
  (`get_many`), each query written in place in a buffer of its own.

## The Parquet index (bbtree-datafusion-parquet)

- **One entry per distinct value**: its key is the value's bytes, its
  value the rows that hold it, in order, each 4 bytes little-endian, or 8
  in a file of 2^32 rows or more. A column of few values takes little more
  than its rows; a lookup is one search, and the rows lie together.
- **Encodings** (`KeyEncoding`) order like the values, and are exact:
  signed integers, dates and timestamps as 64 bits, big-endian, the sign
  bit flipped; unsigned integers as 64 bits, big-endian; floats as their
  bits, flipped to order as numbers; byte strings and text as their bytes.
- **Embedding** (`ParquetIndex::embed`) appends the file map after the last
  row group, at a multiple of its alignment, and writes the footer again
  after it with one more key-value entry, `bbtree.index.<column>`, holding
  the file map's byte range, the encoding and a row's width. Data pages, page
  indexes and bloom filters keep their offsets (tested), and other readers
  read the file as before. If writing fails, the old footer is put back; a
  crash meanwhile can leave the file without one.
- **Reading rows** (`ParquetIndex::access`): the row groups that hold the
  rows found, and a selection within them, applied to an Arrow reader
  builder in one call. Row groups without a match are never opened.

## Measurements

9900X, Linux 7.2, mimalloc, one run each, pinned to one core.

**Maps don't change.** The probes of `get` (`u64`, `i64`, `u128`,
`[u8; 32]`, `String`), `range`, `iter`, `insert`, `remove`, the shared
map's `get_copied` and `collect` compile to the same machine code as
before, instruction for instruction, with and without the `file` feature.
A file map's `get` differs from a map's in its descent loop alone:
`add rbx, [rbx + rdx*8 + 0x70]` where the map has
`mov rbx, [r13 + r12*8 + 0x70]`.

**Lookups at 10M random `u64` keys and values** (`bbtree-disk/examples/lookups.rs`;
the file map 184.7 MB, 18.47 bytes per entry, written in 60 ms):

| | get hit | get miss |
|---|--:|--:|
| map, bulk built | 125.4 ns | 90.4 ns |
| file map, read into memory | 125.9 ns | 94.3 ns |
| file map, mapped, small pages | 133.9 ns | 96.8 ns |
| file map, mapped, huge pages | 126.4 ns | 91.2 ns |

**Lookups of var-length keys and values at 10M entries**
(`bbtree-disk/examples/var_bytes_lookups.rs`; file maps read into memory, in huge
pages; queries written in place):

| | get hit | get miss | bytes an entry |
|---|--:|--:|--:|
| `u64`: map | 125.6 ns | 91.2 ns | |
| `u64`: file map, plain | 127.1 ns | 92.4 ns | 18.5 |
| `u64`: file map, var-length, one query buffer | 216.5 ns | 125.0 ns | 49.1 |
| `u64`: `IndexFile<u64, u64>` | 269.6 ns | 126.6 ns | 49.1 |
| text, 13 to 45 bytes: map of `String` | 311.0 ns | 298.0 ns | |
| text: `IndexFile<String, u64>` | 342.8 ns | 325.8 ns | 73.4 |

**ClickBench `hits.parquet`, 100M rows, 14.8 GB**, an index of `UserID`
(`bbtree-datafusion-parquet/examples/parquet_lookups.rs`):

- Embedding: 5.6 s (read the column, sort, compact, write); the index is
  1.20 GB, 12.0 bytes a row, for 17.6M distinct values. Opening it,
  mapped: 3–17 ms, mostly the Parquet footer.
- Rows of one value, warm: 7–16 µs; reading the column to find them:
  272–275 ms.
- Cold, the file dropped from the page cache, 1,000 values sampled from the
  column: the first 100 lookups 212 µs each in small pages, 809 µs in
  huge; the first 1,000, 80 µs and 149 µs.
- `SELECT *` of the rows of one value (4 to 2,317 rows), the index and its
  row groups: 16–40 ms. Parquet's row filter, which decodes `UserID` whole
  and the rest where it matches: 1.1–1.5 s. The file has no page index:
  each row group read is decoded whole.

**ClickBench `hits_99.parquet`, 997,497 rows, 160 MB**, an index per
column: `UserID`, 214,397 distinct values, 13.3 MB, built in 0.03 s;
`URL`, 289,848, 62.6 MB, 0.31 s; `EventDate`, 11, 4.0 MB. A URL's 32,746
rows in 111 µs, against 40 ms to read the column.

## Limits and open problems

- **Read only.** Changing a file map would need its pages to go back to an
  allocator that doesn't own them; an index of new data is a new file map.
- **Queries are copied.** A lookup of var-length keys writes its key into
  a query buffer first: `IndexFile<String, u64>` is 10% behind a map of
  `String`s, which searches the caller's `&str` in place. bbtree could
  search a `&[u8]` against `VarBytes` keys, as it does a `&str` against
  `String` keys, if it took `VarBytes` for a byte string, as `Auto` takes
  `String`; that is `Auto`'s recognition, which this design avoided.
- **Not in DataFusion yet**, as pq-vector is: the index finds row groups
  and rows; a `TableProvider` or physical optimizer rule in
  bbtree-datafusion-parquet would turn a query's predicate into them.
