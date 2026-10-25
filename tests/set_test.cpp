// `bbtree::unsync::BBTreeSet` against `std::set`: single elements, batches,
// ranges, extraction, and every operation on two sets, through `next` and
// through `for_each`, for pairs of sets that reach each strategy.
#include <algorithm>
#include <cstdint>
#include <iterator>
#include <memory>
#include <set>
#include <string>
#include <vector>

#include "bbtree/unsync/set.hpp"
#include "test_harness.hpp"

using bbtree::Bound;
using bbtree::unsync::BBTreeSet;

namespace {

template <class K>
void check_same(const BBTreeSet<K>& set, const std::set<K>& model) {
  set.check_invariants();
  CHECK_EQ(set.size(), model.size());
  auto at = model.begin();
  for (const K& element : set) {
    CHECK(at != model.end() && element == *at);
    ++at;
  }
  CHECK(at == model.end());
}

template <class K>
BBTreeSet<K> set_of(const std::set<K>& elements) {
  return BBTreeSet<K>::from_sorted(elements.begin(), elements.end());
}

/// An operation's elements, by `next` and by `for_each`.
template <class Operation, class K>
void check_operation(Operation make, const std::vector<K>& expected) {
  {
    auto elements = make();
    std::vector<K> stepped;
    while (const K* element = elements.next()) stepped.push_back(*element);
    CHECK(stepped == expected);
  }
  {
    auto elements = make();
    std::vector<K> folded;
    elements.for_each([&](const K& element) { folded.push_back(element); });
    CHECK(folded == expected);
  }
  {
    auto elements = make();
    std::vector<K> ranged;
    for (const K& element : elements) ranged.push_back(element);
    CHECK(ranged == expected);
  }
}

template <class K>
void check_algebra(const std::set<K>& a_model, const std::set<K>& b_model) {
  const BBTreeSet<K> a = set_of(a_model), b = set_of(b_model);
  std::vector<K> expected;
  std::set_difference(a_model.begin(), a_model.end(), b_model.begin(), b_model.end(), std::back_inserter(expected));
  check_operation([&] { return a.difference(b); }, expected);
  check_same(a - b, std::set<K>(expected.begin(), expected.end()));
  expected.clear();
  std::set_intersection(a_model.begin(), a_model.end(), b_model.begin(), b_model.end(), std::back_inserter(expected));
  check_operation([&] { return a.intersection(b); }, expected);
  check_same(a & b, std::set<K>(expected.begin(), expected.end()));
  CHECK_EQ(a.is_disjoint(b), expected.empty());
  expected.clear();
  std::set_union(a_model.begin(), a_model.end(), b_model.begin(), b_model.end(), std::back_inserter(expected));
  check_operation([&] { return a.union_with(b); }, expected);
  check_same(a | b, std::set<K>(expected.begin(), expected.end()));
  expected.clear();
  std::set_symmetric_difference(a_model.begin(), a_model.end(), b_model.begin(), b_model.end(),
                                std::back_inserter(expected));
  check_operation([&] { return a.symmetric_difference(b); }, expected);
  check_same(a ^ b, std::set<K>(expected.begin(), expected.end()));
  CHECK_EQ(a.is_subset(b), std::includes(b_model.begin(), b_model.end(), a_model.begin(), a_model.end()));
  CHECK_EQ(a.is_superset(b), std::includes(a_model.begin(), a_model.end(), b_model.begin(), b_model.end()));
}

std::set<std::uint64_t> random_set(bbtree_test::Rng& rng, std::size_t count, std::uint64_t from, std::uint64_t to) {
  std::set<std::uint64_t> elements;
  while (elements.size() < count && elements.size() < to - from) elements.insert(from + rng.below(to - from));
  return elements;
}

}  // namespace

TEST(single_elements_agree_with_std_set) {
  bbtree_test::Rng rng(1);
  BBTreeSet<std::string> set;
  std::set<std::string> model;
  for (int round = 0; round < 30000; ++round) {
    const std::string element = "/s/" + std::to_string(rng.below(4000));
    switch (rng.below(6)) {
      case 0:
      case 1:
        CHECK_EQ(set.insert(element), model.insert(element).second);
        break;
      case 2: {
        const auto replaced = set.replace(element);
        CHECK_EQ(replaced.has_value(), model.count(element) == 1);
        model.insert(element);
        break;
      }
      case 3:
        CHECK_EQ(set.remove(element), model.erase(element) == 1);
        break;
      case 4: {
        const auto taken = set.take(element);
        CHECK_EQ(taken.has_value(), model.erase(element) == 1);
        break;
      }
      default:
        if (rng.below(2) == 0) {
          const auto first = set.pop_first();
          CHECK_EQ(first.has_value(), !model.empty());
          if (first) {
            CHECK(*first == *model.begin());
            model.erase(model.begin());
          }
        } else {
          const auto last = set.pop_last();
          CHECK_EQ(last.has_value(), !model.empty());
          if (last) {
            CHECK(*last == *model.rbegin());
            model.erase(std::prev(model.end()));
          }
        }
        break;
    }
    CHECK_EQ(set.contains(element), model.count(element) == 1);
    if (round % 2000 == 0) {
      check_same(set, model);
      CHECK((set.first() == nullptr) == model.empty());
      if (!model.empty()) CHECK(*set.first() == *model.begin() && *set.last() == *model.rbegin());
    }
  }
  check_same(set, model);
}

