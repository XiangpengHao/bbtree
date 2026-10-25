# bbtree-cpp

This repository is the C++ fork of [XiangpengHao/bbtree](https://github.com/XiangpengHao/bbtree):
a header-only C++17 port of the Rust crate, with zero dependencies, built to
be embedded in DuckDB. Only the C++ code lives here; the Rust crate, its
design notes, proofs, fuzzing and visualizer stay upstream. This fork
branched from upstream at `8adae97`.

- `src/bbtree/` is the library, `tests/` its tests (CTest), `bench/` its
  benchmarks.
- `design/NN-*.md`, "doc NN" and `D<n>` in comments refer to upstream's
  [design notes at `8adae97`](https://github.com/XiangpengHao/bbtree/tree/8adae97a0ed97e5c98fa2790312eb0aec9719846/design); `D<n>` is a decision in
  `design/03-decisions.md` there. `PORTING.md` holds the port's own
  decisions (`P<n>`), its status and its measurements; record new ones
  there.
- Upstream is the reference for the data structure and its algorithms. When
  the C++ code departs from the Rust crate, say why in `PORTING.md`.

## Write code that requires minimal human attention 

- Fact: every line of code requires human review, so write readable code not just working code.

- Build reusable, trustworthy components that has deep logic but thin interface, so that human only need to review the complexity once (reuse the attention).

- Follow modern C++ best practices (RAII, `noexcept` where it holds, no raw owning pointers in interfaces) to save human attention from reviewing for common mistakes.

- Before implementing a new feature, identify similar existing features, and check if it's worth to refactor existing code so it's reusable.

## Engineering

- Prefer explicit props and function arguments so ownership and data flow remain visible. Use context only when a dependency must cross at least two component layers and the intermediate parent does not use it. Keep state owned by the component that creates it.


- use descriptive, intention-revealing names for variables and functions, don't use generic names like `Built`, `Common`, `View`.

- do not use free form functions, functions should have a clear owner, e.g., struct, enum; unless it is fully justified stateless function.
  Example 1: `Built build_chart(const DataModel& model)`, instead use a member function like: `Built DataModel::build_chart() const`. Even better, give the chart a constructor from the data model.
  Example 2: `void toggle_expand(views::codemap::CodeState& code, std::pair<uint32_t, uint32_t> key)` instead use a member function like: `void CodeState::toggle_expand(std::pair<uint32_t, uint32_t> key)`.

- try to use minimal visibility for functions and variables: keep members `private` unless callers need them, and keep implementation details in `bbtree::detail` and out of the public headers' API. 

- try to avoid public data members, prefer to use getters and setters, unless the field is truly public (a plain aggregate).

- organize the code by features, not by technical buckets, e.g., don't have `api.hpp`, `views.hpp`, `data.hpp`, etc. Instead, do `settings.hpp`, `data_panel.hpp`, `code_map.hpp`, etc.

- No static or global mutable variables.

## Performance reasoning

- Reason then measure. Don't start with blind sweeping, or brainless micro/premature optimizations. Always first think about what is the bottleneck by profiling and measurements, reason the cost model. Benchmark is the last step, to verify that your reasoning is correct, if not, correct your model and repeat; benchmark is NOT a tool to guide the optimization.

- Never do explicit prefetching: no software prefetch instructions or intrinsics (`_mm_prefetch`, `__builtin_prefetch`, inline `prefetcht0`, ...), not in the library, benchmarks, prototypes or A/B variants, and not behind a feature flag. Fix memory latency with layout instead: put what an operation needs in the cache lines it already loads (D46).
  Exception: the batch lanes (doc 12, D53, P6) prefetch, only through `Prefetcher` in `src/bbtree/detail/tree/batch.hpp`, the line each lane reads next, the query window included. Never in single-key paths, and never whole keys or unchosen edges.

## Verification

- Local checks: build with warnings as errors and run the tests, `cmake -B build -G Ninja && cmake --build build && ctest --test-dir build --output-on-failure`.
- Run the sanitizers on what you touched: `-DBBTREE_SANITIZE="address;undefined"` for memory and lifetimes, `-DBBTREE_SANITIZE=thread` for the shared map (`map.hpp`, `detail/latched/`). Use a build directory of their own (`build-asan`, `build-tsan`).
- CI (`.github/workflows/cpp.yml`) builds and tests every SIMD path (SSE2, AVX2, AVX-512, NEON) on Linux, macOS and MSVC: code under `#if` for one instruction set must still compile on the others.
- Tests are differential: check behaviour against `std::map`/`std::set` and the tree's invariants, with the zero-dependency harness in `tests/test_harness.hpp`.

## Benchmark rule

- A benchmark is useless if it takes more than 5 minutes to run.
- Never run full benchmark sweep without explicit approval.
- It's meaningless to repeat a benchmark, if the run to run variance is high, either the machine is not stable -- then wait for it to stablize, or the benchmark/implementation is broken -- then fix it. Repeating and taking average/median is for losers to cover benchmark flaw.
- Only run a carefully selected subset of relevant benchmarks.
