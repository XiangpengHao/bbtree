# 05 — Fence keys: per-node prefix stripping

Status: implemented (D26), in `tree/prefix.rs` and the node operations of
`tree/node/leaf.rs` and `tree/node/internal.rs`; measured in "Measured". Builds on inline keys
(04-inline-key.md). Applies to every key with inline keys: std byte strings,
byte arrays and the 64- and 128-bit integers under `Auto`, and keys with an
`InlineKey` impl under `WithInlineKey`, through their encoding. Where a
fence is missing, on the root and the borders, the root's fences, the
tree's shared prefix, stand in (D50, "The shared prefix"), in
`tree/root.rs` and `tree/shared_prefix.rs`.

## Problem

8-byte inline keys tie often on real keys, so many probes still read the heap.
The fraction of sorted neighbours sharing their first 8 bytes is 36% for crate
names, 44% for Wikipedia titles at 1M (65% at 10M) and 99% for Linux paths
(04-inline-key.md, Q7).

Ties cost more than a heap read. When every inline key in a node ties, the
inline search decides nothing and costs about 100 instructions per level before
the full-key search starts. With 8-byte inline keys, paths run at 1.2–1.7× std's
time (04-inline-key.md, section 5). Prefix stripping removes most of the cause.

## Idea: the separators are the fences

Every node has two fences, `low` and `high`: the separators beside it in its
ancestors, the nearest on each side. D17 keeps separators as real keys: in
internal nodes, or with inline keys, as leaf heads (D49).

- **`prefix_len = LCP(low, high)`, exactly.** Every key `k` with
  `low ≤ k < high` starts with `P = LCP(low, high)` and is at least `|P|`
  bytes long. A shorter key would be a prefix of `low` and sort before it.
- So a node needs only **`prefix_len: u16`** in its header, in every node
  with inline keys (`page.rs`). Its inline keys are its keys' windows right
  after the prefix, of which it keeps its width.
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
  their prefix is what every key of the tree shares, which the tree keeps
  and every search checks once, at the top (D50, "The shared prefix").

## Inline keys belong to a node

An inline key is not a function of the key alone: it is the key's
**window** at its node's `prefix_len`, the 8 bytes from there, of which the
node keeps its width.

- **Stored:** inline key `i` is key `i`'s window at the node's `prefix_len`
  (`Window` in `tree/node/inline_keys.rs`: an inline key with its prefix length).
- **Search target:** the search key's window at each node's `prefix_len`,
  which sits in the header beside `len`. A std byte string being searched
  for is windowed from its own bytes (`SearchBytes`): one load where 8
  bytes remain, overlapping fixed-size loads near its end, so no `memcpy`
  (D32).
  - **Taken where it is used.** Taking it once per node and handing it to
    the node search went through the stack, written right before the search
    read it back, a store this CPU (Zen 5) can't always forward: every level
    stalled, `hash32` gets +14%. Each node search takes it itself.
  - **One load more per level:** the window's address depends on
    `prefix_len`, which comes with the node, so the inline-key compare
    starts a load later than with a window taken before the node arrives.
    That is what `hash32` pays (see "Measured").
- **Ties:** std byte strings compare from past the prefix and the bytes the
  node keeps of each window, 8 bytes at a time as big-endian words, then by
  length (`SearchBytes::cmp_from`): every key of the node and the search key
  share the prefix, and tied windows the bytes kept. Loads of a fixed size only, so no `memcmp`; a key that ends
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
  encodings say nothing. Windows at offset 0 are inline keys without
  stripping.
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
- **Stricter than a summary:** one that orders consistently but is not a
  window of one encoding does not qualify.
- **Inside the map:** `KeyInliner` gives both strategies the same two
  operations, a key's window at a prefix and the common prefix length of two
  keys. `Auto` reads std byte strings' bytes directly, which lets ties skip
  the bytes known equal.
- **When user code runs.** Searches call `inline_key_at`, before any change.
  An operation then changes the tree's shape without calling it, so that a
  panic can't stop a split, a merge or a cut halfway, which unchecked code
  would trust (D48): the iterators trust the map's length with unchecked
  moves, and a half-done rebalance would leave a node under its minimum.
  Once the entry is in or out and counted, it calls it to fit the nodes
  whose fences changed. An insert also takes the new key's window before
  the first change, and a separator's removal shrinks the border its
  successor widens before the first change. A node being fit writes each
  window as it computes it, and its prefix length last: a panic there leaves
  windows off the prefix, which only makes searches wrong. A wrong impl
  gives wrong answers, never undefined behaviour.
