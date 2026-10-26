# 00 — Init: a C++ fork of bbtree, for DuckDB

Status: done (2026-10-24 to 2026-10-26). The asks that started each phase
of this fork, and the constraints they set. The decisions they led to are
in 03-decisions.md.

## The port (2026-10-24)

> can we port this fork to CPP 17? make sure you keep the same performance
> objectives. The goal is to embed it in DuckDB later.

Asked before starting, the answers that set the scope:

- **Scope: the core maps.** The node code, the unsync map, the shared map
  with optimistic lock coupling, and the batch API; not the file maps,
  `bbtree-mmap`, `bbtree-store`, the visualizer, fuzzing or the Verus
  proofs (D3). The rest of the map API (entries, sets, leaf annexes)
  followed once the core met Rust's performance.
- **Keys: fully generic.** An `InlineKey` trait mirroring Rust's, for
  integers, `std::string` and byte vectors; DuckDB picks (D6).
- **Layout: replace Rust.** The Rust crates stay only as the reference for
  differential tests and side-by-side benchmarks, and go once C++ meets
  their performance (D2, D4).
- **Build: header-only with CMake.** No dependencies; the allocator a
  template parameter, for DuckDB (D1).

"The same performance objectives" means upstream's: the cost model of
OG-design/06-cost-model.md, and the Rust crate's own numbers on the same
workloads as the bar (02-rust-parity.md).

## Building where DuckDB builds (2026-10-25)

> ensure the github CI tests all archi

CI for every architecture and SIMD path, then MSVC (D23, D24). A test-only
string shaped like Rust's `String` was reverted on review:

> i don't think `struct RustString {` is the way to go - this should be a
> port, but not this way

The layout tests check `std::string` itself (D7).

## Closing the gaps to Rust (2026-10-25)

> check now how to address the [...] Shared inserts at 4 and 8 threads

Profiling against the Rust binary, down to single instructions (D31), and
the code-generation decisions D17–D22. Asked whether to build without the
stack protector, since the sanitizers run: no (D22).

## A C++-only fork (2026-10-25)

> time has come to update this fork and keep only the C++ version [...]
> I assume we don't need any Rust code anymore here.

The Rust crates and upstream's design notes leave; the C++ code moves to
the root (D4). Upstream's notes are cited as OG-; this folder records the
fork's own design.

## The race, and hash maps (2026-10-25, 2026-10-26)

Asked for: upstream's README race (`OG-design/assets/readme-race.svg`),
with the well-known C++ tree maps, on the M4 Pro at hand. The race against C++ concurrent maps (D32, 06-race.md), then two heats,
tree maps and hash maps, once the hash maps came over from the
`codex/hashmap` branch: the shared hash map, last in its heat, made
competitive in five steps (D25–D30, 05-hash-maps.md).

## The name (2026-10-26)

With hash maps beside the tree, bbtree, the Blessed B-tree, no longer
fits: the library becomes shoal (D5).

## The notes

| | |
|---|---|
| 00-init.md | this file: the asks and the scope |
| 01-port.md | how the Rust crate was ported: method, module map, layout |
| 02-rust-parity.md | the C++ maps against the Rust crate: measurements, and how the gaps closed |
| 03-decisions.md | the decisions, `D<n>`; upstream's are `OG-D<n>` |
| 04-portability.md | compilers, standard libraries, SIMD paths, CI |
| 05-hash-maps.md | the owned and the shared hash map |
| 06-race.md | the race against C++ concurrent maps |