TEST(batches_ranges_and_extractions_agree_with_std_set) {
  bbtree_test::Rng rng(2);
  BBTreeSet<std::uint64_t> set, batched;
  std::set<std::uint64_t> model;
  std::vector<std::uint64_t> elements;
  for (int index = 0; index < 120000; ++index) elements.push_back(rng.below(300000));
  std::vector<std::uint64_t> copy = elements;
  std::unique_ptr<bool[]> inserted(new bool[elements.size()]);
  for (std::uint64_t element : elements) {
    set.insert(element);
    model.insert(element);
  }
  batched.insert_many(copy.data(), copy.size(), inserted.get());
  batched.check_same_nodes(set);
  std::vector<std::uint64_t> probes;
  for (int index = 0; index < 5000; ++index) probes.push_back(rng.below(300000));
  std::unique_ptr<bool[]> found(new bool[probes.size()]);
  set.contains_many(probes.data(), probes.size(), found.get());
  for (std::size_t index = 0; index < probes.size(); ++index) CHECK_EQ(found[index], model.count(probes[index]) == 1);
  for (int round = 0; round < 300; ++round) {
    const std::uint64_t low = rng.below(300000), high = rng.below(300000);
    std::vector<std::uint64_t> expected;
    if (low <= high) {
      for (auto at = model.lower_bound(low); at != model.end() && *at < high; ++at) expected.push_back(*at);
    }
    std::vector<std::uint64_t> got;
    for (const std::uint64_t& element : set.range(Bound<std::uint64_t>::included(low), Bound<std::uint64_t>::excluded(high))) {
      got.push_back(element);
    }
    CHECK(got == expected);
    got.clear();
    auto backward = set.range_rev(Bound<std::uint64_t>::included(low), Bound<std::uint64_t>::excluded(high));
    while (const std::uint64_t* element = backward.next()) got.push_back(*element);
    std::reverse(got.begin(), got.end());
    CHECK(got == expected);
  }
  auto picked = [](const std::uint64_t& element) { return element % 7 == 0; };
  std::size_t extracted = 0;
  for (std::uint64_t& element : set.extract_if(Bound<std::uint64_t>::excluded(std::uint64_t{1000}),
                                               Bound<std::uint64_t>::included(std::uint64_t{200000}), picked)) {
    CHECK(element > 1000 && element <= 200000 && picked(element));
    ++extracted;
  }
  for (auto at = model.begin(); at != model.end();) {
    at = *at > 1000 && *at <= 200000 && picked(*at) ? model.erase(at) : std::next(at);
  }
  CHECK(extracted > 0);
  check_same(set, model);
  set.retain([](const std::uint64_t& element) { return element % 3 != 0; });
  for (auto at = model.begin(); at != model.end();) at = *at % 3 == 0 ? model.erase(at) : std::next(at);
  check_same(set, model);
  BBTreeSet<std::uint64_t> upper = set.split_off(std::uint64_t{150000});
  std::set<std::uint64_t> upper_model(model.lower_bound(150000), model.end());
  model.erase(model.lower_bound(150000), model.end());
  check_same(set, model);
  check_same(upper, upper_model);
  set.append(upper);
  model.insert(upper_model.begin(), upper_model.end());
  CHECK(upper.empty());
  check_same(set, model);
}

TEST(operations_agree_with_std_algorithms) {
  bbtree_test::Rng rng(3);
  // Similar sizes, overlapping: both walked side by side.
  for (int round = 0; round < 40; ++round) {
    check_algebra(random_set(rng, rng.below(3000), 0, 6000), random_set(rng, rng.below(3000), 0, 6000));
  }
  // Ranges apart, and touching at either end.
  check_algebra(random_set(rng, 500, 0, 1000), random_set(rng, 500, 2000, 3000));
  check_algebra(random_set(rng, 500, 2000, 3000), random_set(rng, 500, 0, 1000));
  {
    auto a = random_set(rng, 300, 0, 1000), b = random_set(rng, 300, 1001, 2000);
    a.insert(1000);
    b.insert(1000);
    check_algebra(a, b);
    check_algebra(b, a);
  }
  // Empty sides, and single elements.
  check_algebra(std::set<std::uint64_t>{}, random_set(rng, 100, 0, 1000));
  check_algebra(random_set(rng, 100, 0, 1000), std::set<std::uint64_t>{});
  check_algebra(std::set<std::uint64_t>{5}, std::set<std::uint64_t>{5});
  check_algebra(std::set<std::uint64_t>{5}, std::set<std::uint64_t>{1, 5, 9});
  // One set 16 times the other or more: lookups, batched once the large
  // set is past the batch cutoff.
  const std::set<std::uint64_t> large = random_set(rng, 200000, 0, 1000000);
  for (std::size_t small_count : {1u, 10u, 31u, 32u, 33u, 1000u, 12000u}) {
    std::set<std::uint64_t> small = random_set(rng, small_count, 1, 999999);
    // Half of it from the large set, so that lookups find some.
    std::size_t taken = 0;
    for (std::uint64_t element : large) {
      if (taken++ % 400 == 0 && small.size() < small_count * 2) small.insert(element);
    }
    check_algebra(small, large);
    check_algebra(large, small);
    // A subset that is one, through lookups.
    std::set<std::uint64_t> inside;
    std::size_t index = 0;
    for (std::uint64_t element : large) {
      if (index++ % 97 == 0) inside.insert(element);
    }
    CHECK(set_of(inside).is_subset(set_of(large)));
  }
  // Strings: byte strings compared by their bytes.
  std::set<std::string> words_a, words_b;
  for (int index = 0; index < 3000; ++index) {
    words_a.insert("/w/" + std::to_string(rng.below(5000)));
    words_b.insert("/w/" + std::to_string(rng.below(5000)));
  }
  check_algebra(words_a, words_b);
}

TEST_MAIN()
