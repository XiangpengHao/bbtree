- Fuzzing covers both maps, each on one thread: the unsync map, and the
  sync map's own layer over the same nodes. How threads interleave is the
  shuttle tests' (09). Verification applies to single thread bbtree only.

- Fuzzing is high level behavior testing, we should match bbtree with std btree. including the map and the set.

- Verification basically means verus (https://github.com/verus-lang/verus) here. I'm not sure if it's is genuinely helpful, but think it's cool to add a verification to the title. Check t4 (https://github.com/XiangpengHao/t4) for how it uses and setup verus.
Verification is unbounded, there're infinite things we can verify, so we want to invest our time on the most important ones: memory issues, complex logic issues, etc. 
Verification can be very long and expensive, so try to write minimal verification code to verify the properties.

- ## Where the fuzzing time goes

Fuzzing time is spent where a bug would reach the most users and do the most
harm. Every operation of the map and the set goes through the same node code
(slots, splits, merges, moves between siblings, cuts, joins, bulk builds,
walks), which branches on the key type in one place: the node plan
(`07-node-plan.md`). So the unsync map is fuzzed once per row of the plan's
table, the sync map once over the layer it puts on the same nodes, the set
only where its code is its own, and the unsafe code under user code that
misbehaves. Eight targets, in `fuzz/fuzz_targets/`, over the harnesses in
`fuzz/src/`:

| target | what only it reaches | share of a run |
|---|---|---|
| `sync_map` | the sync map (`BBTreeMap`), on one thread: optimistic descents, values copied out of leaves without their latches (D36's tags among them), latched writes, scans, `range_cloned`, batches | 20% |
| `map_heap_byte_strings` | `Vec<u8>`: 8/4-byte windows (D43), length tags (D36); the most row-specific code | 18% |
| `user_code_panics` | `Ord`, `InlineKey`, `Clone`, `Drop` that panic or lie: memory safety of every unsafe path, panic isolation (D26), D44 | 16% |
| `map_integers` | `u64`: 2-byte windows past shared bytes (D31); the most common keys with inline keys | 11% |
| `map_user_inline_keys` | a user's `InlineKey` under `WithInlineKey`, at the default width: prefixes kept without calling it while keys move, borrowed lookups by `Ord` alone (D19) | 11% |
| `map_plain_keys` | no inline keys: `u32`, branchless (D19); 64-byte keys whose trees reach three levels from a few hundred entries | 9% |
| `set_algebra` | the operations on two sets: strategy choice, the merge of two walks (`unsync/set/merge.rs`), std's prints | 9% |
| `map_byte_arrays` | `[u8; 16]`: separator keys in heads (D49), whole-key equality (D38) | 6% |

The four unsync map targets with inline keys each run a wide map (`u64`
values) and a narrow one (328-byte values that own heap memory, about a
dozen entries to a leaf), chosen by the input. `sync_map` runs the same four
rows, the sync map needing inline keys, the row and the width chosen by the
input. Its wide maps hold `Copy` values, which lookups copy out of leaves
unlatched: `u128` beside `Vec<u8>` keys, whose latched leaves have room for
length tags only beside 16-byte-aligned values, and `u64` beside the rest.
Its narrow maps hold the same 328-byte values, which lookups clone.

- **Oracles.** Std's answers (map and set targets) or a model (panics);
  `check_invariants` after every operation, so a corrupt node is caught when
  it is made, not when a lookup later crosses it; AddressSanitizer and the
  library's debug assertions. `check_invariants` (on both maps) and
  `check_prefix_lens` are public only in builds with `--cfg fuzzing`, which
  `cargo fuzz` sets.
- **Inputs.** Structured: a list of operations, decoded until the input's
  bytes run out. Keys are a stem from a few the input gives, and a number from
  a small space, so that nodes strip prefixes of every length, inline keys
  tie, and operations meet keys already in the map.
- **Not fuzzed:** 64 KiB keys, whose length tags saturate at 65,535: the
  length-tag tests of both maps cover them exactly
  (`src/unsync/map/tests/length_tag.rs`, `src/map/tests.rs`). And `range`
  with lying keys, which may panic as std's does.
- **Schedule.** `.github/workflows/fuzz.yml`, on the self-hosted bare-metal
  runner `pittsburgh` (label `bbtree-perf`, which `perf.yml` shares, so their
  runs queue behind each other), for 30 minutes: Wednesdays at 10:17 UTC;
  for every release, before it merges (`ci.yml`, which it gates) and on
  main once it's published (`publish.yml`, for the README badge, which
  counts only `fuzz.yml`'s own runs); and on demand. In the shares above,
  each target with `-fork=$(nproc)` (12 processes on its i5-11400). Each run
  continues from the last run's corpora and saves its own for the next. Push
  CI builds the harnesses, runs them on pseudo-random inputs and replays
  `fuzz/regressions/`.

Locally, from the repository root:

```
cargo fuzz run --debug-assertions map_heap_byte_strings -- -max_total_time=60
```

A crash a run finds is minimized (`cargo fuzz tmin`), then kept as
`fuzz/regressions/<target>/<what it found>` with its fix.

### Found

- **`retain` and `extract_if` miscounted after a panicking `InlineKey`**
  (fixed 2026-10-11; `fuzz/regressions/user_code_panics/`, and
  `a_panicking_inline_key_in_a_bulk_change_leaves_a_valid_map`). The
  kept entries go into a `TreeBuilder`, which calls `InlineKey` as it places
  each one and as nodes close. When that panicked, the entry being placed
  was dropped, but the walk, which counts the entries it hands over rather
  than those placed, still counted it: the map's length came out one too
  high, and its next walk failed "the walk's entry count includes a
  separator". The builder now counts the entries it took and dropped, and
  the walk subtracts them. Found by the harness's own test on its first
  pseudo-random input.

## What Verus proves

Verus proves code that bbtree runs: the `proof` module (`src/proof/`) holds
the code with its specs and proofs, in `verus!` blocks, and the rest of the
crate calls the code. A normal build erases the specs and proofs, so vstd,
Verus's library of them, is purely ghost. Fuzzing
samples inputs; a proof covers all of them, so proofs go where the inputs are
unbounded and a bug gives wrong answers that no sanitizer sees.

- **Inline keys of the std key types** (`src/proof/inline_key.rs`). The
  `InlineKey` impls for byte strings, `u64`, `u128`, `i64` and `i128` call
  `ProvenInlineKey`, which is proven to keep `InlineKey`'s contract: for
  every key and offset, the inline key is bytes `offset..offset + 8` of the
  key's encoding, big-endian, zero past its end (the overlapping loads of
  strings under 8 bytes and the shifts included); keys order like their
  encodings (the sign flip included); and so where two encodings agree
  before `offset` and their inline keys there differ, the keys order like
  the inline keys (`lemma_inline_keys_order_keys`). The statements are in
  `inline_key.rs`, the proofs in `inline_key/lemmas.rs`.
- **Trusted, not proven** (`src/proof/std_specs.rs`): what `first_chunk`,
  `last_chunk`, `checked_shl` and `from_be_bytes` do, which vstd has no
  specs for, and that std orders byte strings and integers as
  `lexicographically_before` and `<` say. Writing the loads as byte indexing
  instead, which Verus knows, made LLVM gather the bytes in vectors rather
  than load them with one `movbe`.
- **Not proven:** that the nodes keep only the first 2 or 4 bytes of an
  inline key (`InlineWidth`), and every search and node change built on
  inline keys. Fuzzing and `check_invariants` cover those.

Locally: `cargo verus verify` (about two seconds once vstd is verified;
Verus comes from `flake.nix`), and `verusfmt` formats `src/proof/`. CI: `.github/workflows/verus.yml`.
