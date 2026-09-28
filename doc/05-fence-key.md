# 05 — Fence keys: per-node prefix stripping

Status: implemented (2026-10-08), exact since 2026-10-09, in `node/prefix.rs`
and the node operations of `node/handle.rs`; measured in "Measured". Builds on inline keys
(04-inline-key.md). Applies to every key with inline keys: std byte strings
under `Auto`, and keys with an `InlineKey` impl under `WithInlineKey`, through
their encoding.

## Problem

8-byte inline keys tie often on real keys, so many probes still read the heap.
The fraction of sorted neighbours sharing their first 8 bytes is 36% for crate
names, 44% for Wikipedia titles at 1M (65% at 10M) and 99% for Linux paths
(04-inline-key.md, Q7).

Ties cost more than a heap read. When every inline key in a node ties, the
inline search decides nothing and costs about 100 instructions per level before
the full-key search starts. With 8-byte inline keys, paths run at 1.2–1.7× std's
time (04-inline-key.md, section 5). There is no separate guard for nodes whose
inline keys all tie: prefix stripping removes the cause.

## Idea: the separators are the fences

Every node has two fences, `low` and `high`: the separators beside it in its
ancestors, the nearest on each side. D17 keeps separators as real keys in
internal nodes.

- **`prefix_len = LCP(low, high)`, exactly.** Every key `k` with
  `low ≤ k < high` starts with `P = LCP(low, high)` and is at least `|P|`
  bytes long. A shorter key would be a prefix of `low` and sort before it.
- So a node needs only **`prefix_len: u16`** in its header, already reserved
  in every node with inline keys (`page.rs`). Its inline keys are the 8 bytes
  right after the prefix.
- **An insert never changes a prefix:** a key that goes into a node lies
  between its fences already. Only a separator change does: a split adds
  one, a merge takes one out, a move between siblings or a removal replaces
  one, a cut leaves nodes without one.
- **A child's prefix is never shorter than its parent's,** for free: its
  fences lie within its parent's, and for `a ≤ b ≤ c`,
  `LCP(a, c) = min(LCP(a, b), LCP(b, c))`.
- **No fence keys are stored, and no `K: Clone`.** The prefix bytes are in every
  key already. This doesn't conflict with D10, which rejected *storing* fence
  copies.
- **Search:** a key we descended to is between the node's fences, so the search
  skips `prefix_len` bytes of it with no check.
- The leftmost and rightmost spines have an infinite fence on one side, so
  their prefix is 0.

## Inline keys belong to a node

An inline key is no longer a function of the key alone: it is the key's
**window** at its node's `prefix_len`, the 8 bytes from there.

- **Stored:** inline key `i` is key `i`'s window at the node's `prefix_len`
  (`Window` in `node/inline_keys.rs`: an inline key with its prefix length).
- **Search target:** the search key's window at each node's `prefix_len`,
  which sits in the header beside `len`. A std byte string being searched
  for is copied once per search into a 128-byte buffer, zero-padded
  (`SearchBytes`): each node's window is then one load from it at
  `prefix_len`, with no length to check and nothing to pad. Copying and
  windowing use loads of a fixed size only, so no `memcpy`.
  - **Taken where it is used.** Taking it once per node and handing it to
    the node search went through the stack, written right before the search
    read it back, a store this CPU (Zen 5) can't always forward: every level
    stalled, `hash32` gets +14%. Each node search takes it itself.
  - **One load more per level:** the window's address depends on
    `prefix_len`, which comes with the node, so the inline-key compare
    starts a load later than with a window taken before the node arrives.
    That is what `hash32` pays (see "Measured").
- **Ties:** std byte strings compare from byte `prefix_len + 8`, 8 bytes at a
  time as big-endian words, then by length (`SearchBytes::cmp_from`): every
  key of the node and the search key share the prefix, and tied windows the
  next 8 bytes. Loads of a fixed size only, so no `memcmp`; a key that ends
  within those bytes isn't read at all, which covers what an
  equal-by-length shortcut did. Other keys compare with `Ord`.
- **Moving between nodes:** a run moving to a node with the same prefix is
  copied, as a split does. A key moving up into a node with a shorter prefix
  moves back to it (see "Upkeep"). `KeySlots` stamps every key it reads with the node's
  prefix length, and debug builds check every key written.

## The trait: windows of an order-preserving encoding

