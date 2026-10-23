# 15 — File maps: a map in a file

Status: built (2026-10-19), D58: fixed-size keys and values in the
leaves, and keys and values of any length in heap pages after each leaf
(2026-10-21).

- **bbtree**, behind the `file` feature: the file map, its header and its
  check in `bbtree/src/tree/file.rs`; the API (`FileWriter`, `FileMap`,
  `Storable`, `PlainBytes`, `LeafBytes`, `FileBuf`) in `bbtree/src/file.rs`.
  It knows nothing of bytes of any length but two hooks: a slot type's
  check, and what a writer writes after each leaf.
- **bbtree-mmap** (`bbtree-mmap/`), generic: `BBTreeMapFile`, a typed map in a
  file, each key and value type keeping its own storage (`DiskType`);
  written, mapped or read, whole or as a range of a larger file. Var-length
  keys and values are its own: spans and queries (`VarBytes`,
  `VarBytesQuery`, `var_bytes.rs`) and their heap pages (`heap_pages.rs`).

Terms: a **file map** is a map compacted into one byte string, which a
reader uses where it lies; a **file** alone is the operating system's. A
key or value is **plain** (`PlainBytes`), kept in its slot, or
**var-length** (`VarBytes`), kept in the **heap pages** after its leaf,
its slot holding a **span**, `{len, offset}`. **OS pages** are the
operating system's, 4 KiB or 2 MiB, never a node's page.

## Problem

A map that is built once and read many times, by processes that start
cold, as after a restart, has to live in a file, alone or within a file of
other data, and be usable the moment the file is opened:

- **Opening costs nothing that grows with the map.** Rebuilding a map
  from its entries costs a pass over all of them: 2 ns an entry at best
  (D25), 0.2 s for 100M, after reading 1.6 GB.
