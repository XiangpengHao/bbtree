// `shoal::TreeMap`, the map threads share: one thread against `std::map`,
// with the invariants checked as it goes, then threads racing on it, each
// checking what it can know, and the whole map checked after.
#include <atomic>
#include <cstdint>
#include <map>
#include <string>
#include <thread>
#include <vector>

#include "shoal/map.hpp"
#include "test_harness.hpp"

using shoal::TreeMap;
using shoal::Bound;

namespace {

template <class K, class MakeKey>
void single_thread_differential(std::uint64_t seed, std::size_t operations, std::uint64_t key_space,
                                MakeKey make_key) {
  shoal_test::Rng rng(seed);
  TreeMap<K, std::uint64_t> map;
  std::map<K, std::uint64_t> model;
  for (std::size_t step = 0; step < operations; ++step) {
    const K key = make_key(rng.below(key_space));
    const std::uint64_t choice = rng.below(100);
    if (choice < 45) {
      auto inserted = map.insert(key, step);
      CHECK(inserted.ok());
      auto at = model.find(key);
      CHECK_EQ(inserted.value().has_value(), at != model.end());
      if (at != model.end()) {
        CHECK_EQ(*inserted.value(), at->second);
        at->second = step;
      } else {
        model.emplace(key, step);
      }
    } else if (choice < 75) {
      const auto removed = map.remove(key);
      auto at = model.find(key);
      CHECK_EQ(removed.has_value(), at != model.end());
      if (at != model.end()) {
        CHECK_EQ(*removed, at->second);
        model.erase(at);
      }
    } else if (choice < 85) {
      const auto copied = map.get_copied(key);
      auto at = model.find(key);
      CHECK_EQ(copied.has_value(), at != model.end());
      if (copied) CHECK_EQ(*copied, at->second);
      const auto read = map.read(key, [](const K&, const std::uint64_t& value) { return value + 1; });
      CHECK_EQ(read.has_value(), at != model.end());
      if (read) CHECK_EQ(*read, at->second + 1);
      CHECK_EQ(map.contains_key(key), at != model.end());
    } else if (choice < 92) {
      const auto updated = map.update(key, [](const K&, std::uint64_t& value) { return ++value; });
      auto at = model.find(key);
      CHECK_EQ(updated.has_value(), at != model.end());
      if (updated) CHECK_EQ(*updated, ++at->second);
    } else if (choice < 96) {
      // remove_if only removes odd values.
      const auto removed = map.remove_if(key, [](const K&, const std::uint64_t& value) { return value % 2 == 1; });
      auto at = model.find(key);
      const bool removes = at != model.end() && at->second % 2 == 1;
      CHECK_EQ(removed.has_value(), removes);
      if (removes) model.erase(at);
    } else {
      // A scan of a few entries from the key, both ways.
      std::vector<K> seen;
      map.scan(Bound<K>::included(key), Bound<K>::unbounded(), [&](const K& stored, const std::uint64_t&) {
        seen.push_back(stored);
        return seen.size() < 20;
      });
      auto at = model.lower_bound(key);
      for (const K& stored : seen) {
        CHECK(at != model.end());
        CHECK(!(stored < at->first) && !(at->first < stored));
        ++at;
      }
      seen.clear();
      map.scan_rev(Bound<K>::unbounded(), Bound<K>::excluded(key), [&](const K& stored, const std::uint64_t&) {
        seen.push_back(stored);
        return seen.size() < 20;
      });
      auto back = model.lower_bound(key);
      for (const K& stored : seen) {
        CHECK(back != model.begin());
        --back;
        CHECK(!(stored < back->first) && !(back->first < stored));
      }
    }
    if (step % 499 == 0) map.check_invariants(model.size());
  }
  map.check_invariants(model.size());
  // A whole scan sees the model.
  auto at = model.begin();
  std::size_t visited = 0;
  map.scan(Bound<K>::unbounded(), Bound<K>::unbounded(), [&](const K& key, const std::uint64_t& value) {
    CHECK(at != model.end());
    CHECK(!(key < at->first) && !(at->first < key));
    CHECK_EQ(value, at->second);
    ++at;
    ++visited;
    return true;
  });
  CHECK_EQ(visited, model.size());
}

}  // namespace

TEST(u64_shared_map_agrees_with_std_map) {
  single_thread_differential<std::uint64_t>(1, 200000, 20000, [](std::uint64_t key) { return key * 7919; });
  single_thread_differential<std::uint64_t>(2, 100000, 3000, [](std::uint64_t key) { return key; });
}

