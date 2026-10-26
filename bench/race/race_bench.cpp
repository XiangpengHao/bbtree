// The race: concurrent lookups on random UUID keys, half of which miss, as
// upstream's README race (`OG-design/assets/readme-race.svg`, `get-half`)
// times the Rust maps (D32). shoal against the best-known C++ maps that
// threads can share, in two heats:
//
// - tree maps (ordered): shoal's `TreeMap`; oneTBB's `concurrent_map`
//   and folly's `ConcurrentSkipList` (skip lists); Abseil's `btree_map` and
//   `std::map` behind a `std::shared_mutex`.
// - hash maps (unordered): shoal's `HashMap` (shards read without locks);
//   Boost's `concurrent_flat_map`; folly's `ConcurrentHashMap`; oneTBB's
//   `concurrent_hash_map` and `concurrent_unordered_map`; Abseil's
//   `flat_hash_map` and `std::unordered_map` behind a `std::shared_mutex`.
//
// Each map takes its best use, as upstream's harness gives it: shoal's
// bulk lookups (`get_many_copied`, 64 keys a call) and its plain ones,
// and Boost's bulk visitation (`cvisit` over 64 keys); a
// shared map is built by every thread, a locked one by one thread, its
// writers taking turns. Every thread then looks up its own run of the
// probes, 64 at a time, until the time is up.
//
// usage: race_bench [entries] [threads] [seconds] [maps] [hit_percent];
// prints CSV. `maps`, comma-separated, runs only the maps whose names contain
// one of them ("" for all); `hit_percent` of the lookups find their key (50).
#include <absl/container/btree_map.h>
#include <absl/container/flat_hash_map.h>
#include <folly/ConcurrentSkipList.h>
#include <folly/concurrency/ConcurrentHashMap.h>
#include <oneapi/tbb/concurrent_hash_map.h>
#include <oneapi/tbb/concurrent_map.h>
#include <oneapi/tbb/concurrent_unordered_map.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <boost/unordered/concurrent_flat_map.hpp>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>
#include <shared_mutex>
#include <thread>
#include <type_traits>
#include <utility>
#include <vector>

#include "../workload.hpp"
#include "shoal/hash_map.hpp"
#include "shoal/map.hpp"

namespace {

using Uuid = std::array<std::uint8_t, 16>;
using Value = std::uint64_t;

/// Lookups go 64 keys at a time, as many as shoal's bulk readers take.
constexpr std::size_t lookups_per_call = 64;

/// Random version-4 UUIDs, as upstream's `uuid` keys: `uuid(i)` for `i < n`
/// are the present keys, `uuid(n + i)` absent ones.
class UuidKeys {
 public:
  static Uuid uuid(std::uint64_t index) {
    const std::uint64_t words[2] = {shoal_bench::Keys::mix(2 * index),
                                    shoal_bench::Keys::mix(2 * index + 1)};
    Uuid key;
    std::memcpy(key.data(), words, sizeof(words));
    key[6] = static_cast<std::uint8_t>((key[6] & 0x0F) | 0x40);
    key[8] = static_cast<std::uint8_t>((key[8] & 0x3F) | 0x80);
    return key;
  }

  /// A UUID's 128 bits are already random: its hash folds them.
  struct Hash {
    using is_avalanching = std::true_type;
    std::size_t operator()(const Uuid& key) const noexcept {
      std::uint64_t words[2];
      std::memcpy(words, key.data(), sizeof(words));
      return static_cast<std::size_t>(words[0] ^ words[1]);
    }
  };
};

/// The keys every map holds, each with value `index + 1`, and the probes
/// they all look up: `hit_percent` of them present keys, the others absent
/// ones, in random order.
class RaceKeys {
 public:
  RaceKeys(std::size_t entries, unsigned hit_percent)
      : present_(entries), probes_(entries), hit_percent_(hit_percent) {
    for (std::size_t index = 0; index < entries; ++index) present_[index] = UuidKeys::uuid(index);
    for (std::size_t index = 0; index < entries; ++index) {
      const std::uint64_t draw = shoal_bench::Keys::mix(index ^ 0x5555555555555555ull);
      const bool hit = draw % 100 < hit_percent;
      const std::size_t target = static_cast<std::size_t>((draw / 100) % entries);
      probes_[index] = hit ? present_[target] : UuidKeys::uuid(entries + index);
      if (index < check_count) expected_check_sum_ += hit ? target + 1 : 0;
    }
  }

