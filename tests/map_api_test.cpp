// The unsync map's entries, extractions, consuming iterators, `append` and
// `replace_key` against `std::map`, with the tree's invariants checked after
// each change.
#include <cstdint>
#include <iterator>
#include <map>
#include <type_traits>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "shoal/unsync/map.hpp"
#include "test_harness.hpp"

using shoal::Bound;
using shoal::unsync::TreeMap;

namespace {

/// A value that counts how many of its kind are alive, to check that every
/// entry is destroyed once.
class Tracked {
 public:
  Tracked(int* live, std::uint64_t id) noexcept : live_(live), id_(id) { ++*live_; }
  Tracked(Tracked&& other) noexcept : live_(other.live_), id_(other.id_) { other.live_ = nullptr; }
  Tracked& operator=(Tracked&& other) noexcept {
    if (live_ != nullptr) --*live_;
    live_ = other.live_;
    id_ = other.id_;
    other.live_ = nullptr;
    return *this;
  }
  ~Tracked() {
    if (live_ != nullptr) --*live_;
  }
  std::uint64_t id() const noexcept { return id_; }

 private:
  int* live_;
  std::uint64_t id_;
};

}  // namespace

// `Tracked` points to no part of itself: moving its bytes moves it.
template <>
struct shoal::is_trivially_relocatable<Tracked> : std::true_type {};

namespace {

std::string path_key(std::uint64_t index) { return "/data/" + std::to_string(index % 7) + "/" + std::to_string(index); }

template <class K, class V>
void check_same(const TreeMap<K, V>& map, const std::map<K, V>& model) {
  map.check_invariants();
  CHECK_EQ(map.size(), model.size());
  auto at = model.begin();
  for (auto [key, value] : map) {
    CHECK(at != model.end());
    CHECK(key == at->first && value == at->second);
    ++at;
  }
  CHECK(at == model.end());
}

template <class K, class MakeKey>
void check_entries(std::uint64_t seed, std::uint64_t key_space, MakeKey make_key) {
  shoal_test::Rng rng(seed);
  TreeMap<K, std::uint64_t> map;
  std::map<K, std::uint64_t> model;
  for (int round = 0; round < 20000; ++round) {
    const K key = make_key(rng.below(key_space));
    switch (rng.below(5)) {
      case 0: {
        // Count occurrences, as Rust's doc example does.
        std::move(map.entry(key)).and_modify([](std::uint64_t& value) { value += 1; }).or_insert(1);
        auto [at, inserted] = model.emplace(key, 1);
        if (!inserted) at->second += 1;
        break;
      }
      case 1: {
        auto entry = map.entry(key);
        CHECK(entry.key() == key);
        CHECK_EQ(entry.is_occupied(), model.count(key) == 1);
        if (entry.is_occupied()) {
          CHECK_EQ(std::move(entry).into_occupied().remove(), model[key]);
          model.erase(key);
        } else {
          auto occupied = std::move(entry).into_vacant().insert_entry(round);
          CHECK(occupied.key() == key && occupied.get() == static_cast<std::uint64_t>(round));
          model[key] = round;
        }
        break;
      }
      case 2: {
        std::uint64_t& value = std::move(map.entry(key)).or_insert_with_key([&](const K&) { return 7; });
        value *= 3;
        model.emplace(key, 7);
        model[key] *= 3;
        break;
      }
      case 3: {
        if (auto first = map.first_entry()) {
          CHECK(first->key() == model.begin()->first);
          CHECK_EQ(first->insert(round), model.begin()->second);
          model.begin()->second = round;
        } else {
          CHECK(model.empty());
        }
        break;
      }
      default: {
        if (auto last = map.last_entry()) {
          auto [stored, value] = std::move(*last).remove_entry();
          CHECK(stored == model.rbegin()->first && value == model.rbegin()->second);
          model.erase(std::prev(model.end()));
        } else {
          CHECK(model.empty());
        }
        break;
      }
    }
    if (round % 1000 == 0) check_same(map, model);
  }
  check_same(map, model);
}

}  // namespace

