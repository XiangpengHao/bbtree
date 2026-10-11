# 10 — Visualization: browsing the real tree in a browser

Status: planned (2026-10-11), nothing implemented.

## Goals

From the original note:

- A web page that shows the tree's layout and its nodes, and later the
  operations: insert, get, range.
- Dioxus with Tailwind, with bbtree compiled to wasm.
- Smooth on a tree of 10M entries.
- Faithful to the implementation: it shows what bbtree holds, never a second
  interpretation of it.
- Built on [dioxus-flow](https://github.com/XiangpengHao/dioxus-flow), and
  used to drive that library's performance work.
- Open question in the note: does the viewer need every field exposed?

## Decisions (2026-10-11)

- **wasm32 layouts are acceptable.** On wasm32 a pointer is 4 bytes, so an
  edge takes 4 bytes instead of 8 and internal nodes hold more keys than on
  x86-64. A `String` takes 12 bytes instead of 24, which changes string
  leaves. Leaves of fixed-size keys and values match x86-64 exactly. The
  in-node count takes the portable path instead of AVX-512, with the same
  result. The page states the target whose layouts it shows.
- **First milestone: faithful layout and browsing.** Operation traces come
  later.
- **dioxus-flow may change in any way that is right for dioxus-flow.** The
  test is its own product (`PRODUCT.md`): a general node-graph library for
  any Dioxus app, pure Rust, with claims backed by runnable measurements. The
  viewer's convenience alone justifies no change.
- **Scale target: browsing a tree of 10M entries.** Operations are animated
  one at a time, on trees of any size.

## The rule: bbtree describes itself, the viewer reads and draws

The viewer computes nothing that bbtree computes: no offsets, capacities,
prefix lengths, fill limits, or which child a key belongs to. Every such
number comes from bbtree's own code, through a read-only view. What the viewer
derives from those numbers, such as positions on screen and aggregate colors,
is drawing, not semantics.

## The `inspect` feature: a view, not exposed fields

No field becomes public. The precedent is `check_invariants`
(`map/invariants.rs`), a `pub fn` behind `cfg(fuzzing)` that walks the whole
tree through the node handles. `inspect` is a cargo feature instead of a cfg,
because a dependent crate opts into a feature and needs no `RUSTFLAGS`. It
adds API only, and no dependencies (D2).

```rust
// bbtree, feature "inspect"
impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    pub fn inspect(&self) -> TreeView<'_, K, V, S>;
}

pub mod inspect {
    /// The whole tree: height, len, the root, and each kind's page map.
    pub struct TreeView<'a, K, V, S>;
    /// One node, wrapping a `NodeRef<Immut<'a>, K, V, S, LeafOrInternal>`.
    pub struct NodeView<'a, K, V, S>;
    /// A node's identity: its page address, stable while the node lives.
    pub struct NodeId;
    /// Where each slot array of a kind's page sits.
    pub struct PageMap;
}
```

- **`NodeView` offers:**
  - id, height (which gives the kind), `len`, capacity and `min_len`;
  - `prefix_len`, the local search prefix (D33) and `has_head` (D17);
  - `key(i)` and `inline_key(i)`, plus `length_tag(i)` (D36);
  - for a leaf, `value(i)` and the head; for an internal node, `child(i)`.

  Every accessor calls the existing handle method of the same meaning.
- **`PageMap` comes from `LeafLayout` and `InternalLayout`.** It describes
  each slot array (header, inline keys, length tags, keys, head, values,
  edges) by offset, stride, slot size and capacity. A node's `len` says how
  many slots are in use. Arrays, rather than contiguous regions, because
  inline-key leaves interleave keys with values (D29). Whatever no array
  covers is padding or free space.
- **Not offered:** raw page bytes, because unused slots are uninitialized and
  reading them is UB. No mutable access either.
- **Tests:**
  - a walk through `inspect` agrees with `check_invariants`: same node count,
    entry count and heights;
  - each kind's page map stays within the page and no two slots overlap.

  The layout constants already assert their invariants at compile time
  (`layout.rs`), so building the viewer for wasm32 checks the wasm32 layouts
  of every key type it uses.

## Browsing 10M entries

**Size.** For u64 → u64 with inline keys, an entry takes about 24 bytes of a
1 KB leaf, so a full leaf holds about 40. 10M entries then take 250k–350k
leaves depending on fill, about 10k internal nodes and a height of 4–5: about
300 MB of pages, within wasm32's 4 GB. A tree built from sorted entries
(`collect`, D25) has full nodes; random inserts give realistic fill. The
workload panel offers both, and inserts in chunks so the page stays
responsive.

**No DOM holds 300k nodes.** dioxus-flow's `Flow` mounts every node; its
tiles (`content-visibility`) were measured up to 20k nodes. So the viewer
mounts only what is around the viewport (`RegionLayer`, below), with a budget
of about 2k elements.

**The layout is implicit.** A B-tree needs no graph layout: leaf `i` sits at
`x = i × pitch`, and each internal node is centered over its children's
leaves. The viewer keeps a *tree index* that it rebuilds after every change:

- the leaf range of every internal node, per level, in key order: one pass
  over the ~10k internal nodes;
- each leaf's `len`, for zoomed-out aggregates: one header read per leaf,
  expected to take tens of milliseconds; to be measured.

A viewport query is a binary search per level, O(log n + visible). The leaf
at index `i` is found through its parent in the index, then read through its
`NodeView`. The index caches shape only; every number drawn up close comes
from a live view.

**Detail follows zoom:**

- **Far:** one strip per level. Nodes narrower than about 2 px are binned
  per pixel column and colored by an aggregate (fill, prefix length). Whether
  to draw the strips as DOM rects or as one image per strip generated in Rust
  is decided by measurement.
- **Middle:** one box per node with a fill bar and `len`/capacity. An edge is
  drawn only when both of its nodes are on screen.
- **Near:** the page itself, drawn from its `PageMap` at byte scale: header,
  inline-key slots, key and value slots (used and free), head, edges. Keys and
  values are printed, string keys gray out their first `prefix_len` bytes,
  and inline keys show in hex.

## dioxus-flow changes

The viewer draws on dioxus-flow's `Canvas`, not through `Flow`. `Canvas` is
the lower layer that dioxus-flow already offers for applications that render
their own content: pan, zoom, gestures, overlays. `Flow`'s editable node list,
measurement and auto layout don't apply to a read-only tree whose layout is
implicit, so `Flow` stays as it is. A huge world on `Canvas` is missing three
things, each general to any application with a large world (maps, timelines,
big trees):

1. **`RegionLayer`: drawing only around the viewport.** The layer hands the
   application a `Region`: the viewport padded by an overscan margin, at a
   scale of `2^level`, where `level = floor(log2(zoom))`.
   - The application draws the region in *local* coordinates: pixels at that
     scale, relative to the region's origin.
   - The region changes only when the view leaves it, or when the zoom
     crosses a power of two. In between, a pan or zoom moves one CSS
     transform: `translate` to the region's screen origin, then `scale` by
     `zoom / 2^level`, which stays in `[1, 2)`.
   - The level doubles as the level of detail: what to draw at a scale is the
     application's call.
   - Every number in the DOM stays about screen-sized, at any world size. A
     30M-unit-wide tree drawn in world coordinates would pass Chromium's
     layout limit of about 33.5M px (`LayoutUnit`) and lose precision in the
     compositor's f32.
2. **Content bounds for `Canvas`.** Fit view and the minimap take the bounds
   of what the application draws, which `Flow` derives from its nodes as
   before. The minimap also draws application content, in flow coordinates.
3. **Background spacing stops shrinking at very low zoom.** The dot spacing
   doubles when it would fall below the closest spacing any default zoom
   reaches. Without this, at zoom 10⁻⁴ the dots merge into a solid fill.

The demo gets a `?world=N` mode: a synthetic tree of N leaves with fanout 40,
never materialized, drawn through `RegionLayer` with a level of detail. It
measures the layer on its own, in both engines, as `?stress=N` does for
`Flow`.

Not in this change: `Flow`'s own culling. Its nodes are tiled with
`content-visibility` today, and replacing that is a separate decision with its
own measurements.

## Where the code lives

`vis/` is a workspace of its own, like `fuzz/`, so Dioxus stays out of
bbtree's dependency graph and lockfile. It depends on bbtree with
`features = ["inspect"]`. Files are organized by feature:

- `workload.rs`: key type, entry count and order, chunked building;
- `tree_index.rs`: leaf ranges per level and positions;
- `tree_canvas.rs`: the canvas, the region layer and the zoom levels;
- `node_anatomy.rs`: the near-zoom page view.

There is one map per key type, picked so the interesting layouts all appear:

- u64 → u64: inline keys (D31);
- `[u8; 16]` → u64: 2-byte summaries with children first (D32, D39);
- `String` → u64 with shared prefixes: fence prefixes, length tags and the
  local prefix (D26, D36, D33).

The UI reaches each map through one trait object, implemented once,
generically.

The flake gets a dev shell for `vis/` with the wasm32 target, `dioxus-cli`,
`tailwindcss`, and a `wasm-bindgen-cli` pinned to match, as in dioxus-flow's
flake.

## Plan

1. **dioxus-flow:** `RegionLayer`, content bounds, background spacing, and
   the `?world=N` demo, measured in both engines. It needs nothing from
   bbtree, so it goes first.
2. **bbtree:** the `inspect` feature and its tests. Add the wasm32 target to
   the flake, and run `cargo check` and `cargo clippy` for
   `--target wasm32-unknown-unknown --features inspect`. (The flake has no
   wasm32 target today; the code has non-x86-64 fallbacks but has never been
   built for wasm32.)
3. **`vis/` at small scale** (≤ 1k nodes, about 40k u64 entries): tree,
   node boxes, page anatomy.
4. **`vis/` at 10M:** tree index, zoom levels, chunked loading. Measure build
   time, index rebuild time, and frame time while panning and zooming at each
   zoom level.
5. **Later, operations** (get, insert, remove, range):
   - The effects come from comparing the touched nodes before and after,
     keyed by page address, together with the real `Path` (D24). Comparing
     only the path and its siblings costs O(height × fanout).
   - The process is not one path: D37 records a path only when the leaf is
     full, and D45 appends with no descent. Rebuilding a trace from
     `Path::search` would show descents that never happened. An exact trace
     needs an observer in the real code, whose default does nothing, kept
     only if it shows instruction parity.