```rust
pub trait InlineKey: Ord {
    /// Bytes `offset..offset + 8` of this key's encoding, big-endian,
    /// zero-padded past its end.
    fn inline_key_at(&self, offset: usize) -> InlineWord;
}
```

- **The contract:** each key has a byte-string encoding, and if `a`'s
  encoding orders before `b`'s, both zero-padded, then `a < b`. Equal
  encodings say nothing. Windows at offset 0 are today's inline keys.
- **Why windows, not one summary:** skipping `prefix_len` bytes needs keys to
  have bytes, and keeping prefixes valid needs the common prefix length of
  two keys, which windows give 8 bytes at a time (XOR, leading zeros).
- **Implementing it:** std byte strings window their bytes. A newtype
  delegates in one line. A composite key encodes its fields in order:
  `(priority: u32, name)` as `priority.to_be_bytes()` then `name`.
- **Zero padding is safe.** All of the above holds on zero-padded encodings:
  `"ab"` and `"ab\0"` have the same windows, so they tie and compare in full.
  A key shorter than the prefix is a prefix of the others, windows to zero,
  and sorts first, which matches.
- **Stricter than today:** a summary that orders consistently but is not a
  window of one encoding no longer qualifies. At 0.1.0, changing it costs
  nothing.
- **Inside the map:** `KeyInliner` gives both strategies the same two
  operations, a key's window at a prefix and the common prefix length of two
  keys. `Auto` reads std byte strings' bytes directly, which lets ties skip
  the bytes known equal.
- **When user code runs.** Searches call `inline_key_at`, before any change.
  An operation then changes the tree's shape without calling it, so that a
  panic can't stop a split, a merge or a cut halfway: the iterators trust
  the map's length with unchecked moves, and a half-done rebalance would
  leave a node under its minimum. Once the entry is in or out and counted,
  it calls it to fit the nodes whose fences changed. An insert also takes
  the new key's window before the first change, and a separator's removal
  shrinks the border its successor widens before the first change. A panic
  leaves the map unchanged, or, from a last step, with the entry in or out;
  valid either way, since a node being fit computes every window into a
  buffer before writing any. A wrong impl gives wrong answers, never
  undefined behaviour.
- **8 bytes for every key type, for now;** each key type chooses its own
  width later (see "Width").
- **Capped at `u16::MAX`:** two different keys can have equal encodings (a
  lossy encoding, or `"ab"` and `"ab\0"`), so the common prefix scan stops
  there. A smaller prefix is always valid.
- **Not chosen:**
  - **Stripping for `Auto` only, trait unchanged:** `WithInlineKey` keys
    (newtypes, memtable keys) would get none, and the map would keep two
    inline-key models.
  - **`fn key_bytes(&self) -> &[u8]`:** needs the encoding stored
    contiguously, which rules out composite keys.

## Invariant

- `prefix_len = LCP(low, high)`, capped at `u16::MAX`: 0 along the tree's
  left and right borders, which have no fence on one side;
- every inline key is its key's window at `prefix_len`.

The test-only checker verifies both on every tree (`map/invariants.rs`).
Correctness needs less: any `prefix_len` up to `LCP(low, high)` gives right
answers, only shorter windows. That is what an operation keeps in the middle,
and what a panic in its last step leaves.

## Upkeep

An operation does its upkeep in two steps:

- **During the change, no key is read.** A node whose prefix must shrink
  shifts its windows: shrinking by `d` bytes, every window keeps its top
  `8 − d` bytes, moved down, under `d` bytes that every key of the node
  shares (`Window::moved_back_to`). A fence has them, and the parent's key
  beside the node is one, with its window at the parent's prefix: so a node
  can always take its parent's prefix, and a key moving up always has the
  bytes it needs, both filled from that key. At `d ≥ 8`, every window is
  that one.
- **Once the entry is counted, the nodes whose fences changed fit them**
  (`NodeMut::refit_prefix`): each reads its two fences, and, if what they
  share changed, its keys. A node whose first and last inline keys differ
  in their first byte reads neither: its fences lie outside its keys, so
  they share no more than its prefix, and fitting only ever grows one. On
  inserts, that settles 84% of split halves for `str-json` and 66% for
  `str-16` without a heap read; a prefix changes in 13% and 28%. A node
  whose prefix changes computes every window into a buffer of 128 on the
  stack before writing any (more than any node with inline keys holds: each
  takes 8 bytes of a 1 KB node for its inline key alone).

