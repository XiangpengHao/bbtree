#include <array>
#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>

#include "bbtree/unsync/hash_map.hpp"
#include "test_harness.hpp"

namespace {

struct ConstantHash {
  std::uint64_t operator()(int) const noexcept { return ~std::uint64_t{0}; }
};

struct CountingConstantHash {
  std::size_t *calls;
  std::uint64_t operator()(int) const noexcept {
    ++*calls;
    return 0;
  }
};

struct AllocationState {
  std::size_t attempts = 0;
  std::size_t fail_at = ~std::size_t{0};
  std::size_t bytes = 0;
};

class CountingAllocator {
 public:
  explicit CountingAllocator(AllocationState *state = nullptr) : state_(state) {}
  void *allocate_zeroed(bbtree::Layout layout) noexcept {
    if (state_ && state_->attempts++ == state_->fail_at) return nullptr;
    void *memory = bbtree::Global{}.allocate_zeroed(layout);
    if (memory && state_) state_->bytes += layout.size;
    return memory;
  }
  void deallocate(void *memory, bbtree::Layout layout) noexcept {
    if (state_) state_->bytes -= layout.size;
    bbtree::Global{}.deallocate(memory, layout);
  }

 private:
  AllocationState *state_;
};

struct CountingHash {
  std::size_t *calls;
  std::uint64_t operator()(int key) const {
    if (!calls) throw std::runtime_error("hash called during reserve");
    ++*calls;
    return static_cast<std::uint64_t>(key);
  }
};

class LifetimeValue {
 public:
  explicit LifetimeValue(std::shared_ptr<int> live) noexcept : live_(std::move(live)) { ++*live_; }
  LifetimeValue(LifetimeValue &&other) noexcept : live_(std::move(other.live_)) {}
  LifetimeValue &operator=(LifetimeValue &&) = delete;
  ~LifetimeValue() {
    if (live_) --*live_;
  }

 private:
  std::shared_ptr<int> live_;
};

struct ThrowingEqual {
  bool *fail;
  bool operator()(int left, int right) const {
    if (*fail) throw std::runtime_error("equality refused");
    return left == right;
  }
};

class CopyValue {
 public:
  CopyValue(int *live, int *copies_left) noexcept : live_(live), copies_left_(copies_left) {
    ++*live_;
  }
  CopyValue(const CopyValue &other) : live_(other.live_), copies_left_(other.copies_left_) {
    if ((*copies_left_)-- == 0) throw std::runtime_error("copy refused");
    ++*live_;
  }
  CopyValue(CopyValue &&other) noexcept
      : live_(std::exchange(other.live_, nullptr)), copies_left_(other.copies_left_) {}
  ~CopyValue() {
    if (live_) --*live_;
  }

 private:
  int *live_;
  int *copies_left_;
};

struct alignas(128) AlignedValue {
  std::array<unsigned char, 5000> bytes{};
};

template <class Hasher>
void differential(Hasher hash, std::size_t operations, std::uint64_t key_space) {
  bbtree::unsync::BBHashMap<int, std::uint64_t, Hasher> map(hash);
  std::unordered_map<int, std::uint64_t> model;
  bbtree_test::Rng rng(71);
  for (std::size_t step = 0; step < operations; ++step) {
    const auto key = static_cast<int>(rng.below(key_space));
    auto expected = model.find(key);
    switch (rng.below(4)) {
      case 0:
      case 1: {
        auto previous = map.insert(key, step);
        CHECK_EQ(previous.has_value(), expected != model.end());
        if (previous) CHECK_EQ(*previous, expected->second);
        model[key] = step;
        break;
      }
      case 2: {
        auto removed = map.remove(key);
        CHECK_EQ(removed.has_value(), expected != model.end());
        if (removed) {
          CHECK_EQ(*removed, expected->second);
          model.erase(expected);
        }
        break;
      }
      default: {
        const auto value = map.get(key);
        CHECK_EQ(value != nullptr, expected != model.end());
        if (value) CHECK_EQ(*value, expected->second);
      }
    }
    CHECK_EQ(map.size(), model.size());
    if (step % 251 == 0) map.check_invariants();
  }
  map.check_invariants();
  std::unordered_set<int> seen;
  for (auto [key, value] : map) {
    CHECK(seen.insert(key).second);
    CHECK_EQ(value, model.at(key));
    ++value;
  }
  CHECK_EQ(seen.size(), model.size());
  const auto &constant = map;
  for (auto [key, value] : constant) CHECK_EQ(value, model.at(key) + 1);
}

}  // namespace

