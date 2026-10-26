// Differential and boundary checks for the owned map, the shared map, and
// HashIndex. Hashes are adversarial on purpose: a constant zero, an identity
// that is not mixed, and hashes that pile up on one home slot or differ only
// in the tag bits.
#include <algorithm>
#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <limits>
#include <memory>
#include <new>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

#include "shoal/hash_map.hpp"
#include "shoal/unsync/hash_index.hpp"
#include "shoal/unsync/hash_map.hpp"
#include "test_harness.hpp"

namespace {

struct Identity {
  using is_avalanching = std::true_type;
  std::uint64_t operator()(std::uint64_t key) const noexcept { return key; }
};

struct ZeroHash {
  using is_avalanching = std::true_type;
  std::uint64_t operator()(std::uint64_t) const noexcept { return 0; }
};

// Every key homes at slot 0. The top seven bits, which become the tag, differ.
struct TagBits {
  using is_avalanching = std::true_type;
  std::uint64_t operator()(std::uint64_t key) const noexcept { return key << 57; }
};

struct MixIdentity {
  std::uint64_t operator()(std::uint64_t key) const noexcept { return key; }
};

struct BoomHash {
  bool *armed;
  std::uint64_t operator()(int key) const {
    if (*armed) throw std::runtime_error("hash refused");
    return static_cast<std::uint64_t>(key);
  }
};

struct BoomEqual {
  bool *armed;
  bool operator()(int left, int right) const {
    if (*armed) throw std::runtime_error("equality refused");
    return left == right;
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
  void *allocate_zeroed(shoal::Layout layout) noexcept {
    if (state_ && state_->attempts++ == state_->fail_at) return nullptr;
    void *memory = shoal::Global{}.allocate_zeroed(layout);
    if (memory && state_) state_->bytes += layout.size;
    return memory;
  }
  void deallocate(void *memory, shoal::Layout layout) noexcept {
    if (state_) state_->bytes -= layout.size;
    shoal::Global{}.deallocate(memory, layout);
  }

 private:
  AllocationState *state_;
};

struct StrictState {
  std::unordered_map<void *, shoal::Layout> live;
  std::size_t bytes = 0;
};

class StrictAllocator {
 public:
  explicit StrictAllocator(std::shared_ptr<StrictState> state) : state_(std::move(state)) {}
  void *allocate_zeroed(shoal::Layout layout) noexcept {
    void *memory = shoal::Global{}.allocate_zeroed(layout);
    if (!memory) return nullptr;
    if (!state_->live.emplace(memory, layout).second) {
      std::fprintf(stderr, "allocate returned a live block\n");
      std::abort();
    }
    state_->bytes += layout.size;
    return memory;
  }
  void deallocate(void *memory, shoal::Layout layout) noexcept {
    const auto found = state_->live.find(memory);
    if (found == state_->live.end() || found->second != layout) {
      std::fprintf(stderr, "deallocate mismatch size %zu align %zu\n", layout.size, layout.align);
      std::abort();
    }
    state_->bytes -= layout.size;
    state_->live.erase(found);
    shoal::Global{}.deallocate(memory, layout);
  }

 private:
  std::shared_ptr<StrictState> state_;
};

struct MoveOnlyValue {
  std::shared_ptr<int> live;
  std::unique_ptr<int> value;

