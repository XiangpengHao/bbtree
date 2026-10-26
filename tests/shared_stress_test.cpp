// Stress and differential tests for the shared ordered map (`TreeMap`):
// single-threaded checks against `std::map`, then threads. Batches run on
// small trees too (`SHOAL_TEST_BATCHES`), so the lane path is actually
// taken. Operations are `noexcept` (a throwing key order is outside the
// contract, D9); there is no exception-recovery path to exercise.
#define SHOAL_TEST_BATCHES 1

#include <algorithm>
#include <array>
#include <atomic>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <map>
#include <mutex>
#include <new>
#include <optional>
#include <string>
#include <string_view>
#include <thread>
#include <type_traits>
#include <unordered_set>
#include <utility>
#include <vector>

#include "shoal/inline_key.hpp"
#include "shoal/map.hpp"
#include "test_harness.hpp"

using shoal::Bound;
using shoal::InlineKey;
using shoal::InlineKeyBase;
using shoal::InlineWidth;
using shoal::InlineWord;
using shoal::InsertOutcome;
using shoal::Layout;
using shoal::LeafOutcome;
using shoal::TreeMap;

namespace {

constexpr std::size_t k_threads = 4;

template <class K>
bool same_key(const K& a, const K& b) {
  return !(a < b) && !(b < a);
}

template <class K, class V, class Config, class A, class Annex>
void check_in_order(const TreeMap<K, V, Config, A, Annex>& map, const std::map<K, V>& model) {
  auto at = model.begin();
  std::size_t visited = 0;
  const bool stopped =
      map.scan(Bound<K>::unbounded(), Bound<K>::unbounded(), [&](const K& key, const V& value) {
        CHECK(at != model.end());
        CHECK(same_key(key, at->first));
        CHECK(value == at->second);
        ++at;
        ++visited;
        return true;
      });
  CHECK(!stopped);
  CHECK_EQ(visited, model.size());
  CHECK_EQ(map.empty(), model.empty());
}

/// `lower` / `upper` null means unbounded. Keys must outlive the scans.
template <class K, class V, class Config, class A, class Annex>
void expect_range(const TreeMap<K, V, Config, A, Annex>& map, const std::map<K, V>& model, const K* lower,
                  bool lower_in, const K* upper, bool upper_in) {
  std::vector<std::pair<K, V>> expected;
  for (const auto& entry : model) {
    if (lower != nullptr) {
      if (entry.first < *lower) continue;
      if (same_key(entry.first, *lower) && !lower_in) continue;
    }
    if (upper != nullptr) {
      if (*upper < entry.first) break;
      if (same_key(entry.first, *upper) && !upper_in) break;
    }
    expected.emplace_back(entry.first, entry.second);
  }
  auto lower_bound = [&] {
    if (lower == nullptr) return Bound<K>::unbounded();
    return lower_in ? Bound<K>::included(*lower) : Bound<K>::excluded(*lower);
  };
  auto upper_bound = [&] {
    if (upper == nullptr) return Bound<K>::unbounded();
    return upper_in ? Bound<K>::included(*upper) : Bound<K>::excluded(*upper);
  };
  std::vector<std::pair<K, V>> forward;
  CHECK(!map.scan(lower_bound(), upper_bound(), [&](const K& key, const V& value) {
    forward.emplace_back(key, value);
    return true;
  }));
  CHECK_EQ(forward.size(), expected.size());
  for (std::size_t i = 0; i < expected.size(); ++i) {
    CHECK(same_key(forward[i].first, expected[i].first));
    CHECK(forward[i].second == expected[i].second);
  }
  std::vector<std::pair<K, V>> backward;
  CHECK(!map.scan_rev(lower_bound(), upper_bound(), [&](const K& key, const V& value) {
    backward.emplace_back(key, value);
    return true;
  }));
  CHECK_EQ(backward.size(), expected.size());
  for (std::size_t i = 0; i < expected.size(); ++i) {
    const auto& want = expected[expected.size() - 1 - i];
    CHECK(same_key(backward[i].first, want.first));
    CHECK(backward[i].second == want.second);
  }
}

template <class K, class V, class Config, class A, class Annex>
void sweep_bounds(const TreeMap<K, V, Config, A, Annex>& map, const std::map<K, V>& model) {
  const K* none = nullptr;
  expect_range(map, model, none, false, none, false);
  if (model.empty()) return;
  std::vector<K> keys;
  keys.reserve(model.size());
  for (const auto& entry : model) keys.push_back(entry.first);
  const std::size_t step = keys.size() > 24 ? keys.size() / 24 : 1;
  for (std::size_t i = 0; i < keys.size(); i += step) {
    const K& key = keys[i];
    expect_range(map, model, &key, true, none, false);
    expect_range(map, model, &key, false, none, false);
    expect_range(map, model, none, false, &key, true);
    expect_range(map, model, none, false, &key, false);
    const K& other = keys[(i * 3 + step) % keys.size()];
    expect_range(map, model, &key, true, &other, true);
    expect_range(map, model, &key, false, &other, false);
    expect_range(map, model, &key, true, &key, true);
    expect_range(map, model, &key, false, &key, false);
  }
  // Stop early: the first three entries, and the return says so when the
  // map holds more than that.
  if (model.size() > 3) {
    std::size_t seen = 0;
    const bool stopped =
        map.scan(Bound<K>::unbounded(), Bound<K>::unbounded(), [&](const K&, const V&) { return ++seen < 3; });
    CHECK(stopped);
    CHECK_EQ(seen, 3u);
  }
}

template <class K>
void apply_insert(std::map<K, std::uint64_t>& model, const K& key, std::uint64_t value, const std::optional<std::uint64_t>& previous) {
  auto at = model.find(key);
  CHECK_EQ(previous.has_value(), at != model.end());
  if (at != model.end()) {
    CHECK_EQ(*previous, at->second);
    at->second = value;
  } else {
    model.emplace(key, value);
  }
}

template <class K, class MakeKey>
void differential(std::uint64_t seed, std::size_t operations, std::uint64_t key_space, MakeKey make_key) {
  shoal_test::Rng rng(seed);
  TreeMap<K, std::uint64_t> map;
  std::map<K, std::uint64_t> model;
  CHECK(map.empty());
  map.check_invariants(0);
  for (std::size_t step = 0; step < operations; ++step) {
    const K key = make_key(rng.below(key_space));
    const std::uint64_t choice = rng.below(100);
    if (choice < 28) {
      auto inserted = map.insert(key, step);
      CHECK(inserted.ok());
      apply_insert(model, key, step, inserted.value());
    } else if (choice < 40) {
      const auto removed = map.remove(key);
      auto at = model.find(key);
      CHECK_EQ(removed.has_value(), at != model.end());
      if (at != model.end()) {
        CHECK_EQ(*removed, at->second);
        model.erase(at);
      }
    } else if (choice < 50) {
      const auto copied = map.get_copied(key);
      auto at = model.find(key);
      CHECK_EQ(copied.has_value(), at != model.end());
      if (copied) CHECK_EQ(*copied, at->second);
      const auto read = map.read(key, [](const K&, const std::uint64_t& value) { return value; });
      CHECK_EQ(read.has_value(), copied.has_value());
      if (read) CHECK_EQ(*read, copied ? *copied : 0);
      CHECK_EQ(map.contains_key(key), at != model.end());
      const auto cloned = map.get_cloned(key);
      CHECK_EQ(cloned.has_value(), at != model.end());
      if (cloned) CHECK_EQ(*cloned, at->second);
    } else if (choice < 58) {
      const auto updated = map.update(key, [](const K&, std::uint64_t& value) { return ++value; });
      auto at = model.find(key);
      CHECK_EQ(updated.has_value(), at != model.end());
      if (updated) CHECK_EQ(*updated, ++at->second);
    } else if (choice < 66) {
      int calls = 0;
      const auto removed = map.remove_if(key, [&](const K&, const std::uint64_t& value) {
        ++calls;
        return value % 2 == 1;
      });
      auto at = model.find(key);
      if (at == model.end()) {
        CHECK_EQ(calls, 0);
        CHECK(!removed);
      } else {
        CHECK(calls == 1 || calls == 2);
        const bool removes = at->second % 2 == 1;
        CHECK_EQ(removed.has_value(), removes);
        if (removes) {
          CHECK_EQ(removed->second, at->second);
          model.erase(at);
        }
      }
    } else if (choice < 74) {
      const std::uint64_t value = step + 3;
      if (model.count(key) == 0) {
        auto outcome = map.get_or_insert(key, value, [](const K&, const std::uint64_t& stored) { return stored; });
        CHECK(outcome.ok());
        CHECK_EQ(outcome.value(), value);
        model.emplace(key, value);
      } else {
        auto outcome = map.update_or_insert(key, value, [](const K&, std::uint64_t& stored, std::uint64_t&& incoming) {
          stored += incoming;
          return stored;
        });
        CHECK(outcome.ok());
        CHECK(outcome.value().has_value());
        model[key] += value;
        CHECK_EQ(*outcome.value(), model[key]);
      }
    } else if (choice < 86) {
      const std::size_t count = 1 + rng.below(40);
      std::vector<K> keys(count);
      std::vector<std::uint64_t> values(count);
      std::vector<std::optional<std::uint64_t>> previous(count);
      std::vector<char> failed(count, 0);
      for (std::size_t index = 0; index < count; ++index) {
        // Every fifth batch repeats a key, in order, as separate inserts.
        keys[index] = index > 0 && index % 5 == 0 ? keys[index - 1] : make_key(rng.below(key_space));
        values[index] = step + index;
      }
      std::vector<K> copies = keys;
      map.insert_many(keys.data(), values.data(), count, previous.data(), reinterpret_cast<bool*>(failed.data()));
      for (std::size_t index = 0; index < count; ++index) {
        CHECK(!failed[index]);
        apply_insert(model, copies[index], values[index], previous[index]);
      }
      if (rng.below(2) == 0) {
        std::vector<std::optional<std::uint64_t>> copied(count);
        map.get_many_copied(copies.data(), count, copied.data());
        std::vector<int> seen(count, 0);
        map.read_many(copies.data(), count, [&](std::size_t index, const K& stored, const std::uint64_t& value) {
          CHECK(index < count);
          CHECK(same_key(stored, copies[index]));
          seen[index] = 1;
          auto at = model.find(copies[index]);
          CHECK(at != model.end());
          CHECK_EQ(value, at->second);
        });
        for (std::size_t index = 0; index < count; ++index) {
          auto at = model.find(copies[index]);
          CHECK_EQ(copied[index].has_value(), at != model.end());
          if (at != model.end()) {
            CHECK_EQ(*copied[index], at->second);
            CHECK_EQ(seen[index], 1);
          }
        }
      }
    } else if (choice < 92) {
      std::uint64_t* slot = map.get_mut(key);
      auto at = model.find(key);
      CHECK_EQ(slot != nullptr, at != model.end());
      if (slot != nullptr) {
        *slot ^= 0x5a5a;
        at->second ^= 0x5a5a;
        CHECK_EQ(map.get_copied(key).value_or(0), at->second);
      }
    } else {
      K lower = make_key(rng.below(key_space));
      K upper = make_key(rng.below(key_space));
      expect_range(map, model, &lower, rng.below(2) == 0, &upper, rng.below(2) == 0);
    }
    if (step % 800 == 0) map.check_invariants(model.size());
  }
  if (!model.empty()) {
    std::size_t n = 0;
    for (auto entry : map.iter_mut()) {
      auto at = model.find(entry.first);
      CHECK(at != model.end());
      entry.second += 7;
      at->second += 7;
      ++n;
    }
    CHECK_EQ(n, model.size());
  }
  map.check_invariants(model.size());
  check_in_order(map, model);
  sweep_bounds(map, model);
  map.clear();
  model.clear();
  CHECK(map.empty());
  map.check_invariants(0);
  // The map can be filled again after clear.
  const K again = make_key(0);
  auto inserted = map.insert(again, 1);
  CHECK(inserted.ok());
  CHECK(!inserted.value());
  map.check_invariants(1);
}

std::string mixed_string(std::uint64_t n) {
  switch (n % 6) {
    case 0: return {};
    case 1: return std::string("a\0b", 3) + std::to_string(n);
    case 2: return std::string(static_cast<std::size_t>(n % 13), 'p') + "/" + std::to_string(n);
    case 3: return "shared-prefix/" + std::to_string(n % 3) + "/" + std::to_string(n);
    case 4: {
      // Prefixes of one another, so the shared prefix shrinks and grows the fence.
      const std::size_t len = static_cast<std::size_t>(n % 8);
      return std::string("abcabcab", len) + std::to_string(n);
    }
    default: return std::to_string(n);
  }
}

std::array<std::uint8_t, 16> tied_bytes(std::uint64_t n) {
  std::array<std::uint8_t, 16> key{};
  // Width-2 inline keys: a fixed top forces separator ties and the unlatched head compare.
  key[0] = 0xAB;
  key[1] = 0xCD;
  key[2] = static_cast<std::uint8_t>(n % 3);
  for (int byte = 0; byte < 8; ++byte) key[8 + byte] = static_cast<std::uint8_t>(n >> (8 * byte));
  return key;
}

std::int64_t edge_int(std::uint64_t n) {
  switch (n % 7) {
    case 0: return INT64_MIN;
    case 1: return INT64_MAX;
    case 2: return -1;
    case 3: return 0;
    case 4: return INT64_MIN + static_cast<std::int64_t>(n % 1000);
    case 5: return static_cast<std::int64_t>(n % 5000) - 2500;
    default: return INT64_MAX - static_cast<std::int64_t>(n % 1000);
  }
}

struct BudgetState {
  std::mutex mu;
  std::unordered_set<void*> live;
  std::atomic<std::size_t> in_use{0};
  std::atomic<std::size_t> cap{0};
};

struct BudgetAlloc {
  std::shared_ptr<BudgetState> state;