TEST(hash_map_matches_unordered_map_through_growth_and_churn) {
  differential(std::hash<int>{}, 100000, 10000);
}

TEST(hash_map_handles_collisions_tombstones_and_duplicate_updates) {
  differential(ConstantHash{}, 15000, 200);
  bbtree::unsync::BBHashMap<int, int, ConstantHash> map;
  for (int key = 0; key < 100; ++key) map.insert(key, key);
  for (int key = 0; key < 99; ++key) CHECK(map.remove(key));
  const auto previous = map.insert(99, 1000);
  CHECK_EQ(*previous, 99);
  CHECK_EQ(map.size(), 1u);
  CHECK_EQ(*map.get(99), 1000);
  for (int key = 0; key < 99; ++key) map.insert(key, -key);
  map.check_invariants();
}

TEST(hash_map_owns_strings_and_accepts_borrowed_queries) {
  bbtree::unsync::BBHashMap<std::string, std::string> map;
  map.insert("alpha", "first");
  map.insert("beta", "second");
  CHECK_EQ(*map.get(std::string_view("alpha")), "first");
  CHECK_EQ(*map.get("beta"), "second");
  *map.get_mut("alpha") = "updated";
  CHECK_EQ(map.get_key_value("alpha")->first, "alpha");
  const auto removed = map.remove("alpha");
  CHECK_EQ(*removed, "updated");
  CHECK(!map.contains_key("alpha"));
}

TEST(hash_map_growth_preserves_references_and_uses_cached_hashes) {
  std::size_t hash_calls = 0;
  bbtree::unsync::BBHashMap<int, std::string, CountingHash> map(CountingHash{&hash_calls});
  map.insert(7, "seven");
  const auto *stored = map.get(7);
  const auto before = hash_calls;
  map.reserve(20000);
  CHECK_EQ(hash_calls, before);
  for (int key = 8; key < 5000; ++key) map.insert(key, "other");
  CHECK(map.get(7) == stored);
  CHECK_EQ(*stored, "seven");
  map.check_invariants();
}

TEST(hash_map_hashed_walk_visits_dense_sparse_and_empty_pages_without_hashing) {
  std::size_t hash_calls = 0;
  bbtree::unsync::BBHashMap<int, int, CountingHash> map(CountingHash{&hash_calls});
  std::unordered_map<int, int> expected;
  for (int key = 0; key < 500; ++key) {
    map.insert(key, key + 1);
    expected[key] = key + 1;
  }
  for (int key = 0; key < 64; ++key) {
    map.remove(key);
    expected.erase(key);
  }
  for (int key = 64; key < 128; key += 3) {
    map.remove(key);
    expected.erase(key);
  }
  const auto before = hash_calls;
  std::unordered_map<int, std::uint64_t> hashes;
  map.for_each_hashed([&](const int &key, const int &value, std::uint64_t hash) {
    CHECK_EQ(value, expected.at(key));
    CHECK(hashes.emplace(key, hash).second);
  });
  CHECK_EQ(hash_calls, before);
  CHECK_EQ(hashes.size(), expected.size());
  map.reserve(10000);
  map.for_each_hashed([&](const int &key, const int &value, std::uint64_t hash) {
    CHECK_EQ(value, expected.at(key));
    CHECK_EQ(hash, hashes.at(key));
  });
  CHECK_EQ(hash_calls, before);
  map.clear_retain_capacity();
  map.for_each_hashed([](const int &, const int &, std::uint64_t) { CHECK(false); });
}

