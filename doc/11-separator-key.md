# 11 — Separator keys: internal nodes without keys

Status: phase 1 built (2026-10-13), D49: with inline keys, separator keys
are leaf heads, a tie walks down to one, and internal nodes, which keep no
keys, are 512 B ("Phase 1, as built"). Exact separators are not built. It
changed D9, D15, D17, D29, D32, D36 and D43, and made children before keys
for byte arrays moot (06). The root and the borders skip what every key
shares, which the tree keeps, reading no key (D50); ties are open.

## Problem

The visualizer (10-visualization.md) shows that lookups rarely read an
internal node's full keys: the inline keys decide. About 94% of lookups
pass through an internal node without ending at a key, and 4-byte internal
summaries cost JSON 0.02 heap reads per hit (D43). Yet full keys take most
of an internal node:

| key | bytes per separator: inline key + key + edge | without the key |
|---|--:|--:|
| `u64` | 2 + 8 + 8 = 18 | 10 |
| `u128` | 2 + 16 + 8 = 26 | 10 |
| `String` | 4 + 24 + 8 = 36 | 12 |
| `[u8; 32]` | 2 + 32 + 8 = 42 | 10 |

Those bytes cost lookups less than their size suggests. Caches hold lines,
not nodes: a lookup reads an internal node's inline keys and one edge, and
key lines it never reads are never cached. The bytes lookups keep hot are
about one inline key and one edge per separator, in either layout, and a
tree has about as many separators as leaves. Counted from today's layouts,
removing the keys changes the hot lines per separator by 10% or less:

| key | hot lines per separator, today | without keys |
|---|--:|--:|
| `u64` | 10 / 56 = 0.18 | 0.16 |
| `String` | 6 / 28 = 0.21 | 0.19 |
| `[u8; 32]` | 4 / 24 = 0.17 | 0.16 |

So lookups can gain only in these ways:

- **Ties.** A tie today reads a key line, usually cold. Exact separators
  (below) settle most ties without reading a key. The `hash32` gain of
  children before keys (06) came from those reads.
- **TLB reach.** The same hot lines sit in about a third (`String`) to a
  quarter (`[u8; 32]`) of the pages.
- **Prefetching.** An adjacent-line prefetch next to an inline-key or edge
  line no longer brings in a key line nobody reads.
- **Depth,** if pages stay 1 KiB. Probably a loss: the levels saved are
  warm, and the search scans every slot (06, lessons 1 and 2).

Leaves, where most of a lookup's DRAM fills land, don't change. Measured,
JSON lookups read about 1.8 fewer lines from memory with phase 1 built, so
at 10M the lower internal levels of `String` maps did miss in cache.

The keys are there because of D17: a separator lives in an internal node
and its value in a leaf's head. 06 adds that separators can't be truncated,
because `iter` and `get_key_value` need a real `K` to point to. Both rules
date from before inline keys made full internal keys rarely needed.

## Idea: the head gets its key back

Separator `i` is the smallest key of the subtree under edge `i + 1`. It
belongs at slot 0 of that subtree's leftmost leaf, which is where that
leaf's head is today. Move the key there, next to its value, and keep only
inline keys and edges in internal nodes.

```text
today                                   proposed
root:  inline keys    "ap"     "ba"     root:  inline keys    "ap"     "ba"
       keys        "apple" "banana"            edges       L0      L1      L2
       edges      L0      L1      L2
                                        L0:  aaa, abc
L0:  aaa, abc                           L1:  apple, apricot, avocado
L1:  [apple's value], apricot, avocado  L2:  banana, berry, blueberry
L2:  [banana's value], berry, blueberry
```

- **Keys are still stored once, without `K: Clone`.** Every key, separators
  included, lives in a leaf beside its value, so `iter` and
  `get_key_value` return a leaf's `&K`.
- **Leaves.** Paired leaves kept a place for the head's key in entry 0,
  unused (`(hk head)` in `layout.rs`, D29); the head's key takes it. In
  `u64 → u64` leaves it was free (bytes 128–136), so capacity stays 55. In
  `String → u64` leaves, the D36 length tags (bytes 200–248) extended into
  it (224–248): the leaf gives up an entry to keep them, 24 → 23.
- **Internal nodes.** A header, inline keys and edges, in 512 B pages:
  49 separators at 2-byte inline keys (`u64`, `u128`, `[u8; 32]`), 41 at 4
  (`String`), 31 at 8, against 56, 38, 24 and 28 in 1 KiB with keys.

## Reading a separator's key on a tie

When a query's inline key ties with separator `i`'s, the search needs the
full key. It follows edge `i + 1`, then edge 0 at each level down to a
leaf, and reads slot 0. Today `into_value_head` (`tree/search.rs`) walks
the same path to find a separator's value; here the walk finds its key.

