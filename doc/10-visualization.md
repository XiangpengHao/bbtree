# 10 — Visualization: browsing the real tree in a browser

Status (2026-10-16): the `inspect` feature with lookup and batch traces
(`tree/inspect.rs`, `tree/batch.rs`, `map/mod.rs`), and the visualizer in
`vis/`, one key or a batch at a time; beside it, the post's page, with
one figure so far. Inserts and removals are still to come.

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
- **Teach, for a reader who knows B-trees but not bbtree (2026-10-12,
  third pass).** A critique and audit (22/40 heuristics, 13/20 technical)
  found the second design showed bbtree's state but not its ideas: no
  legend, the explanation below the fold, three unlinked views of one node,
  no motion, and no layout below 1000 px. The third pass keeps the look and
  the rule below, and adds what a learner needs: the search as numbered
  steps first, every bbtree term defined where it is used, one shared hover
  across the views, lookups that land on the interesting cases, motion that
  replays the lookup's way down, and a one-column layout for small screens.
- **One screen, and a guide (2026-10-12, fourth pass).** The header and
  the lookup bar took two rows for little, and readers were still lost on
  where to start, the path cards and the page map. The controls became one
  toolbar: the current key and its outcome as a button that opens the
  search, and the tree as a button that opens the build controls. The path
  sits left, the inspected node right, its page and its slots behind tabs,
  so nothing scrolls at 1440×900. A guide under the path (`guide.rs`) reads
  the page step by step, from the lookup on screen, with numbered marks
  pinned to the parts it names.
- **Fences first, the guide gone, links to share (2026-10-12, fifth
  pass).** The guide took half the path's column to say what the inspector
  could show in place, so it went: the path has the column. The inspector
  shows the reason for each node's prefix, its fences, byte by byte beside
  its first and last keys and the key looked up, with the prefix and the
  inline key's bytes marked; its search shrank to a line a step, the full
  sentence a click away. A lookup inspects the root, where it starts. The
  address holds the page's state, so a copied link opens on the same view.
- **Batches, as bbtree runs them (2026-10-15, sixth pass).** A second
  mode plays a real run of `get_many` (doc 12) poll by poll: the lanes, the
  executor's order and every line each lane fetched come from bbtree's own
  lanes and executor, watched (below). A critique and audit (28/40
  heuristics, 14/20 technical) led the same pass to fix the one-key view,
  and a second round on the batch view (28/40, 15/20) to the breaks, the
  caption across the board and the legend below:
  the ruler's compare flash now covers every inline-key slot up to the
  capacity, as the count reads them, and the inspector says so; the tabs
  are a real tab list; shortcuts leave focused controls and the tab list
  alone; dimmed text meets 4.5:1.
- **Batches show the idea, not a cost model (2026-10-15).** A first cut
  timed searches and lines with a model of a 9900X's caches, which drew
  speedups the measurements don't match; a second marked where the thread
  "waits", which the trace can't know: a prefetch never blocks, and a
  search stalls on its load whenever its line hasn't come, however many
  keys there are. The view now shows searches and prefetches in the order
  they run, four keys, on one thread, and nothing about the executor,
  polls or timing.
- **A post beside the explorer, from the same panels (2026-10-16).** The
  blog post on bbtree and BCTree and the visualizer share one build and
  one look. The explorer stays as it is, at `/`, for readers who want to
  look around; the post is a page of its own, at `/post/`, with tabs
  between them. The post is prose with figures, each figure one idea drawn
  by the explorer's own panels, not a second set of drawings. Rejected:
  the explorer pinned beside scrolling prose (at half width it loses its
  inspector, scroll fights its playback, and phones need figures anyway),
  and the explorer with a story panel (the fourth pass's guide, removed in
  the fifth).
- **Light theme only.**

## The rule: bbtree describes itself, the viewer reads and draws

The viewer computes nothing that bbtree computes: no offsets, capacities,
prefix lengths, fill limits, or which child a key belongs to. Every such
number comes from bbtree's own code, through a read-only view. What the
viewer derives from those numbers, positions on screen and aggregate colors,
is drawing, not semantics. A key's bytes are bbtree's own encoding,
`InlineKey::inline_key_at`, read a window at a time. A node's fences are
the one thing it finds itself, since bbtree stores none (05-fence-key.md):
the separators beside the way down, read as `NodeStackMut::fences` reads
them. A test checks both against bbtree at nodes of every level: each
node's prefix is what its fences share, and its inline keys are those
bytes past it (`fences.rs`).