  const std::vector<Uuid>& present() const { return present_; }
  const std::vector<Uuid>& probes() const { return probes_; }
  unsigned hit_percent() const { return hit_percent_; }

  /// The first probes, which a map must answer right before it races.
  static constexpr std::size_t check_count = 1 << 16;
  Value expected_check_sum() const { return expected_check_sum_; }

 private:
  std::vector<Uuid> present_;
  std::vector<Uuid> probes_;
  unsigned hit_percent_;
  Value expected_check_sum_ = 0;
};

/// Runs `work(thread)` on `threads` threads at once.
class ThreadTeam {
 public:
  template <class Work>
  static void run(std::size_t threads, Work work) {
    std::vector<std::thread> workers;
    for (std::size_t thread = 0; thread < threads; ++thread) workers.emplace_back(work, thread);
    for (auto& worker : workers) worker.join();
  }
};

// The contestants. Each builds from the present keys, and hands every
// thread a reader whose `look_up` sums the values of the keys it finds.

class TreeMapContestant {
 public:
  void build(const std::vector<Uuid>& keys, std::size_t threads) {
    ThreadTeam::run(threads, [&](std::size_t thread) {
      for (std::size_t index = thread; index < keys.size(); index += threads) {
        if (!map_.insert(keys[index], index + 1).ok()) std::abort();
      }
    });
  }

 protected:
  shoal::TreeMap<Uuid, Value> map_;
};

class TreeMapBulk : public TreeMapContestant {
 public:
  static constexpr const char* name = "shoal::TreeMap (bulk)";

  class Reader {
   public:
    explicit Reader(const shoal::TreeMap<Uuid, Value>& map) : map_(map) {}
    Value look_up(const Uuid* probes) {
      std::optional<Value> values[lookups_per_call];
      map_.get_many_copied(probes, lookups_per_call, values);
      Value sum = 0;
      for (const auto& value : values) sum += value.value_or(0);
      return sum;
    }

   private:
    const shoal::TreeMap<Uuid, Value>& map_;
  };
  Reader reader() const { return Reader(map_); }
};

class TreeMapSingle : public TreeMapContestant {
 public:
  static constexpr const char* name = "shoal::TreeMap";

  class Reader {
   public:
    explicit Reader(const shoal::TreeMap<Uuid, Value>& map) : map_(map) {}
    Value look_up(const Uuid* probes) {
      Value sum = 0;
      for (std::size_t index = 0; index < lookups_per_call; ++index) {
        sum += map_.get_copied(probes[index]).value_or(0);
      }
      return sum;
    }

   private:
    const shoal::TreeMap<Uuid, Value>& map_;
  };
  Reader reader() const { return Reader(map_); }
};

/// A map whose `emplace` and `find` threads may call at once.
template <class Map>
class ConcurrentFindMap {
 public:
  void build(const std::vector<Uuid>& keys, std::size_t threads) {
    ThreadTeam::run(threads, [&](std::size_t thread) {
      for (std::size_t index = thread; index < keys.size(); index += threads) map_.emplace(keys[index], index + 1);
    });
  }

  class Reader {
   public:
    explicit Reader(const Map& map) : map_(map) {}
    Value look_up(const Uuid* probes) {
      Value sum = 0;
      for (std::size_t index = 0; index < lookups_per_call; ++index) {
        const auto found = map_.find(probes[index]);
        if (found != map_.cend()) sum += found->second;
      }
      return sum;
    }

   private:
    const Map& map_;
  };
  Reader reader() const { return Reader(map_); }

 private:
  Map map_;
};

class TbbConcurrentMap : public ConcurrentFindMap<tbb::concurrent_map<Uuid, Value>> {
 public:
  static constexpr const char* name = "tbb::concurrent_map";
};

class FollyConcurrentSkipList {
 public:
  static constexpr const char* name = "folly::ConcurrentSkipList";
  using Entry = std::pair<Uuid, Value>;
  struct EntryOrder {
    bool operator()(const Entry& left, const Entry& right) const { return left.first < right.first; }
  };
  using SkipList = folly::ConcurrentSkipList<Entry, EntryOrder>;