- **Lookups take a map's time.** The point of bbtree is its search; a
  map in a file that searches slower is a different structure.
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
              leaves               in key order, each followed by its heap pages
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
  kept in its slot as it is, or bbtree-mmap's `VarBytes`, bytes of any
  length kept in heap pages after the leaf ("Keys and values of any
  length"). A plain type has no
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

`VarBytes`, bbtree-mmap's: each leaf keeps an 8-byte span for each, and
their bytes lie in heap pages right after the leaf, as a store's leaf
keeps its records (16-bbtree-store.md).

```text
leaf      | header | inline keys | hash tags | (key span, value span) … |
heap page | key 0's bytes, value 0's bytes | key 1's bytes, value 1's bytes | … | room |
heap page | …
leaf      | …
```

- **bbtree knows two hooks, and no span.** A slot type is `Storable`: what
  the writer's tree holds in its place, whether it points beyond itself,
  a number for the fingerprint, and a check that what it points to lies in
  the file map, which `FileMap::open` runs on every such slot. A writer's
  `LeafBytes` writes what follows each leaf, and the slots that point
  there, given the leaf's entries and their slots (`WriterLeaf`). Plain
  types need neither (`NoLeafBytes`).
- **Spans** are `{len: u32, offset: u32}`, aligned to 8: a key or value
  under 4 GiB.
- **Offsets count from the span's own slot.** Edges count from their node
  because node code follows them: `edge_unchecked` holds the node's
  address. Keys are followed by `K`'s own `Ord` and `InlineKey`, which
  bbtree calls with `&K` alone: in the leaf search, `cmp_from`, a
  separator tie's walk to its head key (D49), the walks. From the slot,
  `&K` is all a key needs; from the node, each of those calls would need
  the page passed in, through the search's hot path. A span's bytes lie
  outside the span itself, which a reference may reach under Tree
  Borrows, which Miri checks, and not under Stacked Borrows.
- **Heap pages** (`HeapPages`) are the map's node config's
  (`HEAP_PAGE_SIZE`, the leaves' size by default), right after their
  leaf: a hit reads the leaf and one line of its pages; a cold lookup
  reads one place of the file; a range scan reads it front to back. A
  record is an entry's key's bytes, then its value's, whichever a span
  keeps, in key order, the head's first. It stays in one page, starting
  the next if it doesn't fit in what is left of one; one larger than a
  page starts on a page and runs on over the next. The leaf's last page
  ends where the next leaf may start, as the leaves' alignment is the heap
  pages' size.
- **Building.** bbtree-mmap builds the compact tree in memory over
  `ArenaBytes`: 8-byte pointers to length-prefixed bytes in an arena of
  the writer's (`ByteArena`), which read their bytes alone, as inline keys
  and prefixes need, and stay valid as entries move. They have the span's
  size, alignment and inline keys, so the same layout (checked as the
  writer starts): pages still copy byte for byte, and `HeapPages` rewrites
  each key and value slot, the head's included, into a span, as it lays
  the bytes out. Slots past a leaf's entries become zeros, not stale
  pointers.
- **Strategy: `WithInlineKey`**, through `VarBytes`'s `InlineKey` impl,
  which reads its bytes as `String`'s does: 8-byte windows in leaves, 4 in
  internal nodes, its length as its hash tag, as `Auto` gives heap byte
  strings. Var-length keys need inline keys: without them internal nodes
  would keep keys, which no bytes back.
- **Queries point to the caller's bytes** (`VarBytesQuery`): a span whose
  length is one no span has, `u32::MAX`, then the bytes' address, as a
  store's lookup key is. The search compares it as a span of those bytes,
  inline keys and all, and copies nothing. Spans aren't `Copy`, so a
  query lends itself only by reference, from its whole bytes.
- **Checking.** `FileMap::open` also checks that the bytes of each live
  span lie in the file map, and that no span carries a query's mark: it
  reads every leaf, and no heap page.
- **Fixed-size keys stay plain.** Var-length, a `u64` hit reads one more
  line, the key's bytes, and the file map takes 49 bytes an entry against
  18.5: 216 ns against 127 a hit at 10M (Measurements).

## Typed maps in files (bbtree-mmap)

- **`BBTreeMapFile<K, V>`** is a map in a file: written once from a map's
  entries, in their order; mapped or read, whole or from a range of a
  larger file (`write_at`, `read_range`); looked
  up with a borrowed form of the key, as a map is, one key or a batch
  (`get_many`). Lookups hand back views of the file. Mapping is a builder,
  `mmap(path)`, whose OS pages and range are optional: small pages and the
  whole file unless set.
- **Each type picks its storage** (`DiskType`): its stored type, plain or
  `VarBytes`; its keys' strategy; and its view. Integers, floats and byte
  arrays are plain, in their slots, under `Auto`, so a `u64` map in a file
  is the plain file map, at a map's speed. `String` and `Vec<u8>` are
  `VarBytes`, under `WithInlineKey`, and come back as `&str` and `&[u8]`.
  `plain_disk_type!` makes a caller's `PlainBytes` type one, under `Auto`
  or, with its own `InlineKey` impl, `WithInlineKey`. Rust's coherence rules
  forbid a blanket impl over `PlainBytes` beside `String`'s, so each plain
  type opts in.
- **Why not one layout of bytes for every type:** integers as `VarBytes`
  read a heap page on every hit and take 2.6 times the bytes
  (Measurements).
- **Queries** (`DiskQuery`) are of the stored type: a reference to the key
  itself for a plain type, a `VarBytesQuery` of its bytes for text and
  bytes: neither is copied, and both search with inline keys.
- **Inputs** (`DiskInput`): what the writer takes, owned or borrowed:
  `&map` of a map, `&mut map` of the shared map, by its owner,
  `range_cloned(..)` of one threads still change, and `Cow` bytes, which
  pass to the writer without a copy.

## Measurements

9900X, Linux 7.2, mimalloc, one run each, pinned to one core.

**Maps don't change.** The probes of `get` (`u64`, `i64`, `u128`,
`[u8; 32]`, `String`), `range`, `iter`, `insert`, `remove`, the shared
map's `get_copied` and `collect` compile to the same machine code as
before, instruction for instruction, with and without the `file` feature.
A file map's `get` differs from a map's in its descent loop alone:
`add rbx, [rbx + rdx*8 + 0x70]` where the map has
`mov rbx, [r13 + r12*8 + 0x70]`.

**Lookups at 10M random `u64` keys and values** (`bbtree-mmap/examples/lookups.rs`,
and `huge_pages.rs` for the mapped rows, mapped from disk; the file map
184.7 MB, 18.47 bytes per entry, written in 60 ms):

| | get hit | get miss |
|---|--:|--:|
| map, bulk built | 125.4 ns | 90.4 ns |
| file map, read into memory | 125.9 ns | 94.3 ns |
| file map, mapped, small pages | 133.9 ns | 96.8 ns |
| file map, mapped, huge pages | 126.4 ns | 91.2 ns |

**Lookups of var-length keys and values at 10M entries**
(`bbtree-mmap/examples/var_bytes_lookups.rs`; read into memory). One run
each, unpinned, 2026-10-21, while the machine ran about 1.6 times slower
than for the table above (its map took 202–213 ns a hit, against 125.4):
compare rows within this table only. Before: each leaf's bytes in its
tail, queries `&[u8]` under `Auto`; after: in heap pages,
`VarBytesQuery`s under `WithInlineKey`.

| | get hit | get miss | bytes an entry |
|---|--:|--:|--:|
| `u64`: map | 212.9 ns | 143.2 ns | |
| `u64`: `BBTreeMapFile<u64, u64>`, in slots | 203.5 ns | 143.6 ns | 18.47 |
| `u64`: var-length, tails (before) | 329.2 ns | 225.4 ns | 49.06 |
| `u64`: var-length, heap pages | 327.0 ns | 210.7 ns | 49.06 |
| text, 13 to 45 bytes: map of `String` | 351.8 ns | 350.1 ns | |
| text: `BBTreeMapFile<String, u64>`, tails (before) | 322.4 ns | 321.6 ns | 73.42 |
| text: `BBTreeMapFile<String, u64>`, heap pages | 322.4 ns | 319.7 ns | 73.42 |

Heap pages take what tails took: these leaves' bytes fit in their first
page, so no record started a page early, and both round up to the same
1 KiB step. The text map's `u64` values in their slots take no fewer
bytes for the same reason. At 1M keys of about 21 bytes, they take 49.1
bytes an entry against 73.4.

## Limits and open problems

- **Read only.** Changing a file map would need its pages to go back to an
  allocator that doesn't own them; a map of new data is a new file map.
