# bbtree-vis

Look keys up in a bbtree and see how it finds them: the path from the root,
each node's page cache line by cache line, and what the search compared at
every node. Everything is read from bbtree's own read-only views
(`BBTreeMap::inspect`, `TreeView::trace`, the `inspect` feature), never
recomputed. Design and decisions are in `doc/10-visualization.md`.

```bash
nix develop            # the repository's dev shell, or direnv
cd vis
dx serve               # http://127.0.0.1:8080
dx build --release --debug-symbols false   # into target/dx/bbtree-vis/release/web/public
```

The release build needs `--debug-symbols false`: the nixpkgs `wasm-opt` crashes
on the debug info `dx` keeps by default. Fonts come from Google Fonts; offline,
the page falls back to system fonts.

## Using it

- **Build** a map of `u64`, URLs or 16-byte ids, up to ten million entries
  (one million URLs), by random inserts, ascending inserts or `collect`.
  Changing any of the three builds anew; a finished build looks up a random
  key.
- **Look up** a key by typing it (a number, a URL, or 32 hex digits), or
  press *Random*, step to the leaf beside, or *Try* a lookup worth seeing: a
  separator, a missing key, a key past the last one. Keys: `/` to type, `R`
  random, `[` `]` the leaves beside, `↑` `↓` along the path.
- **Watch the path**: one card per node, root first, under the edge its
  parent took. Each lookup plays its way down: at each node every inline
  key flashes at once, the tied ones stay outlined, the result fills and the
  notch drops at the edge taken. *Replay* plays it again. Gauges beside a
  card are its siblings, as full as they are; click one to look up its first
  key.
- **Inspect a node** by clicking its card: what its search did, as
  numbered steps; its page as 64-byte cache lines, with the lines the
  lookup read dotted in the order it read them; its slots in a table.
  Point at any slot, step or underlined term and the others light up what
  it names.

Layouts are those of the wasm32 build the page runs: a pointer takes 4 bytes,
so internal nodes hold more keys than on a 64-bit target, and `String` leaves
differ (no length tags fit). Leaves of fixed-size keys and values match.

## Code

By feature, in `src/`:

- `workload.rs`: what to build, and building it in chunks;
- `inspected_tree.rs`: a built map behind one face for every key type;
- `trace.rs`: a lookup as plain data, and the bar that starts one;
- `scenarios.rs`: lookups worth seeing, on the tree that is built;
- `path_summary.rs`: the lookup in one line, the path's legend, replay;
- `path_tree.rs`: the path, its cards and the siblings' gauges;
- `node_inspector.rs`: the picked node, its search as steps;
- `memory_map.rs`: a page drawn as cache lines, with what a lookup read;
- `slot_table.rs`: a node's slots, one row each;
- `spotlight.rs`: what the reader points at, shared by the panels, and the
  hint;
- `concepts.rs`: bbtree's ideas in plain words, and the terms that name
  them;
- `shortcuts.rs`, `icons.rs`: keys for the common moves, and the icons;
- `tree_shape.rs`: each level's nodes in key order, read once per build;
- `tree_summary.rs`, `count_text.rs`: the header line, and counts as text.