## The `inspect` feature: a view, not exposed fields

No field becomes public. The precedent is `check_invariants`
(`tree/invariants.rs`), a `pub fn` behind `cfg(fuzzing)` that walks the whole
tree through the node handles. `inspect` is a cargo feature instead of a cfg,
because a dependent crate opts into a feature and needs no `RUSTFLAGS`. It
adds API only, no dependencies (D2), and promises no stability. Its module
also compiles in tests, so `cargo test` runs its checks without the feature.

- **`BBTreeMap::inspect() -> TreeView`**, whose `root()` is a `NodeView`,
  and whose `shared_prefix()` is the bytes every key shares (D50).
- **`NodeView`** wraps a `Node`, a leaf or an internal node as the tag of
  the edge to it says, or for the root its header (D52), and answers
  through the node types' own methods:
  - id (the page address), depth below the root, `len`, capacity,
    `min_len`;
  - `prefix_len`;
  - `key(i)`, `inline_key(i)` and `inline_key_width` (D18), `length_tag(i)`
    (D36);
  - for a leaf, `value(i)` and `head()` (D17); for an internal node,
    `child(i)`.
- **`PageMap`** comes from the page's `LeafLayout` or `InternalLayout`: each
  slot array (header, inline keys, length tags, keys, head, values, edges)
  by offset, stride, slot size, capacity, and how many slots the node uses.
  The root's page adds its root area, after room for a node of either kind
  (D52). Arrays, rather than contiguous regions, because inline-key leaves
  interleave keys with values (D29). Whatever no array covers is padding.
- **Not offered:** raw page bytes, since unused slots are uninitialized and
  reading them is UB; and any mutable access.
- **Tests:** a walk through the views over `u64`, `String` and tuple keys
  sees every key once in order, `len()` entries, a head on every leaf but the
  leftmost, and pages whose used slots lie within them without overlapping.
- **Code:** `tree/inspect.rs` holds the views; it is safe code. The
  additions elsewhere are `NodePtr::addr` in `page.rs`, for node ids, and
  the lanes' watch in `tree/batch.rs` (next section but one).

## Lookup traces: `TreeView::trace`

`trace(&key)` runs the searches `get` runs, node by node: `search` in each
internal node and `find` in the leaf, on the same `SearchKey`. Each
`TraceStep` carries:

- the node;
- the outcome: down edge `e`, found at slot `i`, or absent (would go in at
  slot `i`);
- the key's inline key at the node's prefix, from the node's own
  `inline_target`;
- the slots whose inline keys tie with it, from the node's own `equal_range`:
  the only keys compared in full.

A key that lacks the bytes every key shares (D50) has no steps, as `get`
reads no node for it; `placed_by_shared_prefix` says whether it sorts
before or after every key.

A key found in an internal node is a separator (D17). The trace then follows
`Internal::head_leaf` down to the leaf whose head holds the value, and in
debug builds asserts that it reaches the very value `head_leaf` finds.

The node code changes only in visibility: `inline_target` and
`NodeInlineKeys::equal_range` are `pub(in crate::tree)`. A test traces every
key of two maps, `u64` and `String`, and some absent keys, against `get`,
with keys that lack the shared prefix on either side.

## Batch traces: `TreeView::trace_many`

`trace_many(keys)` runs `get_many`'s own lanes and executor
(`Tree::find_many_watched`), whatever the map's size, with a watch on each
lane: every `.await` in a lane names what it fetches (`LaneFetch`: the
query's window, an edge's line, a header, the candidate's entry, its heap
bytes), and the watch hears it, then the lane's end. The trace is every
poll in the executor's order: the lane, and the line it fetched, placed by
address in a page on that key's way down (`TreeView::trace`), as a node and
a 64-byte line from the page's start; the query's and heap bytes lie in no
node. `BatchTrace::batches` says whether `get_many` runs lanes on this map
at all, past its size cutoff.