TEST(hash_map_moves_copies_and_clears_owned_entries) {
  bbtree::unsync::BBHashMap<int, std::string> source;
  for (int key = 0; key < 300; ++key) source.insert(key, std::to_string(key));
  auto copied = source;
  copied.insert(2, "changed");
  CHECK_EQ(*source.get(2), "2");
  auto moved = std::move(source);
  CHECK(source.empty());
  source.insert(4, "reused");
  CHECK_EQ(moved.size(), 300u);
  source = moved;
  CHECK_EQ(source.size(), 300u);
  source = std::move(copied);
  CHECK(copied.empty());
  CHECK_EQ(*source.get(2), "changed");
  source.clear();
  CHECK(source.empty());
  CHECK(source.begin() == source.end());
  source.insert(1, "again");
  source.check_invariants();
}

TEST(hash_map_destroys_each_owned_value_once) {
  auto live = std::make_shared<int>(0);
  {
    bbtree::unsync::BBHashMap<int, LifetimeValue> map;
    for (int key = 0; key < 300; ++key) map.insert(key, LifetimeValue(live));
    CHECK_EQ(*live, 300);
    map.insert(4, LifetimeValue(live));
    CHECK_EQ(*live, 300);
    map.remove(5);
    CHECK_EQ(*live, 299);
    auto moved = std::move(map);
    moved.clear();
    CHECK_EQ(*live, 0);
    moved.insert(1, LifetimeValue(live));
  }
  CHECK_EQ(*live, 0);
}

TEST(hash_map_clear_retain_capacity_destroys_values_and_reuses_allocations) {
  AllocationState state;
  auto live = std::make_shared<int>(0);
  using Map = bbtree::unsync::BBHashMap<int, LifetimeValue, std::hash<int>, std::equal_to<>,
                                        CountingAllocator>;
  {
    Map map(std::hash<int>{}, std::equal_to<>{}, CountingAllocator(&state));
    for (int key = 0; key < 300; ++key) map.insert(key, LifetimeValue(live));
    map.remove(5);
    map.clear_retain_capacity();
    CHECK(map.empty());
    CHECK_EQ(*live, 0);
    const auto allocations = state.attempts;
    state.fail_at = allocations;
    for (int key = 0; key < 300; ++key) map.insert(key + 1000, LifetimeValue(live));
    CHECK_EQ(state.attempts, allocations);
    CHECK_EQ(*live, 300);
    CHECK(!map.get(5));
    map.check_invariants();
    map.clear_retain_capacity();
    map.clear_retain_capacity();
    CHECK_EQ(*live, 0);
  }
  CHECK_EQ(state.bytes, 0u);
}

TEST(hash_map_allocation_failure_preserves_entries_and_releases_memory) {
  using Map =
      bbtree::unsync::BBHashMap<int, int, std::hash<int>, std::equal_to<>, CountingAllocator>;
  for (std::size_t fail_at = 0; fail_at < 20; ++fail_at) {
    AllocationState state;
    {
      Map map(std::hash<int>{}, std::equal_to<>{}, CountingAllocator(&state));
      state.fail_at = fail_at;
      int inserted = 0;
      try {
        for (; inserted < 1000; ++inserted) map.insert(inserted, inserted + 1);
      } catch (const std::bad_alloc &) {
      }
      CHECK_EQ(map.size(), static_cast<std::size_t>(inserted));
      for (int key = 0; key < inserted; ++key) CHECK_EQ(*map.get(key), key + 1);
      CHECK(!map.get(inserted));
      map.check_invariants();
      state.fail_at = ~std::size_t{0};
      map.insert(inserted, 42);
    }
    CHECK_EQ(state.bytes, 0u);
  }
}

