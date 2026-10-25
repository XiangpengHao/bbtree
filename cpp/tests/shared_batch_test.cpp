// The shared map's batches against single operations, with one thread and
// with writers racing them: `get_many_copied` and `read_many` find what single
// lookups find, `insert_many` makes the map single inserts make.
#define BBTREE_TEST_BATCHES 1

#include <atomic>
#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "bbtree/map.hpp"
#include "test_harness.hpp"

using bbtree::BBTreeMap;
using bbtree::Bound;

namespace {

template <class K, class MakeKey>
void check_against_single(std::uint64_t seed, std::uint64_t key_space, MakeKey make_key) {
  bbtree_test::Rng rng(seed);
  BBTreeMap<K, std::uint64_t> map;
  std::map<K, std::uint64_t> model;
  for (int round = 0; round < 3000; ++round) {
    const std::size_t count = 1 + rng.below(70);
    std::vector<K> keys;
    std::vector<std::uint64_t> values;
    for (std::size_t index = 0; index < count; ++index) {
      keys.push_back(make_key(rng.below(key_space)));
      values.push_back(round * 100 + index);
    }
    if (rng.below(3) != 0) {
      std::vector<K> copies = keys;
      std::vector<std::optional<std::uint64_t>> previous(count);
      std::vector<char> failed(count);
      map.insert_many(keys.data(), values.data(), count, previous.data(), reinterpret_cast<bool*>(failed.data()));
      for (std::size_t index = 0; index < count; ++index) {
        CHECK(!failed[index]);
        auto at = model.find(copies[index]);
        CHECK_EQ(previous[index].has_value(), at != model.end());
        if (at != model.end()) {
          CHECK_EQ(*previous[index], at->second);
          at->second = round * 100 + index;
        } else {
          model.emplace(copies[index], round * 100 + index);
        }
      }
    } else {
      std::vector<std::optional<std::uint64_t>> copied(count);
      map.get_many_copied(keys.data(), count, copied.data());
      std::vector<std::optional<std::uint64_t>> read(count);
      map.read_many(keys.data(), count, [&](std::size_t index, const K&, const std::uint64_t& value) {
        read[index] = value;
      });
      for (std::size_t index = 0; index < count; ++index) {
        auto at = model.find(keys[index]);
        CHECK_EQ(copied[index].has_value(), at != model.end());
        CHECK_EQ(read[index].has_value(), at != model.end());
        if (at != model.end()) {
          CHECK_EQ(*copied[index], at->second);
          CHECK_EQ(*read[index], at->second);
        }
      }
    }
    if (round % 97 == 0) {
      // Removals between batches, so that leaves merge and separators move.
      for (int removal = 0; removal < 40; ++removal) {
        const K key = make_key(rng.below(key_space));
        CHECK_EQ(map.remove(key).has_value(), model.erase(key) == 1);
      }
      map.check_invariants(model.size());
    }
  }
  map.check_invariants(model.size());
}

}  // namespace

TEST(shared_u64_batches_agree_with_single_operations) {
  check_against_single<std::uint64_t>(1, 60000, [](std::uint64_t key) { return key * 7919; });
}

TEST(shared_string_batches_agree_with_single_operations) {
  check_against_single<std::string>(2, 30000, [](std::uint64_t key) {
    return "/var/lib/" + std::to_string(key % 5) + "/" + std::to_string(key);
  });
}

TEST(batches_race_with_writers) {
  BBTreeMap<std::uint64_t, std::uint64_t> map;
  // Keys `k * 2` hold `k` throughout; writers churn the odd keys around them.
  for (std::uint64_t key = 0; key < 100000; ++key) map.insert(key * 2, key);
  std::atomic<bool> done{false};
  std::vector<std::thread> threads;
  for (int writer = 0; writer < 3; ++writer) {
    threads.emplace_back([&, writer] {
      bbtree_test::Rng rng(writer + 10);
      while (!done.load(std::memory_order_relaxed)) {
        const std::uint64_t odd = rng.below(100000) * 2 + 1;
        if (rng.below(2) == 0) {
          map.insert(odd, 0);
        } else {
          map.remove(odd);
        }
      }
    });
  }
  for (int reader = 0; reader < 3; ++reader) {
    threads.emplace_back([&, reader] {
      bbtree_test::Rng rng(reader + 20);
      for (int round = 0; round < 20000; ++round) {
        std::uint64_t keys[48];
        for (auto& key : keys) key = rng.below(100000) * 2;
        std::optional<std::uint64_t> values[48];
        map.get_many_copied(keys, 48, values);
        for (int index = 0; index < 48; ++index) {
          CHECK(values[index].has_value());
          CHECK_EQ(*values[index], keys[index] / 2);
        }
        // Batched inserts of the even keys change nothing.
        std::uint64_t copies[16], same[16];
        std::optional<std::uint64_t> previous[16];
        bool failed[16];
        for (int index = 0; index < 16; ++index) {
          copies[index] = keys[index];
          same[index] = keys[index] / 2;
        }
        map.insert_many(copies, same, 16, previous, failed);
        for (int index = 0; index < 16; ++index) {
          CHECK(previous[index].has_value() && *previous[index] == keys[index] / 2);
        }
      }
    });
  }
  for (std::size_t index = 3; index < threads.size(); ++index) threads[index].join();
  done.store(true);
  for (std::size_t index = 0; index < 3; ++index) threads[index].join();
  std::size_t count = 0;
  map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
           [&](const std::uint64_t&, const std::uint64_t&) { return ++count, true; });
  map.check_invariants(count);
}

TEST_MAIN()