  void* allocate_zeroed(Layout layout) noexcept {
    if (layout.size == 0) return nullptr;
    const std::size_t limit = state->cap.load(std::memory_order_acquire);
    std::size_t cur = state->in_use.load(std::memory_order_relaxed);
    while (cur < limit) {
      if (state->in_use.compare_exchange_weak(cur, cur + 1, std::memory_order_acq_rel)) {
        void* block = shoal::Global{}.allocate_zeroed(layout);
        if (block == nullptr) {
          state->in_use.fetch_sub(1, std::memory_order_relaxed);
          return nullptr;
        }
        std::lock_guard<std::mutex> lock(state->mu);
        CHECK(state->live.insert(block).second);
        return block;
      }
    }
    return nullptr;
  }

  void deallocate(void* block, Layout layout) noexcept {
    if (block == nullptr) return;
    {
      std::lock_guard<std::mutex> lock(state->mu);
      CHECK_EQ(state->live.erase(block), 1u);
    }
    state->in_use.fetch_sub(1, std::memory_order_relaxed);
    shoal::Global{}.deallocate(block, layout);
  }
};

using BudgetMap = TreeMap<std::uint64_t, std::uint64_t, shoal::DefaultNodeConfig, BudgetAlloc>;

void expect_budget_quiescent(BudgetState& state) {
  CHECK_EQ(state.in_use.load(), 0u);
  std::lock_guard<std::mutex> lock(state.mu);
  CHECK(state.live.empty());
}

struct Big {
  std::uint64_t words[32];

