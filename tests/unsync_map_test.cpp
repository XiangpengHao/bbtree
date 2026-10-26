// `shoal::unsync::TreeMap` against `std::map`: iteration in both
// directions, ranges of every kind of bound, values changed through ranges.
#include <cstdint>
#include <cstdio>
#include <iterator>
#include <map>
#include <string>
#include <vector>

#include "shoal/unsync/map.hpp"
#include "test_harness.hpp"

using shoal::Bound;
using shoal::unsync::TreeMap;

namespace {

template <class K>
Bound<K> random_bound(shoal_test::Rng& rng, const K& key) {
  switch (rng.below(3)) {
    case 0: return Bound<K>::included(key);
    case 1: return Bound<K>::excluded(key);
    default: return Bound<K>::unbounded();
  }
}

/// The entries of `model` within the bounds, as `std::map` finds them.
template <class K, class V>
std::vector<std::pair<K, V>> model_range(const std::map<K, V>& model, const Bound<K>& lower,
                                         const Bound<K>& upper) {
  auto from = model.begin();
  if (lower.kind() == Bound<K>::Kind::Included) from = model.lower_bound(lower.key());
  if (lower.kind() == Bound<K>::Kind::Excluded) from = model.upper_bound(lower.key());
  auto to = model.end();
  if (upper.kind() == Bound<K>::Kind::Included) to = model.upper_bound(upper.key());
  if (upper.kind() == Bound<K>::Kind::Excluded) to = model.lower_bound(upper.key());
  std::vector<std::pair<K, V>> entries;
  // Crossed bounds give nothing.
  if (from != model.end() && to != model.end() && to->first < from->first) return entries;
  if (from == model.end()) return entries;
  for (auto at = from; at != to; ++at) entries.push_back(*at);
  return entries;
}

template <class K, class V, class MakeKey>
void check_ranges(std::uint64_t seed, std::size_t entries, std::uint64_t key_space, MakeKey make_key) {
  shoal_test::Rng rng(seed);
  TreeMap<K, V> map;
  std::map<K, V> model;
  for (std::size_t index = 0; index < entries; ++index) {
    const K key = make_key(rng.below(key_space));
    map.insert(key, V(index));
    model[key] = V(index);
  }
  map.check_invariants();
  // Whole-map iteration, both ways and folded.
  std::vector<std::pair<K, V>> expected(model.begin(), model.end());
  std::size_t at = 0;
  for (auto [key, value] : map) {
    CHECK(at < expected.size());
    CHECK(key == expected[at].first && value == expected[at].second);
    ++at;
  }
  CHECK_EQ(at, expected.size());
  at = expected.size();
  auto reversed = map.iter_rev();
  while (auto entry = reversed.next()) {
    CHECK(at > 0);
    --at;
    CHECK(entry->first == expected[at].first);
  }
  CHECK_EQ(at, 0u);
  at = 0;
  map.for_each([&](const K& key, const V& value) {
    CHECK(key == expected[at].first && value == expected[at].second);
    ++at;
  });
  CHECK_EQ(at, expected.size());
  // Random ranges.
  for (int round = 0; round < 2000; ++round) {
    const K low = make_key(rng.below(key_space));
    const K high = make_key(rng.below(key_space));
    const Bound<K> lower = random_bound(rng, low), upper = random_bound(rng, high);
    const auto expected_range = model_range(model, lower, upper);
    std::size_t index = 0;
    for (auto [key, value] : map.range(lower, upper)) {
      CHECK(index < expected_range.size());
      CHECK(key == expected_range[index].first && value == expected_range[index].second);
      ++index;
    }
    CHECK_EQ(index, expected_range.size());
    auto backward = map.range_rev(lower, upper);
    while (auto entry = backward.next()) {
      CHECK(index > 0);
      --index;
      CHECK(entry->first == expected_range[index].first);
    }
    CHECK_EQ(index, 0u);
  }
  // Values change through a mutable range.
  for (auto [key, value] : map.range_mut(Bound<K>::unbounded(), Bound<K>::unbounded())) value = value + V(1);
  for (auto& [key, value] : model) value = value + V(1);
  for (auto [key, value] : map) CHECK(value == model.at(key));
}

}  // namespace

TEST(u64_ranges_agree_with_std_map) {
  check_ranges<std::uint64_t, std::uint64_t>(1, 50000, 100000, [](std::uint64_t key) { return key; });
  check_ranges<std::uint64_t, std::uint64_t>(2, 200, 400, [](std::uint64_t key) { return key; });
  check_ranges<std::uint64_t, std::uint64_t>(3, 5, 10, [](std::uint64_t key) { return key; });
}

TEST(string_ranges_agree_with_std_map) {
  check_ranges<std::string, std::uint64_t>(4, 20000, 40000, [](std::uint64_t key) {
    return "/srv/" + std::to_string(key % 13) + "/" + std::to_string(key);
  });
}

TEST(empty_map_iterates_nothing) {
  TreeMap<std::uint64_t, std::uint64_t> map;
  for (auto entry : map) {
    (void)entry;
    CHECK(false);
  }
  CHECK(!map.iter().next().has_value());
  CHECK(!map.range(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::unbounded()).next());
  map.insert(1, 1);
  map.remove(1);
  CHECK(!map.iter().next().has_value());
  CHECK(!map.first_key_value().has_value());
}

