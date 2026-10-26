// A shared map's leaf annexes (`tree/node/annexed_leaf.rs`'s tests): an
// annex that holds a heap page for each entry its leaf took, as a store's
// chain holds its records' pages. A split hands the new leaf half of them, a
// merge takes the other leaf's, and the map's teardown gives the rest back:
// every block the counting allocator gave out comes back.
#include <atomic>
#include <cstdint>
#include <map>
#include <thread>
#include <type_traits>
#include <vector>

#include "shoal/map.hpp"
#include "test_harness.hpp"

using shoal::AnnexedLeaf;
using shoal::TreeMap;
using shoal::Layout;
using shoal::LeafOutcome;

namespace {

/// The global allocator, counting the blocks out; copies count together.
class CountingAlloc {
 public:
  explicit CountingAlloc(std::atomic<long>* live) noexcept : live_(live) {}
  void* allocate_zeroed(Layout layout) noexcept {
    void* block = shoal::Global().allocate_zeroed(layout);
    if (block != nullptr) live_->fetch_add(1, std::memory_order_relaxed);
    return block;
  }
  void deallocate(void* block, Layout layout) noexcept {
    live_->fetch_sub(1, std::memory_order_relaxed);
    shoal::Global().deallocate(block, layout);
  }

 private:
  std::atomic<long>* live_;
};

constexpr Layout page_layout{64, 8};

/// A heap page for each entry the leaf took.
struct PagePerEntry {
  std::vector<void*> pages;

  void split(PagePerEntry& right) noexcept {
    const std::size_t half = pages.size() / 2;
    right.pages.assign(pages.begin() + static_cast<std::ptrdiff_t>(half), pages.end());
    pages.resize(half);
  }
  void merge(PagePerEntry&& right) noexcept {
    pages.insert(pages.end(), right.pages.begin(), right.pages.end());
    right.pages.clear();
  }
  template <class A>
  void release(A& alloc) noexcept {
    for (void* page : pages) alloc.deallocate(page, page_layout);
    pages.clear();
  }
};

}  // namespace

// A vector points to no part of itself: the annex moves with its leaf's bytes.
template <>
struct shoal::is_trivially_relocatable<PagePerEntry> : std::true_type {};

namespace {

using AnnexedMap = TreeMap<std::uint64_t, std::uint64_t, shoal::DefaultNodeConfig, CountingAlloc, PagePerEntry>;
using Leaf = AnnexedMap::annexed_leaf;

bool keys_sorted_and(const Leaf& leaf, std::uint64_t key, bool present) {
  bool sorted = true, found = false, first = true;
  std::uint64_t previous = 0;
  leaf.for_each_key([&](const std::uint64_t& stored) {
    if (!first && stored <= previous) sorted = false;
    first = false;
    previous = stored;
    found |= stored == key;
  });
  return sorted && found == present;
}

/// Inserts `key`, taking a heap page into the annex of the leaf it goes in;
/// whether it was new.
bool insert(const AnnexedMap& map, CountingAlloc& alloc, std::uint64_t key, std::uint64_t value) {
  auto outcome = map.update_or_insert_with_leaf(
      key, value,
      [&](Leaf leaf, const std::uint64_t& stored, std::uint64_t& stored_value) {
        CHECK(keys_sorted_and(leaf, stored, true));
        stored_value = value;
        return LeafOutcome<bool>::done(false);
      },
      [&](Leaf leaf, const std::uint64_t& new_key, const std::uint64_t&) {
        CHECK(keys_sorted_and(leaf, new_key, false));
        leaf.annex().pages.push_back(alloc.allocate_zeroed(page_layout));
        return LeafOutcome<bool>::done(true);
      });
  CHECK(outcome.ok());
  return outcome.value();
}

/// Removes `key`, giving a heap page of its leaf's annex back, if it holds
/// one: the map's teardown gives the rest back.
std::optional<std::uint64_t> remove(const AnnexedMap& map, CountingAlloc& alloc, std::uint64_t key) {
  auto removed = map.remove_with_leaf(key, [&](Leaf leaf, const std::uint64_t& stored, const std::uint64_t&) {
    CHECK(keys_sorted_and(leaf, stored, true));
    auto& pages = leaf.annex().pages;
    if (!pages.empty()) {
      alloc.deallocate(pages.back(), page_layout);
      pages.pop_back();
    }
  });
  if (!removed) return std::nullopt;
  return removed->second;
}

}  // namespace