TEST(hash_map_batches_preserve_input_order_and_duplicate_insert_semantics) {
  bbtree::unsync::BBHashMap<int, int, ConstantHash> map;
  std::array<int, 131> keys{}, values{};
  std::array<std::optional<int>, 131> previous{};
  for (std::size_t index = 0; index < keys.size(); ++index) {
    keys[index] = static_cast<int>(index % 17);
    values[index] = static_cast<int>(index);
  }
  map.insert_many(keys.data(), values.data(), keys.size(), previous.data());
  for (std::size_t index = 0; index < keys.size(); ++index) {
    CHECK_EQ(previous[index].has_value(), index >= 17);
    if (previous[index]) CHECK_EQ(*previous[index], static_cast<int>(index - 17));
  }
  for (std::size_t index = 0; index < keys.size(); ++index)
    keys[index] = static_cast<int>(index % 23);
  std::array<const int *, 131> found{};
  map.get_many(keys.data(), keys.size(), found.data());
  for (std::size_t index = 0; index < keys.size(); ++index)
    CHECK(found[index] == map.get(keys[index]));
  map.get_many(static_cast<const int *>(nullptr), 0, static_cast<const int **>(nullptr));
  map.insert_many(nullptr, nullptr, 0, nullptr);
  map.check_invariants();
}

TEST(hash_map_string_batches_accept_borrowed_queries) {
  bbtree::unsync::BBHashMap<std::string, int> map;
  map.insert("one", 1);
  map.insert("two", 2);
  const std::string_view keys[] = {"one", "absent", "two", "one"};
  const int *found[4]{};
  map.get_many(keys, 4, found);
  CHECK_EQ(*found[0], 1);
  CHECK(!found[1]);
  CHECK_EQ(*found[2], 2);
  CHECK(found[0] == found[3]);
}

TEST(hash_map_batched_find_or_create_initializes_once_and_aliases_duplicate_keys) {
  std::size_t hash_calls = 0;
  bbtree::unsync::BBHashMap<int, int, CountingConstantHash> map(CountingConstantHash{&hash_calls});
  map.reserve(1000);  // Keep cached vacancies valid while colliding rows occupy them.
  map.insert(7, 10);
  std::array<int, 263> keys{};
  std::array<int *, 263> values{};
  std::size_t created = 0;
  for (std::size_t row = 0; row < keys.size(); ++row) keys[row] = static_cast<int>(row % 19);
  const auto before = hash_calls;
  map.get_or_insert_many(keys.data(), keys.size(), values.data(), [&](const int &) {
    ++created;
    return 0;
  });
  CHECK_EQ(hash_calls, before + keys.size());
  CHECK_EQ(created, 18u);
  CHECK_EQ(map.size(), 19u);
  for (std::size_t row = 0; row < keys.size(); ++row) {
    CHECK(values[row] == map.get_mut(keys[row]));
    ++*values[row];
  }
  for (int key = 0; key < 19; ++key) {
    const int count = static_cast<int>((keys.size() + 18 - key) / 19);
    CHECK_EQ(*map.get(key), count + (key == 7 ? 10 : 0));
  }
  map.reserve(10000);
  CHECK(values[0] == map.get_mut(keys[0]));
  map.check_invariants();
}

TEST(hash_map_batches_handle_tombstones_and_missing_duplicate_keys) {
  bbtree::unsync::BBHashMap<int, int, ConstantHash> map;
  for (int key = 0; key < 100; ++key) map.insert(key, key);
  for (int key = 0; key < 100; key += 2) map.remove(key);
  std::array<int, 259> keys{};
  std::array<const int *, 259> found{};
  std::array<int *, 259> created{};
  for (std::size_t row = 0; row < keys.size(); ++row) keys[row] = static_cast<int>(row % 117);
  map.get_many(keys.data(), keys.size(), found.data());
  for (std::size_t row = 0; row < keys.size(); ++row) CHECK(found[row] == map.get(keys[row]));
  map.get_or_insert_many(keys.data(), keys.size(), created.data(),
                         [](const int &key) { return key; });
  CHECK_EQ(map.size(), 117u);
  for (std::size_t row = 0; row < keys.size(); ++row) CHECK_EQ(*created[row], keys[row]);
  map.check_invariants();
}