  static Big of(std::uint64_t n) {
    Big value{};
    for (std::uint64_t& word : value.words) word = n;
    return value;
  }
  bool intact() const {
    for (std::uint64_t word : words)
      if (word != words[0]) return false;
    return true;
  }
};

struct HeapValue {
  static std::atomic<int> live;
  int* pointer;

  HeapValue() noexcept : pointer(nullptr) {}
  explicit HeapValue(int n) noexcept : pointer(new (std::nothrow) int(n)) {
    CHECK(pointer != nullptr);
    live.fetch_add(1, std::memory_order_relaxed);
  }
  HeapValue(HeapValue&& other) noexcept : pointer(other.pointer) { other.pointer = nullptr; }
  HeapValue& operator=(HeapValue&& other) noexcept {
    if (this != &other) {
      reset();
      pointer = other.pointer;
      other.pointer = nullptr;
    }
    return *this;
  }
  HeapValue(const HeapValue&) = delete;
  HeapValue& operator=(const HeapValue&) = delete;
  ~HeapValue() { reset(); }

  int get() const {
    CHECK(pointer != nullptr);
    return *pointer;
  }

  void reset() noexcept {
    if (pointer != nullptr) {
      live.fetch_sub(1, std::memory_order_relaxed);
      delete pointer;
      pointer = nullptr;
    }
  }
};

}  // namespace

template <>
struct shoal::is_trivially_relocatable<HeapValue> : std::true_type {};

struct OrderedId {
  std::uint64_t id;
  friend bool operator<(OrderedId a, OrderedId b) noexcept { return a.id < b.id; }
  friend bool operator==(OrderedId a, OrderedId b) noexcept { return a.id == b.id; }
};

template <>
struct shoal::InlineKey<OrderedId> : shoal::InlineKeyBase<OrderedId, shoal::InlineKey<OrderedId>> {
  static constexpr shoal::InlineWidth width = shoal::InlineWidth::Two;
  static shoal::InlineWord inline_key_at(OrderedId key, std::size_t offset) noexcept {
    return offset < 8 ? key.id << (8 * offset) : 0;
  }
};

namespace {

std::atomic<int> HeapValue::live{0};

}  // namespace

TEST(shared_stress_api_surface) {
  using Map = TreeMap<std::uint64_t, std::uint64_t>;
  static_assert(!std::is_copy_constructible_v<Map>);
  static_assert(!std::is_move_constructible_v<Map>);
  static_assert(!std::is_copy_assignable_v<Map>);
  static_assert(!std::is_move_assignable_v<Map>);
  static_assert(noexcept(std::declval<const Map&>().insert(std::uint64_t{0}, std::uint64_t{0})));
  static_assert(noexcept(std::declval<const Map&>().remove(std::uint64_t{0})));
  static_assert(noexcept(std::declval<Map&>().clear()));
  static_assert(sizeof(bool) == 1);
  static_assert(std::is_trivially_copyable_v<Big>);

  Map map;
  CHECK(map.empty());
  CHECK(!map.contains_key(0));
  CHECK(!map.get_copied(0));
  CHECK(!map.remove(0));
  CHECK(!map.update(1, [](const std::uint64_t&, std::uint64_t&) { return 0; }));
  bool visited = false;
  CHECK(!map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
                  [&](const std::uint64_t&, const std::uint64_t&) {
                    visited = true;
                    return false;
                  }));
  CHECK(!visited);
  map.check_invariants(0);

  CHECK(map.insert(42, 7).ok());
  const int query = 42;
  CHECK_EQ(map.get_copied(query).value_or(0), 7u);
  CHECK(map.contains_key(query));
  CHECK(map.remove(query).has_value());
  CHECK(map.empty());

  CHECK(map.insert(10, 1).ok());
  CHECK(map.insert(20, 2).ok());
  std::map<std::uint64_t, std::uint64_t> model{{10, 1}, {20, 2}};
  const std::uint64_t ten = 10, twenty = 20, zero = 0, thirty = 30;
  const std::uint64_t* none = nullptr;
  expect_range(map, model, none, false, none, false);
  expect_range(map, model, &ten, true, &twenty, true);
  expect_range(map, model, &ten, false, &twenty, false);
  expect_range(map, model, &ten, true, &ten, true);
  expect_range(map, model, &ten, false, &ten, false);
  expect_range(map, model, &zero, true, &ten, false);
  expect_range(map, model, &twenty, false, &thirty, true);
  expect_range(map, model, &thirty, true, &zero, true);
  map.check_invariants(2);
}

TEST(shared_stress_int64_edges_sort) {
  std::array<std::int64_t, 5> keys{INT64_MIN, -1, 0, 1, INT64_MAX};
  std::sort(keys.begin(), keys.end());
  do {
    TreeMap<std::int64_t, std::uint64_t> map;
    std::map<std::int64_t, std::uint64_t> model;
    for (std::int64_t key : keys) {
      auto inserted = map.insert(key, static_cast<std::uint64_t>(key));
      CHECK(inserted.ok());
      CHECK(!inserted.value());
      model.emplace(key, static_cast<std::uint64_t>(key));
    }
    check_in_order(map, model);
    map.check_invariants(model.size());
  } while (std::next_permutation(keys.begin(), keys.end()));
}

