# 12 — Batch API: many keys, one descent

Status: proposed (2026-10-14), not built. Software prefetch is allowed in
the batch paths this doc describes, and only there: an exception to D46
that the owner granted for this design. D46 and AGENTS.md change to
name the exception when it is built.

## Problem

A lookup is a chain of dependent misses: a child's address comes out of its
parent's search, and a `String` hit then reads the entry's key and its heap
bytes (06, "Lookup"). On D45's tree at 10M, prefetch off (D46), a `u64` get
miss took 92.9 ns, JSON 235.9 ns; a random insert `u64` 137.1 ns, JSON
276.7 ns. Inserting 1M shuffled Wikipedia titles one by one took 125 ns
per key (2026-10-14, scratch run).

Consecutive operations don't overlap. Out-of-order execution keeps running
past a missing load only as far as its reorder buffer reaches (448 entries
on Zen 5), and between one operation's first miss and the next operation's
first load lies the whole rest of the first: its remaining levels, misses
and hundreds of instructions. So each operation pays its chain alone, even
when the caller has many independent keys: a join probing an index, a bulk
load in random order, `extend`.

## Idea

Interleave a batch of N independent descents in lockstep: one step visits
the next node of every lane, so while one lane's lines are on their way,
the others search. Every leaf of a B+-tree lies equally deep (D17), so the
schedule is static: every lane takes the same steps, and none needs a
scheduler, a waker or a state machine of its own.

The owner's 2020 post, "A view of async memory access in rust", ran four
linked-list walks as Rust coroutines: each prefetched its next node and
yielded, and a round-robin executor of four slots resumed it later. When
every access missed, that was 4× faster than one walk after another; in
cache it was much slower; a task switch cost about 30 ns. Its open
problems were guessing which accesses miss and finding independent tasks
("task partition"). A batch API answers both: the caller hands over
independent keys, and the tree's size says which levels miss.

Coroutines are not used here. A 30 ns switch per level would eat a 100 ns
operation, and a static schedule needs neither `Future`s nor dynamic
dispatch. The design is group prefetching (Chen, Ailamaki, Gibbons, Mowry,
ICDE 2004) on a tree whose chains all have one length. AMAC (Kocberber,
Falsafi, Grot, PVLDB 2015) and coroutine interleaving (Psaropoulos et al.,
PVLDB 2017; Jonathan et al., PVLDB 2018) handle chains of different lengths,
which a B+-tree doesn't have.

## Terms

- **Batch:** the N keys of one call.
- **Lane:** one key's descent within a batch: its search key, the node it
  is at, and its result once done.
- **Step:** one pass over the batch's live lanes, each doing one stage.
- **Stage:** what a lane does at a node between two misses:
  - *search*: run the node's search over its inline keys; it reads the
    node's header and inline-key lines;
  - *follow*: read the edge the search chose; it reads that edge's line.

  A leaf search is one stage in the first version (see "Open questions").

## Prefetch: the exception and its limits

A lane prefetches exactly the lines its next stage reads unconditionally,
right after the stage that computes their address:

| after | prefetch | lines (512 B internal, 1 KiB leaf) |
|---|---|---|
| *search* in an internal node | the chosen edge's line | 1 |
| *follow* to an internal child | its header and inline-key lines | `u64`: 4; `String`: 3 |
| *follow* to a leaf | its header and inline-key lines | `String`: 4 |

Line counts come from today's layouts (`layout.rs` tests): a `String`
internal node keeps 41 4-byte inline keys in bytes 8–172 and its edges from
176; a `u64` one keeps 31 in bytes 8–256 and its edges from 256; a `String`
leaf keeps 24 8-byte inline keys in bytes 8–200 and its entries from 224.
With AVX-512 the count reads every inline-key line, so these are the lines
the search reads.

What D46 removed doesn't come back. That prefetch fetched every key line of
a node at every level, lines a search rarely reads, and missed the edge
lines, which it always reads. Here no full key or unchosen edge is ever
prefetched, and no single-key path prefetches at all.

- **Where:** one wrapper, `prefetch_line(*const u8)`, in `tree/batch.rs`,
  called only from there. `_mm_prefetch(_MM_HINT_T0)` on x86-64. Other
  targets, which have no stable prefetch intrinsic, and Miri get a no-op.
- **Without prefetch the batch still helps.** In lockstep the next lane's
  loads sit one node search after the previous lane's, inside the reorder
  buffer, so their misses overlap anyway. That variant is measured too, and
  prefetch stays only if it beats it.

## API

```rust
impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// The values of `keys`, as `N` calls to `get` would return them.
    pub fn get_many<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<&V>; N]
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized;

    /// Inserts `entries` in order, returning what `N` calls to `insert`
    /// would return: an entry whose key is there, or came earlier in the
    /// batch, replaces its value.
    pub fn insert_many<const N: usize>(&mut self, entries: [(K, V); N]) -> [Option<V>; N]
    where
        K: Ord;
}

impl<T, S: InlineKeyStrategy<T>, A: Allocator + Clone> BBTreeSet<T, S, A> {
    pub fn contains_many<Q, const N: usize>(&self, values: [&Q; N]) -> [bool; N]
    where
        T: Borrow<Q> + Ord,
        Q: Ord + ?Sized;

    pub fn insert_many<const N: usize>(&mut self, values: [T; N]) -> [bool; N]
    where
        T: Ord;
}
```

