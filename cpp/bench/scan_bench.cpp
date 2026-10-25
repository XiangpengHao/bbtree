// Scans of `bbtree::unsync::BBTreeMap<uint64_t, uint64_t>`: whole-map
// iteration by `next` and by the folded loop, and 64-entry range scans from
// random present keys, as `bench/reference`'s `scan` suite times the Rust map.
//
// usage: scan_bench [entries]
#include <cstdint>
#include <cstdlib>
#include <vector>

#include "bbtree/unsync/map.hpp"
#include "workload.hpp"

using bbtree_bench::Keys;
using bbtree_bench::Stopwatch;

int main(int argc, char** argv) {
  const std::size_t entries = argc > 1 ? std::strtoull(argv[1], nullptr, 10) : 1000000;
  const std::vector<std::uint64_t> keys = Keys::present(entries);
  bbtree::unsync::BBTreeMap<std::uint64_t, std::uint64_t> map;
  for (std::size_t index = 0; index < entries; ++index) map.insert(keys[index], index);
  {
    Stopwatch watch;
    std::uint64_t sum = 0;
    auto range = map.iter();
    while (auto entry = range.next()) sum += entry->second;
    bbtree_bench::keep(sum);
    bbtree_bench::report("u64 iterate_next", entries, watch.ns_per(entries));
  }
  {
    Stopwatch watch;
    std::uint64_t sum = 0;
    map.for_each([&](std::uint64_t, std::uint64_t value) { sum += value; });
    bbtree_bench::keep(sum);
    bbtree_bench::report("u64 iterate_fold", entries, watch.ns_per(entries));
  }
  {
    const std::size_t scans = entries < 200000 ? entries : 200000;
    const std::vector<std::uint64_t> starts = Keys::hits(keys, scans);
    // `SCAN_ROUNDS` repeats the scans, for counting instructions per scan by
    // the difference of two runs (`/usr/bin/time -l`).
    const char* rounds_variable = std::getenv("SCAN_ROUNDS");
    const std::size_t rounds = rounds_variable ? std::strtoull(rounds_variable, nullptr, 10) : 1;
    Stopwatch watch;
    std::uint64_t sum = 0;
    for (std::size_t round = 0; round < rounds; ++round)
    for (const std::uint64_t start : starts) {
      auto range = map.range(bbtree::Bound<std::uint64_t>::included(start),
                             bbtree::Bound<std::uint64_t>::unbounded());
      for (int taken = 0; taken < 64; ++taken) {
        auto entry = range.next();
        if (!entry) break;
        sum += entry->second;
      }
    }
    bbtree_bench::keep(sum);
    bbtree_bench::report("u64 range64", entries, watch.ns_per(scans));
  }
  return 0;
}
