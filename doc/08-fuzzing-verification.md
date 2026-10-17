- Fuzzing and verifications applies to single thread bbtree only.

- Fuzzing is high level behavior testing, we should match bbtree with std btree. including the map and the set.

- Verification basically means verus (https://github.com/verus-lang/verus) here. I'm not sure if it's is genuinely helpful, but think it's cool to add a verification to the title. Check t4 (https://github.com/XiangpengHao/t4) for how it uses and setup verus.
Verification is unbounded, there're infinite things we can verify, so we want to invest our time on the most important ones: memory issues, complex logic issues, etc. 
Verification can be very long and expensive, so try to write minimal verification code to verify the properties.

- ## Where the fuzzing time goes (2026-10-11)

Fuzzing time is spent where a bug would reach the most users and do the most
harm. Every operation of the map and the set goes through the same node code
(slots, splits, merges, moves between siblings, cuts, joins, bulk builds,
walks), which branches on the key type in one place: the node plan
(`07-node-plan.md`). So the map is fuzzed once per row of the plan's table,
the set only where its code is its own, and the unsafe code under user code
that misbehaves. Seven targets, in `fuzz/fuzz_targets/`, over the harnesses in
`fuzz/src/`:

| target | what only it reaches | minutes a week |
|---|---|---|
| `map_heap_byte_strings` | `Vec<u8>`: 8/4-byte windows (D43), length tags (D36); the most row-specific code | 20 |
| `user_code_panics` | `Ord`, `InlineKey`, `Clone`, `Drop` that panic or lie: memory safety of every unsafe path, panic isolation (D26), D44 | 20 |
| `map_integers` | `u64`: 2-byte windows past shared bytes (D31); the most common keys with inline keys | 12 |
| `map_user_inline_keys` | a user's `InlineKey` under `WithInlineKey`, at the default width: prefixes kept without calling it while keys move, borrowed lookups by `Ord` alone (D19) | 12 |
| `map_plain_keys` | no inline keys: `u32`, branchless (D19); 64-byte keys whose trees reach three levels from a few hundred entries | 10 |
| `set_algebra` | the operations on two sets: strategy choice, the merge of two walks (`unsync/set/merge.rs`), std's prints | 10 |
| `map_byte_arrays` | `[u8; 16]`: separator keys in heads (D49), whole-key equality (D38) | 6 |

The four map targets with inline keys each run a wide map (`u64` values) and
a narrow one (328-byte values that own heap memory, about a dozen entries to
a leaf), chosen by the input.

- **Oracles.** Std's answers (map and set targets) or a model (panics);
  `check_invariants` after every operation, so a corrupt node is caught when
  it is made, not when a lookup later crosses it; AddressSanitizer and the
  library's debug assertions. `check_invariants` and `check_prefix_lens`
  are public only in builds with `--cfg fuzzing`, which `cargo fuzz` sets.
- **Inputs.** Structured: a list of operations, decoded until the input's
  bytes run out. Keys are a stem from a few the input gives, and a number from
  a small space, so that nodes strip prefixes of every length, inline keys
  tie, and operations meet keys already in the map.
- **Not fuzzed:** 64 KiB keys, whose length tags wrap (`length_tag_tests.rs`
  covers them exactly), and `range` with lying keys, which may panic as std's
  does.
- **Schedule.** `.github/workflows/fuzz.yml`, Wednesdays at 10:17 UTC, one job
  of about 100 runner minutes, continuing from the last run's corpora. Every
  release also fuzzes, for 120 minutes in the same shares (`ci.yml`), and
  saves its corpora for the next run. Push CI builds
  the harnesses, runs them on pseudo-random inputs and replays
  `fuzz/regressions/`.

Locally, from the repository root:

```
cargo fuzz run --debug-assertions map_heap_byte_strings -- -max_total_time=60
```

A crash the weekly run finds is minimized (`cargo fuzz tmin`), then kept as
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