  void build(const std::vector<Uuid>& keys, std::size_t threads) {
    ThreadTeam::run(threads, [&](std::size_t thread) {
      typename SkipList::Accessor accessor(list_);
      for (std::size_t index = thread; index < keys.size(); index += threads) accessor.add({keys[index], index + 1});
    });
  }

  /// A reader holds one accessor for the race: taking one per lookup would
  /// race every thread on the list's reference count.
  class Reader {
   public:
    explicit Reader(const std::shared_ptr<SkipList>& list) : accessor_(list) {}
    Value look_up(const Uuid* probes) {
      Value sum = 0;
      for (std::size_t index = 0; index < lookups_per_call; ++index) {
        const auto found = accessor_.find(Entry{probes[index], 0});
        if (found != accessor_.end()) sum += found->second;
      }
      return sum;
    }

   private:
    typename SkipList::Accessor accessor_;
  };
  Reader reader() const { return Reader(list_); }

 private:
  std::shared_ptr<SkipList> list_ = SkipList::createInstance(16);
};

/// A map behind a reader-writer lock, taken for every lookup, as upstream's
/// `RwLock<BTreeMap>`. Built by one thread: its writers would take turns.
template <class Map>
class SharedMutexMap {
 public:
  void build(const std::vector<Uuid>& keys, std::size_t) {
    std::unique_lock lock(mutex_);
    for (std::size_t index = 0; index < keys.size(); ++index) map_.emplace(keys[index], index + 1);
  }

  class Reader {
   public:
    explicit Reader(const SharedMutexMap& owner) : owner_(owner) {}
    Value look_up(const Uuid* probes) {
      Value sum = 0;
      for (std::size_t index = 0; index < lookups_per_call; ++index) {
        std::shared_lock lock(owner_.mutex_);
        const auto found = owner_.map_.find(probes[index]);
        if (found != owner_.map_.end()) sum += found->second;
      }
      return sum;
    }

   private:
    const SharedMutexMap& owner_;
  };
  Reader reader() const { return Reader(*this); }

 private:
  mutable std::shared_mutex mutex_;
  Map map_;
};

class AbslBtreeMap : public SharedMutexMap<absl::btree_map<Uuid, Value>> {
 public:
  static constexpr const char* name = "absl::btree_map + shared_mutex";
};

class StdMap : public SharedMutexMap<std::map<Uuid, Value>> {
 public:
  static constexpr const char* name = "std::map + shared_mutex";
};

class AbslFlatHashMap : public SharedMutexMap<absl::flat_hash_map<Uuid, Value, UuidKeys::Hash>> {
 public:
  static constexpr const char* name = "absl::flat_hash_map + shared_mutex";
};

class StdUnorderedMap : public SharedMutexMap<std::unordered_map<Uuid, Value, UuidKeys::Hash>> {
 public:
  static constexpr const char* name = "std::unordered_map + shared_mutex";
};

class FollyConcurrentHashMap
    : public ConcurrentFindMap<folly::ConcurrentHashMap<Uuid, Value, UuidKeys::Hash>> {
 public:
  static constexpr const char* name = "folly::ConcurrentHashMap";
};

class TbbConcurrentUnorderedMap
    : public ConcurrentFindMap<tbb::concurrent_unordered_map<Uuid, Value, UuidKeys::Hash>> {
 public:
  static constexpr const char* name = "tbb::concurrent_unordered_map";
};

/// oneTBB's classic hash map: a lookup holds the bucket's read lock through
/// an accessor.
class TbbConcurrentHashMap {
 public:
  static constexpr const char* name = "tbb::concurrent_hash_map";
  struct HashCompare {
    std::size_t hash(const Uuid& key) const { return UuidKeys::Hash()(key); }
    bool equal(const Uuid& left, const Uuid& right) const { return left == right; }
  };
  using Map = tbb::concurrent_hash_map<Uuid, Value, HashCompare>;

  void build(const std::vector<Uuid>& keys, std::size_t threads) {
    ThreadTeam::run(threads, [&](std::size_t thread) {
      for (std::size_t index = thread; index < keys.size(); index += threads) map_.emplace(keys[index], index + 1);
    });
  }

