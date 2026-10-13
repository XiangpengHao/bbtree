# 10 — Visualization: browsing the real tree in a browser

Status (2026-10-12): the `inspect` feature with lookup traces
(`tree/inspect.rs`, `map/mod.rs`), and the visualizer in `vis/`.
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
- **Light theme only.**

## The rule: bbtree describes itself, the viewer reads and draws

The viewer computes nothing that bbtree computes: no offsets, capacities,
prefix lengths, fill limits, or which child a key belongs to. Every such
number comes from bbtree's own code, through a read-only view. What the
viewer derives from those numbers, positions on screen and aggregate colors,
is drawing, not semantics. A key's bytes are bbtree's own encoding,
`InlineKey::inline_key_at`, read a window at a time. A node's fences are
the one thing it finds itself, since bbtree stores none (05-fence-key.md):
the separators beside the way down, read as `PathMut::bounds` reads them. A
test checks both against bbtree at nodes of every level: each node's prefix
is what its fences share, and its inline keys are those bytes past it
(`fences.rs`).

## The `inspect` feature: a view, not exposed fields

No field becomes public. The precedent is `check_invariants`
(`tree/invariants.rs`), a `pub fn` behind `cfg(fuzzing)` that walks the whole
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
- **Code:** `tree/inspect.rs` holds the views; it is safe code. The one
  addition elsewhere is `NodePtr::addr` in `page.rs`, for node ids.

## Lookup traces: `TreeView::trace`

`trace(&key)` runs the searches `get` runs, node by node: `search` in each
internal node and `search_point` in the leaf, on the same `SearchKey`. Each
`TraceStep` carries:

- the node;
- the outcome: down edge `e`, found at slot `i`, or absent (would go in at
  slot `i`);
- whether the key lacks the node's local search prefix (D33), from the
  node's own `search_prefix_len`: the node then put it at its first or
  last edge after checking it against its first key, comparing no inline
  keys, and the step has none;
- otherwise the key's inline key at the node's prefix, from the node's own
  `inline_target`;
- the slots whose inline keys tie with it, from the node's own `equal_run`:
  the only keys compared in full.

A key found in an internal node is a separator (D17). The trace then follows
`into_value_head` down to the leaf whose head holds the value, and in debug
builds asserts that it reaches the very value `into_value_head` returns.

The node code changes only in visibility: `inline_target`,
`search_prefix_len` and `NodeInlineKeys::equal_run` are
`pub(in crate::tree)`. A test traces every key of two maps, `u64` and
`String`, and some absent keys, against `get`, with keys that lack a local
prefix on either side.

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
  back and forward through the lookups (`shortcuts.rs`).
- **The toolbar**: the search, as the current key and its outcome
  (`trace.rs`); the tree's build controls (`workload.rs`); both open
  panels (`popover.rs`); and the repository's GitHub mark. A credit line
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
  - what the lookup did there, in a few words.

  The parent's other children sit beside the card as gauges of how full
  they are, on a faint fan; pointing at one draws its own edge, clicking it
  looks up that node's first key. A separator's way to its value is dashed.

  Every lookup plays its way down, a level every 260 ms: all the inline
  keys of a ruler flash at once, as the search compares them all, and
  settle dark where smaller than the searched key's inline key, pale where
  larger: the search counts the smaller ones, and the count is where the
  key goes (D18). The tied ones mark; the result and the notch drop in;
  the edge to the next card draws. Between lookups the cards glide to
  their new places.
- **The inspector** (`node_inspector.rs`), for the card you pick, the root
  after each lookup, right of the path: a header line with its place, keys,
  page size and address; its fences, its search, then a tab for the page
  and one for the slots:
  - Its fences (`fences.rs`): the low and high fence, the node's first and
    last keys and the key looked up, in key order, a column per byte (hex,
    or characters for URLs), so the shared columns line up. Brackets mark
    the prefix, a search prefix, and the inline key's bytes; a missing
    fence names the border it is missing on. The heading gives the prefix,
    search prefix and inline key widths. Pointing at a fence lights its separator in the ancestor's card;
    clicking it inspects that ancestor.
  - What the search did, a line a step, each opening to a sentence: skip the
    shared bytes (fence prefix, D26; search prefix, D33, once the key is
    checked against the node's first key, or else go before or after all
    the node's keys at once), count the inline
    keys smaller than the key's and those equal to it (D18): the first count
    places the key, and only the equal ones, if any, are searched in full;
    and the outcome: the edge taken with the keys either side, the slot
    found, a separator's way to its head (D17), or the slot an insert would
    take. Pointing at a step lights up what it read in the page and the
    rulers.
  - The page as 16 rows of 64-byte cache lines (`memory_map.rs`), rows
    numbered 0 to 15 and bytes across. Every
    slot sits where the layout puts it, empty slots are faint, and the slots
    the lookup read are marked: tied keys outlined, the result filled. Each
    cache line the lookup surely reads gets a dot, in the order it reads
    them, replayed with each lookup: the header's, with the first key's
    where a search prefix is checked, all the inline keys'
    (the count over the capacity reads them all on x86-64), a lone tied
    key's, and the result's. The search among two or more tied keys is a
    binary search (`search_run`), whose probes the trace doesn't record, so
    their lines get hollow dots: maybe read. The headline counts both.
  - The slots in a table (`slot_table.rs`), scrolled to the lookup's row.
    Keys show only the bytes past the shared prefix; clicking one looks it
    up. Inline keys are hex bytes, and child links look up their child's
    first key. The empty slots are one row.
  - The three views share one pointer (`spotlight.rs`): pointing at a slot
    in any of them lights it up in the others, with a hint by the pointer.
    Terms in the text (`concepts.rs`) define themselves on hover or focus,
    with the design note behind them, and light up what they name.
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
    fades.

### What it shows

- **u64, 1M random inserts:** every key's 5 high bytes are zeros. The root
  has no fences, so it keeps them as a search prefix (D33) and its 2-byte
  inline keys start after them; below it, fence prefixes run from 5 to 7
  bytes. In a leaf, one inline key matches, and the lookup reads 3 of 16
  cache lines.
- **A key past the end of the tree** follows the right border, where every
  fence prefix is 0. Each internal node there keeps a search prefix the
  key lacks, so it goes to the last child once the node's first key is
  read. The leaf keeps none: its inline keys are all `00 00`. `u64::MAX`
  ties with none of them, but a key just past the last, as each ascending
  insert is, ties with all, and the search among them may read most of
  the page.
- **String URLs:** the root keeps a 25-byte search prefix
  (`https://shop.example.com/`); the levels below skip 35, 36 and 38 bytes,
  with 8-byte inline keys.
- **`[u8; 16]` internal nodes** put children before keys (D39). At 1M ids,
  the root keeps a 13-byte search prefix: `bbtree`, two zero bytes and the
  index's 5 high zero bytes.

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
