# 10 — Visualization: browsing the real tree in a browser

Status (2026-10-11): the `inspect` feature with lookup traces
(`node/inspect.rs`, `map/inspect.rs`), and the visualizer in `vis/`.
Inserts and removals are still to come.

## Goals

From the original note:

- A web page that shows the tree's layout and its nodes, and later the
  operations: insert, get, range.
- Dioxus with Tailwind, with bbtree compiled to wasm.
- Smooth on a tree of 10M entries.
- Faithful to the implementation: it shows what bbtree holds, never a second
  interpretation of it.
- Open question in the note: does the viewer need every field exposed? No:
  see the `inspect` feature below.

## Decisions (2026-10-11)

- **wasm32 layouts are acceptable.** On wasm32 a pointer is 4 bytes, so an
  edge takes 4 bytes instead of 8 and internal nodes hold more keys than on
  x86-64, and a `String` takes 12 bytes instead of 24, which changes string
  leaves: in the wasm32 build no length tags fit in them. Leaves of
  fixed-size keys and values match x86-64. The in-node count takes the
  portable path instead of AVX-512, with the same result. The page says
  which target's layouts it shows.
- **First milestone: faithful layout and lookups.** Inserts and removals
  come later.
- **No dioxus-flow.** It was the first idea, to drive that library's
  performance work. It is a node-graph editor: `Flow` mounts every node and
  zooms both axes alike, and none of its editing applies to a read-only tree
  whose layout is implicit. Bending it to this tree meant culling, local
  coordinates, levels of detail and one-axis zoom, which make it a different
  library. The visualizer draws its own canvas instead; an experiment with
  those changes is stashed in dioxus-flow's repository. `Flow` could still
  serve a later detail view of a few hundred nodes, such as animated
  operations.
- **One lookup at a time, not every node at once (2026-10-11, second
  design).** The first design drew all of a 10M tree, level by level, with
  aggregates and one-axis zoom. It scaled, but it read as strips rather than
  a tree, and it didn't say what was going on. The visualizer now follows one
  key: its path from the root as linked node cards, each node's page as
  cache lines, and the search's own steps.
- **Light theme only.**

## The rule: bbtree describes itself, the viewer reads and draws

The viewer computes nothing that bbtree computes: no offsets, capacities,
prefix lengths, fill limits, or which child a key belongs to. Every such
number comes from bbtree's own code, through a read-only view. What the
viewer derives from those numbers, positions on screen and aggregate colors,
is drawing, not semantics. It shows a shared prefix only for keys that are
their bytes (byte strings and arrays); for integers it shows the inline keys
bbtree stores, not an encoding of its own.

## The `inspect` feature: a view, not exposed fields

No field becomes public. The precedent is `check_invariants`
(`map/invariants.rs`), a `pub fn` behind `cfg(fuzzing)` that walks the whole
tree through the node handles. `inspect` is a cargo feature instead of a cfg,
because a dependent crate opts into a feature and needs no `RUSTFLAGS`. It
adds API only, no dependencies (D2), and promises no stability. Its module
also compiles in tests, so `cargo test` runs its checks without the feature.

- **`BBTreeMap::inspect() -> TreeView`**, whose `root()` is a `NodeView`.
- **`NodeView`** wraps a `NodeRef<Immut, LeafOrInternal>` and answers through
  the existing handle methods:
  - id (the page address), height, `len`, capacity, `min_len`;
  - `prefix_len`, `local_prefix_len` (D33);
  - `key(i)`, `inline_key(i)` and `inline_key_width` (D18), `length_tag(i)`
    (D36);
  - for a leaf, `value(i)` and `head()` (D17); for an internal node,
    `child(i)`.
- **`PageMap`** comes from the page's `LeafLayout` or `InternalLayout`: each
  slot array (header, inline keys, length tags, keys, head, values, edges)
  by offset, stride, slot size, capacity, and how many slots the node uses.
  Arrays, rather than contiguous regions, because inline-key leaves
  interleave keys with values (D29). Whatever no array covers is padding.
- **Not offered:** raw page bytes, since unused slots are uninitialized and
  reading them is UB; and any mutable access.
- **Tests:** a walk through the views over `u64`, `String` and tuple keys
  sees every key once in order, `len()` entries, a head on every leaf but the
  leftmost, and pages whose used slots lie within them without overlapping.
- **Code:** `node/inspect.rs` holds the views; it is safe code. The one
  addition elsewhere is `NodePtr::addr` in `page.rs`, for node ids.

## Lookup traces: `TreeView::trace`

`trace(&key)` runs the searches `get` runs, node by node: `search` in each
internal node and `search_point` in the leaf, on the same `SearchKey`. Each
`TraceStep` carries:

- the node;
- the outcome: down edge `e`, found at slot `i`, or absent (would go in at
  slot `i`);