  class Reader {
   public:
    explicit Reader(const Map& map) : map_(map) {}
    Value look_up(const Uuid* probes) {
      Value sum = 0;
      for (std::size_t index = 0; index < lookups_per_call; ++index) {
        typename Map::const_accessor accessor;
        if (map_.find(accessor, probes[index])) sum += accessor->second;
      }
      return sum;
    }

   private:
    const Map& map_;
  };
  Reader reader() const { return Reader(map_); }

 private:
  Map map_;
};

class HashMapContestant {
 public:
  using Map = shoal::HashMap<Uuid, Value, UuidKeys::Hash>;

  void build(const std::vector<Uuid>& keys, std::size_t threads) {
    ThreadTeam::run(threads, [&](std::size_t thread) {
      for (std::size_t index = thread; index < keys.size(); index += threads) {
        if (!map_.insert(keys[index], index + 1).ok()) std::abort();
      }
    });
  }

 protected:
  Map map_;
};

class HashMapBulk : public HashMapContestant {
 public:
  static constexpr const char* name = "shoal::HashMap (bulk)";

  class Reader {
   public:
    explicit Reader(const Map& map) : map_(map) {}
    Value look_up(const Uuid* probes) {
      std::optional<Value> values[lookups_per_call];
      map_.get_many_copied(probes, lookups_per_call, values);
      Value sum = 0;
      for (const auto& value : values) sum += value.value_or(0);
      return sum;
    }

   private:
    const Map& map_;
  };
  Reader reader() const { return Reader(map_); }
};

class HashMapSingle : public HashMapContestant {
 public:
  static constexpr const char* name = "shoal::HashMap";

  class Reader {
   public:
    explicit Reader(const Map& map) : map_(map) {}
    Value look_up(const Uuid* probes) {
      Value sum = 0;
      for (std::size_t index = 0; index < lookups_per_call; ++index) {
        sum += map_.get_copied(probes[index]).value_or(0);
      }
      return sum;
    }

   private:
    const Map& map_;
  };
  Reader reader() const { return Reader(map_); }
};

class BoostConcurrentFlatMap {
 public:
  static constexpr const char* name = "boost::concurrent_flat_map";
  using Map = boost::concurrent_flat_map<Uuid, Value, UuidKeys::Hash>;

  void build(const std::vector<Uuid>& keys, std::size_t threads) {
    map_.reserve(keys.size());
    ThreadTeam::run(threads, [&](std::size_t thread) {
      for (std::size_t index = thread; index < keys.size(); index += threads) map_.emplace(keys[index], index + 1);
    });
  }

  class Reader {
   public:
    explicit Reader(const Map& map) : map_(map) {}
    Value look_up(const Uuid* probes) {
      // Bulk visitation, which overlaps the keys' waits for memory, as
      // shoal's bulk reads do.
      Value sum = 0;
      map_.cvisit(probes, probes + lookups_per_call, [&](const auto& entry) { sum += entry.second; });
      return sum;
    }

   private:
    const Map& map_;
  };
  Reader reader() const { return Reader(map_); }

 private:
  Map map_;
};

/// Builds each contestant in turn, checks its answers, and times its
/// threads looking up the probes for a fixed time.
class Race {
 public:
  Race(std::size_t entries, unsigned hit_percent, std::size_t threads, double seconds,
       std::vector<std::string> only)
      : keys_(entries, hit_percent), threads_(threads), seconds_(seconds), only_(std::move(only)) {}

