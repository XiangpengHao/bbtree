# bbtree-vis

Look keys up in a bbtree and see how it finds them: the path from the root,
each node's page cache line by cache line, and what the search compared at
every node. Or look a batch of keys up at once and watch `get_many`
search them together on one thread: each key prefetches the line it reads
next, and the others search while it comes. The trees are
`bbtree::unsync::BBTreeMap`s, which keep the same nodes as the
`bbtree::BBTreeMap` that threads share, without its latches. Everything is
read from bbtree's own read-only views (`unsync::BBTreeMap::inspect`,
`TreeView::trace`, the `inspect` feature), never recomputed. Design and decisions are in `doc/10-visualization.md`.

One build, two pages, with tabs between them: the explorer at `/`, and the
post at `/post/`, whose figures are the explorer's own panels.

```bash
nix develop            # the repository's dev shell, or direnv
cd vis
dx serve               # http://127.0.0.1:8080, and the post at /post/
dx build --release --debug-symbols false   # into target/dx/bbtree-vis/release/web/public
# A static host serves the post from its own copy of the page:
cd target/dx/bbtree-vis/release/web/public && mkdir -p post && cp index.html post/
```

The release build needs `--debug-symbols false`: the nixpkgs `wasm-opt` crashes
on the debug info `dx` keeps by default. Fonts come from Google Fonts; offline,
the page falls back to system fonts.

The page is styled from its first paint, before the wasm loads: `dx` puts
the stylesheets in `index.html`'s head (`with_static_head`), the fonts are
linked there, and its body holds a loading screen in the app's own classes,
which `main.css` hides once the app draws. Keep that screen's bar in step
with the header (`site.rs`).

## The explorer

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
- **Inspect a node** by clicking its card (or `↑` `↓`). While the lookup
  plays (Space; `,` `.` step it), the inspector follows it down. First its fences, the separators either side of the way down to
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

## The post

A column of prose with figures wider than it. A figure is an explorer link
drawn in place: it names a tree and a lookup (`ExplorerLink`), draws the
explorer's panels of its choosing, and links to the explorer, which opens
on the same lookup. Figures of one workload share one tree, built as the
reader nears the first of them; a figure starts its lookup once a third of
it is in view, so the way down plays where it is read. Each figure owns its
tracer, batcher and spotlight, so clicking in one changes it alone. The
explorer's keyboard shortcuts and its address are the explorer's only:
on the post, Space scrolls.

Layouts are those of the wasm32 build the page runs: a pointer takes 4 bytes,
so internal nodes hold more keys than on a 64-bit target, and `String` leaves
differ (no length tags fit). Leaves of fixed-size keys and values match.

The post's charts compile in the measurements under `data/<machine>/`:
`bench/tools/blog-runs.sh` measures them on a machine, and
`bench/tools/blog-collect.sh` writes them there (doc 14).

## Code

By feature, in `src/`:

- `site.rs`: the two pages, which one the address names, the header with
  the tabs between them, and the credit;
- `explorer.rs`: the explorer, its panels and controls;
- `post.rs`, `figure.rs`: the post, and a figure: its frame, the panels'
  state it owns, and the trees the figures share;
- `performance_cards.rs`, `single_thread_chart.rs`, `concurrent_chart.rs`,
  `end_to_end_chart.rs`, `memory_chart.rs`, `architectures_chart.rs`,
  `baseline_bars.rs`, `chart_scale.rs`, `measurements.rs`: the post's cards
  and charts, their scales, and the measurements they draw, of bbtree, the
  map threads share, against other maps;
- `element_ids.rs`: element ids unique to the component that draws them,
  for a panel drawn more than once on a page;
- `workload.rs`: what to build, its toolbar button, and building it in
  chunks;
- `inspected_tree.rs`: a built map behind one face for every key type;
- `trace.rs`: a lookup as plain data, and the search that starts one;
- `shared_link.rs`: the explorer's state in its address and the browser's
  history, read at start and kept in step; the link a figure starts from;
- `popover.rs`: the panels the toolbar's buttons open;
- `scenarios.rs`: lookups worth seeing, on the tree that is built;
- `path_summary.rs`: the controls and the key above the path, its legend;
- `descent.rs`: the way down, played a search step at a time;
- `path_tree.rs`: the path, its cards and the siblings' gauges;
- `node_inspector.rs`: the inspected node, its search as steps;
- `fences.rs`: a node's fences and keys as bytes, and its prefix;
- `memory_map.rs`: a page drawn as cache lines, with what a lookup read;
- `slot_table.rs`: a node's slots, one row each;
- `spotlight.rs`: what the reader points at, shared by one view's panels,
  and the page's hint;
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
