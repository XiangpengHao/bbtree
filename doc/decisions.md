# Decisions

Decisions made so far, with the reason and what follows from each.
To change a decision, add a new entry that supersedes it; don't edit the old one.

## Scope (from 00-init)

**D1. Drop-in replacement for std's `BTreeMap`/`BTreeSet`.**
`BBTreeMap` and `BBTreeSet` keep std's API and behavior. Any deviation needs a
strong reason and is documented in README.md. Design as if it could be merged
into std someday; flag anything that would prevent that.

**D2. Zero dependencies.** `serde` and `rand` are optional features.

**D3. `no_std`.**

**D4. Single-threaded**, like std.

**D5. Differential fuzzing against std's btree.** Formal verification with
Verus is optional but welcome.

## Nodes

**D6. Nodes are 4 KB.** (2026-10-07)
A node is a multiple of 4 KB only when the minimum capacity doesn't fit in
4 KB (huge K/V). There are no smaller nodes: no mini-pages, no size classes.

- Consequence: every non-empty map costs at least 4 KB (std: 192 B for a
  one-entry u64→u64 map). Accepted.

**D7. Nodes are always 4 KB-aligned.** (2026-10-07)

- A node never straddles a page: one TLB entry per node, and hardware
  prefetchers, which stop at page boundaries, can stream the whole node.
- The low 12 bits of every node address are zero, which leaves room to pack an
  index next to the pointer.
- Consequence: per-node aligned allocation from the system allocator is too
  expensive (D8).
- Consequence, to measure: the L1d set index is address bits 6–11, which is
  the page offset, so every node's header and first search probe land in the
  same L1 set. If it hurts, offset the key array per node ("coloring").

**D8. Own allocator trait.** (2026-10-07)
Every allocation is a node, counted, with a size known in advance; no runtime
surprises. System and custom allocators are both supported through our trait.

- Consequence: the default allocator cannot request each node as
  `Layout(4096, 4096)`. Measured with glibc (20k nodes, each fully written,
  RSS delta):

  | Strategy                                   | RSS per 4 KB node |
  |--------------------------------------------|-------------------|
  | `alloc(4096, align 16)` (not page-aligned) | 4120 B            |
  | `alloc(4096, align 4096)` per node         | 8200 B (2×)       |
  | carve from 64 KB chunks                    | 4353 B            |
  | carve from 256 KB chunks                   | 4154 B            |
  | carve from 2 MB chunks                     | 4098 B            |

**D9. Keys grow from the front of the node, values from the back.** (2026-10-07)

```
| len | k0 k1 k2 … →            free            ← … v2 v1 v0 |
0     H                                                     4096
```

- Key `i` sits at `H + i·size_of::<K>()`, value `i` at
  `end − (i+1)·size_of::<V>()`.
- With no parent pointer (D11) and no fences (D10), the header is just
  `len: u16`. Node kind comes from the height, as in std.
- Keys are aligned by rounding `H` up to `align_of::<K>()`. Values are aligned
  because the node end is 4 KB-aligned. The free gap needs no padding.
- An insert is one memmove of the keys after the position and one memmove of
  the values; a split is one memcpy for each half.

**D10. Not taken from bf-tree:** slotted layout, fence keys, prefix
compression, preview bytes, mini-pages. (2026-10-07)
Fixed-size generic K/V needs none of the slotted machinery. Fences would need
`K: Clone`. Prefix compression and preview bytes need byte-comparable keys.

**D12. Internal nodes put the edges at the end.** (2026-10-07)

```
| len | k0 k1 k2 … →      free      ← … v2 v1 v0 | e0 e1 … e_cap |
0     H                                     vals_end            4096
```

- The `cap + 1` edges (8 B each) fill the end of the node. Values grow down
  from `vals_end`, the start of the edge array rounded down to
  `align_of::<V>()`.
- Keys sit at the same offset in leaves and internal nodes, so one search
  routine serves both.
- Capacity is the largest `n` that fits, computed per `(K, V)` at compile time.
  u64→u64: leaf 255, internal 170.

## Navigation

**D11. No parent pointers; iterators use a path stack.** (2026-10-07)

- A split or merge never touches the moved children. std rewrites the parent
  link of every moved or shifted child (`node.rs:1045`), which would be ~85
  scattered writes per leaf split at our fanout.
- Iterators carry the root-to-leaf path inline, with no allocation. Unlike
  re-descending from the root (as bf-tree does), this never calls `Ord` inside
  `next()`, matching std.

## Proposed, not decided

- **Path-stack frame** = one word: node address | index in the low 12 bits
  (D7), using strict-provenance `map_addr`. Caps node capacity at 4095.
- **Stack depth `MAX_HEIGHT`:** 16 (128 B per stack; `K + V` above ~170 B
  forces 8 KB internal nodes) or 24 (192 B; only above ~445 B).
- **Exact allocation count before mutating:** insert counts the full nodes on
  its path from the leaf upward, allocates that many first, and so can fail
  without changing the tree. Name of the fallible insert (`try_insert` is
  taken in std).
- **Default allocator:** carve pages from large chunks (≥ 256 KB). Open:
  chunks per map or per allocator instance; when chunks go back to the OS.
- **B-tree, not B+-tree:** B+-tree separators need `K: Clone`.
- **Stable-first:** the library builds on stable; nightly only for fuzzing and
  Miri. Without `#[may_dangle]`, our dropck is stricter than std's (a README
  deviation); an optional `nightly` feature restores it.
- **`alloc`:** whether core code depends on `alloc` or only the default
  allocator does.
- **Fill factor:** right-biased split for appends, redistribution to a sibling
  before splitting, full-node bulk build for `FromIterator`/`append`.