- **Nodes keep `InlineKey::WIDTH` bytes of each window** (see "Width").
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

The test-only checker verifies both on every tree (`tree/invariants.rs`).
Correctness needs less: any `prefix_len` up to `LCP(low, high)` gives right
answers, only shorter windows. That is what an operation keeps in the middle.

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
  (`Child::refit_prefix`): each reads its two fences, and, if what they
  share changed, its keys. A node whose first and last inline keys differ
  in their first byte reads neither: its fences lie outside its keys, so
  they share no more than its prefix, and fitting only ever grows one. On
  inserts, that settles 84% of split halves for `str-json` and 66% for
  `str-16` without a heap read; a prefix changes in 13% and 28%. A node
  whose prefix changes writes each window as it reads its key, then its
  prefix length.

| operation | fences that change | during | once counted |
|---|---|---|---|
| insert, no split | none | the new key's window at the leaf's prefix | — |
| split | both halves': the separator between them is new | both keep the prefix; the separator going up moves back to the parent's, filled from the parent's key beside it | each half of each split, from the leaves up |
| merge | the merged node's widen | it takes the parent's prefix: both halves move back, filled from the separator, which comes down at its window | the merged node |
| steal | the receiver's widen, the giver's narrow; their children's don't | the receiver takes the parent's prefix, filled from the separator, which comes down at its window; the key going up moves back the same way | both |
| removal of separator `s`, replaced by its successor `s'` (D17) | the right border of the subtree left of `s` (upper fence `s` → `s'`) and the left border of the subtree right of it (lower fence `s` → `s'`) | before any change, the first border's nodes shrink to `min(own, LCP(s, s'))`, reading keys; then `s'` moves back to `s`'s node's prefix, filled from `s` | the second border's nodes |
| `split_off` (D25) | the node stack's nodes become borders; the nodes beside the borders give them entries | before the cut, every node on the node stack takes the root's prefix, the shared prefix, from the root down, each filled from its parent's key beside the node stack; both trees keep the shared bytes | once both maps are counted, the nodes beside each border |
| builder (D25) | each node's, as it closes; the left siblings `finish` takes entries from | — | the nodes a new leaf closes, from the leaf up, once it hangs after them; after `finish`, the nodes beside the right border |
| raising or lowering the tree (D52), clone | none | a raised root has the shared prefix, and a lowered root's child had it; a copy copies prefixes and the shared bytes | — |
| insert of a key that shares less, join | the borders' missing ones: the shared bytes shrink | every border node moves back to the new length, filled from the shared bytes' window there | — |

- **Why the first border of a removed separator shrinks first.** Its
  fences widen, so its prefixes must shrink before `s'` replaces `s`. A
  node there has fences `low` and `s`, so its new prefix is
  `min(own, LCP(s, s'))`, with no fence read: `low ≤ s ≤ s'`. It can't be
  skipped: a search for a key in `(s, s')` goes down that border.
- **What a removal fits:** at each node it fixed, the child the node stack
  leads to and the sibling that gave it an entry; for a separator, every node
  below it on the node stack, which goes down the second border. It keeps the
  node stack's indexes up to date as merges and moves shift entries, to find
  them again. A removal that fixes nothing fits nothing.
- **Fences come from the node stack** (D24): a node's `low` is the key left of
  the node stack's edge at the nearest ancestor where that edge isn't the
  first, its `high` the key right of it at the nearest where it isn't the last
  (`NodeStackMut::fences`). Only fitting reads them. A plain search reads no
  fence.
- **A node on a border keeps the shared prefix.** A search for a key past
  the first or last that has the shared bytes goes down a border and reads
  windows there, so no fit gives a border node a longer prefix: the builder
  fits a full rightmost node only once the next leaf hangs after it, off the
  border.
- **An internal node grows at most to its children's prefixes,** which reads
  their headers: a prefix that fits its fences never passes them, but a panic
  during an earlier fit may have left one of theirs short.

## The shared prefix (D50)