TEST(shared_stress_u64_matches_std_map) {
  differential<std::uint64_t>(11, 12000, 2500, [](std::uint64_t key) { return key * 7919; });
  differential<std::uint64_t>(12, 8000, 400, [](std::uint64_t key) { return key; });
}

TEST(shared_stress_i64_matches_std_map) {
  differential<std::int64_t>(13, 4000, 3000, [](std::uint64_t key) { return edge_int(key); });
}

TEST(shared_stress_string_matches_std_map) {
  differential<std::string>(14, 3000, 2000, [](std::uint64_t key) { return mixed_string(key); });
  TreeMap<std::string, std::uint64_t> map;
  const std::string empty;
  const std::string nul("a\0b", 3);
  CHECK(map.insert(empty, 1).ok());
  CHECK(map.insert(nul, 2).ok());
  CHECK(map.insert(std::string("a"), 3).ok());
  CHECK(map.insert(std::string("a\0c", 3), 4).ok());
  const std::string_view empty_view(empty);
  const std::string_view nul_view(nul);
  CHECK_EQ(map.get_copied(empty_view).value_or(0), 1u);
  CHECK_EQ(map.get_copied(nul_view).value_or(0), 2u);
  CHECK(map.contains_key(std::string_view("a")));
  std::map<std::string, std::uint64_t> model{{empty, 1}, {nul, 2}, {"a", 3}, {std::string("a\0c", 3), 4}};
  check_in_order(map, model);
  expect_range(map, model, &empty, true, &nul, true);
  map.check_invariants(4);
}

TEST(shared_stress_tied_bytes_match_std_map) {
  differential<std::array<std::uint8_t, 16>>(15, 3000, 2000, [](std::uint64_t key) { return tied_bytes(key); });
}

TEST(shared_stress_custom_key_matches_std_map) {
  differential<OrderedId>(16, 3000, 2000, [](std::uint64_t key) { return OrderedId{key}; });
}

TEST(shared_stress_bounds_on_a_built_tree) {
  for (int shape = 0; shape < 2; ++shape) {
    TreeMap<std::uint64_t, std::uint64_t> map;
    std::map<std::uint64_t, std::uint64_t> model;
    shoal_test::Rng rng(20 + shape);
    const std::size_t count = 2500;
    std::vector<std::uint64_t> keys(count);
    for (std::size_t i = 0; i < count; ++i) keys[i] = shape == 0 ? i : rng.next();
    for (std::uint64_t key : keys) {
      if (model.count(key) != 0) continue;
      CHECK(map.insert(key, key + 1).ok());
      model.emplace(key, key + 1);
    }
    map.check_invariants(model.size());
    sweep_bounds(map, model);
    // Remove every other key so merges run, then the bounds still match.
    std::vector<std::uint64_t> drop;
    std::size_t index = 0;
    for (const auto& entry : model) {
      if (index++ % 2 == 0) drop.push_back(entry.first);
    }
    for (std::uint64_t key : drop) {
      CHECK(map.remove(key).has_value());
      model.erase(key);
    }
    map.check_invariants(model.size());
    sweep_bounds(map, model);
  }
}

TEST(shared_stress_leaf_callbacks_run_once_and_refusal_keeps_the_entry) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  for (std::uint64_t key = 0; key < 300; ++key) {
    int vacant = 0;
    int occupied = 0;
    auto outcome = map.update_or_insert_with_leaf(
        key, key + 1,
        [&](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&, std::uint64_t&) {
          ++occupied;
          return LeafOutcome<bool>::done(false);
        },
        [&](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&, const std::uint64_t&) {
          ++vacant;
          return LeafOutcome<bool>::done(true);
        });
    CHECK(outcome.ok());
    CHECK(outcome.value());
    CHECK_EQ(vacant, 1);
    CHECK_EQ(occupied, 0);
    vacant = 0;
    occupied = 0;
    outcome = map.update_or_insert_with_leaf(
        key, 0,
        [&](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&, std::uint64_t& stored) {
          ++occupied;
          CHECK_EQ(stored, key + 1);
          return LeafOutcome<bool>::refused(Layout{8, 8});
        },
        [&](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&, const std::uint64_t&) {
          ++vacant;
          return LeafOutcome<bool>::done(true);
        });
    CHECK(!outcome.ok());
    CHECK_EQ(outcome.error().key(), key);
    CHECK_EQ(outcome.error().value(), 0u);
    CHECK((outcome.error().layout() == Layout{8, 8}));
    CHECK_EQ(vacant, 0);
    CHECK_EQ(occupied, 1);
    CHECK_EQ(map.get_copied(key).value_or(0), key + 1);

    int callbacks = 0;
    auto removed = map.remove_with_leaf(key, [&](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&,
                                                 const std::uint64_t&) { ++callbacks; });
    CHECK(removed.has_value());
    CHECK_EQ(removed->second, key + 1);
    CHECK_EQ(callbacks, 1);
    callbacks = 0;
    removed = map.remove_with_leaf(key, [&](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&,
                                            const std::uint64_t&) { ++callbacks; });
    CHECK(!removed);
    CHECK_EQ(callbacks, 0);
  }
  // Refusals where a split would otherwise be required.
  for (std::uint64_t key = 0; key < 4000; key += 2) CHECK(map.insert(key, key).ok());
  map.check_invariants(2000);
  for (std::uint64_t key = 1; key < 4000; key += 2) {
    auto outcome = map.update_or_insert_with_leaf(
        key, 5,
        [](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&, std::uint64_t&) {
          return LeafOutcome<bool>::done(false);
        },
        [](TreeMap<std::uint64_t, std::uint64_t>::annexed_leaf, const std::uint64_t&, const std::uint64_t&) {
          return LeafOutcome<bool>::refused(Layout{1, 1});
        });
    CHECK(!outcome.ok());
    CHECK(!map.contains_key(key));
  }
  map.check_invariants(2000);
}