TEST(map_moves_and_reports_its_size) {
  static_assert(sizeof(TreeMap<std::uint64_t, std::uint64_t>) == 24, "the map is 24 bytes");
  TreeMap<std::string, std::string> map;
  for (int index = 0; index < 1000; ++index) map.insert(std::to_string(index), std::to_string(index * 2));
  TreeMap<std::string, std::string> moved(std::move(map));
  CHECK_EQ(moved.size(), 1000u);
  CHECK_EQ(map.size(), 0u);
  CHECK(*moved.get(std::string("10")) == "20");
  CHECK(*moved.get(std::string_view("999")) == "1998");
  map = std::move(moved);
  CHECK_EQ(map.size(), 1000u);
  CHECK(map.first_key_value()->first == "0");
  CHECK(map.last_key_value()->first == "999");
}

TEST(built_maps_agree_with_std_map) {
  for (std::size_t count : {0u, 1u, 12u, 55u, 56u, 1000u, 54321u}) {
    std::vector<std::pair<std::uint64_t, std::uint64_t>> entries;
    for (std::uint64_t index = 0; index < count; ++index) entries.push_back({index * 3, index});
    auto map = TreeMap<std::uint64_t, std::uint64_t>::from_sorted(entries.begin(), entries.end());
    map.check_invariants();
    CHECK_EQ(map.size(), count);
    std::size_t at = 0;
    for (auto [key, value] : map) {
      CHECK_EQ(key, entries[at].first);
      CHECK_EQ(value, entries[at].second);
      ++at;
    }
    // The built map takes inserts and removals as any other.
    for (std::uint64_t key = 0; key < count * 3; key += 7) map.insert(key, 1);
    for (std::uint64_t key = 0; key < count * 3; key += 5) map.remove(key);
    map.check_invariants();
  }
  // Unsorted, with equal keys: the last kept.
  std::vector<std::pair<std::string, int>> entries = {{"b", 1}, {"a", 2}, {"b", 3}, {"c", 4}, {"a", 5}};
  auto map = TreeMap<std::string, int>::from_entries(entries);
  map.check_invariants();
  CHECK_EQ(map.size(), 3u);
  CHECK_EQ(*map.get(std::string("a")), 5);
  CHECK_EQ(*map.get(std::string("b")), 3);
}

TEST(strings_build_with_shrinking_prefixes) {
  std::vector<std::pair<std::string, std::uint64_t>> entries;
  for (std::uint64_t index = 0; index < 20000; ++index) {
    char key[64];
    std::snprintf(key, sizeof(key), "%s%08llu", index < 10000 ? "/var/lib/a/" : "/var/lib/b/",
                  static_cast<unsigned long long>(index));
    entries.push_back({key, index});
  }
  entries.insert(entries.begin(), {std::string("/"), 0});
  auto map = TreeMap<std::string, std::uint64_t>::from_sorted(entries.begin(), entries.end());
  map.check_invariants();
  CHECK_EQ(map.size(), entries.size());
}

TEST(copies_split_and_retain_agree_with_std_map) {
  shoal_test::Rng rng(11);
  TreeMap<std::string, std::uint64_t> map;
  std::map<std::string, std::uint64_t> model;
  for (int index = 0; index < 30000; ++index) {
    std::string key = "/k/" + std::to_string(rng.below(100000));
    map.insert(key, index);
    model[key] = index;
  }
  TreeMap<std::string, std::uint64_t> copy(map);
  copy.check_invariants();
  copy.check_same_nodes(map);
  for (int round = 0; round < 20; ++round) {
    TreeMap<std::string, std::uint64_t> working(copy);
    const std::string at = "/k/" + std::to_string(rng.below(100000));
    TreeMap<std::string, std::uint64_t> upper = working.split_off(at);
    working.check_invariants();
    upper.check_invariants();
    const auto split = model.lower_bound(at);
    CHECK_EQ(working.size(), static_cast<std::size_t>(std::distance(model.begin(), split)));
    CHECK_EQ(upper.size(), static_cast<std::size_t>(std::distance(split, model.end())));
    if (!upper.empty()) CHECK(upper.first_key_value()->first == split->first);
  }
  map.retain([](const std::string&, std::uint64_t& value) { return value % 3 != 0; });
  for (auto at = model.begin(); at != model.end();) {
    at = at->second % 3 == 0 ? model.erase(at) : std::next(at);
  }
  map.check_invariants();
  CHECK_EQ(map.size(), model.size());
  for (const auto& [key, value] : model) CHECK(map.get(key) != nullptr && *map.get(key) == value);
  // Extend, past the batch cutoff, as separate inserts.
  TreeMap<std::uint64_t, std::uint64_t> extended, single;
  std::vector<std::pair<std::uint64_t, std::uint64_t>> more;
  for (std::uint64_t index = 0; index < 100000; ++index) more.push_back({rng.next() % 300000, index});
  extended.extend(more.begin(), more.end());
  for (auto& [key, value] : more) single.insert(key, value);
  extended.check_same_nodes(single);
  CHECK_EQ(extended.size(), single.size());
}

TEST_MAIN()
