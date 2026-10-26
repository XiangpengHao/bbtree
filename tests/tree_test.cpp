// The tree's inserts, lookups and removals against `std::map`, with the
// invariants checked as it goes: integers, heap strings sharing prefixes,
// byte arrays, keys without inline keys, and sets.
#include <array>
#include <cstdint>
#include <map>
#include <string>
#include <vector>

#include "shoal/allocator.hpp"
#include "shoal/detail/tree/invariants.hpp"
#include "shoal/detail/tree/remove.hpp"
#include "shoal/detail/tree/teardown.hpp"
#include "test_harness.hpp"

using namespace shoal;
using namespace shoal::detail;

namespace {

/// A tree with its count and allocator, as a map keeps them.
template <class K, class V>
class TestTree {
 public:
  using TreeType = Tree<K, V, Strategy<>>;

  TestTree() : pages_(global_) {}
  ~TestTree() { Teardown<K, V, Strategy<>>::destroy(tree_, pages_); }

  std::optional<V> insert(K key, V value) {
    return tree_.insert(std::move(key), std::move(value), pages_, length_);
  }
  std::optional<std::pair<K, V>> remove(const K& key) {
    return TreeRemoval<K, V, Strategy<>>::remove_entry(tree_, key, pages_, length_);
  }
  std::optional<std::pair<K, V>> pop_first() {
    return TreeRemoval<K, V, Strategy<>>::pop_first(tree_, pages_, length_);
  }
  std::optional<std::pair<K, V>> pop_last() {
    return TreeRemoval<K, V, Strategy<>>::pop_last(tree_, pages_, length_);
  }
  const V* get(const K& key) const {
    const auto found = tree_.find(key);
    return found ? found->second : nullptr;
  }
  std::size_t len() const { return length_; }
  void check() const { TreeChecker<K, V, Strategy<>>::check_invariants(tree_, length_); }
  const TreeType& tree() const { return tree_; }

 private:
  Global global_;
  MapPages<Global> pages_;
  TreeType tree_;
  std::size_t length_ = 0;
};

struct PlainU64 {
  std::uint64_t value;
  bool operator<(const PlainU64& other) const { return value < other.value; }
};

struct Unit {};

/// Random operations on a tree and a `std::map`, which must agree, with the
/// invariants checked every `check_every` operations.
template <class K, class V, class MakeKey, class MakeValue, class SameValue>
void differential(std::uint64_t seed, std::size_t operations, std::uint64_t key_space,
                  MakeKey make_key, MakeValue make_value, SameValue same_value,
                  std::size_t check_every) {
  shoal_test::Rng rng(seed);
  TestTree<K, V> tree;
  std::map<K, V> model;
  for (std::size_t step = 0; step < operations; ++step) {
    const std::uint64_t choice = rng.below(100);
    const K key = make_key(rng.below(key_space));
    if (choice < 55) {
      const V value = make_value(step);
      const auto old = tree.insert(key, value);
      auto at = model.find(key);
      CHECK_EQ(old.has_value(), at != model.end());
      if (at != model.end()) {
        CHECK(same_value(*old, at->second));
        at->second = value;
      } else {
        model.emplace(key, value);
      }
    } else if (choice < 85) {
      const auto removed = tree.remove(key);
      auto at = model.find(key);
      CHECK_EQ(removed.has_value(), at != model.end());
      if (at != model.end()) {
        CHECK(!(removed->first < at->first) && !(at->first < removed->first));
        CHECK(same_value(removed->second, at->second));
        model.erase(at);
      }
    } else if (choice < 90) {
      const auto popped = tree.pop_first();
      CHECK_EQ(popped.has_value(), !model.empty());
      if (popped) {
        CHECK(!(popped->first < model.begin()->first) && !(model.begin()->first < popped->first));
        model.erase(model.begin());
      }
    } else if (choice < 93) {
      const auto popped = tree.pop_last();
      CHECK_EQ(popped.has_value(), !model.empty());
      if (popped) {
        auto last = std::prev(model.end());
        CHECK(!(popped->first < last->first) && !(last->first < popped->first));
        model.erase(last);
      }
    } else {
      const V* found = tree.get(key);
      auto at = model.find(key);
      CHECK_EQ(found != nullptr, at != model.end());
      if (found != nullptr) CHECK(same_value(*found, at->second));
    }
    CHECK_EQ(tree.len(), model.size());
    if (step % check_every == 0) tree.check();
  }
  tree.check();
  for (const auto& [key, value] : model) {
    const V* found = tree.get(key);
    CHECK(found != nullptr);
    CHECK(same_value(*found, value));
  }
}

auto same = [](const auto& a, const auto& b) { return a == b; };

std::string path_key(std::uint64_t index) {
  // Keys that share long prefixes and tie in their 8-byte windows.
  return "/var/lib/data/table_" + std::to_string(index % 7) + "/part-" + std::to_string(index);
}

}  // namespace