The root has no parent, so no separator above it gives it fences, and a
node on a border has no separator above it on one side: they would get
nothing from fences, yet their keys often share many bytes (dense integers,
UUIDv7-like arrays, paths behind a common head). Every window there ties,
and searches compare whole keys.

- **Kept** for the root, as its fences, in the map beside the pointer to
  the root page (D52): the leading bytes of the encoding every key shares,
  up to 39,
  by their bytes for byte strings and by their encoding otherwise
  (`SharedPrefix`). A node with no separator
  above it on a side takes the root's fence there, so the root and the
  border nodes take its length as their prefix, and `prefix_len` is what
  every node's fences share.
- **Search:** a query is checked against the bytes once, before the root;
  one without them sorts before or after every key, and reads no node.
  Every query that goes down has them, so every node skips its prefix
  unchecked, as with fences. Heap strings check 32 bytes at a time;
  fixed-width keys a window at a time.
- **Upkeep:** an insert shrinks the shared prefix for its key first
  (`shrink_shared_prefix_for`): an empty tree starts from the key's bytes;
  a key that shares fewer shrinks them, and every border node moves back
  to the new length, filled from their window there, reading no key. Removals leave them, so they are what every key shares, not
  exactly what the keys share. A cut gives both trees the bytes; a join
  shrinks both to what they share with the separator; the builder checks
  each key it appends.
- **Leaves need nothing more:** a border leaf skips the shared bytes like
  any other prefix, and confirms a hit from its window and length.

## Measured

2026-10-09, Ryzen 9 9900X (Zen 5), mimalloc, AVX-512 build, bbtree only,
median of 3 at 1M, ns per call: no stripping → stripped, and the ratio.
The search key was windowed from a padded copy then, before D32.

| keys | insert_random | get_hit | get_miss |
|---|---|---|---|
| `str-json` | 127 → 145 (1.15) | 238 → 245 (1.03) | 217 → 218 (1.01) |
| `str-16` | 116 → 147 (1.26) | 256 → 264 (1.03) | 223 → 221 (0.99) |
| `hash32` | 118 → 122 (1.04) | 160 → 173 (1.09) | 149 → 161 (1.08) |
| `str-path` | 541 → 407 (0.75) | 693 → 457 (0.66) | 708 → 446 (0.63) |
| Wikipedia titles | 193 → 174 (0.90) | 320 → 273 (0.85) | 307 → 237 (0.77) |
| BAN addresses | 325 → 210 (0.65) | 478 → 324 (0.68) | 497 → 286 (0.58) |
| `u64` | 62 → 63 (1.01) | 70 → 68 (0.96) | 60 → 58 (0.98) |

- **Keys with shared prefixes gain the most,** and more at 10M, where ties
  grow: a third to a half off (BAN addresses at 10M: gets 838 → 504,
  misses 861 → 426; prefixes then grown only where inline keys tied).
- **Keys that rarely share prefixes pay:**
  - inserts 15–26%: a split half whose fences share more reads all its
    keys, one heap read each. Skipping the fence reads where a node's own
    inline keys settle it (see "Upkeep") brought `str-json` to 143 ns, at
    no stripping's memory fills; the rest is 106 instructions, 1.1 branch
    misses and the window's load per level;
  - removals 21–23% on `str-16` and `str-json`: a merged node takes its
    parent's prefix during the merge, then reads all its keys to fit
    `min(left, right)`;
  - `hash32` gets 8–9%, with fewer instructions and no branch misses: the
    load per level that the window's address waits on.
- **A `get` calls no library function** (`objdump` of a probe built like
  the bench): no `memcpy` to pad a window, no `memcmp` to confirm a hit or
  break a tie.

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

Windows are 8 bytes; each key type keeps 2, 4 or 8 of them (D18).

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
  - A key stored in the node (`[u8; N]`) reads a line of the node it is
    searching, and after stripping 2–4 bytes rarely tie. A narrow word
    shrinks the inline array from 4 lines to 1, and its count vectorizes on
    baseline x86-64, which has 16- and 32-bit vector compares but no 64-bit
    one (04 Q6).
  - bf-tree's 2-byte preview bytes work for that reason: a tie there reads
    the key from the same page.