TEST(hash_map_batched_creation_allocation_failures_release_unpublished_values) {
  using Map = bbtree::unsync::BBHashMap<int, LifetimeValue, std::hash<int>, std::equal_to<>,
                                        CountingAllocator>;
  for (std::size_t failure = 0; failure < 15; ++failure) {
    AllocationState state;
    auto live = std::make_shared<int>(0);
    {
      Map map(std::hash<int>{}, std::equal_to<>{}, CountingAllocator(&state));
      std::array<int, 263> keys{};
      std::array<LifetimeValue *, 263> values{};
      for (std::size_t row = 0; row < keys.size(); ++row) keys[row] = static_cast<int>(row);
      state.fail_at = failure;
      try {
        map.get_or_insert_many(keys.data(), keys.size(), values.data(),
                               [&](const int &) { return LifetimeValue(live); });
      } catch (const std::bad_alloc &) {
      }
      CHECK_EQ(static_cast<std::size_t>(*live), map.size());
      map.check_invariants();
      state.fail_at = ~std::size_t{0};
      map.get_or_insert_many(keys.data(), keys.size(), values.data(),
                             [&](const int &) { return LifetimeValue(live); });
      CHECK_EQ(map.size(), keys.size());
      for (std::size_t row = 0; row < keys.size(); ++row)
        CHECK(values[row] == map.get_mut(keys[row]));
      map.clear_retain_capacity();
      CHECK_EQ(*live, 0);
    }
    CHECK_EQ(*live, 0);
    CHECK_EQ(state.bytes, 0u);
  }
}