  template <class Contestant>
  void run() {
    if (!selected(Contestant::name)) return;
    auto contestant = std::make_unique<Contestant>();
    shoal_bench::Stopwatch build_watch;
    contestant->build(keys_.present(), threads_);
    const double build_seconds = build_watch.ns_per(1) / 1e9;
    check(*contestant);

    const std::vector<Uuid>& probes = keys_.probes();
    const std::size_t calls_in_probes = probes.size() / lookups_per_call;
    std::atomic<std::size_t> ready{0};
    std::atomic<bool> go{false};
    std::atomic<bool> stop{false};
    std::vector<std::uint64_t> calls_by_thread(threads_, 0);
    std::chrono::steady_clock::time_point start;
    std::chrono::steady_clock::time_point end;
    std::thread timer([&] {
      while (ready.load() < threads_) std::this_thread::yield();
      start = std::chrono::steady_clock::now();
      go.store(true);
      std::this_thread::sleep_for(std::chrono::duration<double>(seconds_));
      stop.store(true);
      end = std::chrono::steady_clock::now();
    });
    ThreadTeam::run(threads_, [&](std::size_t thread) {
      auto reader = contestant->reader();
      std::size_t call = thread * calls_in_probes / threads_;
      std::uint64_t calls = 0;
      Value sum = 0;
      ready.fetch_add(1);
      while (!go.load(std::memory_order_relaxed)) std::this_thread::yield();
      while (!stop.load(std::memory_order_relaxed)) {
        sum += reader.look_up(probes.data() + call * lookups_per_call);
        if (++call == calls_in_probes) call = 0;
        ++calls;
      }
      shoal_bench::keep(sum);
      calls_by_thread[thread] = calls;
    });
    timer.join();

    std::uint64_t calls = 0;
    for (const std::uint64_t thread_calls : calls_by_thread) calls += thread_calls;
    const double elapsed = std::chrono::duration<double>(end - start).count();
    const double mops = static_cast<double>(calls * lookups_per_call) / elapsed / 1e6;
    std::printf("%s,%zu,%u,%zu,%.1f,%.1f\n", Contestant::name, keys_.present().size(), keys_.hit_percent(),
                threads_, mops, build_seconds);
    std::fflush(stdout);
  }

 private:
  template <class Contestant>
  void check(const Contestant& contestant) const {
    auto reader = contestant.reader();
    Value sum = 0;
    for (std::size_t call = 0; call < RaceKeys::check_count / lookups_per_call; ++call) {
      sum += reader.look_up(keys_.probes().data() + call * lookups_per_call);
    }
    if (sum != keys_.expected_check_sum()) {
      std::fprintf(stderr, "%s: wrong answers\n", Contestant::name);
      std::abort();
    }
  }

  bool selected(const std::string& name) const {
    if (only_.empty()) return true;
    for (const auto& part : only_) {
      if (name.find(part) != std::string::npos) return true;
    }
    return false;
  }

  RaceKeys keys_;
  std::size_t threads_;
  double seconds_;
  std::vector<std::string> only_;
};

}  // namespace

int main(int argc, char** argv) {
  const std::size_t entries = argc > 1 ? std::strtoull(argv[1], nullptr, 10) : 10000000;
  const std::size_t threads = argc > 2 ? std::strtoull(argv[2], nullptr, 10) : std::thread::hardware_concurrency();
  const double seconds = argc > 3 ? std::strtod(argv[3], nullptr) : 2.0;
  if (entries < RaceKeys::check_count) {
    std::fprintf(stderr, "race_bench: at least %zu entries\n", RaceKeys::check_count);
    return 1;
  }

  std::vector<std::string> only;
  if (argc > 4) {
    const std::string list = argv[4];
    for (std::size_t start = 0; start <= list.size();) {
      const std::size_t comma = std::min(list.find(',', start), list.size());
      if (comma > start) only.push_back(list.substr(start, comma - start));
      start = comma + 1;
    }
  }
  const unsigned hit_percent = argc > 5 ? static_cast<unsigned>(std::strtoul(argv[5], nullptr, 10)) : 50;
  if (hit_percent > 100) {
    std::fprintf(stderr, "race_bench: hit_percent is at most 100\n");
    return 1;
  }
  Race race(entries, hit_percent, threads, seconds, std::move(only));
  std::printf("map,entries,hit_percent,threads,mops,build_seconds\n");
  race.run<TreeMapBulk>();
  race.run<TreeMapSingle>();
  race.run<TbbConcurrentMap>();
  race.run<FollyConcurrentSkipList>();
  race.run<AbslBtreeMap>();
  race.run<StdMap>();
  race.run<HashMapBulk>();
  race.run<HashMapSingle>();
  race.run<BoostConcurrentFlatMap>();
  race.run<FollyConcurrentHashMap>();
  race.run<TbbConcurrentHashMap>();
  race.run<TbbConcurrentUnorderedMap>();
  race.run<AbslFlatHashMap>();
  race.run<StdUnorderedMap>();
  return 0;
}
