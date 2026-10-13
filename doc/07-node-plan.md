# 07 — Node plans: every per-key-type choice in one place

Status: implemented (2026-10-11), in `tree/node/node_plan.rs`, the key columns of
`tree/node/slots.rs`, and the plan-driven layouts of `tree/node/layout.rs`; D47.
Measured in "Verification".

## Problem

By D46 the nodes specialized on the key type in five ways: summary width
(D31, D32, D43), length tags (D36), local search prefixes (D33), children
before keys (D39) and whole-key equality for one candidate (D38). Each was
spelled where it was used, as a stand-in for type identity:

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
to named answers: summary width, length tags, local prefix, children first,
whether exported windows are read again from keys (D43), the full and tied
searches (D19), whole-key equality (D38). One constructor per row, each
documented with its reason:

| keys | inline keys, leaf / internal | also stored | search over tied inline keys |
|---|---|---|---|
| without inline keys | none | nothing | as the key type picks (D19) |
| std integers | the impl's width in both | internal: a local prefix (D33) | as the key type picks (D19) |
| every key under `WithInlineKey` | the impl's width in both | nothing | as the key type picks (D19) |
| std byte arrays | 2 / 2 (D32) | internal: children before keys (D39), a local prefix (D33) | binary; one candidate compares whole (D38) |
| std heap byte strings | 8 / 4 (D43) | leaf: length tags (D36); internal: a local prefix (D33) | binary |

The rows differ in what a tie costs: a load from the node (arrays), a read
past the node (heap strings), or user code, which the nodes call only while
searching (`InlineKey` impls). Layout, slots, searches and prefix upkeep read
the plan; none asks what the key type is.

**3. Columns: arrays that move with the keys** (`KeyColumns`, `KeyCells`,
`slots.rs`). Inline keys and length tags are both per-key arrays that every
insert, removal and run move alike. `KeySlots` moves keys and calls the
columns; it no longer knows which columns exist. A new per-key array is a
field of `KeyColumns`, a cell of `KeyCells` and a plan field; splits, merges
and steals don't change.

Layouts are a total function of the plan's answers:
`NodeLayout::internal(width, children_first)` picks one of eight constants,
so any row may put its children first without touching `layout.rs`.

### Zero overhead, and how to keep it

`TypeId` comparisons aren't `const`, so for `Auto` a plan can't be an
associated constant; it is a value that folds after monomorphization. Folding
is fragile (D36, D41 pitfalls), so the code follows rules:

- **Derive, don't pass.** Code asks for the plan where it uses it, from type
  parameters (`NodePtr::plan::<Kind>()`, `NodeRef::plan()`), and never takes
  it as an argument or keeps it in a field. `KeySlots` carries `Kind` as a
  type parameter for this reason: its methods derive the plan instead of
  holding one.
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

## Rejected: a public `KeyBytes` trait

A `KeyBytes` trait (`fn key_bytes(&self) -> &[u8]`, `Ord` equal to byte
order) with a `WithKeyBytes` strategy would let a newtype around a byte string
break ties by its bytes, as `Auto` does for std strings. It was built and
measured, then removed:

- 1M lookups of `String`-newtype paths behind a 64-byte shared prefix took
  311 ns with `WithKeyBytes` and 315 ns with `WithInlineKey`, run to run
  noise, and `WithKeyBytes` ran more instructions.
- What makes std heap strings fast on paths is length tags, local prefixes
  and 4-byte internal summaries. All three read keys while keys move, which
  user code can't do safely. What was left, comparing bytes past a tie
  instead of `memcmp` from the start, saves nothing.
- A public trait is permanent API for no measured gain.

Its panic test stays, as a test of a panicking `InlineKey`
(`a_panicking_inline_key_leaves_a_valid_map`).

## Adding a specialization

1. Say what it stores or how it searches as a plan field, set in the row
   constructors of `node_plan.rs`, with its reason.
2. If it stores a per-key array, add it to `KeyColumns` and `KeyCells`.
3. If it needs page room, add it to `layout.rs` as a function of plan
   answers, not of the key type.
4. Check instruction counts on the bench subset against the tree before it,
   every cell, not only the key type it is for.