| operation | fences that change | during | once counted |
|---|---|---|---|
| insert, no split | none | the new key's window at the leaf's prefix | — |
| split | both halves': the separator between them is new | both keep the prefix; the separator going up moves back to the parent's, filled from the parent's key beside it | each half of each split, from the leaves up |
| merge | the merged node's widen | it takes the parent's prefix: both halves move back, filled from the separator, which comes down at its window | the merged node |
| steal | the receiver's widen, the giver's narrow; their children's don't | the receiver takes the parent's prefix, filled from the separator, which comes down at its window; the key going up moves back the same way | both |
| removal of separator `s`, replaced by its successor `s'` (D17) | the right border of the subtree left of `s` (upper fence `s` → `s'`) and the left border of the subtree right of it (lower fence `s` → `s'`) | before any change, the first border's nodes shrink to `min(own, LCP(s, s'))`, reading keys; then `s'` moves back to `s`'s node's prefix, filled from `s` | the second border's nodes |
| `split_off` (D25) | the path's nodes become borders; the nodes beside the borders give them entries | before the cut, every node on the path takes prefix 0, from the root down, each filled from its parent's key beside the path | once both maps are counted, the nodes beside each border |
| builder (D25) | each node's, as it closes; the left siblings `finish` takes entries from | — | the closing nodes, from the leaf up; after `finish`, the nodes beside the right border |
| root push / pop, clone | none | a new root has prefix 0; a copy copies prefixes | — |

- **Why the first border of a removed separator shrinks first.** Its
  fences widen, so its prefixes must shrink before `s'` replaces `s`, and
  reading keys then still leaves the map unchanged on a panic. A node there
  has fences `low` and `s`, so its new prefix is `min(own, LCP(s, s'))`, with
  no fence read: `low ≤ s ≤ s'`. It can't be skipped: a search for a key in
  `(s, s')` goes down that border.
- **What a removal fits:** at each height it fixed, the child the path
  leads to and the sibling that gave it an entry; for a separator, every node
  below it on the path, which goes down the second border. It keeps the
  path's indexes up to date as merges and moves shift entries, to find them
  again. A removal that fixes nothing fits nothing.
- **Fences come from the path** (D24): a node's `low` is the key left of the
  path's edge at the nearest ancestor where that edge isn't the first, its
  `high` the key right of it at the nearest where it isn't the last
  (`PathMut::bounds`). Only fitting reads them. A plain search reads no
  fence.
- **An internal node grows at most to its children's prefixes,** which reads
  their headers: a prefix that fits its fences never passes them, but a panic
  during an earlier fit may have left one of theirs short.

## Measured

### First version: prefixes grown only where inline keys tie

2026-10-08, Ryzen 9 9900X (Zen 5), mimalloc, AVX-512 build, bbtree only,
against the commit before (8-byte inline keys without stripping). ns per
call; median of 3 at 1M, one run at 10M. The first version kept
`prefix_len ≤ LCP(low, high)`, grew a split half's prefix only if two
neighbouring inline keys in it tied, and gave merged and rebalanced nodes
their parent's prefix until they split again.

| keys | size | insert_random | get_hit | get_miss |
|---|---|---|---|---|
| `str-path` | 1M | 541 → 409 (0.76) | 691 → 481 (0.70) | 706 → 465 (0.66) |
| BAN addresses | 1M | 324 → 215 (0.67) | 477 → 336 (0.70) | 489 → 301 (0.62) |
| dblp titles | 1M | 323 → 258 (0.80) | 450 → 342 (0.76) | 438 → 299 (0.68) |
| Wikipedia titles | 1M | 193 → 179 (0.93) | 318 → 284 (0.89) | 304 → 247 (0.81) |
| Wikipedia titles | 10M | 461 → 361 (0.78) | 567 → 430 (0.76) | 575 → 366 (0.64) |
| BAN addresses | 10M | 701 → 410 (0.58) | 838 → 504 (0.60) | 861 → 426 (0.49) |
| `str-json` | 1M | 126 → 137 (1.09) | 237 → 243 (1.02) | 218 → 220 (1.01) |
| `str-16` | 1M | 119 → 130 (1.10) | 255 → 258 (1.01) | 221 → 225 (1.02) |
| `hash32` | 1M | 115 → 122 (1.06) | 159 → 160 (1.01) | 148 → 151 (1.02) |
| `u64` | 1M | 63 → 63 (1.00) | 66 → 67 (1.03) | 61 → 59 (0.97) |

