# 07 — Node plans: every per-key-type choice in one place

Status: implemented (2026-10-11), in `tree/node/node_plan.rs`, the key columns of
`tree/node/slots.rs`, and the plan-driven layouts of `tree/node/layout.rs`; D47.
Measured in "Verification".

## Problem

By D46 the nodes specialized on the key type in four ways: summary width
(D31, D32, D43), hash tags (D36), per-node search prefixes (since
replaced by the tree's shared prefix, D50) and whole-key equality for one
candidate (D38). Each was spelled where it was
used, as a stand-in for type identity:

- "fixed byte array" was `ByteString && width() == Two`, in `layout.rs` and
  `inline_keys.rs`; "heap byte string" was `ByteString && width() == Eight`,
  in `page.rs`, `local_prefix.rs` and `slots.rs`. Changing an array's width
  would have silently changed its layout, its point equality and its tags;
- 26 call sites outside tests resolved the strategy again and derived their
  own answer;
- each specialization lived in three files: a layout field, a search branch,
  and upkeep in the code that moves keys.

## Design

Three layers, each decided once.

**1. Encoding: how keys get inline keys** (`KeyInliner`, `inline_keys.rs`).
Unchanged in shape: std byte strings by type, or windows from an `InlineKey`
impl. Type recognition moved behind facts the plan needs: `ByteString` says
whether a value *is* its bytes (`holds_its_bytes`, arrays) or points to them.

**2. Plan: what each node kind stores and how it searches** (`NodePlan`,
`node_plan.rs`). `NodePlan::of::<S, Kind>()` maps the encoding and the kind
to named answers: summary width, hash tags, whether separator keys are
leaf heads (D49), whether exported windows are read again from keys (D43), the full and tied
searches (D19), whole-key equality (D38). One constructor per row, each
documented with its reason:

| keys | inline keys, leaf / internal | also stored | search over tied inline keys |
|---|---|---|---|
| without inline keys | none | nothing | as the key type picks (D19) |
| std integers | the impl's width in both | nothing | as the key type picks (D19) |
| every key under `WithInlineKey` | the impl's width in both; 8 / 4 with `cmp_from` (D43) | leaf: hash tags, if the impl has them and they fit (D36) | as the key type picks (D19); past the tie with `cmp_from` |
| std byte arrays | 2 / 2 (D32) | nothing | binary; one candidate compares whole (D38) |
| std heap byte strings | 8 / 4 (D43) | leaf: hash tags, where they fit (D36) | binary |

The rows differ in what a tie costs: a load from the node (arrays), a read
past the node (heap strings), or user code, which the nodes call only while
searching (`InlineKey` impls), but for a hash tag, which a leaf computes as
it writes a key, and a window an internal node reads again from its key,
both behind an abort-on-unwind guard (D36, D43, D48). Layout, slots, searches and prefix upkeep read
the plan; none asks what the key type is.

**3. Columns: arrays that move with the keys** (`KeyColumns`, `KeyCells`,
`slots.rs`). Inline keys and hash tags are both per-key arrays that every
insert, removal and run move alike. `KeySlots` moves keys and calls the
columns; it no longer knows which columns exist. A new per-key array is a
field of `KeyColumns`, a cell of `KeyCells` and a plan field; splits, merges
and steals don't change.

Layouts are a total function of the plan's answers:
`NodeLayout::leaf(width, hash_tags)` picks one of seven constants, and
`NodeLayout::internal(width)` one of four, so no row touches `layout.rs`.

### Zero overhead, and how to keep it

`TypeId` comparisons aren't `const`, so for `Auto` a plan can't be an
associated constant; it is a value that folds after monomorphization. Folding
is fragile (D36, D41 pitfalls), so the code follows rules:

- **Derive, don't pass.** Code asks for the plan where it uses it, from type
  parameters (`NodePtr::plan::<Kind>()`, `Leaf::plan()`,
  `Internal::plan()`), and never takes it as an argument or keeps it in a
  field. `KeySlots` carries the node type as a type parameter for this
  reason: its methods derive the plan instead of holding one.
- **Dispatchers that hold plan-derived values are `#[inline(always)]`**:
  `NodePlan`'s accessors, `KeyColumns`' methods, `NodePtr::key_columns`.
  Without it, `KeyColumns::insert` stayed out of line and every insert
  dispatched on which arrays exist at run time: +37 to +83 instructions per
  insert on every key type, u32 included.
- **Keep `KeyInliner` and `NodePlan::of` small.** Every inline key a node
  computes dispatches on `KeyInliner`, and every node operation asks for a
  plan. A third `KeyInliner` variant stopped LLVM inlining
  `KeyInliner::inline_key_at`, so `u64` windows went through a function
  pointer (+142 instructions per `u64` lookup). An extra arm in
  `NodePlan::of`, even one decided by a constant, cost 9 instructions per
  `[u8; 32]` insert by changing what was inlined around it.

Unoptimized builds compare type identities at run time, on every call, and
spent 79% of the test suite there. `ByteString::recognize` and
`StdInteger::recognize` now narrow their candidates by facts known without a
comparison (unsized or not, drop glue, size) first; release builds fold
either way.

## Verification

Instruction counts per operation, from the bench's `--perf` counters, on one
cell per key profile and operation that the refactor touches (33 cells,
18 s), against a binary built from the tree before it. Instruction counts
repeat exactly between runs of one binary, except `range64`, which moves by
±12 between runs.

| cells | change |
|---|---|
| `u64`, `u32`, `str-json`, `str-path`, `memtable16` lookups and inserts, `u64` with `WithInlineKey`, sequential inserts, scans | identical |
| `hash32` lookups | −88 (294 → 205): LLVM now inlines the internal-node search into `get` |
| `memtable16` inserts, `u64` removals | −9, −6 |
| `hash32` inserts, `str-16` removals | +1.3 (0.3%), +4.4 (0.5%) |
| set lookups (`()` values) | +2 (1%) |

The set lookups' two instructions are one null test that LLVM keeps on the
returned key pointer: `FoldBranchToCommonDest` speculates `page + 216` into
an earlier block, which drops `inbounds`, so the pointer is no longer known
non-null. Reverting each plan change on the lookup path in turn left it in
place; timings vary more between runs (29.9–42.4 ns at baseline) than this.

The debug test suite runs in 75 s, against 86 s before.

## Keys with their own impl, at `Auto`'s speed

A key with its own `InlineKey` impl gets what std heap byte strings get
under `Auto` through three hooks, with defaults, so an impl without them
runs as before:

- `HAS_HASH_TAG` and `hash_tag`: leaves keep each key's tag (D36).
- `HAS_CMP_FROM` and `cmp_from`: ties compare past the bytes the inline
  keys showed equal, and internal nodes keep 4 bytes of 8-byte inline keys
  (D43). The two go together: 4 bytes inside pays only with the skip.
- `cmp_prefix`: a search's check against what every key shares (D50),
  by default a window at a time; the std byte strings' impls compare 32
  bytes at a time, as `Auto` does.

A newtype around a std byte string takes all three from its string.

Both call user code while keys move: a leaf computes a key's tag as it
writes it, and an internal node of 4-byte inline keys reads a window's
other bytes from its key. Both run behind an abort-on-unwind guard (D48).

On 10M paths, ns per hit / miss and DRAM lines per hit, one run each:

| variant | hit | miss | lines |
|---|--:|--:|--:|
| `Auto`: 4 bytes inside, ties past the tie | 481 | 385 | 17.2 |
| `Auto`, 8 bytes inside | 565 | 427 | 18.6 |
| `Auto`, ties from byte 0 | 661 | 482 | 28.0 |
| `WithInlineKey`, hash tags only: 8 bytes inside, `memcmp` from byte 0 | 596 | 451 | 21.6 |
| `WithInlineKey`, hash tags and `cmp_from`, D50's check a window at a time | 540 | 423 | 18.8 |
| the same, without D50's check (a measurement only) | 486 | 376 | 16.6 |
| `WithInlineKey`, every hook, the strings' `cmp_prefix` | 486–491 | 376–389 | 18.4 |

The check a window at a time read what two 32-byte loads read, but its 8
ascending windows of the query's first line drew 1.7 more lines per hit
of hardware-prefetch fills past the key. With every hook, the demand reads
from DRAM are `Auto`'s, and the time too.

An earlier `KeyBytes` trait, which only compared ties past the tie, showed
no gain at 1M (311 vs 315 ns), with 8 bytes inside and no tags: the skip
pays at 10M, and most through the 4-byte summaries it allows.

## Adding a specialization

1. Say what it stores or how it searches as a plan field, set in the row
   constructors of `node_plan.rs`, with its reason. What the key type
   alone decides, such as the search over all of a node's keys (D19), is
   no field: the plan answers it from `K`, the same in every row.
2. If it stores a per-key array, add it to `KeyColumns` and `KeyCells`.
3. If it needs page room, add it to `layout.rs` as a function of plan
   answers, not of the key type.
4. Check instruction counts on the bench subset against the tree before it,
   every cell, not only the key type it is for.