TEST(u64_keys_agree_with_std_map) {
  differential<std::uint64_t, std::uint64_t>(
      1, 200000, 20000, [](std::uint64_t key) { return key * 7919; },
      [](std::size_t step) { return std::uint64_t(step); }, same, 997);
}

TEST(dense_u64_keys_agree_with_std_map) {
  differential<std::uint64_t, std::uint64_t>(
      2, 200000, 3000, [](std::uint64_t key) { return key; },
      [](std::size_t step) { return std::uint64_t(step); }, same, 997);
}

TEST(signed_keys_agree_with_std_map) {
  differential<std::int64_t, std::uint64_t>(
      3, 100000, 5000, [](std::uint64_t key) { return std::int64_t(key) - 2500; },
      [](std::size_t step) { return std::uint64_t(step); }, same, 997);
}

TEST(path_strings_agree_with_std_map) {
  differential<std::string, std::uint64_t>(
      4, 100000, 6000, path_key, [](std::size_t step) { return std::uint64_t(step); }, same, 499);
}

TEST(short_strings_agree_with_std_map) {
  differential<std::string, std::string>(
      5, 100000, 4000, [](std::uint64_t key) { return std::to_string(key * 31); },
      [](std::size_t step) { return "value" + std::to_string(step); }, same, 499);
}

TEST(byte_arrays_agree_with_std_map) {
  using Key = std::array<std::uint8_t, 16>;
  differential<Key, std::uint64_t>(
      6, 100000, 5000,
      [](std::uint64_t key) {
        Key bytes{};
        for (int index = 0; index < 8; ++index) bytes[15 - index] = std::uint8_t(key >> (8 * index));
        bytes[0] = 0x42;
        return bytes;
      },
      [](std::size_t step) { return std::uint64_t(step); }, same, 499);
}

TEST(keys_without_inline_keys_agree_with_std_map) {
  differential<PlainU64, std::uint64_t>(
      7, 100000, 5000, [](std::uint64_t key) { return PlainU64{key * 13}; },
      [](std::size_t step) { return std::uint64_t(step); }, same, 997);
}

TEST(sets_agree_with_std_map) {
  differential<std::uint64_t, Unit>(
      8, 100000, 10000, [](std::uint64_t key) { return key; }, [](std::size_t) { return Unit{}; },
      [](const Unit&, const Unit&) { return true; }, 997);
  differential<std::string, Unit>(
      9, 50000, 4000, path_key, [](std::size_t) { return Unit{}; },
      [](const Unit&, const Unit&) { return true; }, 499);
}

TEST(ascending_inserts_append_to_the_last_leaf) {
  TestTree<std::uint64_t, std::uint64_t> tree;
  for (std::uint64_t key = 0; key < 100000; ++key) CHECK(!tree.insert(key, key).has_value());
  CHECK(tree.tree().remembers_rightmost_leaf());
  tree.check();
  for (std::uint64_t key = 0; key < 100000; ++key) CHECK_EQ(*tree.get(key), key);
  CHECK(tree.get(100000) == nullptr);
  // Removing everything empties the tree back to its root leaf.
  for (std::uint64_t key = 0; key < 100000; ++key) CHECK(tree.remove(key).has_value());
  CHECK_EQ(tree.len(), 0u);
  tree.check();
}

TEST_MAIN()