TEST(entries_agree_with_std_map) {
  check_entries<std::uint64_t>(1, 3000, [](std::uint64_t key) { return key; });
  check_entries<std::string>(2, 3000, path_key);
}

TEST(an_empty_map_takes_its_first_entry) {
  TreeMap<std::string, int> map;
  CHECK(!map.first_entry().has_value());
  CHECK_EQ(std::move(map.entry("/a/b")).or_default(), 0);
  // A key that lacks what every key shares goes in at the tree's edge.
  std::move(map.entry("/a/c")).or_insert(2);
  std::move(map.entry("x")).or_insert(3);
  std::move(map.entry("")).or_insert(4);
  map.check_invariants();
  CHECK_EQ(map.size(), 4u);
  CHECK_EQ(*map.get(std::string("x")), 3);
  CHECK(map.first_entry()->key().empty());
}

TEST(extractions_agree_with_std_map) {
  shoal_test::Rng rng(3);
  for (int round = 0; round < 200; ++round) {
    TreeMap<std::uint64_t, std::uint64_t> map;
    std::map<std::uint64_t, std::uint64_t> model;
    for (int index = 0; index < 3000; ++index) {
      const std::uint64_t key = rng.below(5000);
      map.insert(key, index);
      model[key] = index;
    }
    const std::uint64_t low = rng.below(5000), high = rng.below(5000), divisor = 1 + rng.below(4);
    const std::size_t stop_after = rng.below(2) == 0 ? SIZE_MAX : rng.below(200);
    auto picked = [&](std::uint64_t key, std::uint64_t value) { return key >= low && key <= high && value % divisor == 0; };
    // Only part of the extraction runs when it stops early: entries past the
    // last one handed out stay.
    std::vector<std::pair<std::uint64_t, std::uint64_t>> expected;
    for (auto at = model.lower_bound(low); at != model.end() && at->first <= high;) {
      if (expected.size() == stop_after) break;
      if (picked(at->first, at->second)) {
        expected.push_back(*at);
        at = model.erase(at);
      } else {
        ++at;
      }
    }
    std::vector<std::pair<std::uint64_t, std::uint64_t>> extracted;
    {
      auto extraction = map.extract_if(Bound<std::uint64_t>::included(low), Bound<std::uint64_t>::included(high),
                                       [&](const std::uint64_t& key, std::uint64_t& value) { return picked(key, value); });
      while (extracted.size() != stop_after) {
        auto entry = extraction.next();
        if (!entry) break;
        extracted.push_back(*entry);
      }
    }
    CHECK(extracted == expected);
    check_same(map, model);
  }
  // The upper bound's key is copied: a temporary bound outlives nothing.
  TreeMap<std::uint64_t, std::uint64_t> map;
  for (std::uint64_t key = 0; key < 100; ++key) map.insert(key, key);
  auto extraction = map.extract_if(Bound<std::uint64_t>::unbounded(), Bound<std::uint64_t>::excluded(std::uint64_t{50}),
                                   [](const std::uint64_t& key, std::uint64_t&) { return key % 2 == 0; });
  std::size_t count = 0;
  for (auto& entry : extraction) {
    CHECK(entry.first % 2 == 0 && entry.first < 50);
    ++count;
  }
  CHECK_EQ(count, 25u);
  CHECK_EQ(map.size(), 75u);
  map.check_invariants();
}

