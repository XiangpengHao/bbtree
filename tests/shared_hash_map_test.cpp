#include <array>
#include <atomic>
#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

#include "bbtree/hash_map.hpp"
#include "test_harness.hpp"

namespace {

struct RefusingAllocator {
  void *allocate_zeroed(bbtree::Layout) noexcept { return nullptr; }
  void deallocate(void *, bbtree::Layout) noexcept { CHECK(false); }
};

struct ThrowingHash {
  std::uint64_t operator()(int) const { throw std::bad_alloc(); }
};

class MoveConstructOnly {
 public:
  explicit MoveConstructOnly(int value) noexcept : value_(value) {}
  MoveConstructOnly(const MoveConstructOnly &) = default;
  MoveConstructOnly(MoveConstructOnly &&other) noexcept : value_(std::exchange(other.value_, -1)) {}
  MoveConstructOnly &operator=(const MoveConstructOnly &) = delete;
  MoveConstructOnly &operator=(MoveConstructOnly &&) = delete;
  int value() const noexcept { return value_; }

 private:
  int value_;
};

class BudgetAllocator {
 public:
  BudgetAllocator(std::size_t *budget, std::size_t *bytes) noexcept
      : budget_(budget), bytes_(bytes) {}
  void *allocate_zeroed(bbtree::Layout layout) noexcept {
    if (*budget_ == 0) return nullptr;
    --*budget_;
    auto *memory = bbtree::Global{}.allocate_zeroed(layout);
    if (memory) *bytes_ += layout.size;
    return memory;
  }
  void deallocate(void *memory, bbtree::Layout layout) noexcept {
    *bytes_ -= layout.size;
    bbtree::Global{}.deallocate(memory, layout);
  }

 private:
  std::size_t *budget_;
  std::size_t *bytes_;
};

}  // namespace

TEST(shared_hash_map_scoped_access_and_removal) {
  const bbtree::BBHashMap<std::string, int> map;
  CHECK(map.insert("one", 1).ok());
  auto previous = map.insert("one", 2);
  CHECK(previous.ok());
  CHECK_EQ(*previous.value(), 1);
  const auto read =
      map.read("one", [](const auto &key, const int &value) { return key.size() + value; });
  CHECK_EQ(*read, 5u);
  const auto updated = map.update("one", [](const auto &, int &value) { return ++value; });
  CHECK_EQ(*updated, 3);
  CHECK(map.contains_key(std::string_view("one")));
  const auto copied = map.get_copied("one");
  CHECK_EQ(*copied, 3);
  CHECK(map.read("one", [](const auto &, const int &) {}));
  CHECK(!map.read("missing", [](const auto &, const int &) {}));
  CHECK(!map.remove_if("one", [](const auto &, const int &value) { return value == 0; }));
  const auto removed =
      map.remove_if("one", [](const auto &, const int &value) { return value == 3; });
  CHECK(removed);
  CHECK_EQ(removed->first, "one");
  CHECK(map.empty());
  map.check_invariants();
}

TEST(shared_hash_map_readers_and_writers_agree_on_disjoint_keys) {
  bbtree::BBHashMap<int, int> map;
  std::vector<std::thread> threads;
  for (int thread = 0; thread < 6; ++thread) {
    threads.emplace_back([&, thread] {
      for (int offset = 0; offset < 3000; ++offset) {
        const int key = offset * 6 + thread;
        CHECK(map.insert(key, key + 1));
        const auto found = map.get_copied(key);
        CHECK(found && *found == key + 1);
        if (offset % 3 == 0) CHECK(map.remove(key));
      }
    });
  }
  for (auto &thread : threads) thread.join();
  CHECK_EQ(map.size(), 12000u);
  std::size_t visited = 0;
  map.for_each([&](const int &key, const int &value) {
    CHECK(key / 6 % 3 != 0);
    CHECK_EQ(value, key + 1);
    ++visited;
  });
  CHECK_EQ(visited, map.size());
  map.check_invariants();
}

TEST(shared_hash_map_updates_same_key_without_losing_writes) {
  bbtree::BBHashMap<int, int> map;
  CHECK(map.insert(7, 0));
  std::vector<std::thread> threads;
  for (int thread = 0; thread < 6; ++thread) {
    threads.emplace_back([&] {
      for (int index = 0; index < 5000; ++index)
        CHECK(map.update(7, [](const int &, int &value) { ++value; }));
    });
  }
  for (auto &thread : threads) thread.join();
  const auto copied = map.get_copied(7);
  CHECK_EQ(*copied, 30000);
  CHECK_EQ(map.size(), 1u);
}