The map's batches pass `()` as the watch, which does nothing. A lane hands
the watch to `Prefetcher::watched` rather than calling a method on itself:
borrowing the whole lane changed what the async block keeps across awaits,
and cost 1 to 12 instructions a key. As built, `HEAD`'s and this build's
bench binaries run the same instructions per key, to two decimals, on 1M
`batch_get_hit`, `batch_get_miss` and `extend_random` for `u64`, JSON and
`hash32`, and every batch function compiles to the same size. A test polls
`u64` and `String` lanes in turn and finds every fetched node line on its
key's way down.

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
  previous or next leaf; or try a lookup worth seeing (`scenarios.rs`): a
  separator (found in the root, its value a leaf's head), a key past the
  last one (the right border, no fence prefixes), and for URLs a missing
  key between two (where an insert would go). Keys: `/` to type, `R`
  random, `[` `]` the leaves beside, `↑` `↓` along the path, `←` `→`
  back and forward through the lookups, Space to play or pause the way
  down, `,` `.` to step it (`shortcuts.rs`).
- **The toolbar**: the tabs to the post (`site.rs`); the search, as the
  current key and its outcome (`trace.rs`); the tree's build controls
  (`workload.rs`); both open panels (`popover.rs`); and the repository's
  GitHub mark. A credit line
  sits under the path.
- **The headline** (`path_summary.rs`): where the lookup ended, how many
  nodes and cache lines it read, and a replay; the path's legend sits in
  the path's corner.
- **The link** (`shared_link.rs`): the address holds the tree's settings,
  the key, the inspected node's level and the tab, as
  `?keys=u64&n=1000000&build=random&key=851191&level=3&view=page`. The page
  starts from it and keeps it in step. Each lookup and each new tree is a
  history entry, numbered in `history.state`; another node or tab only
  rewrites the entry. `←` `→` and the browser's back and forward step
  through the entries, rebuilding the tree where it differs, and `←` stops
  at the page's first entry rather than leave it.
- **The path** (`path_tree.rs`): one card per node from the root down, each
  hanging under the edge its parent took, with a bold ink connector. A card
  shows:
  - the node's keys over its capacity, the prefix its search skips, and the
    cache lines the lookup read of it;
  - its slots as a ruler: tied slots outlined in highlighter yellow, the
    found slot filled, a notch at the edge taken, a red caret where an
    absent key would go. Pointing at a slot names its key; clicking it
    looks the key up;
  - what the lookup did there, in a few words;

  The parent's other children sit beside the card as gauges of how full
  they are, on a faint fan; pointing at one draws its own edge, clicking it
  looks up that node's first key. A separator's way to its value is dashed.

  Every lookup plays its way down a search step at a time (`descent.rs`),
  through each node's steps as the inspector lists them: skip the shared
  bytes, count the smaller inline keys, search the tied keys, the outcome.
  At the count all the inline keys of a ruler flash at once, as the search
  compares them all, and settle dark where smaller than the searched key's
  inline key, pale where larger: the count of the smaller ones is where
  the key goes (D18), and the tied ones mark. At the outcome the result
  and the notch drop in and the edge to the next card draws. Cards the way
  down hasn't reached are faded. Between lookups the cards glide to their
  new places.
- **Above the path** (`path_summary.rs`), the controls a batch has: play
  or pause the way down, or step it a search step at a time. The way down
  is one value the tracer keeps (`Descent`): each node's steps, how many
  are shown, and, while it plays, the node it stops after. The cards, the
  key and the inspector draw from it, and CSS transitions carry each
  change, so nothing remounts within a node. Playing adds a step every
  700 ms. Each step selects its node, so the inspector follows the way
  down; picking a node yourself pauses it there, and playing on returns
  to the way down's node. A lookup asked for through a node (a key or a
  child picked in a page, a link naming a level) plays down to that node
  and pauses after its search. Where the lookup ended is in the toolbar's
  search button, and read out for screen readers.
- **The key** (`query_key.rs`), above the path, highlighted, a cell per
  byte of its encoding (hex, or characters for URLs; a `u64` also as its
  number). It walks down with the path, step by step: once a node skips
  its prefix those bytes dim; once it counts, a frame slides onto the
  bytes it compares, the key's inline key there; while it compares tied
  keys in full, the bytes after the frame are underlined; zeros a window
  reads past the key's end show once a node reads them. Down the path the frame steps right, as the fences
  close in and the prefixes grow (05-fence-key.md, D50), and it rests on
  the last node's window. The prefix comes from the shape, the window's
  width from the trace (`TraceStep::inline_width`, the node's own
  `inline_key_width`). It has no hover: a strip of the key in every card,
  with a hint per part, flickered under the pointer and repeated the key
  four times, so the key is drawn once.