- **Nothing to keep up to date.** The walk finds the place from the tree
  itself every time. Nothing is ever inserted before slot 0 of a leaf other
  than the leftmost: its keys are at least the separator, and smaller keys
  go left. That is why today's head stays at the front of its leaf.
  Removing that key is a separator change, as removing a separator is
  today (`fit_left_of_separator` in `tree/remove.rs`).
- **Cost.** Above a leaf, a tie reads one leaf line, often of the leaf the
  query goes to next anyway. In a node `h` levels above the leaves, it
  costs `h` hops, and the path down need not be cached. About
  `1 − 1/fanout` (94–97%) of internal nodes are parents of leaves.
- **Several ties in one node** each need a key: about log₂ k walks for `k`
  tied separators, as the tied search is binary.
- **Built as** `SeparatorKeys`, which lends an internal node's separator
  keys, in place without inline keys, or walked to; searches and prefix
  upkeep read keys through it, whatever the node kind. The tied search over
  walked keys doesn't inline, so a search whose inline keys tie with none
  returns before it: called on every search, it cost `u64` lookups 135
  instructions.

**Go right first.** Instead of walking to the key, a tied search can take
the right child and keep searching normally. Every key on the right is at
least the separator, so the normal search settles the tie. If the query
leaves any node through a child other than the first, or reaches a leaf
and is at least its slot 0, then right was correct and the descent was the
real one. Otherwise, the query is smaller than slot 0, belongs on the left,
and the search resumes from the tie node through edge `i`.

| per tie | answer is right | answer is left |
|---|---|---|
| stored pointer to the leaf | 1 leaf line | 1 leaf line |
| walk, then decide | `h` hops | `h` hops |
| go right first | 0 | `h` hops, then the left descent |

Going right first needs the descent to remember where to resume: a stack
no deeper than the tree, for ties inside ties. Point lookups keep no path
today (D32), and small changes to that loop have moved instruction counts
before (D41).

## Exact separators

Once internal nodes hold no `K`, a separator only has to route. Any value
`s` with `left max < s ≤ right min` routes correctly, and it no longer
needs to be a key. Take such an `s` that ends inside the window its node
keeps, and the inline key stops being "the first bytes of some key" and
becomes the separator itself. Then a tie means `query ≥ s`: go right, and
read no key.

- **Exact.** In a node that skips `p` bytes before its windows (what its
  fences share, or on the borders what every key shares, D50), with
  `w`-byte inline keys, `s` is
  exact when `|s| ≤ p + w`. Keys of varying length also need `s`'s last
  byte nonzero, since zero padding would otherwise make "ap" and "ap\0"
  equal. This rule also covers keys under `WithInlineKey`, whose encoding
  length the nodes don't know. Fixed-width keys need nothing more: `s` is
  the right key with every byte after `|s|` set to zero, which is itself a
  key.
- **Choosing `s` at a leaf split.** With `d = LCP(left max, right min)` and
  `high` the right leaf's upper fence, take
  `s = right min[0 .. max(d + 1, LCP(right min, high))]`. Every fence
  prefix stays what the full right min would give:
  - left node: `LCP(low, s) = LCP(low, right min)`, because
    `|s| > d ≥ LCP(low, right min)`;
  - right node: `LCP(s, high) = LCP(right min, high)`, because
    `|s| ≥ LCP(right min, high)`.

  "abc" | "apple" gives "ap"; "apple" | "apricot" gives "apr", which
  doesn't fit 2 bytes.
- **Choosing where to split.** Among a few positions near the middle,
  split where `s` is shortest: the shortest-separator rule of prefix
  B-trees (Bayer and Unterauer, 1977). No layout changes.
- **What changes.** An exact `s` is no key, so slot 0 of the right leaf
  need not equal it. "ape" ≥ "ap" goes right and lands before "apple",
  which is valid. Removing the right leaf's smallest key changes no fence.
- **A tie with an exact separator costs nothing.** In a node, at most one
  exact separator has a given window, and it is the smallest of the
  separators that tie: an exact `s` is the smallest value with its window.
  A query that ties is at least `s`; only the other tied separators, which
  aren't exact, need a key.

### Separators that don't fit

When `s` doesn't fit in its node's window, the separator is the full right
min instead. Slot 0 of the right subtree's leftmost leaf then holds it, a
tie walks to it or goes right first, and removing it is a separator change.
Each separator has a bit that says which kind it is.

Separators move between levels:

- **Down** (merges, steals into a child): the child's prefix is at least the
  parent's, so an exact separator stays exact, and a full one stays
  full.
