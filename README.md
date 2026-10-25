# bbtree-cpp

[![cpp](https://img.shields.io/github/actions/workflow/status/redox/bbtree-cpp/cpp.yml?branch=main&label=cpp)](https://github.com/redox/bbtree-cpp/actions/workflows/cpp.yml?query=branch%3Amain)

A header-only C++17 port of [bbtree](https://github.com/XiangpengHao/bbtree),
the B+ tree ordered map by [Xiangpeng Hao](https://xiangpeng.systems).

This fork keeps only the C++ version. The Rust crate, its formal
verification, fuzzing and visualizer live upstream; the port keeps the same
data structure, node layouts and algorithms, and was measured against the
Rust crate on the same workloads before the Rust code left this repository
([PORTING.md](PORTING.md)). How the tree works:
[bbtree.xiangpeng.systems](https://bbtree.xiangpeng.systems/post) and the
upstream [design notes](https://github.com/XiangpengHao/bbtree/tree/8adae97a0ed97e5c98fa2790312eb0aec9719846/design) at `8adae97`, where this fork branched.

- Header-only C++17, zero dependencies, built to be embedded in DuckDB.
- `bbtree::BBTreeMap`: a concurrent map, latched nodes with optimistic reads.
- `bbtree::unsync::BBTreeMap` and `BBTreeSet`: single-threaded, with the
  `std::map`/`std::set` operations, entries, ranges and set algebra.
- SIMD node search on SSE2, AVX2, AVX-512 and NEON; tested on Linux, macOS
  and Windows (MSVC), and under AddressSanitizer, UBSan and ThreadSanitizer.

## Usage

Add `src` to the include path, or with CMake:

```cmake
add_subdirectory(bbtree-cpp)
target_link_libraries(your_target PRIVATE bbtree)
```

### Concurrent map

```cpp
#include <bbtree/map.hpp>

bbtree::BBTreeMap<std::uint64_t, std::uint64_t> map;

std::thread a([&] { map.insert(1, 10); });
std::thread b([&] { map.insert(2, 20); });
a.join();
b.join();

assert(map.get_copied(1) == 10u);
assert(map.read(2, [](const auto&, const auto& value) { return value + 1; }) == 21u);
map.scan(bbtree::Bound<std::uint64_t>::unbounded(), bbtree::Bound<std::uint64_t>::unbounded(),
         [](const auto& key, const auto& value) { return true; /* keep going */ });
```

`insert` returns an `InsertOutcome`: the previous value, or an
`InsertError` when allocation failed, leaving the map unchanged.

### Single thread

```cpp
#include <bbtree/unsync/map.hpp>

bbtree::unsync::BBTreeMap<std::uint64_t, std::string> map;
map.insert(1, "one");
map.insert(2, "two");
assert(*map.get(1) == "one");
for (const auto& [key, value] : map) { /* in key order */ }
```

### Keys and values

- Keys and values must be trivially relocatable: the nodes move them as
  bytes. Trivially copyable types are; `std::vector`, `std::unique_ptr`,
  `std::shared_ptr` and libc++'s `std::string` are declared so.
  libstdc++'s `std::string` is not: use `std::vector<uint8_t>` or a
  `std::string_view` into stable storage.
- Keys order by `operator<`. Comparisons must not throw.
- Specialize `bbtree::InlineKey<K>` for the fastest lookups on your own key
  types; the byte strings and 64/128-bit integers come specialized.
- Batched lookups and inserts (`get_many_copied`, `read_many`,
  `insert_many`) overlap their keys' memory latency: use them when the
  semantics allow.

## Build and test

```bash
cmake -B build -G Ninja && cmake --build build && ctest --test-dir build --output-on-failure
```

`-DBBTREE_SANITIZE="address;undefined"` (or `thread`) builds the tests
under sanitizers; `-DBBTREE_NATIVE=OFF` drops `-march=native`. The
benchmarks are `bench/*_bench.cpp`.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at
your option; [LICENSE](LICENSE) holds both texts.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in bbtree by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
