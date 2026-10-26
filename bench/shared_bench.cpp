// The shared `shoal::TreeMap<uint64_t, uint64_t>` under threads: a
// parallel build by random inserts, then `get_copied` hits, at each thread
// count, as the Rust reference's `shared` suite timed the Rust map.
//
// usage: shared_bench [entries] [threads,...]
#include <cstdint>
#include <cstdlib>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "shoal/map.hpp"
#include "workload.hpp"

using shoal_bench::Keys;
using shoal_bench::Stopwatch;

namespace {

/// Runs `work(thread)` on `threads` threads at once.
template <class Work>
void on_threads(std::size_t threads, Work work) {
  std::vector<std::thread> workers;
  for (std::size_t thread = 0; thread < threads; ++thread) workers.emplace_back(work, thread);
  for (auto& worker : workers) worker.join();
}

}  // namespace

int main(int argc, char** argv) {
  const std::size_t entries = argc > 1 ? std::strtoull(argv[1], nullptr, 10) : 10000000;
  std::vector<std::size_t> thread_counts = {1, 4, 8, 12};
  if (argc > 2) {
    thread_counts.clear();
    std::string list = argv[2];
    for (std::size_t start = 0; start < list.size();) {
      const std::size_t comma = list.find(',', start);
      thread_counts.push_back(std::stoul(list.substr(start, comma - start)));
      start = comma == std::string::npos ? list.size() : comma + 1;
    }
  }
  const std::vector<std::uint64_t> keys = Keys::present(entries);
  const std::vector<std::uint64_t> hits = Keys::hits(keys, entries);
  for (const std::size_t threads : thread_counts) {
    shoal::TreeMap<std::uint64_t, std::uint64_t> map;
    {
      Stopwatch watch;
      on_threads(threads, [&](std::size_t thread) {
        for (std::size_t index = thread; index < entries; index += threads) map.insert(keys[index], index);
      });
      const double ns = watch.ns_per(entries);
      std::printf("shared insert   threads %2zu  %8.1f Mops/s\n", threads, 1000.0 / ns);
    }
    {
      // `GET_ROUNDS` repeats the lookups, as `BATCH_ROUNDS` the batched ones.
      const char* rounds_variable = std::getenv("GET_ROUNDS");
      const std::size_t rounds = rounds_variable ? std::strtoull(rounds_variable, nullptr, 10) : 1;
      Stopwatch watch;
      on_threads(threads, [&](std::size_t thread) {
        std::uint64_t sum = 0;
        for (std::size_t round = 0; round < rounds; ++round)
        for (std::size_t index = thread; index < entries; index += threads) sum += *map.get_copied(hits[index]);
        shoal_bench::keep(sum);
      });
      const double ns = watch.ns_per(entries);
      std::printf("shared get_hit  threads %2zu  %8.1f Mops/s\n", threads, 1000.0 / ns);
    }
    {
      // 64 keys a call, each thread its own calls.
      constexpr std::size_t per_call = 64;
      const std::size_t calls = entries / per_call;
      // `BATCH_ROUNDS` repeats the batched lookups, for counting
      // instructions per key by the difference of two runs.
      const char* rounds_variable = std::getenv("BATCH_ROUNDS");
      const std::size_t rounds = rounds_variable ? std::strtoull(rounds_variable, nullptr, 10) : 1;
      Stopwatch watch;
      on_threads(threads, [&](std::size_t thread) {
        std::uint64_t sum = 0;
        std::optional<std::uint64_t> values[per_call];
        for (std::size_t round = 0; round < rounds; ++round)
        for (std::size_t call = thread; call < calls; call += threads) {
          map.get_many_copied(hits.data() + call * per_call, per_call, values);
          for (const auto& value : values) sum += *value;
        }
        shoal_bench::keep(sum);
      });
      const double ns = watch.ns_per(calls * per_call);
      std::printf("shared batch_get_hit threads %2zu  %8.1f Mops/s\n", threads, 1000.0 / ns);
    }
  }
  return 0;
}