TEST(string_shared_map_agrees_with_std_map) {
  single_thread_differential<std::string>(3, 100000, 6000, [](std::uint64_t key) {
    return "/var/lib/data/table_" + std::to_string(key % 7) + "/part-" + std::to_string(key);
  });
}

TEST(threads_insert_disjoint_keys) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::size_t threads = 8, per_thread = 50000;
  std::vector<std::thread> workers;
  for (std::size_t thread = 0; thread < threads; ++thread) {
    workers.emplace_back([&, thread] {
      shoal_test::Rng rng(thread + 1);
      for (std::size_t index = 0; index < per_thread; ++index) {
        // Interleaved keys, so that threads share leaves.
        const std::uint64_t key = (rng.next() % 1000000) * threads + thread;
        map.insert(key, key + 1);
        const auto copied = map.get_copied(key);
        CHECK(copied.has_value() && *copied == key + 1);
      }
    });
  }
  for (auto& worker : workers) worker.join();
  std::size_t count = 0;
  std::uint64_t previous = 0;
  map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
           [&](const std::uint64_t& key, const std::uint64_t& value) {
             CHECK(count == 0 || previous < key);
             CHECK_EQ(value, key + 1);
             previous = key;
             ++count;
             return true;
           });
  map.check_invariants(count);
}

TEST(threads_insert_and_remove_their_own_keys_while_others_read) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::size_t writers = 6, readers = 2, rounds = 40000;
  std::atomic<bool> done{false};
  std::vector<std::thread> workers;
  // Each writer owns keys `k * writers + id`: it knows exactly which are in.
  std::vector<std::map<std::uint64_t, std::uint64_t>> models(writers);
  for (std::size_t id = 0; id < writers; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(100 + id);
      auto& model = models[id];
      for (std::size_t round = 0; round < rounds; ++round) {
        const std::uint64_t key = rng.below(4000) * writers + id;
        if (rng.below(2) == 0) {
          const auto old = map.insert(key, round);
          CHECK(old.ok());
          CHECK_EQ(old.value().has_value(), model.count(key) == 1);
          model[key] = round;
        } else {
          const auto removed = map.remove(key);
          CHECK_EQ(removed.has_value(), model.count(key) == 1);
          if (removed) CHECK_EQ(*removed, model[key]);
          model.erase(key);
        }
      }
    });
  }
  for (std::size_t id = 0; id < readers; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(200 + id);
      while (!done.load(std::memory_order_relaxed)) {
        const std::uint64_t start = rng.below(4000 * writers);
        std::uint64_t previous = 0;
        bool first = true;
        map.scan(Bound<std::uint64_t>::included(start), Bound<std::uint64_t>::unbounded(),
                 [&](const std::uint64_t& key, const std::uint64_t&) {
                   CHECK(first || previous < key);
                   CHECK(key >= start);
                   first = false;
                   previous = key;
                   return key < start + 500;
                 });
        map.get_copied(rng.below(4000 * writers));
      }
    });
  }
  for (std::size_t id = 0; id < writers; ++id) workers[id].join();
  done.store(true);
  for (std::size_t id = writers; id < workers.size(); ++id) workers[id].join();
  std::size_t expected = 0;
  for (const auto& model : models) {
    expected += model.size();
    for (const auto& [key, value] : model) CHECK_EQ(map.get_copied(key).value_or(~value), value);
  }
  map.check_invariants(expected);
}

TEST(threads_share_string_keys) {
  TreeMap<std::string, std::uint64_t> map;
  constexpr std::size_t threads = 6, per_thread = 20000;
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < threads; ++id) {
    workers.emplace_back([&, id] {
      for (std::size_t index = 0; index < per_thread; ++index) {
        // Keys whose shared prefix shrinks as threads add them.
        std::string key = index % 3 == 0 ? "/a/" : "/var/lib/";
        key += std::to_string(id) + "/" + std::to_string(index * 7919 % 100003);
        map.insert(key, index);
        CHECK(map.read(key, [](const std::string&, const std::uint64_t&) { return true; }).has_value());
        if (index % 4 == 0) CHECK(map.remove(key).has_value());
      }
    });
  }
  for (auto& worker : workers) worker.join();
  std::size_t count = 0;
  map.scan(Bound<std::string>::unbounded(), Bound<std::string>::unbounded(),
           [&](const std::string&, const std::uint64_t&) { return ++count, true; });
  map.check_invariants(count);
}

TEST_MAIN()