`Extend` keeps std's signature and contract ("inserts each entry in turn")
and uses `insert_many` underneath (below).

- **Arrays, not iterators.** A constant N lets the lanes stay on the stack
  and their loops unroll. It is the shape of std's
  `HashMap::get_disjoint_mut`. A caller with a stream takes it in chunks.
- **Not drop-in.** Std has no batch lookup, so `get_many` helps only
  callers written for it. `extend` is the one path drop-in callers reach.
- **Same results as one call per key,** for any keys, duplicates in the
  batch included. A panic in `Ord` or `InlineKey` is not reconciled (D48):
  `insert_many` may then drop entries of the batch it had not placed yet,
  where one-by-one inserts would have placed them.

## Design

### Exact-key lookups: `Tree::find_many`

1. **Small trees go the single-key way.** If the root is a leaf, or the
   tree is below a length threshold (open question), each lane calls
   `Tree::find`, today's code. Nothing misses there, and lockstep is pure
   overhead (the 2020 post's in-cache result).
2. **Start.** Each lane gets its `SearchKey`. A key that lacks the shared
   prefix (D50) is done, absent (`place_outside`, as in `Tree::find`).
   Every live lane starts at the root, whose header and inline-key lines
   get prefetched.
3. **Internal levels, two steps each.**
   - *Search step:* each live lane runs `Internal::search`. `Ok(index)`
     ends the lane at a separator (`Found::Separator`, D17/D49); `Err(edge)`
     keeps it, and its edge line gets prefetched.
   - *Follow step:* each live lane reads its edge with `child_unchecked`
     and prefetches the child's header and inline-key lines.
   - *Kind, once per level:* all live lanes sit at one depth, so the first
     live lane's edge tag (D52) says whether the level below is leaves.
     The kind is decided once per level, not per lane (branch early, loop
     late).
4. **Leaf step:** each live lane runs `Leaf::find`.

A lane that ended at a separator or was absent from the start is skipped by
later steps. That is one predictable branch per lane per step. A tree has
one separator per leaf, so a random hit ends at one about once per leaf's
worth of entries: about 1 in 17 for 24-key `String` leaves at 70% fill.

```text
lanes: [Lane; N]            // search key, node, result
prefetch root lines
loop over internal levels:
    for lane in live:  search -> separator (done) | edge (prefetch edge line)
    for lane in live:  child = edge read; prefetch child's lines
    if first live child is a leaf: break
for lane in live:      Leaf::find
```

The code reuses the node searches as they are (`Internal::search`,
`child_unchecked`, `Leaf::find`). It adds no shared descent with `Node::find`:
a shared descent once cost every hit an instruction (`search.rs`).

### Inserts: `Tree::insert_many`

1. **Appends go the single-key way.** If lane 0 appends to the remembered
   last leaf (D45), the batch is probably an ascending stream: the lanes
   then insert one by one, each trying the append first, as today.
2. **Shared prefix first.** Each lane shrinks the shared prefix for its
   key (`shrink_shared_prefix_for`, D50) before any search. Shrinking
   changes only border prefixes, never where a key goes, and almost
   always changes nothing.
3. **Read-only descent** of every lane in lockstep, as `find_many` does,
   ending each lane at `Node::search_for_insert`'s result: the existing
   key, or the leaf and edge where the key belongs.
4. **Apply in input order.** Each lane does what `Tree::insert` does after
   its search: replace the value; insert into a leaf with room
   (`insert_fit`); or, when the leaf is full, `insert_with_path`, which
   searches again with a path and splits.

Earlier lanes change the tree under later ones. A later lane's result goes
stale as follows, and each stale lane searches again before it applies:

| an earlier lane... | stale later lanes | they redo |
|---|---|---|
| replaced a value | none | |
| inserted into leaf `L`, which had room | lanes ending in `L` | `Leaf::search_always_inlined` on `L` (cached) |
| split (`insert_with_path`), raise included | all | `search_for_insert` from the root (path cached) |

A split moves keys between halves and adds separators up the path, and a
raise moves the root node to a new page, so after a split every later lane
descends again. With 24-key `String` leaves at 70% fill, the tree gains a
leaf, so a split, about every 17 random inserts. A batch of 8 then has a
split about half the time, with 3.5 later lanes on average: about 1.6
cached descents more per 8 inserts. A finer rule, which re-descends only lanes that
pass through a split node, is an open question.

Results equal one-by-one inserts. Lanes apply in input order. Each applies
from the position a fresh search would find in the tree as it is then.
Whether a leaf splits depends only on how full it is. Duplicates in the
batch come out right: the later lane finds the leaf changed, searches it
again and finds the earlier lane's key.

### `extend`

```rust
fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
    // Branch early: a small tree, or one too short to batch, goes one by one.
    // Otherwise, take EXTEND_LANES entries at a time into an array and
    // call `insert_many`; the remainder goes one by one.
}
```

The batched part is `#[inline(never)]`, so the loop each caller of `extend`
inlines stays small. `collect` and `From` keep sorting and building
(`tree/build.rs`): a batch can't beat a sort (2026-10-14: `sort_unstable`
of 1M titles took 64 ms; inserting them one by one took 125 ms).