TEST(shared_stress_allocation_failure_leaves_entries_unchanged) {
  auto state = std::make_shared<BudgetState>();
  state->cap.store(0);
  {
    BudgetMap map(BudgetAlloc{state});
    auto failed = map.insert(1, 2);
    CHECK(!failed.ok());
    CHECK_EQ(failed.error().key(), 1u);
    CHECK_EQ(failed.error().value(), 2u);
    CHECK(map.empty());
    map.check_invariants(0);
    state->cap.store(6);
    std::map<std::uint64_t, std::uint64_t> model;
    shoal_test::Rng rng(21);
    for (std::size_t step = 0; step < 600; ++step) {
      const std::uint64_t key = rng.below(80);
      const std::uint64_t value = step + 1;
      if (rng.below(4) == 0 && model.count(key) != 0) {
        const auto removed = map.remove(key);
        CHECK(removed.has_value());
        CHECK_EQ(*removed, model[key]);
        model.erase(key);
        continue;
      }
      auto outcome = map.insert(key, value);
      if (!outcome.ok()) {
        CHECK_EQ(outcome.error().key(), key);
        CHECK_EQ(outcome.error().value(), value);
        const auto got = map.get_copied(key);
        if (model.count(key) == 0) CHECK(!got);
        else CHECK(got.has_value() && *got == model[key]);
      } else {
        apply_insert(model, key, value, outcome.value());
      }
      if (step % 50 == 0) {
        map.check_invariants(model.size());
        check_in_order(map, model);
      }
    }
    map.check_invariants(model.size());
    check_in_order(map, model);
    // Replacing a value needs no page: it stands when the budget is closed.
    state->cap.store(state->in_use.load());
    if (!model.empty()) {
      const auto key = model.begin()->first;
      const auto old = model.begin()->second;
      auto outcome = map.insert(key, old + 9);
      CHECK(outcome.ok());
      CHECK(outcome.value().has_value());
      CHECK_EQ(*outcome.value(), old);
      model[key] = old + 9;
      CHECK_EQ(map.get_copied(key).value_or(0), old + 9);
    }
    map.clear();
    expect_budget_quiescent(*state);
    state->cap.store(32);
    CHECK(map.insert(7, 8).ok());
    CHECK_EQ(map.get_copied(7u).value_or(0), 8u);
    map.check_invariants(1);
  }
  expect_budget_quiescent(*state);
}

TEST(shared_stress_disjoint_threads_match_oracles) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::size_t per_thread = 4000;
  constexpr std::uint64_t space = 1500;
  std::vector<std::map<std::uint64_t, std::uint64_t>> models(k_threads);
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(30 + id);
      auto& model = models[id];
      for (std::size_t step = 0; step < per_thread; ++step) {
        const std::uint64_t key = rng.below(space) * k_threads + id;
        const std::uint64_t choice = rng.below(100);
        if (choice < 45) {
          auto inserted = map.insert(key, step);
          CHECK(inserted.ok());
          apply_insert(model, key, step, inserted.value());
        } else if (choice < 70) {
          const auto removed = map.remove(key);
          auto at = model.find(key);
          CHECK_EQ(removed.has_value(), at != model.end());
          if (at != model.end()) {
            CHECK_EQ(*removed, at->second);
            model.erase(at);
          }
        } else if (choice < 82) {
          const auto updated = map.update(key, [](const std::uint64_t&, std::uint64_t& value) { return value += 3; });
          auto at = model.find(key);
          CHECK_EQ(updated.has_value(), at != model.end());
          if (updated) {
            at->second += 3;
            CHECK_EQ(*updated, at->second);
          }
        } else if (choice < 92) {
          const auto copied = map.get_copied(key);
          auto at = model.find(key);
          CHECK_EQ(copied.has_value(), at != model.end());
          if (copied) CHECK_EQ(*copied, at->second);
        } else {
          std::uint64_t previous = 0;
          bool first = true;
          const std::size_t limit = 32;
          std::size_t seen = 0;
          map.scan(Bound<std::uint64_t>::included(key), Bound<std::uint64_t>::unbounded(),
                   [&](const std::uint64_t& stored, const std::uint64_t&) {
                     CHECK(first || previous < stored);
                     CHECK(stored >= key);
                     first = false;
                     previous = stored;
                     return ++seen < limit;
                   });
        }
      }
    });
  }
  for (auto& worker : workers) worker.join();
  std::map<std::uint64_t, std::uint64_t> all;
  for (const auto& model : models) all.insert(model.begin(), model.end());
  check_in_order(map, all);
  map.check_invariants(all.size());
}

TEST(shared_stress_same_key_inserts_linearize) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::size_t keys = 4;
  constexpr std::size_t per_thread = 800;
  std::vector<std::vector<std::optional<std::uint64_t>>> previous(keys);
  std::mutex mutexes[keys];
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&, id] {
      for (std::size_t step = 0; step < per_thread; ++step) {
        for (std::size_t key = 0; key < keys; ++key) {
          const std::uint64_t value = id * per_thread + step;
          auto outcome = map.insert(key, value);
          CHECK(outcome.ok());
          std::lock_guard<std::mutex> lock(mutexes[key]);
          previous[key].push_back(outcome.value());
        }
      }
    });
  }
  for (auto& worker : workers) worker.join();
  for (std::size_t key = 0; key < keys; ++key) {
    const auto final_value = map.get_copied(key);
    CHECK(final_value.has_value());
    std::vector<std::uint64_t> got;
    int missing = 0;
    for (const auto& value : previous[key]) {
      if (value) got.push_back(*value);
      else ++missing;
    }
    got.push_back(*final_value);
    std::vector<std::uint64_t> want;
    want.reserve(k_threads * per_thread);
    for (std::size_t id = 0; id < k_threads; ++id) {
      for (std::size_t step = 0; step < per_thread; ++step) want.push_back(id * per_thread + step);
    }
    std::sort(got.begin(), got.end());
    std::sort(want.begin(), want.end());
    CHECK_EQ(missing, 1);
    CHECK(got == want);
  }
  map.check_invariants(keys);
}

TEST(shared_stress_increments_linearize) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  CHECK(map.insert(0, 0).ok());
  constexpr std::size_t per_thread = 2000;
  std::vector<std::uint64_t> returned(k_threads * per_thread);
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&, id] {
      for (std::size_t step = 0; step < per_thread; ++step) {
        auto updated = map.update(0, [](const std::uint64_t&, std::uint64_t& value) { return ++value; });
        CHECK(updated.has_value());
        returned[id * per_thread + step] = *updated;
      }
    });
  }
  for (auto& worker : workers) worker.join();
  std::sort(returned.begin(), returned.end());
  for (std::size_t i = 0; i < returned.size(); ++i) CHECK_EQ(returned[i], i + 1);
  CHECK_EQ(map.get_copied(0u).value_or(0), returned.size());
  map.check_invariants(1);
}

TEST(shared_stress_get_or_insert_picks_one_value) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::size_t keys = 8;
  constexpr std::size_t per_thread = 200;
  std::vector<std::vector<std::uint64_t>> seen(keys);
  std::mutex mutexes[keys];
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&, id] {
      for (std::size_t step = 0; step < per_thread; ++step) {
        for (std::size_t key = 0; key < keys; ++key) {
          const std::uint64_t offered = (id + 1) * 1000003u + key * 17u + step;
          auto outcome = map.get_or_insert(key, offered, [](const std::uint64_t&, const std::uint64_t& stored) {
            return stored;
          });
          CHECK(outcome.ok());
          std::lock_guard<std::mutex> lock(mutexes[key]);
          seen[key].push_back(outcome.value());
        }
      }
    });
  }
  for (auto& worker : workers) worker.join();
  for (std::size_t key = 0; key < keys; ++key) {
    const auto final_value = map.get_copied(key);
    CHECK(final_value.has_value());
    for (std::uint64_t value : seen[key]) CHECK_EQ(value, *final_value);
  }
  map.check_invariants(keys);
}