- **The inspector** (`node_inspector.rs`), for the node the way down is
  at, or the card you pick, right of the path: a header line with its place, keys,
  page size and address; its fences, its search, then a tab for the page
  and one for the slots:
  - Its fences (`fences.rs`): the low and high fence, the node's first and
    last keys and the key looked up, in key order, a column per byte (hex,
    or characters for URLs), so the shared columns line up. Brackets mark
    the prefix and the inline key's bytes; a missing fence names the
    border it is missing on, where the prefix is what every key shares.
    The heading gives the prefix and inline key widths. Pointing at a fence
    lights its separator in the ancestor's card; clicking it inspects that
    ancestor.
  - What the search did, a line a step, each opening to a sentence: skip the
    shared bytes (fence prefix, D26; on a border, what every key shares,
    checked once before the root, D50), count the inline
    keys smaller than the key's and those equal to it (D18): the first count
    places the key, and only the equal ones, if any, are searched in full;
    and the outcome: the edge taken with the keys either side, the slot
    found, a separator's way to its head (D17), or the slot an insert would
    take. Pointing at a step lights up what it read in the page, the
    fences and the rulers, and dims the rest; the way down's current step
    does the same while the pointer is on nothing, so playing walks the
    lighting through each node's search.
  - The page as 16 rows of 64-byte cache lines (`memory_map.rs`), rows
    numbered 0 to 15 and bytes across. Every
    slot sits where the layout puts it, empty slots are faint, and the slots
    the lookup read are marked: tied keys outlined, the result filled. Each
    cache line the lookup surely reads gets a dot, in the order it reads
    them, replayed with each lookup: the header's, all the inline keys'
    (the count over the capacity reads them all on x86-64), a lone tied
    key's, and the result's. The search among two or more tied keys is a
    binary search (`search_candidates`), whose probes the trace doesn't record, so
    their lines get hollow dots: maybe read. The headline counts both.
  - The slots in a table (`slot_table.rs`), scrolled to the lookup's row.
    Keys show only the bytes past the shared prefix; clicking one looks it
    up. Inline keys are hex bytes, and child links look up their child's
    first key. The empty slots are one row.
  - The three views share one pointer (`spotlight.rs`): pointing at a slot
    in any of them lights it up in the others, with a hint by the pointer.
    Terms in the text (`concepts.rs`) define themselves on hover or focus,
    with the design note behind them, and light up what they name.
- **A batch** (`batch_view.rs`), from the toolbar's switch, `B`, or a link
  that holds its keys (`bk=…`, four times): four keys, picked at random,
  for URLs the last one a key the map lacks, or typed: each key is a field
  the reader edits, and Enter looks it up in the batch, or says why the
  text names no key. It shows search and prefetch, in the words of the
  lookup, not of the executor: each key searches a node and prefetches the
  line it reads next, and the others search while that line comes; all on
  one thread. It plays a search at a time, the first round slowly; Space
  pauses, `,` and `.` step, a click on the rows jumps there, `R` or the
  keys' button picks new keys. Paused or stepped, every view shows the
  moment just after a search. One grid, the keys and the tree left, the
  code right:
  - the keys (`batch_timeline.rs`), each beside a row of its searches,
    blocks, and its prefetches, drawn from each search to the key's next:
    across the rows the prefetches stack up, the lines fetched together;
  - the nodes the batch reads (`batch_tree.rs`), level by level, spread
    over the panel's height, each as its page, a strip of 64-byte lines
    tinted as the page map tints its slots; each key a numbered token on
    the node it searches or prefetches from, searching, prefetching or
    done. A prefetched line outlines and fills until the key's next search
    reads it; an edge draws from the parent's line that holds it to the
    child's header;
  - the code (`lane_program.rs`), `find` as a dozen lines of highlighted
    pseudo-Rust under "All searches run in a single thread": searches,
    which work on lines at hand, in the keys' blue; prefetches, which ask
    for the line read next without waiting, in the highlighter's ink; each
    `.await`, where the other keys search until this key's turn, marked;
    and a comment on every line, aligned. Each key sits in the gutter of
    the line it stopped at; the search running lights the lines it ran;
    lines no key of the batch runs are dim. A test walks every lane of
    real batches through it, so it can't drift from `find`. Below it, the
    search running, in a sentence.
  The trace says nothing of when a line arrives, so the view claims no
  stall or speed: a prefetch is only drawn as pending until its key's next
  search. Clicking a key's number looks it up alone, in the one-key view.
  One number drives the motion: a loop writes the playback's time, in
  searches, to the board as `--t`, and CSS `calc()` on it fills the lines
  and draws the rows, so the panels redraw only when another search starts.
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
    for data; nothing below 11 px, and secondary text at 4.5:1 or more.
  - Motion explains the lookup and nothing else; with reduced motion,
    nothing travels or grows, and the reads still arrive in order, as
    fades. A batch with reduced motion jumps from poll to poll.
  - In a batch, ink for polls and lanes, and the highlighter's yellow for
    a line on its way, as for a line read.

