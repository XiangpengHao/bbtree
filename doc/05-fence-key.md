# 05 — Fence keys: per-node prefix stripping

Status: draft. Builds on inline keys (04-inline-key.md); to start after they
land. Applies only to byte-string keys (the std types `Auto` recognizes, or
opted-in byte-string keys), since it needs the key's bytes.

## Problem

8-byte inline keys tie often on real keys, so many probes still read the heap.
The fraction of sorted neighbours sharing their first 8 bytes is 36% for crate
names, 44% for Wikipedia titles at 1M (65% at 10M) and 99% for Linux paths
(04-inline-key.md, Q7).

## Idea: the separators are the fences

Every node has bounds `[low, high)`: the separators bounding it in its
ancestors. D17 keeps those separators as real keys in internal nodes.

- Every key `k` with `low ≤ k < high` starts with `P = LCP(low, high)` and is
  at least `|P|` bytes long. A shorter key would be a prefix of `low` and sort
  before it.
- So a node needs only **`prefix_len: u16`** in its header. Its inline keys are
  the 8 bytes right after the prefix.
- **No fence keys are stored, and no `K: Clone`.** The prefix bytes are in every
  key already. This doesn't conflict with D10, which rejected *storing* fence
  copies.
- **Search:** a key we descended to is inside the node's bounds, so the search
  skips `prefix_len` bytes of it with no check.
- The leftmost and rightmost nodes have an infinite fence on one side, so their
  prefix is 0.

## Invariant

`prefix_len ≤ LCP(low, high)`, and every inline key equals the 8 bytes of its
key after `prefix_len`.

A smaller `prefix_len` is still correct, only less effective. That leaves
freedom in when to recompute.

| operation | bounds | work |
|---|---|---|
| insert, no split | unchanged | none (the new key's inline key uses the node's `prefix_len`) |
| split | both halves narrow | optional: grow each half's prefix and recompute its inline keys while moving entries |
| merge, steal | widen | recompute `prefix_len` and the inline keys of the nodes being rewritten anyway |
| remove of a separator key (replaced by its successor, D17) | nodes along the two paths beside it may widen | recompute those nodes: at most 2 × height |
| root push / pop | children's bounds unchanged | none |

Bounds come from the insert/remove recursion, which already holds the path: it
passes `low` and `high` as byte slices of the ancestor separators. Bound keys
are read only on structural changes, never on a plain search.

The invariant checker verifies both parts on every test tree.

## Expected benefit (simulation on real keys)

Sorted keys chopped into leaf-sized runs of 17, about a 1 KB `String` leaf 70%
full. Each leaf's prefix is `LCP` of its separators; inline keys are 8 bytes.
Fraction of neighbouring keys inside a leaf whose inline keys tie:

| keys | no stripping | leaf prefix stripping | mean leaf prefix |
|---|---|---|---|
| crate names (347k) | 36.3% | **9.7%** | 4.1 B |
| Wikipedia titles (~1M sample) | 43.9% | **9.2%** | 5.1 B |
| Linux paths (87k) | 99.0% | **39.0%** | 22.5 B |

Stripping with 8 bytes about matches 16 bytes without stripping for crates and
Wikipedia, at half the space per entry. It's the only thing that helps paths.

Caveats:
- leaves only, at a fixed fill;
- internal nodes near the root have wide bounds, so their prefixes are short and
  gain little.

## Plan

1. `prefix_len` in the leaf header, added with inline keys so the layout
   doesn't need reshaping later.
2. Bounds passed down the insert recursion; recompute on split.
3. `remove` designed with merge, steal and separator replacement keeping the
   invariant.
4. Invariant checks.
5. Measure on the real key sets (crates, Wikipedia, paths at 1M and `all`)
   against 8-byte inline keys without stripping and 16-byte inline keys without
   stripping.

## Open questions

- **Internal nodes too, or leaves only?** Decide by measurement.
- **Split policy:** always grow the prefix at a split (tighter inline keys, an
  extra pass over the moved entries), or only when it grows by at least 8
  bytes.