- the key's inline key at the node's prefix, from the node's own
  `inline_target`;
- the slots whose inline keys tie with it, from the node's own `equal_run`:
  the only keys compared in full.

A key found in an internal node is a separator (D17). The trace then follows
`into_value_head` down to the leaf whose head holds the value, and in debug
builds asserts that it reaches the very value `into_value_head` returns.

The only change to the node code is visibility: `inline_target` and
`NodeInlineKeys::equal_run` become `pub(super)`. A test traces every key of
two maps, `u64` and `String`, and some absent keys, against `get`.

## The visualizer (`vis/`)

A workspace of its own, like `fuzz/`, so Dioxus stays out of bbtree's
dependency graph and lockfile; nested packages are left out of bbtree's
published package. It depends on bbtree with `features = ["inspect"]`. Its
tools (`dx`, Tailwind, the wasm32 target) are in the default dev shell; see
`vis/README.md`.

- **Build** `u64`, `String` URL or `[u8; 16]` maps of up to 10M entries (1M
  strings), by random inserts, ascending inserts or `collect`. Each build
  looks up a random key, so there is always a path to see.
- **Look up** any key, or a random one, or step to the first key of the
  previous or next leaf.
- **The path** (`path_tree.rs`): one card per node from the root down, each
  hanging under the edge its parent took, with a bold ink connector. A card
  shows:
  - the node's keys over its capacity, and the prefix its search skips (its
    shared bytes in a tooltip);
  - its slots as a ruler: tied slots outlined in highlighter yellow, the
    found slot filled, and a notch at the edge taken;
  - what the lookup did there, in a few words.

  The parent's other children sit beside the card as gauges of how full
  they are, fanned from their own edge slots. Clicking a gauge looks up that
  node's first key. A separator's way to its value is dashed.
- **The inspector** (`node_inspector.rs`), for the card you pick, the leaf
  by default:
  - What the search did, in sentences drawn from the trace: the bytes
    skipped and why (fence prefix, D26; search prefix, D33), the inline key
    compared and how many slots tied (D18), and the edge taken with the keys
    either side, or the slot found, or the slot an insert would take.
  - The page as 16 rows of 64-byte cache lines (`memory_map.rs`). Every
    slot sits where the layout puts it, room is faint, and the slots the
    lookup read are highlighted: candidates outlined, the result filled.
    Each cache line the lookup touches gets a dot: the header's, all the
    inline keys' (the count reads them all), the candidates' and the
    result's.
  - The slots in a table, scrolled to the lookup's row. Keys show only the
    bytes past the shared prefix. Inline keys are hex bytes, and child links
    look up their child's first key.
- **The tree's shape** (`tree_shape.rs`): one top-down read of every node
  per build gives each level's nodes in key order, with parents, children,
  key counts and prefixes. That is enough to name nodes, find neighbors and
  draw gauges without a page read. Everything else is read live from a
  `NodeView`, reached from the root along the shape's path.
- **Look:** an annotated memory map.
  - Cool paper, ink and graphite for structure.
  - The three slot hues that validate in every pairing: keys blue, inline
    keys orange, values, heads and children aqua.
  - Two neutral grays for the header and length tags.
  - Highlighter yellow for whatever the lookup read.
  - Type: Martian Mono for titles, IBM Plex Sans for prose, IBM Plex Mono
    for data.

### What it shows

- **u64, 1M random inserts:** the fence prefix grows 0, 5, 6, 7 bytes from
  the root down. At the root every 2-byte inline key is `00 00`, so all of
  them tie and each is compared in full. In a leaf, one inline key matches,
  and the lookup touches 3 of 16 cache lines.
- **A key past the end of the tree** follows the right border, where every
  fence prefix is 0. All 30 keys in the leaf tie on `00 00`, and the lookup
  touches 10 of 16 lines. Ascending inserts land there.
- **String URLs:** the root keeps a 25-byte search prefix
  (`https://shop.example.com/`); the levels below skip 35, 36 and 38 bytes,
  with 8-byte inline keys.
- **`[u8; 16]` internal nodes** put children before keys (D39).

## Not done

- Inserts and removals. The effects come from comparing the touched nodes
  before and after, keyed by page address. Their way down is not one path:
  D37 records a path only when the leaf is full, and D45 appends with no
  descent. An exact trace needs an observer in the real code, whose default
  does nothing, kept only if it shows instruction parity.
- A view of the whole tree's health (fill and prefix per level), which the
  first design had and this one leaves out.
- CI for `vis/`: it is a workspace of its own, which bbtree's CI doesn't
  build.
- `cargo clippy --target wasm32-unknown-unknown` on bbtree fails one lint
  expectation: `#[expect(clippy::large_enum_variant)]` in `map/entry.rs`
  holds only where pointers take 8 bytes. Plain builds are unaffected.
