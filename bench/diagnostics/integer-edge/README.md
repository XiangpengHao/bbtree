# Integer edge-first structural diagnostic

This is a structural access model, not a latency benchmark and not library code.
It traces actual integer searches on one tree, validates every internal result
and returned value, and maps the accessed indexes onto the current key-first and
hypothetical edge-first arrays. It reports line footprints, occupancy, tied runs,
comparison indexes and child indexes. No production mutation or static counters
are added. The two diagnostic Rust files forbid unsafe code.

`node_model.rs` must be a child of the private node module; `driver.rs` must be a
child of map. The recipe below copies the library into a disposable directory
before adding these private modules. It modifies no repository source. Run from
the repository root inside its Nix development shell, after placing this folder
at `bench/diagnostics/integer-edge`:

```sh
diag_dir="$PWD/bench/diagnostics/integer-edge"
model_dir=$(mktemp -d /tmp/bbtree-integer-edge.XXXXXX)
cp -R src "$model_dir/src"
printf '\n#[path = "%s"]\npub(crate) mod integer_edge_model;\n' "$diag_dir/node_model.rs" >> "$model_dir/src/node/mod.rs"
printf '\n#[path = "%s"]\npub(crate) mod integer_edge_driver;\n' "$diag_dir/driver.rs" >> "$model_dir/src/map.rs"
cat > "$model_dir/src/main.rs" <<'RS'
#![allow(dead_code, unused_imports)]
extern crate alloc;
mod inline_key;
mod map;
mod node;
pub use inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWidth, InlineWord, WithInlineKey};
pub use map::BBTreeMap;
fn main() { map::integer_edge_driver::Diagnostic::main(); }
RS
rustc --edition=2024 -C opt-level=3 -C target-cpu=native "$model_dir/src/main.rs" -o "$model_dir/model"
"$model_dir/model" u64-random 2000000 100000 > "$model_dir/u64-random.txt"
"$model_dir/model" u128-random 2000000 100000 > "$model_dir/u128-random.txt"
"$model_dir/model" u64-ordered 2000000 100000 > "$model_dir/u64-ordered.txt"
"$model_dir/model" u128-ordered 2000000 100000 > "$model_dir/u128-ordered.txt"
```

The direct rustc build enables no Cargo features, so software prefetch is off.
Each selected run builds 2M entries and samples 100k hits plus 100k misses; the
original runs completed far below five minutes. No clock or timing is read.
Native versus portable SIMD does not change this logical access model.

The checked source baseline is the accepted key-first integer layout after
internal string narrowing and pointer leaf runs; u16 summary capacities are 56
for u64 internal nodes and 38 for u128. The model reads actual capacities and offsets, asserts 1KiB/64-byte page
geometry, and requires original keys before child pointers.
It also checks every inline summary against its key. Source/API drift should be
reviewed rather than silencing assertions. The original copied-source manifest
is `/tmp/bbtree-integer-edge-model-src.sha256`; the analysis is in
`doc/06-cost-model.md`.

`rejected-layout.patch` records the tested production proposal and its geometry
and differential coverage; it is retained as research, not applied by this
recipe. The timed diagnostic already includes the `u128-dense` case; no harness
patch is needed.

Provenance matters: structural random insertion uses shuffled ranks, whereas
the portable timing harness inserts mixed keys in index order. The structural
dense workload inserts even integers in order and samples random odd misses;
the timed dense case visits odd misses in ascending order. Distinct touched
lines are not measured cache misses and these schedules are not interchangeable.
The native dense-miss regression of the proposal has no established mechanism.