TEST(hash_map_batched_find_or_create_accepts_borrowed_keys_and_unwinds_factory_failure) {
  bbtree::unsync::BBHashMap<std::string, std::unique_ptr<int>> map;
  const std::string_view keys[] = {"one", "two", "one", "stop"};
  std::unique_ptr<int> *values[4]{};
  bool threw = false;
  try {
    map.get_or_insert_many(keys, 4, values, [](std::string_view key) {
      if (key == "stop") throw std::runtime_error("factory refused");
      return std::make_unique<int>(static_cast<int>(key.size()));
    });
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  CHECK_EQ(map.size(), 2u);
  CHECK(values[0] == values[2]);
  CHECK_EQ(**map.get("two"), 3);
  CHECK(!map.get("stop"));
  map.check_invariants();
}

TEST(hash_map_batched_upsert_orders_callbacks_and_initializes_only_new_keys) {
  std::size_t calls = 0;
  bbtree::unsync::BBHashMap<int, std::uint64_t, CountingConstantHash> map(
      CountingConstantHash{&calls});
  map.reserve(1000);
  map.insert(5, 1000);
  std::unordered_map<int, std::uint64_t> expected{{5, 1000}};
  std::array<int, 263> keys{};
  std::vector<std::size_t> order;
  for (std::size_t row = 0; row < keys.size(); ++row) {
    const int key = static_cast<int>(row % 23);
    keys[row] = key;
    auto found = expected.find(key);
    if (found == expected.end())
      expected.emplace(key, row + 1);
    else
      found->second = found->second * 3 + row;
  }
  const auto before = calls;
  map.update_or_insert_many(
      keys.data(), keys.size(),
      [&](std::size_t row, const int &) {
        order.push_back(row);
        return std::uint64_t(row + 1);
      },
      [&](std::size_t row, const int &, std::uint64_t &value) {
        order.push_back(row);
        value = value * 3 + row;
      });
  CHECK_EQ(calls, before + keys.size());
  CHECK_EQ(order.size(), keys.size());
  for (std::size_t row = 0; row < order.size(); ++row) CHECK_EQ(order[row], row);
  for (auto [key, value] : expected) CHECK_EQ(*map.get(key), value);
  map.check_invariants();
}

TEST(hash_map_batched_upsert_preserves_completed_work_on_callback_exception) {
  bbtree::unsync::BBHashMap<int, std::unique_ptr<int>> map;
  const int keys[] = {0, 1, 0, 2};
  bool threw = false;
  try {
    map.update_or_insert_many(
        keys, 4,
        [](std::size_t row, const int &) {
          return std::make_unique<int>(static_cast<int>(row + 10));
        },
        [](std::size_t, const int &, std::unique_ptr<int> &value) {
          ++*value;
          throw std::runtime_error("update refused");
        });
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  CHECK_EQ(map.size(), 2u);
  CHECK_EQ(**map.get(0), 11);
  CHECK_EQ(**map.get(1), 11);
  CHECK(!map.get(2));
  map.check_invariants();
}

TEST(hash_map_batch_replacement_only_requires_move_construction) {
  auto live = std::make_shared<int>(0);
  bbtree::unsync::BBHashMap<int, LifetimeValue> map;
  int keys[] = {1, 1};
  LifetimeValue values[] = {LifetimeValue(live), LifetimeValue(live)};
  std::optional<LifetimeValue> previous[2];
  map.insert_many(keys, values, 2, previous);
  CHECK(!previous[0]);
  CHECK(previous[1]);
  CHECK_EQ(*live, 2);
  previous[1].reset();
  map.clear();
  CHECK_EQ(*live, 0);
}

TEST(hash_map_policy_exceptions_leave_entries_unchanged) {
  bool fail = false;
  bbtree::unsync::BBHashMap<int, int, ConstantHash, ThrowingEqual> map(ConstantHash{},
                                                                       ThrowingEqual{&fail});
  map.insert(1, 10);
  fail = true;
  bool threw = false;
  try {
    map.insert(1, 20);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  map.reserve(100);
  map.check_invariants();
  fail = false;
  CHECK_EQ(*map.get(1), 10);
  CHECK_EQ(map.size(), 1u);
}

TEST(hash_map_copy_failure_destroys_partial_copy_and_preserves_assignment_target) {
  int live = 0, copies_left = 100;
  {
    bbtree::unsync::BBHashMap<int, CopyValue> source, target;
    for (int key = 0; key < 20; ++key) source.insert(key, CopyValue(&live, &copies_left));
    target.insert(100, CopyValue(&live, &copies_left));
    copies_left = 3;
    bool threw = false;
    try {
      target = source;
    } catch (const std::runtime_error &) {
      threw = true;
    }
    CHECK(threw);
    CHECK_EQ(live, 21);
    CHECK_EQ(target.size(), 1u);
    CHECK(target.get(100));
    source.check_invariants();
  }
  CHECK_EQ(live, 0);
}

TEST(hash_map_supports_oversized_overaligned_entries_and_empty_maps_allocate_nothing) {
  AllocationState state;
  using Map = bbtree::unsync::BBHashMap<int, AlignedValue, std::hash<int>, std::equal_to<>,
                                        CountingAllocator>;
  {
    Map map(std::hash<int>{}, std::equal_to<>{}, CountingAllocator(&state));
    CHECK_EQ(state.attempts, 0u);
    CHECK(!map.get(0));
    map.reserve(0);
    CHECK_EQ(state.attempts, 0u);
    for (int key = 0; key < 12; ++key) {
      AlignedValue value;
      value.bytes.back() = static_cast<unsigned char>(key);
      map.insert(key, std::move(value));
    }
    for (int key = 0; key < 12; ++key) {
      const auto *value = map.get(key);
      CHECK_EQ(reinterpret_cast<std::uintptr_t>(value) % alignof(AlignedValue), 0u);
      CHECK_EQ(value->bytes.back(), static_cast<unsigned char>(key));
    }
    map.check_invariants();
  }
  CHECK_EQ(state.bytes, 0u);
}

TEST_MAIN()