- **Up** (internal splits, steals into a parent): the parent's prefix can be
  shorter, and an exact `s` may stop fitting. Its bytes past the parent's
  window are then stored nowhere. Either the split chooses a separator that
  still fits, or `s` is replaced by the right part's smallest key, a full
  separator. That moves a fence, and the nodes beside it refit their
  prefixes as after any separator change (D26).

## Interactions

- **Fences (D26).** Fences become separator values, exact or full. The
  choice above keeps each prefix what it would be with full keys at the
  moment the separator is made. "Only a separator change moves a fence"
  still holds. The new separator change is a promoted separator replaced
  by a full key.
- **The shared prefix (D50).** A per-node prefix check would read key 0,
  a walk to a leaf here, on every search of the root and the borders.
  Instead the tree keeps the bytes every key shares and checks a query
  against them once, before the root; no search reads a key but on a tie.
- **Window recomputation (D43).** Keys no longer move from internal nodes
  to leaves. A window an internal node hands out is read again from its
  separator's key, walked to; leaf operations that bring a separator's key
  into the leaf use the parent's window, read so.
- **Heads (D17).** Heads stay, and gain their separator's key: it moves
  with the head's value in splits, moves between siblings, merges and
  removals (`Rebalance` in `tree/node/handle.rs`), and walks, clones,
  teardown and joins take it from the head. Keys without inline keys keep
  D17 unchanged, so the code holds both kinds of separator
  (`SeparatorSlots`).

## Not pursued

- **Children before keys in every internal node**, the first step
  considered. Byte arrays already did this (06), and its gain came
  from 32-byte keys no longer crossing a cache line. A `u64` node's
  inline keys end at byte 120, and in either order edge `i` sits in a line
  of its own except edge 0. `u128` keys are already within lines. Heap
  strings would have lost a line where a search read key 0 right after the
  inline keys (why byte arrays alone had it). This change also
  removes the key array, so the order would not carry over.
- **A stored pointer per separator.** 8 bytes, as much as a `u64` key, and
  an invariant that every structural operation keeps (split, merge,
  steal, clone, join, cut, build). It might be added later to nodes two or more levels up
  only, if their ties are measured to matter. Those nodes are a few percent of
  internal nodes.
- **An inline-key width per node**, chosen so that every separator fits.
  Width is a constant of each layout today (D47). Varying it per node
  means choosing the search at run time on every level, and a capacity per
  node, which an insert that widens a node could overflow. Worth trying
  only if many separators don't fit.
- **Spending the freed bytes on fanout.** At 1 KiB, capacity nearly
  doubles or triples, but the search scans every slot, and the levels it
  saves are warm (06; 2 KiB internal nodes were slower). Measured: 10M
  `hash32` hits 232 ns at 1 KiB against 160 at 512 B, `u64` 138 against
  124, JSON 287 against 276. 512 B it is (D15).

## Measure first

Each of these runs at 10M on random `u64`, dense `u64`, JSON, paths and
`hash32`:

1. **How many separators would be exact,** per level, at 2- and 4-byte
   inline keys, with the choice rule, with and without choosing where to
   split. A count over a built tree, no timing.
2. **Ties today:** full-key reads in internal nodes per lookup, per level,
   separating ties from per-node prefix checks.
3. **Where DRAM fills land:** per hit, internal levels against the leaf,
   from precise sampling, and dTLB misses per lookup.

What exact separators can save is (2), as cold lines per lookup, times
the share of separators (1) finds exact. If that and (3)'s page walks are
small, the change is a simplification, judged by the code it removes.

### Census, 2026-10-13

(1) and (2) at 10M entries, 1M hits and 1M misses, from
`bbtree-separator-census` (bench crate, `inspect` feature): the bench's
keys and insertion order, today's tree, its lookups traced through
`TreeView::trace`. Not yet with choosing where to split; (3) not yet run.

| keys | width | exact at height 1 | exact above | ties per hit | of which inexact |
|---|--:|--:|--:|--:|--:|
| `u64` | 2 | 72% | about 1% | 0.18 | 0.16 |
| `u64`, dense | 2 | 95% | about 1% | 0.26 | 0.24 |
| `hash32` | 2 | 75% | 0–65% | 0.20 | 0.14 |
| JSON | 4 | 99.9% | 77–98% | 0.059 | 0.0003 |
| paths | 4 | 0.1% | 0–5% | 1.07 | 1.07 |

- At 4 bytes, every `u64`, dense `u64` and `hash32` separator but a few
  is exact, at every height. At their 2 bytes, most ties stay inexact.
- JSON ties are mostly hits that equal a separator (5.6% of hits at
  height 1), all exact.
