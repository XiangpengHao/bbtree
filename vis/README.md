# bbtree-vis

Browse a bbtree in the browser, as bbtree stores it: a map is built in the
page, and every node is drawn from bbtree's own read-only views
(`BBTreeMap::inspect`, the `inspect` feature). Design and decisions are in
`doc/10-visualization.md`.

```bash
nix develop            # the repository's dev shell, or direnv
cd vis
dx serve               # http://127.0.0.1:8080
dx build --release --debug-symbols false   # into target/dx/bbtree-vis/release/web/public
```

The release build needs `--debug-symbols false`: the nixpkgs `wasm-opt` crashes
on the debug info `dx` keeps by default.

## What it shows

- **The whole tree, one row per level**, the root on top. The x axis counts
  leaves; scrolling zooms along it, and rows keep their height, so every level
  stays in view from all ten million entries down to one leaf across the
  screen. Drag or shift-scroll to pan, double-click a node to frame it, `0`
  to see everything, `Esc` to deselect.
- **Detail by width.** Nodes too narrow for a box of their own make a strip,
  binned per two pixels; then a colored box each, then a box with its key
  count, then its page laid out byte by byte. Color by **fill** (keys over
  capacity) or **prefix** (the bytes the node's searches skip).
- **A node in full.** Click one: its header, its page (header, inline keys,
  length tags, keys, values or children, empty slots, padding, where the
  layout puts them), and every key with its inline key, length tag and value
  or child. String and byte-array keys gray out the bytes the node shares.
- **Builds** of `u64`, `String` URLs or `[u8; 16]` ids, up to ten million
  entries (one million strings), by random inserts, ascending inserts or
  `collect`.

Layouts are those of the wasm32 build the page runs: a pointer takes 4 bytes,
so internal nodes hold more keys than on a 64-bit target, and `String` leaves
differ (no length tags fit). Leaves of fixed-size keys and values match.

## Code

By feature, in `src/`:

- `workload.rs`: what to build, and building it in chunks;
- `inspected_tree.rs`: a built map behind one face for every key type;
- `tree_shape.rs`: each level's nodes in key order, read once per build;
- `tree_canvas.rs`: the rows, the zoom, the strips and boxes;
- `page_bar.rs`: a page drawn byte by byte;
- `node_detail.rs`: the selected node's panel;
- `tree_summary.rs`: the built tree in one line.
