# Portable map diagnostic

This standalone crate reproduces portable `hash32`, `u64`, `u128`, and borrowed-string workloads:
the Rust executable's default System allocator, generic x86-64 instructions,
no prefetch, one thread, and one operation per fresh process. It has no
dependencies beyond the local bbtree library.

From the repository root, build in its Nix development environment:

```sh
nix-shell --expr '(builtins.getFlake ("git+file://" + toString ./.)).devShells.x86_64-linux.default' --run 'RUSTFLAGS="-C target-cpu=x86-64" cargo build --release --manifest-path bench/diagnostics/portable/Cargo.toml'
```

Run a selected operation pinned to CPU 6, with other builds and benchmarks idle:

```sh
nix-shell --expr '(builtins.getFlake ("git+file://" + toString ./.)).devShells.x86_64-linux.default' --run 'taskset -c 6 bench/diagnostics/portable/target/release/bbtree-portable-diagnostic hash32 bbtree get_hit 2000000'
```

Arguments are `hash32|u64|u128|u128-dense|borrowed-json|borrowed-path`, `bbtree|std-btree`,
`get_hit|get_miss|random_insert|iterate_next`, and the initial entry count (default
2,000,000; must exceed 1,000,000). Output columns are key kind, backend,
operation, initial entries, operation count, nanoseconds per operation,
and checksum. This adds the key-kind column to the original diagnostic output.

Numeric and array setup inserts unique SplitMix64-derived keys in index order. A hash key has
four salted big-endian words; a `u64` key uses the first word. A `u128` key
combines that word in its upper half with an independently salted word in its
lower half. The upper-word permutation ensures distinct keys. Hit probes
use the original independent seed and multiply-shift rank mapping. Misses
and insertions use previously absent indices. Key and probe generation,
initial map construction, length/checksum assertions, and destruction are
outside the timer. Reads perform one operation per initial entry; insertion
adds one quarter as many entries. All backends use identical prepared inputs.
The expected hit checksum is calculated from probe ranks during setup and
checked exactly after timing; misses and insertions must have zero checksum.
After insertion timing, a separate readback checks every added key is present
and its value contributes exactly one to the insertion checksum.

`u128-dense` inserts even integers in ascending order, then measures only reads:
hits select existing ranks with the same independent mapping, while misses use
intervening odd integers. Leading bytes tie, including at the root, so this case
exercises full-key comparisons after the summaries. It accepts `get_hit`,
`get_miss`, and `iterate_next`; it rejects `random_insert`.

`iterate_next` scans the full map in ascending key order by explicitly
calling `Iterator::next` until it returns `None`, summing the values with
wrapping addition. It does not call `fold` or `for_each`, so specialized
fold implementations cannot hide manual iteration costs. The timer includes
iterator construction and the final `None`; the operation count is the
number of entries visited, and the reported cost is nanoseconds per entry.
This operation prepares no probes. Its expected checksum, the sum of values
`1..=size`, is calculated during setup and checked after timing. All key
types and both backends run the same loop.

`borrowed-json` and `borrowed-path` store `&str` keys. A `Vec<String>` owns all
backing allocations until the map and borrowed workload have been dropped;
the timed loops copy only string slices. Both backends receive identical
strings, insertion order, probes, and values. No allocation is leaked, and
no string is allocated or cloned inside the timed loop.

The dependency-free `src/string_keys.rs` copies the JSON length histogram,
identifier alphabet, registry-path shapes, and uniqueness rule from
`bench/src/keys.rs`. Its SplitMix64 stream copies `bench/src/random.rs`'s
`KeyContent` seed, `mix(0)`, and exact draw order. Registry directory setup
uses its own fresh stream, as in the main suite; distinct path generation
restarts that stream and uses `ceil(initial_entries / 32)` crate directories.
Paths share the same 64-byte registry prefix. Each string is generated once;
a fixed-key content hash set detects repeats and appends deterministic random
identifier characters until the candidate has a new hash. Hash collisions
can lengthen otherwise-new keys but cannot admit duplicate strings. This
set is discarded before map setup and timing.

The first `size` generated strings populate the map in generation order;
miss and insertion probes use distinct subsequent strings. This diagnostic
uses the portable harness's existing independent hit-rank selection and
values `rank + 1`, rather than the main suite's shuffled insertion schedule.
It preserves those string shapes to exercise short-key collisions and long
shared prefixes, but does not reproduce the main suite's allocator or access
schedule. All ownership, uniqueness handling, string generation, and probe
preparation are outside timing. Initial/final length and exact hit, miss,
iteration, and insertion-readback checks validate the prepared workload.
