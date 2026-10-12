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

- **Build** a map of `u64`, `String` URLs or `[u8; 16]` ids, up to ten
  million entries (one million strings), by random inserts, ascending inserts
  or `collect`. A finished build looks up a random key.
- **Look up** a key by typing it (a number, a URL, or 32 hex digits), or press
  *Random key*, or step to the previous or next leaf.
- **Read the path**: one card per node, root first, under the edge its parent
  took. Highlighter yellow marks what the search compared: slots whose inline
  keys tied are outlined, the slot found is filled, and the notch is the edge
  taken. Gauges beside a card are its siblings, filled as full as they are;
  click one to look up its first key.
- **Inspect a node** by clicking its card: what the search did there in
  words, its page as 64-byte cache lines with the slots and lines the lookup
  read, and its slots in a table.

Layouts are those of the wasm32 build the page runs: a pointer takes 4 bytes,
so internal nodes hold more keys than on a 64-bit target, and `String` leaves
differ (no length tags fit). Leaves of fixed-size keys and values match.

## Code

By feature, in `src/`:

- `workload.rs`: what to build, and building it in chunks;
- `inspected_tree.rs`: a built map behind one face for every key type;
- `trace.rs`: a lookup as plain data, and the bar that starts one;
- `path_tree.rs`: the path, its cards and the siblings' gauges;
- `node_inspector.rs`: the picked node, in words, page and slots;
- `memory_map.rs`: a page drawn as cache lines;
- `tree_shape.rs`: each level's nodes in key order, read once per build;
- `tree_summary.rs`, `count_text.rs`: the header line, and counts as text.