TEST(shared_hash_map_reports_allocator_refusals_without_swallowing_user_exceptions) {
  bbtree::BBHashMap<int, std::unique_ptr<int>, bbtree::Hash<int>, std::equal_to<>,
                    RefusingAllocator>
      map;
  auto result = map.insert(8, std::make_unique<int>(9));
  CHECK(!result.ok());
  CHECK_EQ(result.error().key(), 8);
  CHECK_EQ(*result.error().value(), 9);
  CHECK(result.error().layout().size != 0);
  CHECK(map.empty());

  bbtree::BBHashMap<int, int, ThrowingHash> throwing;
  bool threw = false;
  try {
    throwing.insert(1, 2);
  } catch (const std::bad_alloc &) {
    threw = true;
  }
  CHECK(threw);
  CHECK(throwing.empty());
}

TEST(shared_hash_map_batch_results_have_input_order) {
  bbtree::BBHashMap<int, int> map;
  std::array<int, 67> keys{}, values{};
  std::array<std::optional<int>, 67> previous{};
  std::array<bool, 67> failed{};
  for (std::size_t index = 0; index < keys.size(); ++index) {
    keys[index] = static_cast<int>(index % 11);
    values[index] = static_cast<int>(index);
  }
  map.insert_many(keys.data(), values.data(), keys.size(), previous.data(), failed.data());
  for (std::size_t index = 0; index < keys.size(); ++index) {
    CHECK(!failed[index]);
    CHECK_EQ(previous[index].has_value(), index >= 11);
    if (previous[index]) CHECK_EQ(*previous[index], static_cast<int>(index - 11));
  }
  keys[3] = 9999;
  map.get_many_copied(keys.data(), keys.size(), previous.data());
  for (std::size_t index = 0; index < keys.size(); ++index)
    CHECK(previous[index] == map.get_copied(keys[index]));
  std::size_t visited = 0;
  map.read_many(keys.data(), keys.size(), [&](std::size_t index, const int &key, const int &value) {
    CHECK(index != 3);
    CHECK_EQ(key, keys[index]);
    CHECK(value >= 56);
    ++visited;
  });
  CHECK_EQ(visited, keys.size() - 1);
  map.clear();
  CHECK(map.empty());
}

TEST(shared_hash_map_batches_accept_values_without_assignment) {
  bbtree::BBHashMap<int, MoveConstructOnly> map;
  int keys[] = {1, 1};
  MoveConstructOnly values[] = {MoveConstructOnly(10), MoveConstructOnly(20)};
  std::optional<MoveConstructOnly> previous[2];
  bool failed[2]{};
  map.insert_many(keys, values, 2, previous, failed);
  CHECK(!previous[0]);
  CHECK_EQ(previous[1]->value(), 10);
  map.get_many_copied(keys, 2, previous);
  CHECK_EQ(previous[0]->value(), 20);

  bbtree::BBHashMap<int, MoveConstructOnly, bbtree::Hash<int>, std::equal_to<>, RefusingAllocator>
      refused;
  MoveConstructOnly refused_values[] = {MoveConstructOnly(10), MoveConstructOnly(20)};
  refused.insert_many(keys, refused_values, 2, previous, failed);
  CHECK(failed[0] && failed[1]);
  CHECK_EQ(refused_values[0].value(), 10);
  CHECK_EQ(refused_values[1].value(), 20);
}

TEST(shared_hash_map_refusals_during_growth_return_the_entry_and_allow_retry) {
  using Map = bbtree::BBHashMap<int, std::unique_ptr<int>, bbtree::Hash<int>, std::equal_to<>,
                                BudgetAllocator, 1>;
  for (std::size_t allocations = 0; allocations < 20; ++allocations) {
    std::size_t budget = allocations, bytes = 0;
    {
      Map map(bbtree::Hash<int>{}, std::equal_to<>{}, BudgetAllocator(&budget, &bytes));
      for (int key = 0; key < 1000; ++key) {
        auto inserted = map.insert(key, std::make_unique<int>(key + 1));
        if (inserted.ok()) continue;
        CHECK_EQ(inserted.error().key(), key);
        CHECK_EQ(*inserted.error().value(), key + 1);
        CHECK_EQ(map.size(), static_cast<std::size_t>(key));
        for (int previous = 0; previous < key; ++previous) {
          const auto read =
              map.read(previous, [](const int &, const auto &value) { return *value; });
          CHECK_EQ(*read, previous + 1);
        }
        map.check_invariants();
        budget = 100;
        auto entry = std::move(inserted.error()).into_entry();
        CHECK(map.insert(std::move(entry.first), std::move(entry.second)));
        CHECK_EQ(map.size(), static_cast<std::size_t>(key + 1));
        break;
      }
    }
    CHECK_EQ(bytes, 0u);
  }
}

