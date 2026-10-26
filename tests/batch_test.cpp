// Batches against single operations: `get_many` gives what separate `get`s
// give, and `insert_many` makes the same tree, node by node, as separate
// `insert`s, on trees past the batch cutoff, for integers and heap strings.
#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "shoal/unsync/map.hpp"
#include "test_harness.hpp"

using shoal::unsync::TreeMap;

namespace {

template <class K, class MakeKey>
void check_batches(std::uint64_t seed, std::size_t entries, std::uint64_t key_space, MakeKey make_key) {
  shoal_test::Rng rng(seed);
  TreeMap<K, std::uint64_t> single, batched;
  // Build both, one by single inserts, the other by batches of every size.
  std::size_t inserted = 0;
  while (inserted < entries) {
    const std::size_t count = 1 + rng.below(70);
    std::vector<K> keys, copies;
    std::vector<std::uint64_t> values, value_copies;
    for (std::size_t index = 0; index < count; ++index) {
      // Repeats within a batch, and an ascending run now and then.
      const K key = rng.below(10) == 0 ? make_key(key_space + inserted + index) : make_key(rng.below(key_space));
      keys.push_back(key);
      copies.push_back(key);
      values.push_back(inserted + index);
      value_copies.push_back(inserted + index);
    }
    std::vector<std::optional<std::uint64_t>> previous(count);
    batched.insert_many(keys.data(), values.data(), count, previous.data());
    for (std::size_t index = 0; index < count; ++index) {
      const auto expected = single.insert(std::move(copies[index]), value_copies[index]);
      CHECK_EQ(previous[index].has_value(), expected.has_value());
      if (expected) CHECK_EQ(*previous[index], *expected);
    }
    inserted += count;
  }
  single.check_invariants();
  batched.check_invariants();
  CHECK_EQ(batched.size(), single.size());
  batched.check_same_nodes(single);
  // The same entries, in the same nodes: compared entry by entry here, the
  // nodes by the invariant checks and sizes.
  auto expected = single.iter();
  for (auto [key, value] : batched) {
    const auto entry = expected.next();
    CHECK(entry.has_value());
    CHECK(!(entry->first < key) && !(key < entry->first));
    CHECK_EQ(entry->second, value);
  }
  // Lookups, hits and misses mixed.
  for (int round = 0; round < 200; ++round) {
    const std::size_t count = 1 + rng.below(100);
    std::vector<K> keys;
    for (std::size_t index = 0; index < count; ++index) keys.push_back(make_key(rng.below(2 * key_space)));
    std::vector<const std::uint64_t*> found(count);
    batched.get_many(keys.data(), count, found.data());
    for (std::size_t index = 0; index < count; ++index) {
      const std::uint64_t* expected_value = single.get(keys[index]);
      CHECK_EQ(found[index] != nullptr, expected_value != nullptr);
      if (expected_value != nullptr) CHECK_EQ(*found[index], *expected_value);
    }
  }
}

}  // namespace

TEST(u64_batches_agree_with_single_operations) {
  check_batches<std::uint64_t>(1, 120000, 400000, [](std::uint64_t key) { return key * 2654435761u; });
}

TEST(string_batches_agree_with_single_operations) {
  check_batches<std::string>(2, 60000, 200000, [](std::uint64_t key) {
    return "/srv/data/" + std::to_string(key % 11) + "/" + std::to_string(key);
  });
}

TEST_MAIN()