- **Keys with shared prefixes gain the most,** and more at 10M, where ties
  grow: a third to a half off at 10M.
- **Keys that rarely tie keep their gets,** within 2%, and pay 6–10% on
  inserts: about 100 more instructions each, from the target taken at each
  node of the insert's search, twice where the search expects an append,
  and the new key's window taken at its leaf. Taking the target once per
  node and inlining both searches that use it made gets 22–41% slower
  instead.
- **`u64`** has no inline keys, and is unchanged.

### Exact prefixes

2026-10-09, same machine, idle; median of 3 at 1M, ns per call.
`remove_random` removes every key in random order, so the tree shrinks to
nothing and merges often.

| keys | operation | no stripping | first version | exact |
|---|---|---|---|---|
| `str-16` | insert_random / get_hit / remove_random | 117 / 254 / 295 | 131 / 253 / 297 | 147 / 267 / 364 |
| `str-json` | same | 127 / 240 / 271 | 134 / 240 / 274 | 157 / 263 / 328 |
| `hash32` | insert_random / get_hit | 116 / 160 | 123 / 161 | 127 / 168 |
| `str-path` | insert_random / get_hit / remove_random | 540 / 688 / 602 | 411 / 479 / 537 | 413 / 483 / 522 |
| Wikipedia titles | same | 194 / 319 / 332 | 175 / 286 / 321 | 177 / 280 / 372 |
| BAN addresses | insert_random / get_hit | 324 / 481 | 218 / 333 | 215 / 334 |

- **Keys with shared prefixes keep the gains,** and `str-path` removals gain
  3% more: rebalanced nodes no longer lose their prefixes.
- **Keys that rarely share prefixes pay more,** against the first version:
  - inserts +12–17% on `str-16` and `str-json`: a split half whose fences
    share more reads all its keys, one heap read each, about one per insert;
  - gets +4–10% on `str-16`, `str-json` and `hash32`: nodes the first version
    left at prefix 0 have one, and the search takes its target at each of
    them instead of once;
  - removals +16–23% on `str-16`, `str-json` and Wikipedia titles: a merged
    node takes its parent's prefix during the merge, then reads all its keys
    to fit `min(left, right)`.

### Search from a padded copy, ties without `memcmp`

2026-10-09, same machine; median of 3 at 1M, ns per call: no stripping /
exact (above) / exact with the search key's padded copy and word-by-word
ties, and the last against no stripping.

| keys | insert_random | get_hit | get_miss |
|---|---|---|---|
| `str-json` | 127 / 154 / 145 (1.15) | 238 / 258 / 245 (1.03) | 217 / 236 / 218 (1.01) |
| `str-16` | 116 / 146 / 147 (1.26) | 256 / 268 / 264 (1.03) | 223 / 235 / 221 (0.99) |
| `hash32` | 118 / 125 / 122 (1.04) | 160 / 169 / 173 (1.09) | 149 / 153 / 161 (1.08) |
| `str-path` | 541 / 412 / 407 (0.75) | 693 / 475 / 457 (0.66) | 708 / 466 / 446 (0.63) |
| Wikipedia titles | 193 / 175 / 174 (0.90) | 320 / 284 / 273 (0.85) | 307 / 257 / 237 (0.77) |
| BAN addresses | 325 / 216 / 210 (0.65) | 478 / 333 / 324 (0.68) | 497 / 306 / 286 (0.58) |
| `u64` | 62 / 63 / 63 (1.01) | 70 / 66 / 68 (0.96) | 60 / 59 / 58 (0.98) |

- **A `get` calls no library function** (`objdump` of a probe built like the
  bench): before, it called `memcpy` to pad a short key's window, at the
  root and at each node, and `memcmp` to confirm a hit or break a tie.
- **`str-json` gets are back to no stripping's,** within 3%, at fewer
  instructions (689 a hit, against 692); loads that couldn't take a
  forwarded store fell from 83M to 20M a run.
- **Keys with shared prefixes gain more:** ties skip their prefix.
- **`hash32` gets pay 8–9%,** with fewer instructions and no branch misses:
  the load per level that the window's address waits on.