### What it shows

- **u64, 1M random inserts:** every key's 5 high bytes are zeros. The tree
  keeps them as its shared prefix (D50), the prefix of the root and the
  borders, whose 2-byte inline keys start after them; below the root,
  fence prefixes run from 5 to 7 bytes. In a leaf, one inline key matches,
  and the lookup reads 3 of 16 cache lines.
- **A key past the end of the tree** that lacks the shared bytes, as
  `u64::MAX` does, reads no node: the check before the root places it after
  every key. One just past the last key has them, and follows the right
  border, whose nodes skip them.
- **String URLs:** the tree keeps a 25-byte shared prefix
  (`https://shop.example.com/`); the levels below skip 35, 36 and 38 bytes,
  with 8-byte inline keys.
- **`[u8; 16]` internal nodes** keep no keys, only inline keys and
  children: each separator's key is a head below (D49). At 1M ids, the tree
  keeps a 13-byte shared prefix: `bbtree`, two zero bytes and the index's 5
  high zero bytes.

## The post (`vis/`, `/post/`)

- **Two pages, one build, no router** (`site.rs`). The app reads the path
  once at start: `/post/` mounts the post, anything else the explorer, so
  every link made before the post still opens it. The tabs are plain
  links, and moving between the pages loads the other afresh. The
  explorer alone writes the address and the history (`shared_link.rs`); a
  router beside it would fight it for both. A static host serves the post
  from a copy of `index.html` at `post/`; `dx serve` serves it as is.
- **A figure is an explorer link drawn in place** (`figure.rs`). The link
  the explorer keeps in its address (`ExplorerLink`: the workload, the key
  and the level to inspect, or a batch's keys, and the tab) is what a
  figure starts from, and the figure links to the explorer with it, which
  opens on the same tree and lookup. A figure owns what its panels share in
  the explorer: the tracer, the batcher, the spotlight and the
  inspector's tab, so clicking in one figure changes it alone. Each figure
  is a component of its own that picks its panels; there is no generic
  list of panels to configure until a pattern repeats.
- **The panels are unchanged.** They took their state as explicit props
  and handles already; what was page-wide was the DOM and the CSS. Ids the
  browser looks elements up by (the batch's board, the inspector's tabs,
  the slot table's box) are unique to the component that draws them
  (`element_ids.rs`). The page's one hint is split from each view's
  spotlight (`PointerHint`), since `NodeAt` names no tree and two figures
  would light each other's nodes. The explorer's one-screen layout is
  scoped to it, so the post scrolls.
- **Trees are shared and built as the reader nears** (`TreeStore`).
  Figures of one workload share one tree, built when the reader comes
  within a screen of the first of them. A figure starts its lookup once a
  third of it is in view, so the way down plays where it is read; until
  then its frame keeps the panels' room.
- **Styled from the first paint.** On a slow network the wasm takes
  seconds, and stylesheets the app linked itself came only after it: a
  blank page, then the page unstyled. The stylesheets are in the static
  page's head instead (`with_static_head`), the fonts load beside the
  wasm without holding up the paint, and `index.html` shows a loading
  screen in the app's own classes (its bar, the explorer's waiting state;
  white for the post) until the app draws, so nothing moves when it does.
- **Shortcuts and the address are the explorer's.** On the post, Space and
  the arrows scroll; the figures' buttons play and step them.
- **So far:** one figure, the way down of one `u64` lookup (the key and the
  path's cards); the prose is a placeholder. The post's text, its order and
  how it is written (Markdown with figure markers, or prose in `rsx!`) are
  still to decide.

## Not done

- Batched inserts (`insert_many`, `extend`): their lanes descend as
  lookups do and could be traced the same way; the inserts that follow
  are serial.
- Inserts and removals. The effects come from comparing the touched nodes
  before and after, keyed by page address. Their way down is not one node
  stack: D37 records a node stack only when the leaf is full, and D45 appends
  with no descent. An exact trace needs an observer in the real code, whose
  default does nothing, kept only if it shows instruction parity.
- A view of the whole tree's health (fill and prefix per level), which the
  first design had and this one leaves out.
- CI for `vis/`: it is a workspace of its own, which bbtree's CI doesn't
  build.
- `cargo clippy --target wasm32-unknown-unknown` on bbtree fails one lint
  expectation: `#[expect(clippy::large_enum_variant)]` in `map/entry.rs`
  holds only where pointers take 8 bytes. Plain builds are unaffected.