  MoveOnlyValue(std::shared_ptr<int> counter, int held)
      : live(std::move(counter)), value(std::make_unique<int>(held)) {
    ++*live;
  }
  MoveOnlyValue(const MoveOnlyValue &) = delete;
  MoveOnlyValue &operator=(const MoveOnlyValue &) = delete;
  MoveOnlyValue &operator=(MoveOnlyValue &&) = delete;
  MoveOnlyValue(MoveOnlyValue &&other) noexcept
      : live(std::move(other.live)), value(std::move(other.value)) {}
  ~MoveOnlyValue() {
    if (value) --*live;
  }
  int get() const noexcept { return *value; }
};

struct Wide {
  int id = 0;
  unsigned char pad[590]{};
  explicit Wide(int id_in) noexcept : id(id_in) {
    pad[0] = pad[589] = static_cast<unsigned char>(id_in);
  }
  bool operator==(const Wide &other) const noexcept {
    return id == other.id && pad[0] == other.pad[0] && pad[589] == other.pad[589];
  }
};

template <std::size_t Align>
struct alignas(Align) AlignedValue {
  int id = 0;
  unsigned char pad[200]{};
  explicit AlignedValue(int id_in) noexcept : id(id_in) { pad[0] = pad[199] = 1; }
  bool operator==(const AlignedValue &other) const noexcept { return id == other.id; }
};
using OverAligned = AlignedValue<128>;
// The shared map's `insert` takes its value by value, and MSVC refuses an
// over-aligned parameter (C2719): there the shared map's strict-allocator
// test holds a value of fundamental alignment instead.
#if defined(_MSC_VER)
using SharedOverAligned = AlignedValue<alignof(std::max_align_t)>;
#else
using SharedOverAligned = OverAligned;
#endif

struct CountingHash {
  std::size_t *calls;
  bool *count;
  std::uint64_t operator()(std::uint64_t key) const noexcept {
    if (*count) ++*calls;
    return key;
  }
};

struct ArmedHash {
  bool *armed = nullptr;
  std::uint64_t operator()(int key) const {
    if (armed && *armed) throw std::runtime_error("hash refused");
    return static_cast<std::uint64_t>(key);
  }
};

struct ArmedEqual {
  bool *armed = nullptr;
  bool operator()(int left, int right) const {
    if (armed && *armed) throw std::runtime_error("equality refused");
    return left == right;
  }
};

struct ZeroString {
  using is_avalanching = std::true_type;
  std::uint64_t operator()(std::string_view) const noexcept { return 0; }
};

struct Quad {
  std::uint64_t word[4]{};
  friend bool operator==(const Quad &left, const Quad &right) noexcept {
    return left.word[0] == right.word[0] && left.word[1] == right.word[1] && left.word[2] == right.word[2] &&
           left.word[3] == right.word[3];
  }
};

Quad quad_of(std::uint64_t key) noexcept {
  return Quad{{key, key ^ 0x1111111111111111ull, key + 3, ~key}};
}

template <class Map, class Model>
void expect_same(const Map &map, const Model &model) {
  CHECK_EQ(map.size(), model.size());
  std::size_t seen = 0;
  for (auto [key, value] : map) {
    const auto found = model.find(key);
    CHECK(found != model.end());
    CHECK(value == found->second);
    ++seen;
  }
  CHECK_EQ(seen, model.size());
  map.check_invariants();
}

template <class Hasher>
void unsync_differential(Hasher hash, std::size_t operations, std::uint64_t space, std::uint64_t seed) {
  using Map = shoal::unsync::HashMap<std::uint64_t, std::uint64_t, Hasher>;
  Map map(hash);
  std::unordered_map<std::uint64_t, std::uint64_t, Hasher> model(0, hash);
  shoal_test::Rng rng(seed);
  const std::uint64_t specials[] = {0, 1, ~std::uint64_t{0}, std::uint64_t{1} << 63,
                                    (std::uint64_t{1} << 63) - 1};
  for (std::size_t step = 0; step < operations; ++step) {
    const std::uint64_t key =
        rng.below(8) == 0 ? specials[rng.below(5)] : rng.below(space);
    const auto choice = rng.below(5);
    if (choice < 2) {
      const auto previous = map.insert(key, step);
      const auto found = model.find(key);
      if (previous.has_value() != (found != model.end()) ||
          (previous && *previous != found->second)) {
        std::fprintf(stderr, "insert step %zu key %llu\n", step, static_cast<unsigned long long>(key));
        CHECK(false);
      }
      model[key] = step;
    } else if (choice == 2) {
      const auto removed = map.remove(key);
      const auto found = model.find(key);
      if (removed.has_value() != (found != model.end()) || (removed && *removed != found->second)) {
        std::fprintf(stderr, "erase step %zu key %llu\n", step, static_cast<unsigned long long>(key));
        CHECK(false);
      }
      if (found != model.end()) model.erase(found);
    } else if (choice == 3) {
      auto *value = map.get_mut(key);
      const auto found = model.find(key);
      CHECK_EQ(value != nullptr, found != model.end());
      if (value) {
        CHECK_EQ(*value, found->second);
        ++*value;
        ++found->second;
      }
    } else {
      const auto *value = map.get(key);
      const auto found = model.find(key);
      CHECK_EQ(value != nullptr, found != model.end());
      if (value) CHECK_EQ(*value, found->second);
    }
    CHECK_EQ(map.size(), model.size());
    if (step % 2500 == 0) expect_same(map, model);
  }
  expect_same(map, model);

  std::vector<std::uint64_t> queries;
  queries.push_back(0);
  queries.push_back(~std::uint64_t{0});
  for (std::uint64_t key = 0; key < 200 && key < space; ++key) queries.push_back(key);
  std::vector<const std::uint64_t *> found(queries.size());
  map.get_many(queries.data(), queries.size(), found.data());
  for (std::size_t index = 0; index < queries.size(); ++index) {
    const auto *single = map.get(queries[index]);
    CHECK(found[index] == single);
  }
  map.clear();
  CHECK(map.empty());
  CHECK(map.begin() == map.end());
  map.check_invariants();
}

void expect_members(shoal::unsync::HashMap<std::uint64_t, std::uint64_t, Identity> &map,
                    const std::vector<std::uint64_t> &keys, std::size_t count) {
  CHECK_EQ(map.size(), count);
  for (std::size_t index = 0; index < count; ++index) CHECK_EQ(*map.get(keys[index]), keys[index] ^ 0x5a);
  for (std::size_t index = count; index < keys.size(); ++index) CHECK(!map.get(keys[index]));
  map.check_invariants();
}

}  // namespace

TEST(unsync_differential_against_unordered_map) {
  unsync_differential(Identity{}, 40000, 8000, 11);
  unsync_differential(MixIdentity{}, 40000, 8000, 12);
  unsync_differential(ZeroHash{}, 20000, 400, 13);
  unsync_differential(TagBits{}, 12000, 80, 14);
  unsync_differential(std::hash<std::uint64_t>{}, 40000, 8000, 15);
}

TEST(unsync_load_factor_boundaries_and_tombstone_rebuild_keep_references) {
  shoal::unsync::HashMap<std::uint64_t, std::uint64_t, Identity> map(Identity{});
  for (std::uint64_t count : {1ull, 12ull, 13ull, 24ull, 25ull, 48ull, 49ull, 96ull, 97ull}) {
    map.clear();
    for (std::uint64_t key = 0; key < count; ++key) map.insert(key, key + 1);
    CHECK_EQ(map.size(), static_cast<std::size_t>(count));
    for (std::uint64_t key = 0; key < count; ++key) CHECK_EQ(*map.get(key), key + 1);
    map.check_invariants();
  }

  map.clear();
  for (std::uint64_t key = 0; key < 12; ++key) map.insert(key, key);
  auto *stable = map.get_mut(0);
  for (std::uint64_t key = 1; key <= 6; ++key) CHECK(map.remove(key));
  // size 6, six tombstones, capacity 16: the next insert rebuilds in place.
  map.insert(100, 100);
  CHECK(map.get_mut(0) == stable);
  CHECK_EQ(*stable, 0u);
  CHECK(!map.get(1));
  CHECK_EQ(*map.get(100), 100u);
  for (std::uint64_t key = 7; key < 12; ++key) CHECK_EQ(*map.get(key), key);
  map.check_invariants();

  // Erase invalidates only the removed entry, so key 0 stays put while the
  // others cycle through tombstones and in-place rebuilds.
  for (int round = 0; round < 30; ++round) {
    for (std::uint64_t key = 1; key < 12; ++key) map.remove(key);
    for (std::uint64_t key = 1; key < 12; ++key) map.insert(key, key + static_cast<std::uint64_t>(round));
    map.insert(0, static_cast<std::uint64_t>(round));
    CHECK(map.get_mut(0) == stable);
    CHECK_EQ(*map.get(100), 100u);
    for (std::uint64_t key = 1; key < 12; ++key) CHECK_EQ(*map.get(key), key + static_cast<std::uint64_t>(round));
  }
  CHECK_EQ(*stable, 29u);
  map.check_invariants();
}

TEST(unsync_zero_hash_tombstones_do_not_swallow_later_keys) {
  shoal::unsync::HashMap<std::uint64_t, std::uint64_t, ZeroHash> map(ZeroHash{});
  for (std::uint64_t key = 0; key < 40; ++key) map.insert(key, key * 3);
  for (std::uint64_t key = 0; key < 40; key += 2) CHECK(map.remove(key));
  for (std::uint64_t key = 0; key < 40; ++key) {
    const auto *value = map.get(key);
    if (key % 2 == 0) {
      CHECK(!value);
    } else {
      CHECK_EQ(*value, key * 3);
    }
  }
  auto *stable = map.get_mut(1);
  for (std::uint64_t key = 0; key < 40; key += 2) map.insert(key, key);
  CHECK(map.get_mut(1) == stable);
  CHECK_EQ(map.size(), 40u);
  map.reserve(10);
  CHECK(map.get_mut(1) == stable);
  map.check_invariants();
}

TEST(unsync_wrapped_identity_chain_survives_growth_and_erasure) {
  shoal::unsync::HashMap<std::uint64_t, std::uint64_t, Identity> map(Identity{});
  std::vector<std::uint64_t> keys;
  for (std::uint64_t index = 0; index < 24; ++index) keys.push_back(31 + 32 * index);
  for (std::size_t count = 0; count < keys.size(); ++count) {
    map.insert(keys[count], keys[count] ^ 0x5a);
    expect_members(map, keys, count + 1);
  }
  const auto survivor = keys[5];
  auto *stable = map.get_mut(survivor);
  const std::uint64_t drop[] = {keys[0], keys[10], keys[23], keys[1], keys[22]};
  for (const auto key : drop) {
    CHECK(map.remove(key));
    keys.erase(std::find(keys.begin(), keys.end(), key));
    CHECK(map.get_mut(survivor) == stable);
    expect_members(map, keys, keys.size());
  }
  while (!keys.empty()) {
    const auto key = keys.back();
    CHECK(map.remove(key));
    keys.pop_back();
    if (key == survivor) break;
    CHECK(map.get_mut(survivor) == stable);
  }
  while (!keys.empty()) {
    CHECK(map.remove(keys.back()));
    keys.pop_back();
  }
  CHECK(map.empty());
  map.insert(0, 7);
  CHECK_EQ(*map.get(0), 7u);
  map.check_invariants();
}

TEST(unsync_tag_bits_and_aliased_hashes_stay_distinct) {
  shoal::unsync::HashMap<std::uint64_t, int, TagBits> map(TagBits{});
  for (std::uint64_t key = 0; key < 20; ++key) map.insert(key, static_cast<int>(key));
  for (std::uint64_t key = 0; key < 20; ++key) CHECK_EQ(*map.get(key), static_cast<int>(key));
  for (std::uint64_t key = 0; key < 20; key += 2) CHECK(map.remove(key));
  for (std::uint64_t key = 1; key < 20; key += 2) CHECK_EQ(*map.get(key), static_cast<int>(key));
  map.check_invariants();

  // 1 and 129 collapse to the same hash under that shift, and both must live.
  map.insert(1, 11);
  map.insert(129, 22);
  CHECK_EQ(*map.get(1), 11);
  CHECK_EQ(*map.get(129), 22);
  CHECK_EQ(map.size(), 11u);
}

TEST(unsync_wide_pages_iterate_completely_and_keep_survivor_addresses) {
  shoal::unsync::HashMap<int, Wide> map;
  constexpr int count = 200;
  for (int key = 0; key < count; ++key) map.insert(key, Wide(key));
  auto *stable = map.get_mut(0);
  std::unordered_map<int, int> model;
  for (int key = 0; key < count; ++key) model[key] = key;
  for (int key = 0; key < count; key += 3) {
    if (key == 0) continue;
    CHECK(map.remove(key));
    model.erase(key);
  }
  for (int key = count; key < count + 50; ++key) {
    map.insert(key, Wide(key));
    model[key] = key;
  }
  CHECK(map.get_mut(0) == stable);
  CHECK_EQ(stable->id, 0);
  CHECK_EQ(stable->pad[589], 0);
  std::size_t seen = 0;
  for (auto [key, value] : map) {
    CHECK_EQ(value.id, model.at(key));
    CHECK_EQ(value.pad[0], static_cast<unsigned char>(value.id));
    ++seen;
  }
  CHECK_EQ(seen, model.size());
  const auto &constant = map;
  std::size_t const_seen = 0;
  for (auto [key, value] : constant) {
    CHECK_EQ(value, Wide(model.at(key)));
    ++const_seen;
  }
  CHECK_EQ(const_seen, model.size());
  map.check_invariants();
}

TEST(unsync_move_only_values_follow_erase_overwrite_and_moves) {
  auto live = std::make_shared<int>(0);
  {
    shoal::unsync::HashMap<int, MoveOnlyValue> map;
    for (int key = 0; key < 300; ++key) map.insert(key, MoveOnlyValue(live, key));
    CHECK_EQ(*live, 300);
    for (int key = 0; key < 300; key += 2) {
      auto removed = map.remove(key);
      CHECK_EQ(removed->get(), key);
    }
    CHECK_EQ(*live, 150);
    for (int key = 0; key < 300; key += 2) map.insert(key, MoveOnlyValue(live, -key));
    CHECK_EQ(*live, 300);
    {
      auto replaced = map.insert(1, MoveOnlyValue(live, 8));
      CHECK_EQ(replaced->get(), 1);
    }
    CHECK_EQ(map.get_mut(1)->get(), 8);
    CHECK_EQ(*live, 300);
    auto moved = std::move(map);
    CHECK_EQ(map.size(), 0u);
    CHECK_EQ(*live, 300);
    moved.clear();
    CHECK_EQ(*live, 0);
  }
  CHECK_EQ(*live, 0);
}

TEST(unsync_strings_empty_prefix_and_interior_nul_round_trip) {
  shoal::unsync::HashMap<std::string, std::string> map;
  const std::string empty;
  const std::string nul(1, '\0');
  const std::string nuls(64, '\0');
  const std::string prefix(180, 'a');
  map.insert(empty, "empty");
  map.insert(nul, "nul");
  map.insert(nuls, "nuls");
  map.insert(prefix + "x", "x");
  map.insert(prefix + "y", "y");
  map.insert(std::string(400, 'b'), "long");
  CHECK_EQ(*map.get(std::string_view{}), "empty");
  CHECK_EQ(*map.get(std::string_view(nul)), "nul");
  CHECK_EQ(*map.get(std::string_view(nuls)), "nuls");
  CHECK_EQ(*map.get(prefix + "x"), "x");
  CHECK_EQ(*map.get(std::string_view(prefix + "y")), "y");
  auto *stable = map.get_mut(empty);
  map.insert(std::string(empty), "replaced");
  CHECK(map.get_mut(empty) == stable);
  CHECK_EQ(*stable, "replaced");
  const auto held = map.get_key_value(std::string_view{});
  const std::string *key_address = &held->first;
  map.reserve(10000);
  for (int extra = 0; extra < 500; ++extra) map.insert(std::to_string(extra), "v");
  CHECK(&map.get_key_value(std::string_view{})->first == key_address);
  CHECK(map.remove(nul));
  CHECK(!map.contains_key(std::string_view(nul)));
  CHECK_EQ(*map.get(std::string_view(nuls)), "nuls");
  map.check_invariants();
}

TEST(unsync_batches_cross_tiles_without_reserve_and_with_tombstones) {
  shoal::unsync::HashMap<int, int, ZeroHash> crowded(ZeroHash{});
  for (int key = 0; key < 50; ++key) crowded.insert(key, key);
  for (int key = 0; key < 50; key += 2) crowded.remove(key);
  std::vector<int> keys(300, 7);
  keys[128] = 8;
  keys[129] = 50;
  keys[255] = 1;
  std::vector<int *> created(keys.size());
  int calls = 0;
  crowded.get_or_insert_many(keys.data(), keys.size(), created.data(), [&](const int &key) {
    ++calls;
    return key + 100;
  });
  CHECK_EQ(calls, 2);
  CHECK(created[0] == created[1]);
  CHECK(created[0] == crowded.get_mut(7));
  CHECK_EQ(*created[128], 108);
  CHECK_EQ(*created[129], 150);
  CHECK_EQ(*crowded.get(1), 1);
  CHECK_EQ(*crowded.get(9), 9);
  // 25 survivors of the first 50, plus the tombstoned key and the new key.
  CHECK_EQ(crowded.size(), 27u);
  crowded.check_invariants();

  shoal::unsync::HashMap<int, int> fresh;
  std::vector<int> duplicates(200, 4);
  std::vector<int *> values(duplicates.size());
  int created_once = 0;
  fresh.get_or_insert_many(duplicates.data(), duplicates.size(), values.data(), [&](const int &) {
    ++created_once;
    return 0;
  });
  CHECK_EQ(created_once, 1);
  for (int *value : values) CHECK(value == values[0]);
  fresh.update_or_insert_many(
      duplicates.data(), duplicates.size(),
      [](std::size_t, const int &) { return 0; },
      [](std::size_t, const int &, int &value) { ++value; });
  CHECK_EQ(*fresh.get(4), 200);
  fresh.check_invariants();

  std::vector<int> tile(129);
  std::vector<const int *> looked(tile.size());
  for (std::size_t index = 0; index < tile.size(); ++index) tile[index] = static_cast<int>(index % 17);
  fresh.get_many(tile.data(), tile.size(), looked.data());
  for (std::size_t index = 0; index < tile.size(); ++index)
    CHECK(looked[index] == fresh.get(tile[index]));
  fresh.get_many(static_cast<const int *>(nullptr), 0, static_cast<const int **>(nullptr));
}

TEST(unsync_rebuild_does_not_rehash_and_policies_can_throw) {
  std::size_t calls = 0;
  bool count = false;
  shoal::unsync::HashMap<std::uint64_t, int, CountingHash> map(CountingHash{&calls, &count});
  for (std::uint64_t key = 0; key < 12; ++key) map.insert(key, static_cast<int>(key));
  count = true;
  calls = 0;
  map.insert(12, 12);
  CHECK_EQ(calls, 1u);
  map.check_invariants();

  bool armed = false;
  shoal::unsync::HashMap<int, int, BoomHash, BoomEqual> guarded(BoomHash{&armed}, BoomEqual{&armed});
  guarded.insert(1, 10);
  guarded.insert(2, 20);
  armed = true;
  bool threw = false;
  try {
    guarded.remove(1);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  threw = false;
  try {
    (void)guarded.get(2);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  armed = false;
  CHECK_EQ(*guarded.get(1), 10);
  CHECK_EQ(*guarded.get(2), 20);
  CHECK_EQ(guarded.size(), 2u);
}

TEST(unsync_copy_move_swap_and_allocation_failure_balance) {
  auto left_state = std::make_shared<StrictState>();
  auto right_state = std::make_shared<StrictState>();
  using Map = shoal::unsync::HashMap<int, OverAligned, std::hash<int>, std::equal_to<>, StrictAllocator>;
  {
    Map left(std::hash<int>{}, {}, StrictAllocator{left_state});
    Map right(std::hash<int>{}, {}, StrictAllocator{right_state});
    CHECK_EQ(left_state->bytes, 0u);
    for (int key = 0; key < 30; ++key) left.insert(key, OverAligned(key));
    right.insert(100, OverAligned(100));
    auto *stable = left.get_mut(0);
    left.swap(right);
    CHECK(right.get_mut(0) == stable);
    CHECK_EQ(right.get(0)->id, 0);
    CHECK(!left.get(0));
    CHECK_EQ(left.get(100)->id, 100);
    Map copied = right;
    CHECK_EQ(copied.size(), right.size());
    CHECK_EQ(copied.get(0)->id, 0);
    copied.insert(0, OverAligned(5));
    CHECK_EQ(right.get(0)->id, 0);
    CHECK(right.get_mut(0) == stable);
    left.clear();
    left = std::move(right);
    CHECK(left.get_mut(0) == stable);
    CHECK(right.empty());
    left.clear();
    copied.clear();
  }
  CHECK_EQ(left_state->bytes, 0u);
  CHECK(left_state->live.empty());
  CHECK_EQ(right_state->bytes, 0u);
  CHECK(right_state->live.empty());

  AllocationState counted;
  using Counted = shoal::unsync::HashMap<int, std::string, std::hash<int>, std::equal_to<>, CountingAllocator>;
  Counted source(std::hash<int>{}, {}, CountingAllocator{&counted});
  for (int key = 0; key < 40; ++key) source.insert(key, std::to_string(key));
  const auto bytes = counted.bytes;
  const auto attempts = counted.attempts;
  for (std::size_t fail = 0; fail < 12; ++fail) {
    counted.fail_at = attempts + fail;
    bool threw = false;
    bool completed = false;
    try {
      Counted copy = source;
      CHECK_EQ(copy.size(), source.size());
      completed = true;
    } catch (const std::bad_alloc &) {
      threw = true;
    }
    CHECK(threw != completed);
    CHECK_EQ(counted.bytes, bytes);
    CHECK_EQ(*source.get(3), "3");
  }
  counted.fail_at = ~std::size_t{0};
  source.clear();
  CHECK_EQ(counted.bytes, 0u);
}

TEST(shared_wrapped_chain_scalar_lookup_matches_bulk_read) {
  using Map = shoal::HashMap<std::uint64_t, std::uint64_t, Identity, std::equal_to<>, shoal::Global, 1>;
  Map map(Identity{});
  std::vector<std::uint64_t> keys;
  for (std::uint64_t index = 0; index < 24; ++index) keys.push_back(31 + 32 * index);
  for (std::size_t count = 0; count < keys.size(); ++count) {
    CHECK(map.insert(keys[count], keys[count] ^ 0x9e).ok());
    for (std::size_t index = 0; index <= count; ++index) {
      const auto found = map.get_copied(keys[index]);
      CHECK_EQ(*found, keys[index] ^ 0x9e);
    }
  }
  std::vector<std::uint64_t> queries = keys;
  queries.push_back(0);
  queries.push_back(32);
  queries.push_back(~std::uint64_t{0});
  std::vector<std::optional<std::uint64_t>> bulk(queries.size());
  map.get_many_copied(queries.data(), queries.size(), bulk.data());
  for (std::size_t index = 0; index < queries.size(); ++index)
    CHECK(bulk[index] == map.get_copied(queries[index]));
  map.get_many_copied(static_cast<std::uint64_t *>(nullptr), 0, static_cast<std::optional<std::uint64_t> *>(nullptr));

  const std::uint64_t drop[] = {keys[0], keys[10], keys[23]};
  for (const auto key : drop) {
    const auto removed = map.remove(key);
    CHECK_EQ(*removed, key ^ 0x9e);
    keys.erase(std::find(keys.begin(), keys.end(), key));
    map.get_many_copied(queries.data(), queries.size(), bulk.data());
    for (const auto remaining : keys) {
      const auto found = map.get_copied(remaining);
      CHECK_EQ(*found, remaining ^ 0x9e);
    }
    for (std::size_t index = 0; index < queries.size(); ++index)
      CHECK(bulk[index] == map.get_copied(queries[index]));
    map.check_invariants();
  }
  map.clear();
  CHECK(map.empty());
  CHECK(map.insert(0, 4).ok());
  const auto zero = map.get_copied(0);
  CHECK_EQ(*zero, 4u);
}

TEST(shared_zero_hash_and_tag_bits_match_unordered_map) {
  using ZeroMap = shoal::HashMap<std::uint64_t, std::string, ZeroHash, std::equal_to<>, shoal::Global, 1>;
  ZeroMap zero(ZeroHash{});
  std::unordered_map<std::uint64_t, std::string> model;
  for (std::uint64_t key = 0; key < 36; ++key) {
    const auto value = std::to_string(key);
    auto inserted = zero.insert(key, value);
    CHECK(inserted.ok());
    CHECK(!inserted.value());
    model.emplace(key, value);
  }
  for (std::uint64_t key = 0; key < 36; key += 3) {
    auto removed = zero.remove(key);
    CHECK(*removed == model[key]);
    model.erase(key);
  }
  std::vector<std::uint64_t> queries(48);
  for (std::size_t index = 0; index < queries.size(); ++index) queries[index] = index;
  std::vector<std::optional<std::string>> bulk(queries.size());
  zero.get_many_copied(queries.data(), queries.size(), bulk.data());
  for (std::size_t index = 0; index < queries.size(); ++index) {
    const auto found = model.find(queries[index]);
    CHECK_EQ(bulk[index].has_value(), found != model.end());
    if (found != model.end()) CHECK(*bulk[index] == found->second);
  }
  zero.check_invariants();

  using TagMap = shoal::HashMap<std::uint64_t, std::uint64_t, TagBits, std::equal_to<>, shoal::Global, 1>;
  TagMap tags(TagBits{});
  for (std::uint64_t key = 0; key < 20; ++key) CHECK(tags.insert(key, key + 1).ok());
  std::array<std::uint64_t, 24> tag_keys{};
  std::array<std::optional<std::uint64_t>, 24> tag_values{};
  for (std::size_t index = 0; index < tag_keys.size(); ++index) tag_keys[index] = index;
  tags.get_many_copied(tag_keys.data(), tag_keys.size(), tag_values.data());
  for (std::size_t index = 0; index < tag_keys.size(); ++index) {
    if (tag_keys[index] < 20) {
      CHECK_EQ(*tag_values[index], tag_keys[index] + 1);
    } else {
      CHECK(!tag_values[index]);
    }
  }
  tags.check_invariants();
}

TEST(shared_single_thread_differential_and_clear_keeps_its_block) {
  using Map = shoal::HashMap<std::uint64_t, std::uint64_t, Identity, std::equal_to<>, shoal::Global, 8>;
  Map map(Identity{});
  std::unordered_map<std::uint64_t, std::uint64_t> model;
  shoal_test::Rng rng(99);
  for (std::size_t step = 0; step < 30000; ++step) {
    const std::uint64_t key = rng.below(10) == 0 ? 0 : rng.below(5000);
    const auto choice = rng.below(5);
    if (choice < 2) {
      const auto inserted = map.insert(key, step);
      CHECK(inserted.ok());
      const auto found = model.find(key);
      CHECK_EQ(inserted.value().has_value(), found != model.end());
      if (found != model.end()) CHECK_EQ(*inserted.value(), found->second);
      model[key] = step;
    } else if (choice == 2) {
      const auto removed = map.remove(key);
      const auto found = model.find(key);
      CHECK_EQ(removed.has_value(), found != model.end());
      if (found != model.end()) {
        CHECK_EQ(*removed, found->second);
        model.erase(found);
      }
    } else if (choice == 3 && model.count(key) != 0) {
      const auto updated = map.update(key, [](const std::uint64_t &, std::uint64_t &value) {
        ++value;
        return value;
      });
      CHECK(updated);
      model[key] = *updated;
    } else {
      const auto copied = map.get_copied(key);
      const auto found = model.find(key);
      CHECK_EQ(copied.has_value(), found != model.end());
      if (copied) CHECK_EQ(*copied, found->second);
    }
    if (step % 4000 == 0) {
      CHECK_EQ(map.size(), model.size());
      map.check_invariants();
    }
  }
  CHECK_EQ(map.size(), model.size());
  std::size_t seen = 0;
  map.for_each([&](const std::uint64_t &key, const std::uint64_t &value) {
    CHECK_EQ(value, model.at(key));
    ++seen;
  });
  CHECK_EQ(seen, model.size());
  map.check_invariants();

  AllocationState state;
  {
    shoal::HashMap<int, int, shoal::Hash<int>, std::equal_to<>, CountingAllocator, 1> counted(
        shoal::Hash<int>{}, {}, CountingAllocator{&state});
    CHECK_EQ(state.bytes, 0u);
    for (int key = 0; key < 1000; ++key) CHECK(counted.insert(key, key).ok());
    const auto held = state.bytes;
    CHECK(held != 0);
    counted.clear();
    CHECK_EQ(state.bytes, held);
    CHECK(counted.empty());
    state.fail_at = state.attempts;
    CHECK(counted.insert(1001, 5).ok());
    const auto kept = counted.get_copied(1001);
    CHECK_EQ(*kept, 5);
    CHECK_EQ(counted.size(), 1u);
    CHECK_EQ(state.bytes, held);
  }
  CHECK_EQ(state.bytes, 0u);
}

TEST(unsync_clear_retain_capacity_inserts_without_allocating) {
  AllocationState state;
  using Map = shoal::unsync::HashMap<int, int, std::hash<int>, std::equal_to<>, CountingAllocator>;
  Map map(std::hash<int>{}, {}, CountingAllocator{&state});
  for (int key = 0; key < 40; ++key) map.insert(key, key);
  const auto bytes = state.bytes;
  const auto attempts = state.attempts;
  CHECK(bytes != 0);
  map.clear_retain_capacity();
  CHECK(map.empty());
  CHECK(!map.get(0));
  CHECK_EQ(state.bytes, bytes);
  CHECK_EQ(state.attempts, attempts);
  map.insert(1, 5);
  CHECK_EQ(*map.get(1), 5);
  CHECK_EQ(map.size(), 1u);
  CHECK_EQ(state.bytes, bytes);
  CHECK_EQ(state.attempts, attempts);
  map.check_invariants();
  map.clear();
  CHECK_EQ(state.bytes, 0u);
}

TEST(unsync_move_assignment_releases_through_the_source_allocator) {
  AllocationState first;
  AllocationState second;
  using Map = shoal::unsync::HashMap<int, std::string, std::hash<int>, std::equal_to<>, CountingAllocator>;
  {
    Map destination(std::hash<int>{}, {}, CountingAllocator{&first});
    Map source(std::hash<int>{}, {}, CountingAllocator{&second});
    destination.insert(1000, "dest");
    for (int key = 0; key < 20; ++key) source.insert(key, std::to_string(key));
    CHECK(first.bytes != 0);
    CHECK(second.bytes != 0);
    destination = std::move(source);
    CHECK_EQ(first.bytes, 0u);
    CHECK(second.bytes != 0);
    CHECK(source.empty());
    CHECK(!destination.get(1000));
    CHECK_EQ(*destination.get(2), "2");
    destination.clear();
    CHECK_EQ(second.bytes, 0u);
  }
  CHECK_EQ(first.bytes, 0u);
  CHECK_EQ(second.bytes, 0u);
}

TEST(shared_colliding_strings_survive_erasure_and_bulk_copy) {
  using Map = shoal::HashMap<std::string, std::string, ZeroString, std::equal_to<>, shoal::Global, 1>;
  Map map(ZeroString{});
  std::vector<std::string> keys;
  keys.emplace_back();
  keys.emplace_back(1, '\0');
  keys.emplace_back(32, '\0');
  const std::string prefix(120, 'p');
  keys.push_back(prefix + "x");
  keys.push_back(prefix + "y");
  keys.emplace_back(300, 'q');
  for (int index = 0; index < 30; ++index) keys.push_back(prefix + std::to_string(index));
  for (const auto &key : keys) {
    auto inserted = map.insert(key, "v:" + key);
    CHECK(inserted.ok());
    CHECK(!inserted.value());
  }
  std::vector<std::string> queries = keys;
  queries.emplace_back("missing");
  queries.push_back(prefix);
  std::vector<std::optional<std::string>> bulk(queries.size());
  map.get_many_copied(queries.data(), queries.size(), bulk.data());
  for (std::size_t index = 0; index < keys.size(); ++index) CHECK_EQ(*bulk[index], "v:" + keys[index]);
  CHECK(!bulk[keys.size()]);
  CHECK(!bulk[keys.size() + 1]);
  for (std::size_t index = 0; index < keys.size(); index += 3) {
    auto removed = map.remove(keys[index]);
    CHECK(removed.has_value());
    CHECK_EQ(*removed, "v:" + keys[index]);
  }
  map.check_invariants();
  for (std::size_t index = 0; index < keys.size(); ++index) {
    const auto found = map.get_copied(std::string_view(keys[index]));
    if (index % 3 == 0) {
      CHECK(!found);
    } else {
      CHECK_EQ(*found, "v:" + keys[index]);
    }
  }
  map.check_invariants();
}

TEST(shared_move_only_values_are_destroyed_once) {
  auto live = std::make_shared<int>(0);
  {
    shoal::HashMap<int, MoveOnlyValue, std::hash<int>, std::equal_to<>, shoal::Global, 4> map;
    for (int key = 0; key < 200; ++key) {
      auto inserted = map.insert(key, MoveOnlyValue(live, key));
      CHECK(inserted.ok());
      CHECK(!inserted.value());
    }
    CHECK_EQ(*live, 200);
    for (int key = 0; key < 200; key += 2) {
      auto removed = map.remove(key);
      CHECK(removed.has_value());
      CHECK_EQ(removed->get(), key);
    }
    CHECK_EQ(*live, 100);
    const auto read = map.read(1, [](const int &, const MoveOnlyValue &value) { return value.get(); });
    CHECK_EQ(*read, 1);
    {
      auto replaced = map.insert(1, MoveOnlyValue(live, 8));
      CHECK(replaced.ok());
      CHECK_EQ(replaced.value()->get(), 1);
    }
    CHECK_EQ(*live, 100);
    const auto after = map.read(1, [](const int &, const MoveOnlyValue &value) { return value.get(); });
    CHECK_EQ(*after, 8);
    map.clear();
    CHECK_EQ(*live, 0);
    CHECK(map.empty());
  }
  CHECK_EQ(*live, 0);
}

TEST(shared_policy_exception_releases_the_latch) {
  bool hash_armed = false;
  bool equal_armed = false;
  using Map = shoal::HashMap<int, int, ArmedHash, ArmedEqual, shoal::Global, 1>;
  Map map(ArmedHash{&hash_armed}, ArmedEqual{&equal_armed});
  CHECK(map.insert(1, 10).ok());
  CHECK(map.insert(2, 20).ok());
  equal_armed = true;
  bool threw = false;
  try {
    (void)map.remove(1);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  threw = false;
  try {
    (void)map.insert(1, 99);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  equal_armed = false;
  const auto first = map.get_copied(1);
  const auto second = map.get_copied(2);
  CHECK_EQ(*first, 10);
  CHECK_EQ(*second, 20);
  CHECK_EQ(map.size(), 2u);
  hash_armed = true;
  threw = false;
  try {
    (void)map.insert(3, 30);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  hash_armed = false;
  CHECK(map.insert(3, 30).ok());
  const auto third = map.get_copied(3);
  CHECK_EQ(*third, 30);
  CHECK_EQ(map.size(), 3u);
  map.check_invariants();
}

TEST(shared_strict_allocator_tracks_growth_clear_and_destruction) {
  auto state = std::make_shared<StrictState>();
  {
    using Map = shoal::HashMap<int, SharedOverAligned, std::hash<int>, std::equal_to<>, StrictAllocator, 1>;
    Map map(std::hash<int>{}, {}, StrictAllocator{state});
    CHECK_EQ(state->bytes, 0u);
    CHECK(state->live.empty());
    for (int key = 0; key < 40; ++key) CHECK(map.insert(key, SharedOverAligned(key)).ok());
    const auto held = state->bytes;
    CHECK(held != 0);
    for (int key = 0; key < 40; key += 2) {
      auto removed = map.remove(key);
      CHECK(removed.has_value());
      CHECK_EQ(removed->id, key);
    }
    CHECK_EQ(state->bytes, held);
    map.clear();
    CHECK(map.empty());
    CHECK_EQ(state->bytes, held);
    CHECK(map.insert(1, SharedOverAligned(7)).ok());
    const auto kept = map.get_copied(1);
    CHECK_EQ(kept->id, 7);
    CHECK_EQ(state->bytes, held);
  }
  CHECK_EQ(state->bytes, 0u);
  CHECK(state->live.empty());
}

TEST(shared_growth_refusal_returns_the_entry) {
  AllocationState state;
  using Map = shoal::HashMap<int, std::string, std::hash<int>, std::equal_to<>, CountingAllocator, 1>;
  Map map(std::hash<int>{}, {}, CountingAllocator{&state});
  for (int key = 0; key < 12; ++key) CHECK(map.insert(key, std::to_string(key)).ok());
  const auto bytes = state.bytes;
  state.fail_at = state.attempts;
  auto refused = map.insert(12, std::string("kept"));
  CHECK(!refused.ok());
  CHECK_EQ(refused.error().key(), 12);
  CHECK_EQ(refused.error().value(), "kept");
  CHECK_EQ(map.size(), 12u);
  CHECK(!map.get_copied(12));
  const auto kept_zero = map.get_copied(0);
  CHECK_EQ(*kept_zero, "0");
  CHECK_EQ(state.bytes, bytes);
  state.fail_at = ~std::size_t{0};
  auto retried = map.insert(std::move(refused.error().key()), std::move(refused.error().value()));
  CHECK(retried.ok());
  CHECK(!retried.value());
  const auto kept = map.get_copied(12);
  CHECK_EQ(*kept, "kept");
  CHECK_EQ(map.size(), 13u);
  map.check_invariants();
}

TEST(shared_same_key_insert_and_erase_happen_once) {
  constexpr int threads = 4;
  shoal::HashMap<int, int> map;
  std::atomic<int> fresh{0};
  std::vector<std::thread> workers;
  workers.reserve(threads);
  for (int thread = 0; thread < threads; ++thread) {
    workers.emplace_back([&, thread] {
      auto inserted = map.insert(42, thread);
      CHECK(inserted.ok());
      if (!inserted.value()) fresh.fetch_add(1, std::memory_order_relaxed);
    });
  }
  for (auto &worker : workers) worker.join();
  CHECK_EQ(fresh.load(), 1);
  const auto stored = map.get_copied(42);
  CHECK(stored.has_value());
  CHECK(*stored >= 0 && *stored < threads);
  CHECK_EQ(map.size(), 1u);

  std::atomic<int> erased{0};
  workers.clear();
  for (int thread = 0; thread < threads; ++thread) {
    workers.emplace_back([&] {
      if (map.remove(42)) erased.fetch_add(1, std::memory_order_relaxed);
    });
  }
  for (auto &worker : workers) worker.join();
  CHECK_EQ(erased.load(), 1);
  CHECK(!map.get_copied(42));
  CHECK(map.empty());
  map.check_invariants();
}

TEST(shared_concurrent_disjoint_keys_and_contended_updates) {
  constexpr int threads = 4;
  constexpr int span = 1500;
  constexpr int shared_keys = 8;
  constexpr int bumps = 200;
  shoal::HashMap<int, int> map;
  for (int key = 0; key < shared_keys; ++key) CHECK(map.insert(key, 0).ok());
  std::atomic<int> errors{0};
  std::vector<std::thread> workers;
  for (int thread = 0; thread < threads; ++thread) {
    workers.emplace_back([&, thread] {
      const int base = 1000 + thread * span;
      for (int key = base; key < base + span; ++key) {
        auto inserted = map.insert(key, key);
        if (!inserted.ok() || inserted.value()) errors.fetch_add(1, std::memory_order_relaxed);
      }
      for (int key = base; key < base + span; key += 2) {
        auto removed = map.remove(key);
        if (!removed || *removed != key) errors.fetch_add(1, std::memory_order_relaxed);
      }
      for (int key = base; key < base + span; ++key) {
        const auto found = map.get_copied(key);
        const bool want = (key - base) % 2 != 0;
        if (found.has_value() != want || (found && *found != key))
          errors.fetch_add(1, std::memory_order_relaxed);
      }
      for (int bump = 0; bump < bumps; ++bump) {
        for (int key = 0; key < shared_keys; ++key) {
          const auto updated = map.update(key, [](const int &, int &value) {
            ++value;
            return value;
          });
          if (!updated) errors.fetch_add(1, std::memory_order_relaxed);
        }
      }
    });
  }
  for (auto &worker : workers) worker.join();
  CHECK_EQ(errors.load(), 0);
  for (int thread = 0; thread < threads; ++thread) {
    const int base = 1000 + thread * span;
    for (int key = base; key < base + span; ++key) {
      const auto found = map.get_copied(key);
      if ((key - base) % 2 == 0) {
        CHECK(!found);
      } else {
        CHECK_EQ(*found, key);
      }
    }
  }
  for (int key = 0; key < shared_keys; ++key) {
    const auto found = map.get_copied(key);
    CHECK_EQ(*found, threads * bumps);
  }
  CHECK_EQ(map.size(), static_cast<std::size_t>(shared_keys + threads * (span / 2)));
  map.check_invariants();
}

TEST(shared_concurrent_bulk_reads_see_intact_quads) {
  constexpr int span = 256;
  using Map = shoal::HashMap<std::uint64_t, Quad, Identity, std::equal_to<>, shoal::Global, 1>;
  Map map(Identity{});
  std::vector<std::uint64_t> keys;
  keys.reserve(span);
  for (int index = 0; index < span; ++index) keys.push_back(31 + 32 * static_cast<std::uint64_t>(index));
  std::atomic<bool> run{true};
  std::atomic<int> errors{0};
  std::vector<std::thread> workers;
  for (int thread = 0; thread < 2; ++thread) {
    workers.emplace_back([&, thread] {
      const int begin = thread * (span / 2);
      const int end = begin + span / 2;
      for (int round = 0; round < 4; ++round) {
        for (int index = begin; index < end; ++index) {
          auto inserted = map.insert(keys[index], quad_of(keys[index]));
          if (!inserted.ok()) errors.fetch_add(1, std::memory_order_relaxed);
        }
        for (int index = begin; index < end; index += 2) {
          auto removed = map.remove(keys[index]);
          if (!removed || !(*removed == quad_of(keys[index]))) errors.fetch_add(1, std::memory_order_relaxed);
        }
      }
    });
  }
  for (int thread = 0; thread < 2; ++thread) {
    workers.emplace_back([&] {
      std::vector<std::optional<Quad>> values(keys.size());
      while (run.load(std::memory_order_relaxed)) {
        map.get_many_copied(keys.data(), keys.size(), values.data());
        for (std::size_t index = 0; index < keys.size(); ++index) {
          if (values[index] && !(*values[index] == quad_of(keys[index])))
            errors.fetch_add(1, std::memory_order_relaxed);
        }
      }
    });
  }
  for (int index = 0; index < 2; ++index) workers[index].join();
  run.store(false, std::memory_order_relaxed);
  for (int index = 2; index < 4; ++index) workers[index].join();
  CHECK_EQ(errors.load(), 0);
  for (int index = 0; index < span; ++index) {
    const auto found = map.get_copied(keys[index]);
    if (index % 2 == 0) {
      CHECK(!found);
    } else {
      CHECK(*found == quad_of(keys[index]));
    }
  }
  map.check_invariants();
}

TEST(shared_concurrent_strings_match_per_thread_oracles) {
  constexpr int threads = 4;
  constexpr int span = 250;
  shoal::HashMap<std::string, std::string> map;
  std::atomic<int> errors{0};
  std::vector<std::thread> workers;
  for (int thread = 0; thread < threads; ++thread) {
    workers.emplace_back([&, thread] {
      const std::string prefix(80, static_cast<char>('a' + thread));
      std::vector<std::string> mine;
      mine.reserve(span);
      for (int index = 0; index < span; ++index) mine.push_back(prefix + std::to_string(index));
      if (thread == 0) mine.emplace_back();
      for (const auto &key : mine) {
        auto inserted = map.insert(key, "v" + key);
        if (!inserted.ok() || inserted.value()) errors.fetch_add(1, std::memory_order_relaxed);
      }
      for (std::size_t index = 0; index < mine.size(); index += 2) {
        auto removed = map.remove(mine[index]);
        if (!removed || *removed != "v" + mine[index]) errors.fetch_add(1, std::memory_order_relaxed);
      }
      for (std::size_t index = 0; index < mine.size(); ++index) {
        const auto found = map.get_copied(mine[index]);
        if (index % 2 == 0) {
          if (found) errors.fetch_add(1, std::memory_order_relaxed);
        } else if (!found || *found != "v" + mine[index]) {
          errors.fetch_add(1, std::memory_order_relaxed);
        }
      }
    });
  }
  for (auto &worker : workers) worker.join();
  CHECK_EQ(errors.load(), 0);
  std::size_t seen = 0;
  map.for_each([&](const std::string &key, const std::string &value) {
    CHECK_EQ(value, "v" + key);
    ++seen;
  });
  CHECK_EQ(seen, map.size());
  map.check_invariants();
}

namespace {

std::uint64_t hash_zero(std::uint64_t) { return 0; }
std::uint64_t hash_ones(std::uint64_t) { return ~std::uint64_t{0}; }
std::uint64_t hash_identity(std::uint64_t key) { return key; }
std::uint64_t hash_shift48(std::uint64_t key) { return key << 48; }
std::uint64_t hash_shift57(std::uint64_t key) { return key << 57; }
std::uint64_t hash_mix(std::uint64_t key) { return key * 0x9E3779B97F4A7C15ull; }

void check_index_hashes(std::uint64_t (*hash_of)(std::uint64_t)) {
  struct Rows {
    std::vector<std::uint64_t> stored;
    std::vector<std::uint64_t> queries;
    std::uint64_t (*hash_of)(std::uint64_t) = nullptr;
    auto hashes() {
      return [this](std::uint32_t row) { return hash_of(queries[row]); };
    }
    auto cached() {
      return [this](std::uint64_t handle) { return hash_of(stored[handle - 1]); };
    }
    auto compare() {
      return [this](std::uint32_t *rows, std::size_t count, std::uint64_t *results, std::uint32_t *rejected) {
        std::size_t rejected_count = 0;
        for (std::size_t index = 0; index < count; ++index) {
          const auto handle = results[rows[index]];
          if (handle == 0 || handle > stored.size() || queries[rows[index]] != stored[handle - 1])
            rejected[rejected_count++] = rows[index];
        }
        return rejected_count;
      };
    }
    auto create() {
      return [this](const std::uint32_t *rows, std::size_t count, std::uint64_t *handles) {
        for (std::size_t index = 0; index < count; ++index) {
          stored.push_back(queries[rows[index]]);
          handles[index] = stored.size();
        }
      };
    }
  };

  Rows rows;
  rows.hash_of = hash_of;
  for (std::uint64_t key = 0; key < 90; ++key) rows.queries.push_back(key);
  for (std::uint64_t key : {std::uint64_t{65537}, ~std::uint64_t{0}, std::uint64_t{1} << 63,
                            (std::uint64_t{1} << 48) - 2, std::uint64_t{1}})
    rows.queries.push_back(key);
  rows.queries.push_back(0);
  rows.queries.push_back(65537);

  using Index = shoal::unsync::HashIndex<std::uint64_t>;
  Index index;
  Index::ProbeState scratch;
  std::vector<std::uint64_t> results(rows.queries.size());
  std::vector<std::uint64_t> positions(rows.queries.size());
  index.find_or_create_many(scratch, rows.queries.size(), rows.hashes(), rows.cached(), rows.compare(),
                            rows.create(), results.data(), positions.data());
  std::unordered_map<std::uint64_t, std::uint64_t> expected;
  for (std::size_t index_row = 0; index_row < rows.queries.size(); ++index_row) {
    const auto handle = results[index_row];
    CHECK(handle >= 1 && handle <= rows.stored.size());
    CHECK_EQ(rows.stored[handle - 1], rows.queries[index_row]);
    CHECK_EQ(index.handle_at(positions[index_row]), handle);
    const auto [placed, inserted] = expected.emplace(rows.queries[index_row], handle);
    CHECK_EQ(placed->second, handle);
    (void)inserted;
  }
  CHECK_EQ(index.size(), expected.size());
  index.check_invariants(rows.cached());

  rows.queries.push_back(424242);
  rows.queries.push_back((std::uint64_t{1} << 48) - 3);
  results.assign(rows.queries.size(), 7);
  positions.assign(rows.queries.size(), 7);
  index.find_many(scratch, rows.queries.size(), rows.hashes(), rows.compare(), results.data(), positions.data());
  for (std::size_t index_row = 0; index_row < rows.queries.size(); ++index_row) {
    const auto known = expected.find(rows.queries[index_row]);
    if (known == expected.end()) {
      CHECK_EQ(results[index_row], 0u);
      CHECK_EQ(positions[index_row], std::numeric_limits<std::uint64_t>::max());
    } else {
      CHECK_EQ(results[index_row], known->second);
      CHECK_EQ(index.handle_at(positions[index_row]), results[index_row]);
    }
  }

  std::vector<std::uint64_t> handles;
  handles.reserve(expected.size());
  for (const auto &entry : expected) handles.push_back(entry.second);
  std::reverse(handles.begin(), handles.end());
  const auto capacity_before = index.capacity();
  index.reserve_from(expected.size() * 4, [&](auto visit) {
    for (const auto handle : handles) visit(handle, hash_of(rows.stored[handle - 1]));
  });
  CHECK(index.capacity() > capacity_before);
  index.check_invariants(rows.cached());
  results.assign(rows.queries.size(), 0);
  positions.assign(rows.queries.size(), 0);
  index.find_many(scratch, rows.queries.size(), rows.hashes(), rows.compare(), results.data(), positions.data());
  for (std::size_t index_row = 0; index_row < rows.queries.size(); ++index_row) {
    const auto known = expected.find(rows.queries[index_row]);
    if (known == expected.end()) {
      CHECK_EQ(results[index_row], 0u);
    } else {
      CHECK_EQ(results[index_row], known->second);
    }
  }

  const auto retained = index.capacity();
  index.clear_retain_capacity();
  CHECK_EQ(index.size(), 0u);
  CHECK_EQ(index.capacity(), retained);
  index.find_many(scratch, 1, rows.hashes(), rows.compare(), results.data(), positions.data());
  CHECK_EQ(results[0], 0u);
  CHECK_EQ(positions[0], std::numeric_limits<std::uint64_t>::max());

  const auto stored_before = rows.stored.size();
  index.find_or_create_many(scratch, rows.queries.size(), rows.hashes(), rows.cached(), rows.compare(),
                            rows.create(), results.data(), positions.data());
  std::unordered_map<std::uint64_t, std::uint64_t> rebuilt;
  for (std::size_t index_row = 0; index_row < rows.queries.size(); ++index_row) {
    CHECK_EQ(rows.stored[results[index_row] - 1], rows.queries[index_row]);
    const auto [placed, inserted] = rebuilt.emplace(rows.queries[index_row], results[index_row]);
    CHECK_EQ(placed->second, results[index_row]);
    (void)inserted;
  }
  CHECK_EQ(index.size(), rebuilt.size());
  CHECK_EQ(rows.stored.size(), stored_before + rebuilt.size());
  index.check_invariants(rows.cached());
}

}  // namespace

TEST(hash_index_adversarial_hashes_reserve_and_clear_retain) {
  check_index_hashes(hash_zero);
  check_index_hashes(hash_ones);
  check_index_hashes(hash_identity);
  check_index_hashes(hash_shift48);
  check_index_hashes(hash_shift57);
  check_index_hashes(hash_mix);
}

TEST(hash_index_duplicate_across_the_tile_boundary_is_one_row) {
  for (auto hash_of : {hash_identity, hash_zero, hash_shift48}) {
    struct Rows {
      std::vector<std::uint64_t> stored;
      std::vector<std::uint64_t> queries;
      std::uint64_t (*hash_of)(std::uint64_t) = nullptr;
      auto hashes() {
        return [this](std::uint32_t row) { return hash_of(queries[row]); };
      }
      auto cached() {
        return [this](std::uint64_t handle) { return hash_of(stored[handle - 1]); };
      }
      auto compare() {
        return [this](std::uint32_t *rows, std::size_t count, std::uint64_t *results, std::uint32_t *rejected) {
          std::size_t rejected_count = 0;
          for (std::size_t index = 0; index < count; ++index) {
            const auto handle = results[rows[index]];
            if (handle == 0 || handle > stored.size() || queries[rows[index]] != stored[handle - 1])
              rejected[rejected_count++] = rows[index];
          }
          return rejected_count;
        };
      }
      auto create() {
        return [this](const std::uint32_t *rows, std::size_t count, std::uint64_t *handles) {
          for (std::size_t index = 0; index < count; ++index) {
            stored.push_back(queries[rows[index]]);
            handles[index] = stored.size();
          }
        };
      }
    };
    Rows rows;
    rows.hash_of = hash_of;
    rows.queries.resize(2050);
    for (std::size_t index = 0; index < rows.queries.size(); ++index) rows.queries[index] = index + 1;
    const std::uint64_t shared = 9000001;
    rows.queries[2047] = shared;
    rows.queries[2048] = shared;
    rows.queries[10] = 1;
    rows.queries[11] = 65537;
    using Index = shoal::unsync::HashIndex<std::uint64_t>;
    Index index;
    Index::ProbeState scratch;
    std::vector<std::uint64_t> results(rows.queries.size());
    std::vector<std::uint64_t> positions(rows.queries.size());
    index.find_or_create_many(scratch, rows.queries.size(), rows.hashes(), rows.cached(), rows.compare(),
                              rows.create(), results.data(), positions.data());
    CHECK_EQ(results[2047], results[2048]);
    CHECK_EQ(rows.stored[results[2047] - 1], shared);
    CHECK(results[10] != results[11]);
    CHECK_EQ(rows.stored[results[10] - 1], 1u);
    CHECK_EQ(rows.stored[results[11] - 1], 65537u);
    index.check_invariants(rows.cached());
    index.find_many(scratch, rows.queries.size(), rows.hashes(), rows.compare(), results.data(), positions.data());
    CHECK_EQ(results[2047], results[2048]);
    CHECK_EQ(index.handle_at(positions[2047]), results[2047]);
    CHECK_EQ(index.handle_at(positions[2048]), results[2048]);
  }
}

TEST(hash_index_maximum_handle_and_empty_batch) {
  using Index = shoal::unsync::HashIndex<std::uint64_t>;
  Index index;
  Index::ProbeState scratch;
  index.find_or_create_many(
      scratch, 0, [](std::uint32_t) -> std::uint64_t { return 0; },
      [](std::uint64_t) -> std::uint64_t { return 0; },
      [](std::uint32_t *, std::size_t, std::uint64_t *, std::uint32_t *) { return std::size_t{0}; },
      [](const std::uint32_t *, std::size_t, std::uint64_t *) {}, static_cast<std::uint64_t *>(nullptr));
  index.find_many(
      scratch, 0, [](std::uint32_t) -> std::uint64_t { return 0; },
      [](std::uint32_t *, std::size_t, std::uint64_t *, std::uint32_t *) { return std::size_t{0}; },
      static_cast<std::uint64_t *>(nullptr));
  CHECK_EQ(index.size(), 0u);

  const auto max_handle = (std::uint64_t{1} << 48) - 2;
  std::unordered_map<std::uint64_t, std::uint64_t> stored;
  std::uint64_t queries[] = {42, 99};
  auto hash_at = [&](std::uint32_t row) { return queries[row] << 48; };
  auto cached = [&](std::uint64_t handle) { return stored.at(handle) << 48; };
  auto compare = [&](std::uint32_t *rows, std::size_t count, std::uint64_t *results, std::uint32_t *rejected) {
    std::size_t rejected_count = 0;
    for (std::size_t index_row = 0; index_row < count; ++index_row) {
      const auto found = stored.find(results[rows[index_row]]);
      if (found == stored.end() || found->second != queries[rows[index_row]]) rejected[rejected_count++] = rows[index_row];
    }
    return rejected_count;
  };
  auto create = [&](const std::uint32_t *rows, std::size_t count, std::uint64_t *handles) {
    CHECK_EQ(count, 2u);
    stored[max_handle] = queries[rows[0]];
    stored[1] = queries[rows[1]];
    handles[0] = max_handle;
    handles[1] = 1;
  };
  std::uint64_t results[2]{};
  std::uint64_t positions[2]{};
  index.find_or_create_many(scratch, 2, hash_at, cached, compare, create, results, positions);
  CHECK_EQ(results[0], max_handle);
  CHECK_EQ(results[1], 1u);
  CHECK_EQ(index.handle_at(positions[0]), max_handle);
  CHECK_EQ(index.handle_at(positions[1]), 1u);
  index.check_invariants(cached);
  std::uint64_t found[2]{};
  index.find_many(scratch, 2, hash_at, compare, found, positions);
  CHECK_EQ(found[0], max_handle);
  CHECK_EQ(found[1], 1u);
  std::uint64_t absent_result = 5;
  std::uint64_t absent_position = 5;
  const std::uint64_t absent_key = 7;
  index.find_many(
      scratch, 1, [&](std::uint32_t) { return absent_key << 48; }, compare, &absent_result, &absent_position);
  CHECK_EQ(absent_result, 0u);
  CHECK_EQ(absent_position, std::numeric_limits<std::uint64_t>::max());
  CHECK_EQ(index.size(), 2u);
  index.find_or_create_many(
      scratch, 0, hash_at, cached, compare, create, static_cast<std::uint64_t *>(nullptr));
  CHECK_EQ(index.size(), 2u);
  index.check_invariants(cached);
}

TEST_MAIN()