TEST(annexes_follow_their_leaves_through_splits_merges_and_teardown) {
  std::atomic<long> live{0};
  {
    CountingAlloc alloc(&live);
    AnnexedMap map(alloc);
    shoal_test::Rng rng(7);
    std::map<std::uint64_t, std::uint64_t> model;
    for (std::uint64_t step = 0; step < 60000; ++step) {
      const std::uint64_t key = rng.below(4000);
      if (rng.below(3) < 2) {
        CHECK_EQ(insert(map, alloc, key, step), model.find(key) == model.end());
        model[key] = step;
      } else {
        const auto removed = remove(map, alloc, key);
        const auto at = model.find(key);
        CHECK_EQ(removed.has_value(), at != model.end());
        if (removed) {
          CHECK_EQ(*removed, at->second);
          model.erase(at);
        }
      }
      if (step % 5000 == 0) map.check_invariants(model.size());
    }
    map.check_invariants(model.size());
    for (const auto& [key, value] : model) CHECK(map.get_copied(key) == std::optional<std::uint64_t>(value));
  }
  CHECK_EQ(live.load(), 0);
}

TEST(a_refusing_closure_leaves_the_map_as_it_was) {
  std::atomic<long> live{0};
  {
    CountingAlloc alloc(&live);
    AnnexedMap map(alloc);
    for (std::uint64_t key = 0; key < 5000; ++key) insert(map, alloc, key * 2, key);
    const long before = live.load();
    // Refused where the leaf has room, and where it would split.
    for (std::uint64_t key = 1; key < 10000; key += 2) {
      auto outcome = map.update_or_insert_with_leaf(
          key, 99, [](Leaf, const std::uint64_t&, std::uint64_t&) { return LeafOutcome<bool>::done(false); },
          [](Leaf, const std::uint64_t&, const std::uint64_t&) { return LeafOutcome<bool>::refused(page_layout); });
      CHECK(!outcome.ok());
      CHECK_EQ(outcome.error().key(), key);
      CHECK_EQ(outcome.error().value(), 99u);
      CHECK(outcome.error().layout() == page_layout);
    }
    CHECK_EQ(live.load(), before);
    map.check_invariants(5000);
    CHECK(!map.contains_key(std::uint64_t{1}));
  }
  CHECK_EQ(live.load(), 0);
}

TEST(threads_keep_annexes_whole) {
  std::atomic<long> live{0};
  {
    CountingAlloc alloc(&live);
    AnnexedMap map(alloc);
    constexpr std::size_t threads = 4;
    std::vector<std::thread> workers;
    for (std::size_t thread = 0; thread < threads; ++thread) {
      workers.emplace_back([&, thread] {
        CountingAlloc own(alloc);
        shoal_test::Rng rng(100 + thread);
        for (int step = 0; step < 20000; ++step) {
          // Keys interleave between threads, so they share leaves.
          const std::uint64_t key = rng.below(3000) * threads + thread;
          if (rng.below(3) < 2) {
            insert(map, own, key, step);
          } else {
            remove(map, own, key);
          }
        }
      });
    }
    for (auto& worker : workers) worker.join();
    std::size_t count = 0;
    map.scan(shoal::Bound<std::uint64_t>::unbounded(), shoal::Bound<std::uint64_t>::unbounded(),
             [&](const std::uint64_t&, const std::uint64_t&) {
               ++count;
               return true;
             });
    map.check_invariants(count);
  }
  CHECK_EQ(live.load(), 0);
}

TEST_MAIN()