- **Inserts and removals are unchanged:** the keys a fit reads dominate them.
- **Then, skipping the fence reads where a node's own inline keys settle
  it** (see "Upkeep"): inserts 143 ns on `str-json`, at no stripping's
  memory fills (3.07 against 3.03 a call), and 144 on `str-16`. What `str-json`
  still pays, 13%, is 106 instructions, 1.1 branch misses and the window's
  load per level; `str-16`, 24%, also re-reads the 0.41 keys an insert's
  growing halves hold.

## Expected benefit (simulation on real keys)

Simulated 2026-10-08 on `~/.cache/bbtree-bench` (crates and paths whole, the
others 1M-key samples). Sorted unique keys are chopped into leaf runs at 70% of
a 1 KB `String`→`u64` leaf's capacity, 17 keys at 8 bytes. A leaf's prefix is
the LCP of its fences, the first keys of its run and of the next, and 0 at both
ends; level 1 does the same over the separators.

- **Ties:** the fraction of neighbouring pairs in a node whose 8-byte inline
  keys are equal. Equal inline keys are contiguous, so the fraction of keys
  that share theirs with another is a little higher (stripped: crates 14%,
  Wikipedia 13%, paths 49%, URLs 66%).
- **Hits whole:** keys whose bytes past the prefix fit in the inline key: a
  hit on one is confirmed by length, with no heap read.

| keys | leaf ties, plain → stripped | level-1 ties, plain → stripped | mean leaf prefix | hits whole, plain → stripped |
|---|---|---|---|---|
| crate names (347k) | 36.3% → **9.7%** | 7.3% → **2.6%** | 4.1 B | 27% → 56% |
| Wikipedia titles | 43.8% → **9.2%** | 16.2% → **3.5%** | 5.1 B | 8% → 26% |
| dblp titles | 78.9% → **23.3%** | 56.0% → **16.0%** | 8.7 B | 0% → 0% |
| BAN addresses | 90.5% → **20.4%** | 71.0% → **18.2%** | 9.6 B | 0% → 0% |
| Linux paths (87k) | 99.0% → **39.0%** | 94.8% → **45.8%** | 22.5 B | 0% → 16% |
| Common Crawl URLs | 100% → **56.9%** | 99.9% → **52.4%** | 26.5 B | 0% → 4% |
| random `[u8; 32]` | 0% → 0% | 0% → 0% | 1.1 B | n/a |

- **Stripping beats widening.** Stripped 8-byte inline keys tie less than
  unstripped 16-byte ones on dblp, addresses, paths and URLs, and about as
  often on crates and Wikipedia, at half the space per entry.
- **URLs still tie 57%;** 16 bytes stripped: 34%.
- **Ties grow with the map** (Wikipedia unstripped: 44% at 1M, 65% at 10M),
  which is what stripping removes.

Caveats:
- tie structure, not time: "Measured" has that;
- a fixed fill, every prefix at its fences' LCP;
- level 1 only: nodes nearer the root have wider fences and shorter
  prefixes.

## Width

8 bytes for every key type for now. Later, each key type chooses its own.

Stripped leaf ties by width, same simulation (each width at its own leaf
capacity: 20 keys a run at 2 bytes, 14 at 16):

| width | crates | Wikipedia | paths | URLs | random `[u8; 32]` |
|---|---|---|---|---|---|
| 2 B | 62% | 59% | 78% | 79% | 3% |
| 4 B | 34% | 29% | 65% | 67% | 0% |
| 8 B | 10% | 9% | 39% | 57% | 0% |
| 16 B | 1% | 2% | 10% | 34% | 0% |

- **Narrow inline keys tie on text even after stripping.** A node's prefix is
  what its fences share, the *least* any two neighbours in it share.
  Neighbours share more past it: a median of 2 bytes, 7 or more for one pair
  in ten (crates and Wikipedia alike).
- **The width follows what a tie costs.**
  - A key behind a pointer (`String`, `Vec<u8>`) reads the heap on a tie, a
    dependent cache miss that outweighs any saving in the compares: 8 bytes or
    more.
  - A key stored in the node (`[u8; N]`) reads a line the search already
    prefetched (D20), and after stripping 2–4 bytes rarely tie. A narrow word
    shrinks the inline array from 4 lines to 1, and its count vectorizes on
    baseline x86-64, which has 16- and 32-bit vector compares but no 64-bit
    one (04 Q6).
  - bf-tree's 2-byte preview bytes work for that reason: a tie there reads
    the key from the same page.