- **Per key type, without changing the contract.** The trait's 8-byte windows
  stay what keys give; a node keeps the top bytes of one, which is still a
  window. A key type chooses its width in its impl, a defaulted associated
  const (`InlineKey::WIDTH`), stable Rust unlike a defaulted associated
  type; `Auto` chooses for the std types it recognizes.
- **The node word is not just `Ord`:** the map builds it from bytes, shifts
  and XORs it, so it is a sealed unsigned integer (`InlineSlot`). That the
  encoding orders like the key stays the contract.
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
  per leaf), one more array to shift, and the upkeep. Measured in D31.

### Narrow slots

- **The trait is unchanged:** `inline_key_at` gives 8-byte windows, and
  `InlineKey::WIDTH` (an `InlineWidth`: `Two`, `Four`, `Eight`, defaulting
  to `Eight`) says how many of their first bytes a node keeps. Windows,
  their shifts and fills, and common prefix lengths stay 8 bytes; a node
  narrows a window as it stores it and as it compares a search's with its
  own (`InlineSlot`: `u16`, `u32`, `u64`). A window read out of a node holds
  only the kept bytes, and moved back to a shorter prefix it still holds at
  least as many right ones; nothing moves a window to a longer prefix.
- **Prefixes stay in whole bytes.** `Auto` keeps 8 for std heap strings'
  leaves and 4 for their internal nodes (D43), 2 for byte arrays (D32);
  `u64`, `i64`, `u128` and `i128` implement `InlineKey` with 2 (signed ones
  with the sign bit flipped), and `Auto` uses them (D31).
- **What the search needs:** without these, inline keys were 1.5–1.8×
  slower than none on `u64`:
  - slots past `len` hold all ones, so the count needs no mask per slot:
    `filter().count()` over every slot compiles to a few vector compares
    whose masks are counted (`popcnt`), where the masked count took about
    75 instructions per 56-slot node;
  - `SearchKey` resolves the strategy where it uses it, a constant, so the
    `InlineKey` impl inlines and a map runs only its own way, instead of
    calling through a function pointer past dead byte-string code;
  - the count goes through the node's kind, so its trip count, the kind's
    capacity, is a constant;
  - within a run of tied inline keys, keys with their own impl search as
    D19 picks: with 2-byte inline keys, dense integers tie wherever a
    node's keys share more than its prefix.
- **The count's code depended on the capacity.** LLVM vectorizes
  `filter().count()` well for some slot counts and not others. A `u64 →
  [u64; 4]` leaf has 23 slots: LLVM compared 16 in a vector, then took its
  mask apart a bit at a time (`kshiftrw`, `kmovd`, `and`, `add` per slot),
  compared the rest as scalars, and merged the leaf's and the internal
  node's counts through permutes: 1,043 instructions per `get_hit` against
  450 without inline keys. Isolated, by slot count, 2-byte slots: 16 → 15
  instructions, 23 → 45, 24 → 25, 56 → 24, 64 → 77, 112 → 140; 8-byte
  slots: 25 → 34, 26 → 43, 28 → 195.
- **So 2- and 4-byte slots count in explicit AVX-512 vectors**
  (`vector_count.rs`): the search's 2 or 4 bytes broadcast to every lane,
  one load and two compares (below, equal) per 64 bytes, over the capacity
  with constant masks, so neither waits for `len`; the compares' bits
  gathered 64 slots to a word, one `bzhi` by `len` keeps the live ones, one
  `popcnt` counts them. It reads nothing past `len` into the answer, so it
  needs no padding. Against the versions it replaced, `u64` keys, one run
  each:

  | count | ns per 10M miss | ns per 1M 1 KB hit | instructions per 1M hit |
  |---|--:|--:|--:|
  | LLVM's, over padded slots | 107 | 226 | 312 |
  | explicit, looping over `len` | — | 200 | 404 |
  | explicit, loads masked by `len` | — | 212 | 320 |
  | explicit, padded slots, counts cut at `len` | 109 | 158 | 270 |
  | explicit, each 32 slots' bits masked by `len` | 131 | 203 | 327 |
  | explicit, 64 slots' bits masked by one `bzhi` | 104 | 144 | 258 |

