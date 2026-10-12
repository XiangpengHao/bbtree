# 10 — Visualization: browsing the real tree in a browser

Status: implemented (2026-10-11), uncommitted: the `inspect` feature
(`node/inspect.rs`, `map/inspect.rs`), and the visualizer in `vis/`, whose
tools (`dx`, Tailwind, the wasm32 target) are in the default dev shell. Operation traces are still to come.

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
- **First milestone: faithful layout and browsing.** Operation traces come
  later.
- **No dioxus-flow.** It was the first idea, to drive that library's
  performance work. It is a node-graph editor: `Flow` mounts every node and
  zooms both axes alike, and none of its editing applies to a read-only tree
  whose layout is implicit. Bending it to this tree meant culling, local
  coordinates, levels of detail and one-axis zoom, which make it a different
  library. The visualizer draws its own canvas instead; an experiment with
  those changes is stashed in dioxus-flow's repository. `Flow` could still
  serve a later detail view of a few hundred nodes, such as animated
  operations.
- **The canvas zooms only horizontally.** A tree of 250k leaves is about
  350,000 times wider than tall, so under a uniform zoom its overview is a
  line under a pixel tall, and levels only come apart in windows of about
  1,000 leaves. With only x zoomed, every level keeps a row of fixed height
  at every zoom: the overview shows all levels at once, and zoomed into one
  leaf, the leaf is a wide strip, the shape of its page's byte map. The
  model is a trace viewer's, such as Perfetto's.
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

## The visualizer (`vis/`)

A workspace of its own, like `fuzz/`, so Dioxus stays out of bbtree's
dependency graph and lockfile; nested packages are left out of bbtree's
published package. It depends on bbtree with `features = ["inspect"]`. See
`vis/README.md` for running it.

- **Builds** `u64`, `String` URL or `[u8; 16]` maps of up to 10M entries (1M
  strings), by random inserts, ascending inserts or `collect`. Inserts go in
  chunks of 50k, with a yield to the browser between them. Random order is a
  full-period LCG walk through a bijective mix, so no index array is held.
- **The tree's shape** (`tree_shape.rs`): one top-down read of every node
  per build, which records each level's nodes in key order: leaf range,
  `len`, prefix, parent and children, plus running sums of `len` and prefix.
  The mean of any run of nodes then takes constant time, so a strip bin costs
  two binary searches however many leaves it covers. Everything drawn up
  close is read live from a `NodeView`, reached from the root along the
  shape's path.
- **The canvas** (`tree_canvas.rs`): the x axis counts leaves, and only the
  span around the view is in the document, at the current zoom, in its own
  pixels. Every number that reaches the DOM stays screen-sized: in a 10M
  tree, zoomed onto one leaf, the view sits billions of pixels into the
  tree. A pan inside the span moves one transform; leaving it, or zooming,
  redraws it.
- **Detail by on-screen width, row by row:**
  - a row whose nodes average under 4 px is a strip, binned per 2 px and
    colored by mean fill or prefix, with one SVG path per ramp step;
  - then one colored box per node;
  - from 92 px, a box with its key count and a fill bar;
  - from 300 px, its page drawn from its `PageMap`, with its first and last
    keys;
  - a node wider than the screen gets a caption that follows the view.
- **The detail panel** (`node_detail.rs`): header numbers, the page bar with
  its legend, and every key with its shared prefix grayed, its inline key in
  hex, its length tag, and its value or child. Links lead to the children,
  the parent and the neighbors.
- **Colors** follow the dataviz method:
  - fill and prefix use one sequential blue ramp;
  - page slots use the only three categorical hues that validate in every
    pairing (keys blue, inline keys orange, values, heads and children aqua);
  - the header and length tags are two neutral grays;
  - every color has a legend entry and a tooltip.

### Measured

Headless Chrome over the DevTools protocol, 1600×900, release build, real
input events:

| Build | Time | Leaves | Internal | Height | Pages |
|---|---|---|---|---|---|
| 10M `u64`, random inserts | 4.2 s | 258,282 | 4,950 | 4 | 257 MiB |
| 10M `u64`, ascending inserts | 1.1 s | 227,273 | 4,210 | 4 | 226 MiB |
| 10M `u64`, `collect` | 0.1 s | 178,572 | 2,482 | 3 | 177 MiB |
| 1M `String` URLs, random | 0.4 s | 45,143 | 1,306 | 3 | 45 MiB |
| 1M `[u8; 16]` ids, random | 0.4 s | 37,245 | 1,148 | 3 | 38 MiB |

The times are the page's own: inserts, then the read of every node.

- **Frame times:** 3 s zoom gestures and 3 s pans over the 1M and 10M
  trees: every frame within p50 16.7 ms, max 16.8 ms, none over 20 ms.
- **Elements:** the document holds at most about 1,800 elements at any zoom.
- **What it shows:**
  - u64 leaves: capacity 55, 2-byte inline keys under a 7-byte fence prefix;
  - URL leaves: a 39-byte fence prefix and 8-byte inline keys;
  - `[u8; 16]` internal nodes: children before keys (D39);
  - the fill each insert order leaves.

## Not done

- Operation traces (get, insert, remove, range).
  - The effects come from comparing the touched nodes before and after,
    keyed by page address, together with the real `Path` (D24).
  - The process is not one path: D37 records a path only when the leaf is
    full, and D45 appends with no descent. An exact trace needs an observer
    in the real code, whose default does nothing, kept only if it shows
    instruction parity.
- CI for `vis/`: it is a workspace of its own, which bbtree's CI doesn't
  build.
- `cargo clippy --target wasm32-unknown-unknown` on bbtree fails one lint
  expectation: `#[expect(clippy::large_enum_variant)]` in `map/entry.rs`
  holds only where pointers take 8 bytes. Plain builds are unaffected.