TEST(shared_stress_removes_happen_once_then_pages_are_reused) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::size_t count = 1000;
  for (std::size_t key = 0; key < count; ++key) CHECK(map.insert(key, key + 1).ok());
  std::vector<std::atomic<int>> wins(count);
  for (auto& win : wins) win.store(0);
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&] {
      for (std::size_t key = 0; key < count; ++key) {
        if (auto removed = map.remove(key)) {
          CHECK_EQ(*removed, key + 1);
          wins[key].fetch_add(1, std::memory_order_relaxed);
        }
      }
    });
  }
  for (auto& worker : workers) worker.join();
  for (std::size_t key = 0; key < count; ++key) CHECK_EQ(wins[key].load(), 1);
  CHECK(map.empty());
  map.check_invariants(0);

  // The same keys again: freed pages come back off the free list.
  constexpr std::size_t rounds = 8;
  std::vector<std::vector<std::optional<std::uint64_t>>> previous(count);
  std::mutex mutexes[k_threads];
  // One mutex per stripe of keys, so recording the linearization does not
  // itself serialize every insert.
  workers.clear();
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&, id] {
      for (std::size_t step = 0; step < rounds; ++step) {
        for (std::size_t key = 0; key < count; ++key) {
          const std::uint64_t value = id * rounds + step;
          auto outcome = map.insert(key, value);
          CHECK(outcome.ok());
          std::lock_guard<std::mutex> lock(mutexes[key % k_threads]);
          previous[key].push_back(std::move(outcome.value()));
        }
      }
    });
  }
  for (auto& worker : workers) worker.join();
  for (std::size_t key = 0; key < count; ++key) {
    const auto final_value = map.get_copied(key);
    CHECK(final_value.has_value());
    int missing = 0;
    std::vector<std::uint64_t> got;
    for (const auto& value : previous[key]) {
      if (value) got.push_back(*value);
      else ++missing;
    }
    got.push_back(*final_value);
    std::vector<std::uint64_t> want;
    for (std::size_t id = 0; id < k_threads; ++id) {
      for (std::size_t step = 0; step < rounds; ++step) want.push_back(id * rounds + step);
    }
    std::sort(got.begin(), got.end());
    std::sort(want.begin(), want.end());
    CHECK_EQ(missing, 1);
    CHECK(got == want);
  }
  map.check_invariants(count);
}

TEST(shared_stress_scans_see_stable_keys_once) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::uint64_t stable = 2000;
  for (std::uint64_t i = 0; i < stable; ++i) CHECK(map.insert(i * 2, i * 2 + 1).ok());
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < 2; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(40 + id);
      for (std::size_t step = 0; step < 5000; ++step) {
        if (rng.below(2) == 0) {
          const std::uint64_t key = rng.below(stable) * 2 + 1;
          if (rng.below(2) == 0) map.insert(key, key);
          else map.remove(key);
        } else {
          const std::uint64_t key = stable * 2 + rng.below(stable);
          if (rng.below(2) == 0) map.insert(key, key);
          else map.remove(key);
        }
      }
    });
  }
  for (std::size_t id = 0; id < 2; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(50 + id);
      for (int scans = 0; scans < 24; ++scans) {
        std::uint64_t previous = 0;
        bool first = true;
        std::size_t evens = 0;
        auto visit = [&](const std::uint64_t& key, const std::uint64_t& value) {
          CHECK(first || previous < key);
          first = false;
          previous = key;
          if (key % 2 == 0 && key < stable * 2) {
            CHECK_EQ(value, key + 1);
            ++evens;
          }
          return true;
        };
        CHECK(!map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(), visit));
        CHECK_EQ(evens, static_cast<std::size_t>(stable));
        previous = 0;
        first = true;
        evens = 0;
        auto visit_rev = [&](const std::uint64_t& key, const std::uint64_t& value) {
          CHECK(first || key < previous);
          first = false;
          previous = key;
          if (key % 2 == 0 && key < stable * 2) {
            CHECK_EQ(value, key + 1);
            ++evens;
          }
          return true;
        };
        CHECK(!map.scan_rev(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(), visit_rev));
        CHECK_EQ(evens, static_cast<std::size_t>(stable));
        const std::uint64_t lower = rng.below(stable / 2) * 2;
        const std::uint64_t upper = lower + rng.below(stable / 2) * 2;
        std::size_t in_range = 0;
        map.scan(Bound<std::uint64_t>::included(lower), Bound<std::uint64_t>::included(upper),
                 [&](const std::uint64_t& key, const std::uint64_t& value) {
                   CHECK(key >= lower && key <= upper);
                   if (key % 2 == 0) {
                     CHECK_EQ(value, key + 1);
                     ++in_range;
                   }
                   return true;
                 });
        CHECK_EQ(in_range, static_cast<std::size_t>((upper - lower) / 2 + 1));
      }
    });
  }
  for (auto& worker : workers) worker.join();
  std::size_t count = 0;
  std::uint64_t previous = 0;
  bool first = true;
  std::size_t evens = 0;
  map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
           [&](const std::uint64_t& key, const std::uint64_t& value) {
             CHECK(first || previous < key);
             first = false;
             previous = key;
             if (key % 2 == 0 && key < stable * 2) {
               CHECK_EQ(value, key + 1);
               ++evens;
             }
             ++count;
             return true;
           });
  CHECK_EQ(evens, static_cast<std::size_t>(stable));
  map.check_invariants(count);
}

TEST(shared_stress_append_and_delete_max) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::uint64_t appends = 10000;
  constexpr std::uint64_t base = 1000000;
  std::atomic<std::uint64_t> published{0};
  std::vector<std::atomic<std::uint8_t>> gone(appends);
  for (auto& flag : gone) flag.store(0);
  std::map<std::uint64_t, std::uint64_t> middle;
  std::mutex middle_mu;
  std::vector<std::thread> workers;
  workers.emplace_back([&] {
    for (std::uint64_t i = 0; i < appends; ++i) {
      const std::uint64_t key = base + i;
      auto outcome = map.insert(key, key);
      CHECK(outcome.ok());
      CHECK(!outcome.value());
      published.store(i + 1, std::memory_order_release);
    }
  });
  for (std::size_t id = 0; id < 2; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(60 + id);
      for (std::size_t step = 0; step < 8000; ++step) {
        const std::uint64_t hi = published.load(std::memory_order_acquire);
        if (hi == 0) continue;
        const std::uint64_t index = rng.below(hi);
        const std::uint64_t key = base + index;
        if (auto removed = map.remove(key)) {
          CHECK_EQ(*removed, key);
          gone[index].store(1, std::memory_order_relaxed);
        }
      }
    });
  }
  workers.emplace_back([&] {
    shoal_test::Rng rng(70);
    for (std::size_t step = 0; step < 4000; ++step) {
      const std::uint64_t key = rng.below(8000);
      auto outcome = map.insert(key, key + 3);
      CHECK(outcome.ok());
      std::lock_guard<std::mutex> lock(middle_mu);
      apply_insert(middle, key, key + 3, outcome.value());
      if (rng.below(3) == 0) {
        const auto removed = map.remove(key);
        auto at = middle.find(key);
        CHECK_EQ(removed.has_value(), at != middle.end());
        if (at != middle.end()) {
          CHECK_EQ(*removed, at->second);
          middle.erase(at);
        }
      }
    }
  });
  for (auto& worker : workers) worker.join();
  std::map<std::uint64_t, std::uint64_t> model = middle;
  for (std::uint64_t i = 0; i < appends; ++i) {
    if (gone[i].load() == 0) model.emplace(base + i, base + i);
  }
  check_in_order(map, model);
  map.check_invariants(model.size());
}

