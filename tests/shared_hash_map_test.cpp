#include <array>
#include <atomic>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
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

TEST_MAIN()