TEST(consuming_iterators_hand_out_every_entry_once) {
  shoal_test::Rng rng(4);
  for (std::size_t count : {0u, 1u, 2u, 55u, 56u, 1000u, 30000u}) {
    for (int pattern = 0; pattern < 4; ++pattern) {
      int live = 0;
      std::vector<std::uint64_t> ids;
      {
        TreeMap<std::string, Tracked> map;
        for (std::uint64_t index = 0; index < count; ++index) {
          map.insert(path_key(index * 13), Tracked(&live, index * 13));
        }
        std::map<std::string, std::uint64_t> model;
        for (std::uint64_t index = 0; index < count; ++index) model[path_key(index * 13)] = index * 13;
        CHECK_EQ(live, static_cast<int>(count));
        auto entries = std::move(map).into_iter();
        CHECK(map.empty());
        CHECK_EQ(entries.size(), count);
        auto front = model.begin();
        auto back = model.end();
        // 0: forward; 1: backward; 2: both ends at random, with peeks;
        // 3: half, then dropped.
        const std::size_t take = pattern == 3 ? count / 2 : count;
        for (std::size_t step = 0; step < take; ++step) {
          const bool from_back = pattern == 1 || (pattern == 2 && rng.below(2) == 0);
          if (pattern == 2 && rng.below(3) == 0) {
            auto peeked = entries.peek();
            CHECK(peeked && peeked->first == front->first);
          }
          auto entry = from_back ? entries.next_back() : entries.next();
          CHECK(entry.has_value());
          const auto& expected = from_back ? *std::prev(back) : *front;
          CHECK(entry->first == expected.first && entry->second.id() == expected.second);
          if (from_back) {
            --back;
          } else {
            ++front;
          }
        }
        if (pattern != 3) {
          CHECK(!entries.next().has_value() && !entries.next_back().has_value());
          CHECK(!entries.peek().has_value());
        }
      }
      CHECK_EQ(live, 0);
    }
  }
  // A peek holds the front's next entry: with one left, the back takes that
  // one, and the front then has none.
  for (std::size_t count : {1u, 2u, 57u}) {
    int live = 0;
    {
      TreeMap<std::uint64_t, Tracked> map;
      for (std::uint64_t key = 0; key < count; ++key) map.insert(key, Tracked(&live, key));
      auto entries = std::move(map).into_iter();
      for (std::uint64_t key = 0; key + 1 < count; ++key) {
        const auto entry = entries.next();
        CHECK(entry && entry->first == key);
      }
      const auto peeked = entries.peek();
      CHECK(peeked && peeked->first == count - 1);
      const auto last = entries.next_back();
      CHECK(last && last->first == count - 1);
      CHECK(!entries.next().has_value() && !entries.peek().has_value());
    }
    CHECK_EQ(live, 0);
  }
  TreeMap<std::uint64_t, std::string> map;
  for (std::uint64_t key = 0; key < 500; ++key) map.insert(key, std::to_string(key));
  TreeMap<std::uint64_t, std::string> copy(map);
  std::uint64_t expected = 0;
  for (std::uint64_t key : std::move(map).into_keys()) CHECK_EQ(key, expected++);
  expected = 0;
  for (std::string& value : std::move(copy).into_values()) CHECK(value == std::to_string(expected++));
  CHECK_EQ(expected, 500u);
}

TEST(append_agrees_with_std_map) {
  shoal_test::Rng rng(5);
  for (int round = 0; round < 60; ++round) {
    TreeMap<std::string, std::uint64_t> ours, theirs;
    std::map<std::string, std::uint64_t> model;
    const std::size_t our_count = rng.below(4) == 0 ? 0 : rng.below(5000);
    const std::size_t their_count = rng.below(4) == 0 ? 0 : rng.below(5000);
    for (std::size_t index = 0; index < our_count; ++index) {
      const std::string key = path_key(rng.below(8000));
      ours.insert(key, index);
      model[key] = index;
    }
    for (std::size_t index = 0; index < their_count; ++index) {
      const std::string key = path_key(rng.below(8000));
      theirs.insert(key, 100000 + index);
      model[key] = 100000 + index;
    }
    // `insert` would leave the last of a key: theirs, inserted after.
    for (auto [key, value] : theirs) model[key] = value;
    ours.append(theirs);
    CHECK(theirs.empty());
    theirs.check_invariants();
    check_same(ours, model);
    ours.insert("/z", 1);
    ours.check_invariants();
  }
}

TEST(replace_key_keeps_the_value) {
  TreeMap<std::string, int> map;
  CHECK(!map.replace_key("/a", 1).has_value());
  CHECK(!map.replace_key("/b", 2).has_value());
  const std::optional<std::string> old = map.replace_key("/a", 9);
  CHECK(old.has_value() && *old == "/a");
  CHECK_EQ(*map.get(std::string("/a")), 1);
  CHECK_EQ(map.size(), 2u);
  map.check_invariants();
}

TEST_MAIN()
