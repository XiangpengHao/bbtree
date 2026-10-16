# bbtree-vis

Look keys up in a bbtree and see how it finds them: the path from the root,
each node's page cache line by cache line, and what the search compared at
every node. Or look a batch of keys up at once and watch `get_many`
search them together on one thread: each key prefetches the line it reads
next, and the others search while it comes. Everything is read from
bbtree's own read-only views
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

One screen, no scrolling on a laptop: a slim toolbar, the lookup's path on
the left, the inspected node on the right.

- **Share what you see** by copying the address: it holds the tree's
  settings, the key, the node inspected and its tab, as in
  `?keys=urls&n=100000&build=collect&key=…&level=1&view=slots`. Builds are
  deterministic, so the link opens on the same tree, node for node.
- **Go back** to the lookup before with `←`, forward with `→`, or the
  browser's own back and forward: each lookup and each new tree is a
  history entry.
- **Look up** a key from the toolbar's search (`/`): type a number, a URL or
  32 hex digits, take a random key (`R`), step to the leaf beside (`[`
  `]`), or pick a lookup worth seeing: a separator, a missing key, a key
  past the last one.
- **Change the tree** from the toolbar's tree button: `u64`, URLs or 16-byte
  ids, up to ten million entries (one million URLs), by random inserts,
  ascending inserts or `collect`. Any change builds anew.
- **Watch the path**: one card per node, root first. Each lookup plays its
  way down a search step at a time: skip the bytes a node's keys share,
  count the smaller inline keys (every inline key flashes, then settles
  dark if smaller than the searched key's and pale if larger, so the dark
  run's length is where the key goes), compare any tied keys in full, then
  go down. Play, pause or step it with the buttons above the path; the key
  beside them dims the bytes each node skips and frames the bytes it
  compares, stepping right as the prefixes grow.
- **Inspect a node** by clicking its card (or `↑` `↓`); a lookup starts at
  the root. First its fences, the separators either side of the way down to
  it, with its first and last keys and the key looked up, byte by byte: the
  bytes the fences share are its prefix, and the inline keys are the bytes
  after. Then its search, a line a step (click one for the whole
  sentence), then its page as 64-byte cache lines, or its slots as a table.
  Point at any slot, step or underlined term and the others light up what
  it names.

- **Look up a batch** with the toolbar's switch (`B`): four keys searched
  together on one thread, as bbtree's `get_many` runs them. Edit any key
  and press Enter, or take new ones (`R`). Each key sits beside its row of
  searches and prefetches; below, the nodes they read, drawn as their
  pages' cache lines, with a token for each key; right, the code all
  searches run, with every key at the line it stopped at. Space plays or
  pauses, `,` and `.` step a search; click a key's number to look it up
  alone.

Layouts are those of the wasm32 build the page runs: a pointer takes 4 bytes,
so internal nodes hold more keys than on a 64-bit target, and `String` leaves
differ (no length tags fit). Leaves of fixed-size keys and values match.

## Code

By feature, in `src/`:

- `workload.rs`: what to build, its toolbar button, and building it in
  chunks;
- `inspected_tree.rs`: a built map behind one face for every key type;
- `trace.rs`: a lookup as plain data, and the search that starts one;
- `shared_link.rs`: the page's state in its address and the browser's
  history, read at start and kept in step;
- `popover.rs`: the panels the toolbar's buttons open;
- `scenarios.rs`: lookups worth seeing, on the tree that is built;
- `path_summary.rs`: the lookup in one line, replay, the path's legend;
- `path_tree.rs`: the path, its cards and the siblings' gauges;
- `node_inspector.rs`: the picked node, its search as steps;
- `fences.rs`: a node's fences and keys as bytes, and its prefix;
- `memory_map.rs`: a page drawn as cache lines, with what a lookup read;
- `slot_table.rs`: a node's slots, one row each;
- `spotlight.rs`: what the reader points at, shared by the panels, and the
  hint;
- `concepts.rs`: bbtree's ideas in plain words, and the terms that name
  them;
- `shortcuts.rs`, `icons.rs`: keys for the common moves, and the icons;
- `batch_run.rs`: a batch's lanes and the executor's polls as plain data,
  and the keys a batch looks up;
- `batch_view.rs`: the batch's switch, button, headline, and the loop that
  plays it;
- `batch_tree.rs`, `lane_program.rs`, `batch_timeline.rs`: the batch's
  pages and keys, the search's code, and the searches in order;
- `query_key.rs`: the key above the path, walking down with it;
- `tree_shape.rs`: each level's nodes in key order, read once per build;
- `tree_summary.rs`, `count_text.rs`: the header line, and counts as text.