- **8-byte slots keep LLVM's count, but at 28 to 31 slots.** The explicit
  one measured slower on string maps: 368 against 341 ns per 10M
  `str-json` hit, with 56 more instructions per lookup; why is open. At 28
  to 31 slots, LLVM takes the compares' masks apart a bit at a time (195
  instructions for 28 slots, above), and the 31-slot internal nodes of
  8-byte inline keys count in explicit AVX-512 vectors instead, through the
  narrow slots' loop. `memtable16` and `memtable24` → 100-byte values at
  1M, one run each, ns [instructions] per op: `get_hit` 369 → 294 and 349
  → 274 [1,102 → 393], `random_insert` 240 → 148 [1,384 → 637], `range64`
  1,119 → 1,041 [3,119 → 2,418]; `u64` and `str-json` lookups and `String`
  sets keep their instructions.
  `InlineKeysOfWidth::equal_range` is `#[inline(always)]`: generic over the slot
  width alone, out of line it was one copy shared by every map with 8-byte
  slots, with the capacity a variable.
- **Measured,** `u64` keys with 2-byte inline keys against none, ns per
  operation, one repetition; a 1M map sits at the size of the L3 of one CCD,
  and moves between runs:

  | cell | none | 2 B |
  |---|--:|--:|
  | 10M `get_hit` / `get_miss` | 168 / 150 | 134 / 107 |
  | 10M `insert_random` / `insert_sequential` | 150 / 9.7 | 131 / 15.0 |
  | 10M `remove_random` / `pop_first` | 234 / 33.3 | 232 / 34.1 |
  | 10M `iterate` / `range64`, before the explicit count | 5.7 / 714 | 5.2 / 719 |
  | 1M `get_hit` / `insert_random` | 61 / 62 | 61 / 52 |
  | 1k `get_hit` / `get_miss` | 13.1 / 12.6 | 6.4 / 4.3 |
  | 1k `insert_random` / `insert_sequential` | 16.3 / 5.3 | 14.5 / 8.3 |
  | 1k `remove_random` / `pop_first` | 43.4 / 27.5 | 48.7 / 24.8 |
  | 1M `u64` → 32-byte struct, `get_hit` | 138 | 109 |
  | 1M `u64` → 1 KB inline value, `get_hit` | 153 | 148 |
  | bytes per entry, `u64` → `u64` / set | 23.5 / 11.8 | 26.4 / 14.9 |

  The 1 KB values gain even though their tree is a level taller: 12-entry
  leaves and internal nodes of 56 keys rather than 63 put the 1M map's
  leaves 4 levels below its root against 3.
- **Std byte strings, through the same changes:** `hash32` 3–16% faster,
  `str-path` and `kernel-paths` within 3%, `str-json` within 4% at 1M and
  8% slower at 10M, and 1k `str-16` and `crate-names` lookups about 20%
  slower (29 → 35 ns, five repetitions each), all with 8–18% fewer
  instructions, and none of it from the count: their 8-byte count is the
  same code as before the explicit one.
- **Sequential inserts** with integer inline keys lost only to the insert
  path's append check (D30): without it, 40.1 against 44.5 ns per 10M
  insert, and `Auto` gives integers inline keys (D31); every entry takes 2
  bytes more.

## Open questions

- **Merges and steals read every key of the nodes they change** (see
  "Measured"). Doing them exactly in place would read one or two keys: the
  merged node's prefix is `min(left, right)`, a shift filled from the
  separator's window there; a receiver's is `min(own, LCP(k, s))`. But
  that reads in the middle of a rebalance, which with user `InlineKey`
  impls needs an abort-on-unwind guard (D48).
- **Inserts on keys that rarely share prefixes** pay 15–26% over no
  stripping: the keys a growing half reads.
- **The window's load per level** (`hash32`, 8–9%): a target that doesn't
  wait on the node's `prefix_len`, such as the windows at a few prefix
  lengths kept in registers and picked by a shift, might hide it.
- **1k string lookups** (see "Narrow slots"): about 20% slower after the
  narrow-slot changes, with fewer instructions; why is open.
- **The padding past `len`** (invariant 6) is now needed only by LLVM's
  counts: 8-byte slots but 28 to 31 of them, and every width without
  AVX-512. Explicit counts
  for those, masking by `len` as the AVX-512 one does (AVX2's
  `movemask`, NEON's lane compares), would let the nodes drop it, and its
  upkeep on every shrink.