### Where the code goes

- `tree/batch.rs`, new: lanes, `find_many`, the insert descent and apply,
  `prefetch_line`.
- `map/batch.rs`, new: `get_many`, `insert_many`. The set's methods sit
  beside its others in `set/mod.rs`.
- `internal.rs`, `leaf.rs`: read-only accessors for the lines a search
  reads and an edge's line, computed from the layout; used only by
  `tree/batch.rs`.
- `map/from_entries.rs`, `set/mod.rs`: `extend` as above.
- `insert.rs`: `insert_with_path` becomes `pub(in crate::tree)`;
  nothing else changes. `search.rs`: unchanged.

## Will it regress existing code?

| area | risk | why | check |
|---|---|---|---|
| single `get`, `insert` | low | Their code is unchanged. The node searches they share are `#[inline(always)]`, except a heap string's `search_maybe_inlined`, which LLVM inlines or not at each caller; another caller can change that choice | instruction parity: before/after bench binaries, the ~33-cell `--perf` subset, `nm` for outlined `get` and `search_index` |
| `extend` | medium | Every caller gets the new path | 10K, 100K, 1M, 10M cells: random and ascending; no cell slower |
| ascending inserts | low | The D45 append path stays first | ascending `extend` cell |
| prefetch reaching single-key paths | low | Only `tree/batch.rs` calls `prefetch_line` | grep in CI; single-key `instructions_per_op` unchanged |
| DRAM traffic | medium | A wrong line list repeats D46's 4–6× fills | `lines_from_memory_per_op` of `get_many` ≤ `get`'s |
| panics | accepted | `insert_many` and `extend` may drop entries they hadn't placed (D48) | none |
| Miri, fuzzing | none | The wrapper is a no-op under Miri | Miri on the new tests; fuzz ops for `get_many`, `insert_many`, `extend` against std and against one-by-one |
| code size, compile time | low | Instantiated only when called; `extend`'s batch part is out of line | binary size of the bench |
| `no_std` | none | `core::arch` only | the `no_std` build |

The equivalence tests insert the same entries one by one into one tree and
batched into another, both on `TestPages`. They compare returned values,
entries, and node by node keys and prefix lengths, and check
`invariants.rs`.

## Cost model

```text
one by one:  t ≈ chain + compute
batch of N:  t ≈ max(compute + lane bookkeeping, chain / overlap) per key
```

Here `chain` is the dependent misses' latency, and `overlap` is how many
lanes' misses are in flight together: N with prefetch; without it, as many
as the reorder buffer spans. Upper bound: `(chain + compute) / compute`.

Estimates, not measurements:

- **Large trees.** 10M `u64`: 512-byte internal nodes take under 10 MB
  and stay in L3, while the leaves, hundreds of MB, come from DRAM. Most of the 93–137 ns is the
  chain, so get and insert should gain 2× or more. 10M `String`: the leaf
  adds the key line and the heap line after the inline keys, and compute is
  larger, so 1.5–2×.
- **Trees in cache:** no gain. Lane bookkeeping is a loss there, hence step
  1's threshold.
- **Prefetch vs lockstep only:** prefetch should win when a stage computes
  for long (`String` ties, binary searches without AVX-512), and tie when the
  reorder buffer already spans N lanes.

## Measurement plan

Five minutes at most, one run per cell. `--perf` needs `nmi_watchdog=0` on
the 9900X.

- **Lookups:** `get_many` against a loop of `get` on the same tree, for
  `u64` random, JSON strings, `hash32` and one real-key dataset, at 10M
  (headline), 1M and 10K. Variants: N = 8, with and without prefetch, plus
  N = 4 and 16 for `u64` at 10M only.
- **Inserts:** `insert_many` and `extend` against one-by-one inserts,
  random and ascending, `u64` and JSON, at 1M and 10M.
- **Counters:** `instructions_per_op` and `lines_from_memory_per_op` for
  every cell. The single-key cells must keep their instruction counts
  exactly (parity).

Keep the design if `get_many` at 10M is at least 1.5× a loop of `get` for
`u64` and `String`, with no more DRAM lines per key, and no single-key or
`extend` cell regresses.

## Open questions

- **N per key type:** a `NodePlan` answer, or one constant.
- **The batching threshold:** a length, or depth plus key type.
- **Leaf stages for `String` hits:** splitting the leaf search into
  inline keys, entry line and heap bytes would let the last two misses
  overlap too. It needs a split leaf search, kept separate from `Leaf::find`
  so that single lookups don't change.
- **Finer staleness after a split:** re-descend only lanes through a split
  node, if re-descents show up in the measurements.
- **An iterator form** (`get_each`) for streams, if chunking at the
  caller proves awkward.
