// Point operations on `shoal::unsync::TreeMap`: random and sequential
// inserts, hits, misses and random removals, for `uint64_t` and path-string
// keys, as the Rust reference timed the Rust map.
//
// usage: point_ops_bench [entries] [u64|string|all]
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "shoal/unsync/map.hpp"
#include "workload.hpp"

using shoal_bench::Keys;
using shoal_bench::Stopwatch;

namespace {

template <class Key, class MakeKey>
void run(const char* name, std::size_t entries, MakeKey make_key) {
  const std::vector<std::uint64_t> ids = Keys::present(entries);
  const std::vector<std::uint64_t> hit_ids = Keys::hits(ids, entries);
  const std::vector<std::uint64_t> miss_ids = Keys::misses(entries, entries);
  std::vector<Key> keys, hits, misses;
  keys.reserve(entries);
  for (std::uint64_t id : ids) keys.push_back(make_key(id));
  for (std::uint64_t id : hit_ids) hits.push_back(make_key(id));
  for (std::uint64_t id : miss_ids) misses.push_back(make_key(id));
  std::string cell;

  shoal::unsync::TreeMap<Key, std::uint64_t> map;
  {
    std::vector<Key> input = keys;
    Stopwatch watch;
    for (std::size_t index = 0; index < entries; ++index) {
      map.insert(std::move(input[index]), index);
    }
    shoal_bench::report((cell = std::string(name) + " insert_random").c_str(), entries,
                         watch.ns_per(entries));
  }
  {
    // `GET_ROUNDS` repeats the lookups, for counting instructions per lookup
    // by the difference of two runs (`/usr/bin/time -l`).
    const char* rounds_variable = std::getenv("GET_ROUNDS");
    const std::size_t rounds = rounds_variable ? std::strtoull(rounds_variable, nullptr, 10) : 1;
    Stopwatch watch;
    std::uint64_t sum = 0;
    for (std::size_t round = 0; round < rounds; ++round) {
      for (const Key& key : hits) sum += *map.get(key);
    }
    shoal_bench::keep(sum);
    shoal_bench::report((cell = std::string(name) + " get_hit").c_str(), entries,
                         watch.ns_per(entries));
  }
  {
    Stopwatch watch;
    std::size_t found = 0;
    for (const Key& key : misses) found += map.get(key) != nullptr;
    shoal_bench::keep(found);
    shoal_bench::report((cell = std::string(name) + " get_miss").c_str(), entries,
                         watch.ns_per(entries));
  }
  {
    Stopwatch watch;
    std::size_t removed = 0;
    for (const Key& key : hits) removed += map.remove(key).has_value();
    shoal_bench::keep(removed);
    shoal_bench::report((cell = std::string(name) + " remove_random").c_str(), entries,
                         watch.ns_per(entries));
  }
  map.clear();
  {
    std::vector<Key> sorted = keys;
    std::sort(sorted.begin(), sorted.end());
    Stopwatch watch;
    for (std::size_t index = 0; index < entries; ++index) {
      map.insert(std::move(sorted[index]), index);
    }
    shoal_bench::report((cell = std::string(name) + " insert_sequential").c_str(), entries,
                         watch.ns_per(entries));
  }
}

}  // namespace

int main(int argc, char** argv) {
  const std::size_t entries = argc > 1 ? std::strtoull(argv[1], nullptr, 10) : 1000000;
  const std::string which = argc > 2 ? argv[2] : "all";
  if (which == "all" || which == "u64") {
    run<std::uint64_t>("u64", entries, [](std::uint64_t id) { return id; });
  }
  if (which == "all" || which == "string") {
    run<std::string>("string", entries, [](std::uint64_t id) { return Keys::path(id); });
  }
  return 0;
}