TEST(shared_stress_wide_values_are_not_torn) {
  TreeMap<std::uint64_t, Big> map;
  constexpr std::size_t keys = 64;
  for (std::size_t key = 0; key < keys; ++key) CHECK(map.insert(key, Big::of(1)).ok());
  constexpr std::size_t per_thread = 1500;
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(80 + id);
      for (std::size_t step = 0; step < per_thread; ++step) {
        const std::uint64_t key = rng.below(keys);
        const std::uint64_t stamp = id * per_thread + step + 2;
        map.update(key, [&](const std::uint64_t&, Big& value) {
          for (std::uint64_t& word : value.words) word = stamp;
          return 0;
        });
        const auto copied = map.get_copied(key);
        CHECK(copied.has_value());
        CHECK(copied->intact());
        const auto read = map.read(key, [](const std::uint64_t&, const Big& value) { return value; });
        CHECK(read.has_value());
        CHECK(read->intact());
      }
    });
  }
  for (auto& worker : workers) worker.join();
  for (std::size_t key = 0; key < keys; ++key) {
    const auto copied = map.get_copied(key);
    CHECK(copied.has_value());
    CHECK(copied->intact());
  }
  map.check_invariants(keys);
}

TEST(shared_stress_heap_values_are_freed_once) {
  CHECK_EQ(HeapValue::live.load(), 0);
  {
    TreeMap<std::uint64_t, HeapValue> map;
    shoal_test::Rng rng(90);
    std::map<std::uint64_t, int> model;
    for (std::size_t step = 0; step < 2000; ++step) {
      const std::uint64_t key = rng.below(400);
      if (rng.below(3) != 0) {
        auto outcome = map.insert(key, HeapValue(static_cast<int>(key)));
        CHECK(outcome.ok());
        model[key] = static_cast<int>(key);
      } else if (auto removed = map.remove(key)) {
        CHECK_EQ(removed->get(), static_cast<int>(key));
        model.erase(key);
      }
      if (step % 200 == 0) {
        map.check_invariants(model.size());
        CHECK_EQ(HeapValue::live.load(), static_cast<int>(model.size()));
      }
    }
    std::size_t seen = 0;
    map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
             [&](const std::uint64_t& key, const HeapValue& value) {
               CHECK_EQ(value.get(), static_cast<int>(key));
               ++seen;
               return true;
             });
    CHECK_EQ(seen, model.size());
    CHECK_EQ(HeapValue::live.load(), static_cast<int>(model.size()));
    map.check_invariants(model.size());
  }
  CHECK_EQ(HeapValue::live.load(), 0);

  {
    TreeMap<std::uint64_t, HeapValue> map;
    std::vector<std::thread> workers;
    std::vector<std::map<std::uint64_t, int>> models(k_threads);
    for (std::size_t id = 0; id < k_threads; ++id) {
      workers.emplace_back([&, id] {
        shoal_test::Rng rng(100 + id);
        for (std::size_t step = 0; step < 600; ++step) {
          const std::uint64_t key = rng.below(300) * k_threads + id;
          if (rng.below(3) != 0) {
            auto outcome = map.insert(key, HeapValue(static_cast<int>(key)));
            CHECK(outcome.ok());
            models[id][key] = static_cast<int>(key);
          } else if (auto removed = map.remove(key)) {
            CHECK_EQ(removed->get(), static_cast<int>(key));
            models[id].erase(key);
          }
        }
      });
    }
    workers.emplace_back([&] {
      for (int scan = 0; scan < 20; ++scan) {
        std::uint64_t previous = 0;
        bool first = true;
        map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
                 [&](const std::uint64_t& key, const HeapValue& value) {
                   CHECK(first || previous < key);
                   CHECK_EQ(value.get(), static_cast<int>(key));
                   first = false;
                   previous = key;
                   return true;
                 });
      }
    });
    for (auto& worker : workers) worker.join();
    std::size_t expect = 0;
    for (const auto& model : models) {
      expect += model.size();
      for (const auto& entry : model) {
        const auto read = map.read(entry.first, [](const std::uint64_t&, const HeapValue& value) { return value.get(); });
        CHECK(read.has_value());
        CHECK_EQ(*read, entry.second);
      }
    }
    map.check_invariants(expect);
    CHECK_EQ(HeapValue::live.load(), static_cast<int>(expect));
  }
  CHECK_EQ(HeapValue::live.load(), 0);
}

TEST(shared_stress_batches_of_duplicate_keys) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  std::map<std::uint64_t, std::uint64_t> model;
  shoal_test::Rng rng(110);
  for (int round = 0; round < 400; ++round) {
    const std::size_t count = rng.below(8) == 0 ? 0 : 1 + rng.below(70);
    std::vector<std::uint64_t> keys(count);
    std::vector<std::uint64_t> values(count);
    for (std::size_t index = 0; index < count; ++index) {
      if (index > 0 && rng.below(3) == 0) keys[index] = keys[rng.below(index)];
      else keys[index] = rng.below(400);
      values[index] = static_cast<std::uint64_t>(round) * 100 + index;
    }
    std::vector<std::uint64_t> key_copies = keys;
    std::vector<std::optional<std::uint64_t>> previous(count);
    std::vector<char> failed(count, 0);
    if (count == 0) {
      map.insert_many(nullptr, nullptr, 0, nullptr, nullptr);
    } else {
      map.insert_many(keys.data(), values.data(), count, previous.data(), reinterpret_cast<bool*>(failed.data()));
    }
    for (std::size_t index = 0; index < count; ++index) {
      CHECK(!failed[index]);
      apply_insert(model, key_copies[index], values[index], previous[index]);
    }
    if (round % 40 == 0) {
      for (int removal = 0; removal < 30; ++removal) {
        const std::uint64_t key = rng.below(400);
        const auto removed = map.remove(key);
        CHECK_EQ(removed.has_value(), model.erase(key) == 1);
      }
      map.check_invariants(model.size());
    }
  }
  check_in_order(map, model);
  map.check_invariants(model.size());
}

