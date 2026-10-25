// Batched lookups and inserts of `bbtree::unsync::BBTreeMap`, against one
// key a call, as the Rust reference's `batch` suite timed the Rust map: hits
// and misses 64 keys a call, inserts in random order 64 entries a call.
//
// usage: batch_bench [entries]
#include <cstdint>
#include <cstdlib>
#include <optional>
#include <vector>

#include "bbtree/unsync/map.hpp"
#include "workload.hpp"

using bbtree_bench::Keys;
using bbtree_bench::Stopwatch;

int main(int argc, char** argv) {
  const std::size_t entries = argc > 1 ? std::strtoull(argv[1], nullptr, 10) : 10000000;
  constexpr std::size_t per_call = 64;
  const std::vector<std::uint64_t> keys = Keys::present(entries);
  const std::vector<std::uint64_t> hits = Keys::hits(keys, entries);
  const std::vector<std::uint64_t> misses = Keys::misses(entries, entries);
  bbtree::unsync::BBTreeMap<std::uint64_t, std::uint64_t> map;
  {
    std::vector<std::uint64_t> input = keys;
    std::vector<std::uint64_t> values(entries);
    for (std::size_t index = 0; index < entries; ++index) values[index] = index;
    std::vector<std::optional<std::uint64_t>> previous(per_call);
    Stopwatch watch;
    for (std::size_t start = 0; start < entries; start += per_call) {
      const std::size_t count = entries - start < per_call ? entries - start : per_call;
      map.insert_many(input.data() + start, values.data() + start, count, previous.data());
    }
    bbtree_bench::report("u64 batch_insert_random", entries, watch.ns_per(entries));
  }
  for (const auto* probes : {&hits, &misses}) {
    const char* cell = probes == &hits ? "u64 batch_get_hit" : "u64 batch_get_miss";
    const std::uint64_t* found[per_call];
    Stopwatch watch;
    std::uint64_t sum = 0;
    for (std::size_t start = 0; start + per_call <= entries; start += per_call) {
      map.get_many(probes->data() + start, per_call, found);
      for (const std::uint64_t* value : found) sum += value != nullptr ? *value : 1;
    }
    bbtree_bench::keep(sum);
    bbtree_bench::report(cell, entries, watch.ns_per(entries / per_call * per_call));
  }
  return 0;
}