- **Per key type, without changing the contract.** The trait's 8-byte windows
  stay what keys give; a node keeps the top bytes of one, which is still a
  window, or two for 16 bytes. A key type would choose its width in its impl:
  a defaulted associated const (`const WIDTH: usize = 8`) is stable Rust,
  unlike a defaulted associated type. `Auto` would choose for the std types
  it recognizes.
- **The node word is not just `Ord`:** the map builds it from bytes, shifts
  and XORs it, so it would be a sealed unsigned integer. That the encoding
  orders like the key stays the contract.
- **Each width needs its own search tuning** (04 Q6: the AVX-512 count is
  three 64-bit compares, and `u128` has no vector compare).

### Fixed-width keys: 2 bytes, with a prefix counted in bits

Integers, UUIDs and hashes could get inline keys too, narrower ones. Same
simulation, 1M keys each, leaves at 70% of a 1 KB leaf with `u64` values,
stripped; neighbouring pairs whose inline keys tie, and in parentheses the
keys sharing a hit's:

| keys | 1 B, byte prefix | 1 B, bit prefix | 2 B, byte prefix | 2 B, bit prefix |
|---|--:|--:|--:|--:|
| `u64`, random | 94% (14) | 27% (2.2) | 3.8% (1.2) | 0.3% |
| `u64`, dense ids | 97% (23) | 18% (1.9) | 0.5% | 0.1% |
| UUID v7 | 45% (7.9) | 20% (1.7) | 4.4% (1.1) | 0.2% |
| UUID v4 | 94% (13) | 22% (1.8) | 3.5% (1.1) | 0.2% |
| `[u8; 32]` hash | 86% (9.3) | 15% (1.4) | 3.0% (1.1) | 0.1% |

Every set ties 0% at 4 bytes.

- **2 bytes after the prefix are enough** for keys whose bytes are dense.
- **1 byte needs the prefix counted in bits.** A leaf's few dozen keys span a
  fraction of one value of the byte after a whole-byte prefix, so up to 7 of
  a 1-byte window's bits repeat what all its keys share. Counted in bits, all
  8 go to the spread. Free for narrow words: the node keeps the 8-byte window
  shifted by the leftover bits, then its top bytes; `prefix_len: u16` in
  bits still covers 8 KB.
- **1 byte buys little over 2 at 1 KB nodes:** the inline array is one or two
  cache lines either way. It pays with larger nodes.
- **For `u64`, the gain is different:** its keys are in the node, so there is
  no heap read to save. A 2-byte array would be 4× smaller than the keys,
  for a smaller hot set and a count over independent loads, which
  vectorizes on baseline x86-64; it costs 2 bytes per entry (63 → about 55
  per leaf), one more array to shift, and the upkeep. Measure against
  today's branchless search first.

## Plan

1. **Done.** The trait: `inline_key_at`, and `KeyInliner`'s window and common
   prefix length for both strategies.
2. **Done.** Windows per node: the search target per node, windows moved
   back on every move, and the upkeep in the table.
3. **Done.** Invariant checks on every test tree, and map-level tests that
   nodes get long prefixes and keep them fitting when fences move across
   clusters of keys (`map/prefix_tests.rs`).
4. **Done.** Fitting after every change of fences.
5. **Done.** Measured (see "Measured").
6. **Done (2026-10-09).** Exact prefixes, replacing the first version's
   `prefix_len ≤ LCP(low, high)` with growth gated on ties: a prefix is a
   function of the fences, so the invariant is one equality, there is no
   gate, no test-only rule that grows every prefix, no separate "at least
   the parent's" invariant, and nothing decays.

## Open questions

- **Merges and steals read every key of the nodes they change** (see
  "Measured"). Doing them exactly in place would read one or two keys: the
  merged node's prefix is `min(left, right)`, a shift filled from the
  separator's window there; a receiver's is `min(own, LCP(k, s))`. But
  that reads in the middle of a rebalance, where a panicking `InlineKey`
  would have to abort, or leave a node a key under its minimum.
- **Inserts on keys that rarely share prefixes** pay 15–26% over no
  stripping: the keys a growing half reads.
- **The window's load per level** (`hash32`, 8–9%): a target that doesn't
  wait on the node's `prefix_len`, such as the windows at a few prefix
  lengths kept in registers and picked by a shift, might hide it.
- **The fuzzer has no inline keys:** it fuzzes `u64` keys only. A target with
  string keys sharing prefixes would fuzz all of the above.
- **Per-type widths,** and fixed-width keys (see "Width").