TEST(shared_stress_batches_leave_stable_keys_alone) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  constexpr std::uint64_t stable = 1500;
  for (std::uint64_t key = 0; key < stable; ++key) CHECK(map.insert(key * 2, key * 2 + 1).ok());
  std::vector<std::thread> workers;
  for (std::size_t id = 0; id < k_threads; ++id) {
    workers.emplace_back([&, id] {
      shoal_test::Rng rng(120 + id);
      for (int round = 0; round < 250; ++round) {
        constexpr std::size_t count = 16;
        std::uint64_t keys[count];
        std::uint64_t values[count];
        std::optional<std::uint64_t> previous[count];
        bool failed[count];
        for (std::size_t index = 0; index < count; ++index) {
          // Disjoint odd keys, with duplicates inside the batch.
          const std::uint64_t slot = rng.below(400);
          keys[index] = index % 4 == 3 ? keys[index - 1] : slot * k_threads * 2 + id * 2 + 1;
          values[index] = static_cast<std::uint64_t>(round) * count + index;
        }
        map.insert_many(keys, values, count, previous, failed);
        for (bool bad : failed) CHECK(!bad);
        std::uint64_t probe[count];
        std::optional<std::uint64_t> copied[count];
        for (std::size_t index = 0; index < count; ++index) probe[index] = rng.below(stable) * 2;
        map.get_many_copied(probe, count, copied);
        for (std::size_t index = 0; index < count; ++index) {
          CHECK(copied[index].has_value());
          CHECK_EQ(*copied[index], probe[index] + 1);
        }
      }
    });
  }
  for (auto& worker : workers) worker.join();
  std::size_t evens = 0;
  std::size_t count = 0;
  std::uint64_t previous = 0;
  bool first = true;
  map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
           [&](const std::uint64_t& key, const std::uint64_t& value) {
             CHECK(first || previous < key);
             first = false;
             previous = key;
             if (key % 2 == 0) {
               CHECK_EQ(value, key + 1);
               ++evens;
             }
             ++count;
             return true;
           });
  CHECK_EQ(evens, static_cast<std::size_t>(stable));
  map.check_invariants(count);
}

TEST(shared_stress_string_prefix_scans) {
  TreeMap<std::string, std::uint64_t> map;
  constexpr int stable = 400;
  auto stable_key = [](int index) {
    std::string key = "mmm/";
    key += std::to_string(10000000 + index);
    return key;
  };
  for (int index = 0; index < stable; ++index) CHECK(map.insert(stable_key(index), static_cast<std::uint64_t>(index)).ok());
  std::vector<std::thread> workers;
  workers.emplace_back([&] {
    shoal_test::Rng rng(130);
    for (int step = 0; step < 2000; ++step) {
      if (rng.below(3) == 0) {
        const std::string shrink = rng.below(2) == 0 ? "a" : "zzz";
        if (rng.below(2) == 0) map.insert(shrink, 1);
        else map.remove(shrink);
      } else {
        const int index = static_cast<int>(rng.below(stable));
        std::string key = "mmm/";
        key += std::to_string(index);
        if (rng.below(2) == 0) map.insert(std::move(key), 1);
        else map.remove(key);
      }
    }
  });
  for (int id = 0; id < 3; ++id) {
    workers.emplace_back([&, id] {
      for (int scan = 0; scan < 12; ++scan) {
        std::vector<int> seen;
        std::string previous;
        bool first = true;
        map.scan(Bound<std::string>::unbounded(), Bound<std::string>::unbounded(),
                 [&](const std::string& key, const std::uint64_t& value) {
                   CHECK(first || previous < key);
                   first = false;
                   previous = key;
                   if (key.size() == 12 && key.compare(0, 4, "mmm/") == 0) {
                     int index = 0;
                     for (std::size_t byte = 4; byte < key.size(); ++byte) {
                       CHECK(key[byte] >= '0' && key[byte] <= '9');
                       index = index * 10 + (key[byte] - '0');
                     }
                     CHECK(index >= 10000000);
                     CHECK_EQ(value, static_cast<std::uint64_t>(index - 10000000));
                     seen.push_back(index - 10000000);
                   }
                   return true;
                 });
        CHECK_EQ(seen.size(), static_cast<std::size_t>(stable));
        for (int index = 0; index < stable; ++index) CHECK_EQ(seen[static_cast<std::size_t>(index)], index);
        (void)id;
      }
    });
  }
  for (auto& worker : workers) worker.join();
  map.check_invariants([&] {
    std::size_t count = 0;
    map.scan(Bound<std::string>::unbounded(), Bound<std::string>::unbounded(),
             [&](const std::string&, const std::uint64_t&) {
               ++count;
               return true;
             });
    return count;
  }());
}

TEST(shared_stress_allocation_failure_under_threads) {
  auto state = std::make_shared<BudgetState>();
  state->cap.store(12);
  {
    BudgetMap map(BudgetAlloc{state});
    std::vector<std::map<std::uint64_t, std::uint64_t>> models(k_threads);
    std::vector<std::thread> workers;
    for (std::size_t id = 0; id < k_threads; ++id) {
      workers.emplace_back([&, id] {
        shoal_test::Rng rng(140 + id);
        auto& model = models[id];
        for (std::size_t step = 0; step < 500; ++step) {
          const std::uint64_t key = rng.below(200) * k_threads + id;
          if (rng.below(5) == 0) {
            const auto removed = map.remove(key);
            auto at = model.find(key);
            CHECK_EQ(removed.has_value(), at != model.end());
            if (at != model.end()) {
              CHECK_EQ(*removed, at->second);
              model.erase(at);
            }
            continue;
          }
          const std::uint64_t value = step + 1;
          auto outcome = map.insert(key, value);
          if (!outcome.ok()) {
            CHECK_EQ(outcome.error().key(), key);
            CHECK_EQ(outcome.error().value(), value);
            const auto got = map.get_copied(key);
            if (model.count(key) == 0) CHECK(!got);
            else CHECK(got.has_value() && *got == model[key]);
          } else {
            apply_insert(model, key, value, outcome.value());
          }
        }
      });
    }
    for (auto& worker : workers) worker.join();
    std::size_t count = 0;
    std::uint64_t previous = 0;
    bool first = true;
    map.scan(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded(),
             [&](const std::uint64_t& key, const std::uint64_t&) {
               CHECK(first || previous < key);
               first = false;
               previous = key;
               ++count;
               return true;
             });
    std::size_t expect = 0;
    for (std::size_t id = 0; id < k_threads; ++id) {
      expect += models[id].size();
      for (const auto& entry : models[id]) CHECK_EQ(map.get_copied(entry.first).value_or(~entry.second), entry.second);
    }
    CHECK_EQ(count, expect);
    map.check_invariants(expect);
  }
  expect_budget_quiescent(*state);
}

TEST_MAIN()