- Paths tie at 97% of height-1 searches, with 1.9 separators each, and
  even at 8 bytes 0.1% of their separators are exact: keeping the child's
  prefix (`LCP(right min, high)`) makes them long. Misses look the same.
- Only dense `u64` and paths checked a per-node prefix (since replaced by
  D50), at the root (100%) and next level (50%).

### Pricing ties, 2026-10-13

Today's tree with 4-byte internal inline keys for std integers and byte
arrays (leaves keep 2, and read a window from its key when it moves up),
against the tree before, at 10M, one run each, `--perf`:

| cell | ns | instructions | branch misses | L1d misses | lines from memory |
|---|--:|--:|--:|--:|--:|
| `u64` hit | 129.6 → 138.2 | 242 → 327 | 0.48 → 0.18 | 13.3 → 17.5 | 3.75 → 3.72 |
| `u64` miss | 93.7 → 105.3 | 221 → 306 | 0.42 → 0.17 | 12.5 → 16.7 | 3.02 → 3.05 |
| `hash32` hit | 192.1 → 183.9 | 270 → 280 | 0.43 → 0.14 | 13.5 → 15.4 | 5.88 → 5.92 |
| `hash32` miss | 136.3 → 133.6 | 244 → 253 | 0.33 → 0.08 | 11.9 → 13.8 | 3.91 → 4.06 |

- Ties cost branch misses, not memory: lines from memory don't move.
  Removing them saves 0.25–0.3 mispredicts a lookup.
- `hash32` hits gain 4%. `u64` loses: its internal searches scan 4 lines
  of inline keys instead of 2, and run 85 more instructions (not traced).

## Phase 1, as built

Keys move to leaves, full separators only, a walk on ties, 512 B internal
pages. Against the tree before, 10M (paths 1M, the bench's size for them),
ns per operation, one run each, `--perf`:

| cell | before | after | instructions | lines from memory |
|---|--:|--:|--:|--:|
| `u64` hit | 128 | 124 | 242 → 239 | 3.75 → 3.87 |
| `u64` miss | 90 | 94 | 221 → 214 | 2.99 → 3.31 |
| `u64` dense hit | 158 | 148 | 252 → 340 | 4.66 → 4.19 |
| `u64` dense miss | 88 | 98 | 231 → 316 | 2.99 → 3.21 |
| `hash32` hit | 181 | 160 | 270 → 366 | 5.57 → 4.64 |
| `hash32` miss | 135 | 145 | 244 → 336 | 3.90 → 4.07 |
| JSON hit | 311 | 276 | 514 → 462 | 8.29 → 7.62 |
| JSON miss | 249 | 179 | 481 → 425 | 7.22 → 5.48 |
| paths hit | 371 | 426 | 640 → 935 | 11.8 → 12.5 |
| paths miss | 252 | 312 | 574 → 867 | 7.38 → 8.91 |
| `u64` random insert | 146 | 136 | 352 → 348 | 7.52 → 7.38 |
| `hash32` random insert | 193 | 171 | 451 → 427 | 10.9 → 10.2 |
| JSON random insert | 289 | 241 | 618 → 610 | 12.2 → 11.1 |
| `u64` random remove | 249 | 249 | 657 → 657 | 6.67 → 7.16 |
| `u64` `range64` | 652 | 630 | 1603 → 1598 | 34.7 → 35.5 |

- Inserts, hits on random keys and JSON gain; JSON misses most, −28%.
- Misses on `u64`, dense `u64` and `hash32` lose 5–11%, and paths 15–24%:
  dense `u64` and paths walked for their per-node prefix on every search
  of the root, and ties walk, about once per path lookup. D50 removed the
  prefix walks: against this table, dense `u64` 340 / 316 → 294 / 270
  instructions, paths 935 / 867 → 642 / 575, 426 / 312 → 375 / 325 ns
  (D50 has the rest).

## Plan

1. Done: the census and the pricing of ties.
2. Done: keys move to leaves, a walk on ties, 512 B internal pages.
3. Done (D50): the tree keeps the bytes every key shares, so no search
   reads a key for a prefix.
4. Ties: exact separators, choosing where to split, going right first, or
   pointers in upper levels, by which the tie counts call for.

## Open questions

- **Keys without inline keys** have nothing else to search an internal
  node by, so their internal nodes still hold keys, and D17 with its heads
  stays for them: the crate keeps two kinds of separator unless every key
  type gets an inline key. Small std integers, for example, would be their
  own exact inline key.
- **The shared prefix's cap,** 64 bytes (D50): keys that share more skip
  only 64 on the borders, whose windows then tie. The bench's paths share
  exactly 64.
