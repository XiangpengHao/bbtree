// Point and bulk operations on `shoal::unsync::HashMap<uint64_t,
// uint64_t>`, which probe its tags (D27): random inserts, then hits and
// misses one key a call and 64 keys a call. The default size leaves the
// map at 0.69 load, where clusters are longest, and its index far out of
// cache; 90000 entries is the same load in cache.
//
// usage: hash_bench [entries]
#include <cstdint>
#include <cstdlib>
#include <vector>

#include "shoal/unsync/hash_map.hpp"
#include "workload.hpp"

using shoal_bench::Keys;
using shoal_bench::Stopwatch;
using Map = shoal::unsync::HashMap<std::uint64_t, std::uint64_t>;

int main(int argc, char** argv) {
  const std::size_t entries = argc > 1 ? std::strtoull(argv[1], nullptr, 10) : 11500000;
  constexpr std::size_t per_call = 64;
  constexpr std::size_t probe_count = 10000000;
  const std::vector<std::uint64_t> keys = Keys::present(entries);
  const std::vector<std::uint64_t> hits = Keys::hits(keys, probe_count);
  const std::vector<std::uint64_t> misses = Keys::misses(entries, probe_count);
  Map map;
  {
    // Small maps fill again and again, so the cell times as many inserts
    // as a large one does, growth included.
    const std::size_t fills = entries < probe_count ? probe_count / entries : 1;
    Stopwatch watch;
    for (std::size_t fill = 0; fill < fills; ++fill) {
      map = Map();
      for (const std::uint64_t key : keys) map.insert(key, key);
    }
    shoal_bench::report("u64 hash_insert_random", entries, watch.ns_per(fills * entries));
  }
  for (const auto* probes : {&hits, &misses}) {
    const char* cell = probes == &hits ? "u64 hash_get_hit" : "u64 hash_get_miss";
    Stopwatch watch;
    std::uint64_t sum = 0;
    for (const std::uint64_t key : *probes) {
      const std::uint64_t* value = map.get(key);
      sum += value != nullptr ? *value : 1;
    }
    shoal_bench::keep(sum);
    shoal_bench::report(cell, entries, watch.ns_per(probes->size()));
  }
  for (const auto* probes : {&hits, &misses}) {
    const char* cell = probes == &hits ? "u64 hash_get_many_hit" : "u64 hash_get_many_miss";
    const std::uint64_t* found[per_call];
    Stopwatch watch;
    std::uint64_t sum = 0;
    for (std::size_t start = 0; start + per_call <= probes->size(); start += per_call) {
      map.get_many(probes->data() + start, per_call, found);
      for (const std::uint64_t* value : found) sum += value != nullptr ? *value : 1;
    }
    shoal_bench::keep(sum);
    shoal_bench::report(cell, entries, watch.ns_per(probes->size() / per_call * per_call));
  }
  return 0;
}