TEST(shared_hash_map_callback_exception_releases_lock) {
  bbtree::BBHashMap<int, int> map;
  CHECK(map.insert(1, 2));
  bool threw = false;
  try {
    map.update(1, [](const int &, int &value) {
      ++value;
      throw std::runtime_error("callback refused");
    });
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  const auto copied = map.get_copied(1);
  CHECK_EQ(*copied, 3);
  CHECK(map.remove(1));
}

// A shard's table moves its entries: growing, and closing the hole a
// removal leaves (Algorithm R). Checked against std::unordered_map on one
// shard, with a hash that sends every eighth key to the same slot, so that
// probe runs are long and wrap, and with an ordinary one; values are strings,
// so that the sanitizers see every move and destruction.
template <class Hasher>
void check_against_unordered_map(std::uint64_t seed) {
  bbtree::BBHashMap<std::uint64_t, std::string, Hasher, std::equal_to<>, bbtree::Global, 1> map;
  std::unordered_map<std::uint64_t, std::string> model;
  bbtree_test::Rng rng(seed);
  for (int step = 0; step < 60000; ++step) {
    const std::uint64_t key = rng.below(4000);
    const std::uint64_t choice = rng.below(100);
    if (choice < 45) {
      const auto value = std::to_string(step);
      auto inserted = map.insert(key, value);
      CHECK(inserted.ok());
      auto at = model.find(key);
      CHECK_EQ(inserted.value().has_value(), at != model.end());
      if (at != model.end()) {
        CHECK(*inserted.value() == at->second);
        at->second = value;
      } else {
        model.emplace(key, value);
      }
    } else if (choice < 80) {
      const auto removed = map.remove(key);
      auto at = model.find(key);
      CHECK_EQ(removed.has_value(), at != model.end());
      if (at != model.end()) {
        CHECK(*removed == at->second);
        model.erase(at);
      }
    } else if (choice < 85) {
      const auto removed = map.remove_if(key, [](const std::uint64_t &, const std::string &value) {
        return value.size() % 2 == 0;
      });
      auto at = model.find(key);
      const bool expected = at != model.end() && at->second.size() % 2 == 0;
      CHECK_EQ(removed.has_value(), expected);
      if (expected) model.erase(at);
    } else {
      const auto copied = map.get_copied(key);
      auto at = model.find(key);
      CHECK_EQ(copied.has_value(), at != model.end());
      if (copied) CHECK(*copied == at->second);
    }
    if (step % 5000 == 0) {
      map.check_invariants();
      CHECK_EQ(map.size(), model.size());
    }
  }
  map.check_invariants();
  std::size_t visited = 0;
  map.for_each([&](const std::uint64_t &key, const std::string &value) {
    CHECK(model.at(key) == value);
    ++visited;
  });
  CHECK_EQ(visited, model.size());
  map.clear();
  CHECK(map.empty());
  map.check_invariants();
}

struct EighthsCollide {
  std::uint64_t operator()(std::uint64_t key) const noexcept { return key % 8; }
};

TEST(shared_hash_map_agrees_with_unordered_map_on_colliding_keys) {
  check_against_unordered_map<EighthsCollide>(1);
}

TEST(shared_hash_map_agrees_with_unordered_map) {
  check_against_unordered_map<bbtree::Hash<std::uint64_t>>(2);
}

// Keys and values that copy as bytes are read without a latch. Readers race
// writers that grow the index and the page directory, erase (tombstones,
// then rebuilds in place) and clear, on two shards to make them meet often.
// A value is two words derived from its key: a torn read would split them.
TEST(shared_hash_map_optimistic_readers_see_whole_entries_while_writers_change_the_table) {
  using Value = std::array<std::uint64_t, 2>;
  using Map = bbtree::BBHashMap<std::uint64_t, Value, bbtree::Hash<std::uint64_t>, std::equal_to<>,
                                bbtree::Global, 2>;
  const auto value_of = [](std::uint64_t key) { return Value{key * 3 + 1, ~key}; };
  constexpr std::uint64_t key_space = 1 << 15;
  Map map;
  std::atomic<int> writers_left{2};
  std::vector<std::thread> threads;
  for (std::uint64_t writer = 0; writer < 2; ++writer) {
    threads.emplace_back([&, writer] {
      // Another writer's clear can take any key away: only readers check.
      for (int round = 0; round < 40; ++round) {
        for (std::uint64_t key = writer; key < key_space; key += 2) CHECK(map.insert(key, value_of(key)).ok());
        for (std::uint64_t key = writer; key < key_space; key += 4) map.remove(key);
        for (std::uint64_t key = writer + 2; key < key_space; key += 8)
          map.update(key, [&](const std::uint64_t &, Value &value) { value = value_of(key); });
        if (round % 3 == 2) map.clear();
      }
      writers_left.fetch_sub(1);
    });
  }
  for (std::uint64_t reader = 0; reader < 3; ++reader) {
    threads.emplace_back([&, reader] {
      std::uint64_t state = reader + 1;
      std::array<std::uint64_t, 64> keys;
      std::array<std::optional<Value>, 64> values;
      while (writers_left.load() != 0) {
        for (auto &key : keys) {
          state = state * 6364136223846793005ull + 1442695040888963407ull;
          key = (state >> 33) % key_space;
        }
        map.get_many_copied(keys.data(), keys.size(), values.data());
        for (std::size_t lane = 0; lane < keys.size(); ++lane) {
          if (!values[lane]) continue;
          CHECK(*values[lane] == value_of(keys[lane]));
        }
        const auto single = map.read(keys[0], [](const std::uint64_t &key, const Value &value) {
          return value == Value{key * 3 + 1, ~key};
        });
        CHECK(!single || *single);
      }
    });
  }
  for (auto &thread : threads) thread.join();
  map.check_invariants();
  map.for_each([&](const std::uint64_t &key, const Value &value) { CHECK(value == value_of(key)); });
}

// A reader that took a copy made while a writer rewrote the entry would see
// words from two writes: the version check must turn such a copy away.
TEST(shared_hash_map_optimistic_readers_never_see_a_value_half_written) {
  using Value = std::array<std::uint64_t, 8>;
  using Map = bbtree::BBHashMap<std::uint64_t, Value, bbtree::Hash<std::uint64_t>, std::equal_to<>,
                                bbtree::Global, 4>;
  constexpr std::uint64_t hot_keys = 16;
  Map map;
  for (std::uint64_t key = 0; key < hot_keys; ++key) CHECK(map.insert(key, Value{}).ok());
  std::atomic<bool> stop{false};
  std::atomic<std::uint64_t> torn{0};
  std::vector<std::thread> threads;
  for (std::uint64_t writer = 0; writer < 2; ++writer) {
    threads.emplace_back([&, writer] {
      for (std::uint64_t round = 1; round <= 200000; ++round) {
        const std::uint64_t key = (round * 2 + writer) % hot_keys;
        map.update(key, [&](const std::uint64_t &, Value &value) { value.fill(round); });
      }
      stop.store(true);
    });
  }
  for (int reader = 0; reader < 3; ++reader) {
    threads.emplace_back([&] {
      std::array<std::uint64_t, 64> keys;
      for (std::size_t lane = 0; lane < keys.size(); ++lane) keys[lane] = lane % hot_keys;
      std::array<std::optional<Value>, 64> values;
      while (!stop.load()) {
        map.get_many_copied(keys.data(), keys.size(), values.data());
        for (const auto &value : values) {
          CHECK(value.has_value());
          for (const auto word : *value) torn.fetch_add(word != (*value)[0], std::memory_order_relaxed);
        }
        const auto whole = map.read(keys[3], [](const std::uint64_t &, const Value &value) {
          for (const auto word : value) {
            if (word != value[0]) return false;
          }
          return true;
        });
        CHECK(whole && *whole);
      }
    });
  }
  for (auto &thread : threads) thread.join();
  CHECK_EQ(torn.load(), 0u);
}

// What a shard keeps for its optimistic readers goes back to the allocator
// when the map is destroyed.
TEST(shared_hash_map_returns_retained_memory_on_destruction) {
  std::size_t budget = 1 << 20, bytes = 0;
  {
    bbtree::BBHashMap<int, int, bbtree::Hash<int>, std::equal_to<>, BudgetAllocator, 4> map(
        bbtree::Hash<int>{}, std::equal_to<>{}, BudgetAllocator(&budget, &bytes));
    for (int round = 0; round < 2; ++round) {
      for (int key = 0; key < 50000; ++key) CHECK(map.insert(key, key + 1).ok());
      for (int key = 0; key < 50000; key += 2) CHECK(map.remove(key));
      CHECK(map.get_copied(1) == std::optional<int>(2));
      map.clear();
    }
    CHECK(bytes != 0);
  }
  CHECK_EQ(bytes, 0u);
}

TEST_MAIN()
