# 11 — Separator keys: internal nodes without keys

Status: proposed (2026-10-13), not built. It would change D17 (separators
kept in internal nodes, values in heads) and touches D26 (fences), D33
(local prefixes), D36 (length tags) and D43. It applies to keys with inline
keys; keys without them keep D17 (see "Open questions").

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

What this costs is cache residency, not memory. Internal nodes are a few
percent of a tree's bytes, but every lookup crosses them, and saving a
level saves no miss when that level is warm anyway (06, lessons 1 and 2). At 10M
entries, `u64` internal nodes take about 6 MiB, which fits one 32 MiB L3,
and `String` internal nodes about 30 MiB, which barely does (06).
Without keys, the `String` and `[u8; 32]` internal levels would shrink to
about a third and a quarter.

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
- **Leaves.** Paired leaves already keep a place for the head's key in
  entry 0, unused (`(_ head)` in `layout.rs`, D29). In `u64 → u64` leaves
  it is free (bytes 128–136), so capacity stays 55. In `String → u64`
  leaves, the D36 length tags (bytes 200–248) extend into it (224–248), so
  either the tags move or capacity drops from 24 to 23. Leaves without inline
  keys need room for one more key.
- **Internal nodes.** A header, inline keys and edges. At 1 KiB that is
  about 100 separators for `u64`, `u128` and `[u8; 32]`, and 84 for
  `String`, against 56, 38, 24 and 28 today. At 512 B it would be about 49,
  49, 49 and 41: today's fanout or more, in half the bytes.

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

- **Exact.** In a node that skips `p` bytes before its windows (its fence
  prefix, or its local prefix, D33), with `w`-byte inline keys, `s` is
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
- **Local prefixes (D33).** In a node with a local prefix, the search
  checks the query against key 0, now in a leaf. That node is the root,
  the tree's borders or a node of dense keys, on every lookup. The node
  must store its local prefix's bytes, up to a cap, from the space the
  keys free.
- **Window recomputation (D43).** Keys no longer move from internal nodes
  to leaves. A separator that moves up still needs its window at the new
  prefix: an exact one has its node's prefix bytes and its window, and a
  full one's bytes are its key in the leaf.
- **Heads (D17).** For keys with inline keys, heads and everything that
  serves them go away: `has_head`, `EntryPosition::Separator`
  (`tree/walk.rs`), `into_value_head`, the successor key that moves up on
  removal (`tree/remove.rs`), and range iterators reading a head's key from
  the path. Every entry is in a leaf.

## Not pursued

- **Children before keys in every internal node**, the first step
  considered. D39 already does this for byte arrays, and its gain came
  from 32-byte keys no longer crossing a cache line. A `u64` node's
  inline keys end at byte 120, and in either order edge `i` sits in a line
  of its own except edge 0. `u128` keys are already within lines. Heap
  strings would lose a line in nodes with a local prefix, which read key 0
  right after the inline keys (why D39 excludes them). This change also
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
  would save are warm (06; 2 KiB internal nodes were slower). 512 B
  internal pages keep today's fanout. They change D15, so they are a
  separate experiment.

## Measure first

Each of these runs at 10M on random `u64`, dense `u64`, JSON, paths and
`hash32`:

1. **How many separators would be exact,** per level, at 2- and 4-byte
   inline keys, with the choice rule, with and without choosing where to
   split. A count over a built tree, no timing.
2. **Ties today:** full-key reads in internal nodes per lookup, per level,
   separating ties from local-prefix checks (D33).
3. **Where DRAM fills land:** per hit, internal levels against the leaf,
   from precise sampling.

If (1) is high and (3) shows internal fills for `String` and `hash32`,
the change should pay on lookups. If the internal levels are already
L3-resident, it is a simplification, judged by the code it removes.

## Plan

1. The measurements above.
2. Keys move to leaves: full separators only, a walk on ties, 1 KiB pages.
   Routing is the same as today; only node contents and the cost of a tie
   change.
3. Exact separators, and choosing where to split.
4. Go right first, or pointers in upper levels, if (2)'s tie counts call for
   it.
5. 512 B internal pages (D15).

## Open questions

- **Keys without inline keys** have nothing else to search an internal
  node by, so their internal nodes still hold keys, and D17 with its heads
  stays for them. The crate keeps two kinds of separator unless every key
  type gets an inline key. Small std integers, for example, would be their own exact
  inline key.
- **Length tags in `String` leaves:** where they go once the head's key
  place is taken.
- **How many local-prefix bytes** an internal node stores, and what a node
  whose keys share more than that does.
