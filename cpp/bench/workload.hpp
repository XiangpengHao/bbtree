// The workloads the C++ benches and their Rust reference (`bench/reference`)
// share, so that both time the same operations on the same keys: keys are a
// bijective mix of their index, so `mix(i)` for `i < n` are `n` distinct
// keys in random order, and `mix(n + i)` are absent ones.
#pragma once

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

namespace bbtree_bench {

class Keys {
 public:
  /// The splitmix64 finalizer: a bijection of 64-bit words.
  static std::uint64_t mix(std::uint64_t value) {
    value += 0x9E3779B97F4A7C15ull;
    value = (value ^ (value >> 30)) * 0xBF58476D1CE4E5B9ull;
    value = (value ^ (value >> 27)) * 0x94D049BB133111EBull;
    return value ^ (value >> 31);
  }

  /// `count` distinct keys in random order.
  static std::vector<std::uint64_t> present(std::size_t count) {
    std::vector<std::uint64_t> keys(count);
    for (std::size_t index = 0; index < count; ++index) keys[index] = mix(index);
    return keys;
  }

  /// `count` probes of the present keys, in another random order.
  static std::vector<std::uint64_t> hits(const std::vector<std::uint64_t>& present,
                                         std::size_t count) {
    std::vector<std::uint64_t> probes(count);
    for (std::size_t index = 0; index < count; ++index) {
      probes[index] = present[mix(index ^ 0x5555555555555555ull) % present.size()];
    }
    return probes;
  }

  /// `count` keys none of which is present.
  static std::vector<std::uint64_t> misses(std::size_t present_count, std::size_t count) {
    std::vector<std::uint64_t> probes(count);
    for (std::size_t index = 0; index < count; ++index) probes[index] = mix(present_count + index);
    return probes;
  }

  /// A path-like string key for `value`, sharing long prefixes with others.
  static std::string path(std::uint64_t value) {
    char buffer[96];
    std::snprintf(buffer, sizeof(buffer), "/data/warehouse/table_%02u/part-%016llx.parquet",
                  static_cast<unsigned>(value % 37), static_cast<unsigned long long>(value));
    return buffer;
  }
};

/// Times a region, in nanoseconds per operation.
class Stopwatch {
 public:
  Stopwatch() : start_(std::chrono::steady_clock::now()) {}

  double ns_per(std::size_t operations) const {
    const auto elapsed = std::chrono::steady_clock::now() - start_;
    return std::chrono::duration<double, std::nano>(elapsed).count() / static_cast<double>(operations);
  }

 private:
  std::chrono::steady_clock::time_point start_;
};

/// Prints one result line: `cell ns`.
inline void report(const char* cell, std::size_t size, double ns) {
  std::printf("%-28s %10zu %10.1f ns/op\n", cell, size, ns);
  std::fflush(stdout);
}

/// Keeps `value` from being optimized away.
template <class T>
inline void keep(const T& value) {
  asm volatile("" : : "r,m"(value) : "memory");
}

}  // namespace bbtree_bench
