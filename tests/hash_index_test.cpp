#include "shoal/unsync/hash_index.hpp"

#include <algorithm>
#include <atomic>
#include <limits>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

#include "test_harness.hpp"

namespace {
using Index = shoal::unsync::HashIndex<std::uint64_t>;
struct Rows {
  std::vector<unsigned> keys;
  std::vector<unsigned> stored;
  std::size_t rounds = 0;
  std::uint64_t hash = 0;
  auto hashes() {
    return [this](std::uint32_t) { return hash; };
  }
  auto cached() {
    return [this](std::uint64_t) { return hash; };
  }
  auto compare() {
    return [this](std::uint32_t *rows, std::size_t count, std::uint64_t *results,
                  std::uint32_t *rejected) {
      std::reverse(rows, rows + count);
      std::size_t n = 0;
      for (std::size_t i = 0; i < count; ++i)
        if (keys[rows[i]] != stored[results[rows[i]] - 1]) rejected[n++] = rows[i];
      return n;
    };
  }
  auto create() {
    return [this](const std::uint32_t *rows, std::size_t count, std::uint64_t *handles) {
      ++rounds;
      for (std::size_t i = 0; i < count; ++i) {
        stored.push_back(keys[rows[i]]);
        handles[i] = stored.size();
      }
    };
  }
};
}  // namespace
TEST(external_duplicates_collisions_and_tiles) {
  for (auto hash : {std::uint64_t{0}, ~std::uint64_t{0}}) {
    Rows rows;
    rows.hash = hash;
    for (unsigned i = 0; i < 4100; ++i) rows.keys.push_back(i % 37);
    std::vector<std::uint64_t> results(rows.keys.size()), positions(rows.keys.size());
    Index index;
    Index::ProbeState scratch;
    index.find_or_create_many(scratch, rows.keys.size(), rows.hashes(), rows.cached(),
                              rows.compare(), rows.create(), results.data(), positions.data());
    CHECK_EQ(index.size(), 37u);
    CHECK_EQ(rows.stored.size(), 37u);
    for (std::size_t i = 0; i < results.size(); ++i) {
      CHECK_EQ(rows.stored[results[i] - 1], rows.keys[i]);
      CHECK_EQ(index.handle_at(positions[i]), results[i]);
    }
    index.check_invariants(rows.cached());
    index.find_many(scratch, rows.keys.size(), rows.hashes(), rows.compare(), results.data());
    for (std::size_t i = 0; i < results.size(); ++i)
      CHECK_EQ(rows.stored[results[i] - 1], rows.keys[i]);
    index.clear_retain_capacity();
    CHECK_EQ(index.size(), 0u);
    CHECK_EQ(rows.stored.size(), 37u);
  }
}
// All keys share a home, five fingerprints between them, and come new in one
// batch: a row's empty slot is taken by an earlier row's claim of the same
// round, of its fingerprint (a candidate) or another, past which it probes on
// instead of reaching `compare`.
TEST(claims_of_one_round_send_later_rows_on) {
  std::vector<unsigned> keys, stored;
  for (unsigned i = 0; i < 300; ++i) keys.push_back(i % 23);
  const auto hash_of_key = [](unsigned key) { return (std::uint64_t{key % 5 + 1} << 48) | 3; };
  const auto hash_at = [&](std::uint32_t row) { return hash_of_key(keys[row]); };
  const auto stored_hash = [&](std::uint64_t handle) { return hash_of_key(stored[handle - 1]); };
  const auto compare = [&](std::uint32_t *rows, std::size_t count, std::uint64_t *results,
                           std::uint32_t *rejected) {
    std::size_t n = 0;
    for (std::size_t i = 0; i < count; ++i) {
      CHECK_EQ(hash_of_key(stored[results[rows[i]] - 1]) >> 48, hash_at(rows[i]) >> 48);
      if (keys[rows[i]] != stored[results[rows[i]] - 1]) rejected[n++] = rows[i];
    }
    return n;
  };
  const auto create = [&](const std::uint32_t *rows, std::size_t count, std::uint64_t *handles) {
    for (std::size_t i = 0; i < count; ++i) {
      stored.push_back(keys[rows[i]]);
      handles[i] = stored.size();
    }
  };
  std::vector<std::uint64_t> results(keys.size());
  Index index;
  Index::ProbeState scratch;
  index.find_or_create_many(scratch, keys.size(), hash_at, stored_hash, compare, create, results.data());
  CHECK_EQ(index.size(), 23u);
  CHECK_EQ(stored.size(), 23u);
  for (std::size_t i = 0; i < keys.size(); ++i) CHECK_EQ(stored[results[i] - 1], keys[i]);
  index.check_invariants(stored_hash);
}

TEST(claim_rollback_and_empty_lookup) {
  Index index;
  Index::ProbeState scratch;
  Rows rows{{1, 2, 3}, {}, 0, 0};
  std::uint64_t results[3], positions[3];
  index.find_many(scratch, 3, rows.hashes(), rows.compare(), results, positions);
  for (unsigned i = 0; i < 3; ++i) {
    CHECK_EQ(results[i], 0u);
    CHECK_EQ(positions[i], std::numeric_limits<std::uint64_t>::max());
  }
  for (auto invalid : {std::uint64_t{0}, (std::uint64_t{1} << 48) - 1, std::uint64_t{1} << 48}) {
    bool threw = false;
    try {
      index.find_or_create_many(
          scratch, 3, rows.hashes(), rows.cached(), rows.compare(),
          [invalid](const std::uint32_t *, std::size_t n, std::uint64_t *out) {
            for (std::size_t i = 0; i < n; ++i) out[i] = invalid;
          },
          results);
    } catch (const std::invalid_argument &) {
      threw = true;
    }
    CHECK(threw);
    CHECK_EQ(index.size(), 0u);
    index.check_invariants(rows.cached());
  }
  bool threw = false;
  try {
    index.find_or_create_many(
        scratch, 3, rows.hashes(), rows.cached(), rows.compare(),
        [](const std::uint32_t *, std::size_t, std::uint64_t *) {
          throw std::runtime_error("create");
        },
        results);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  CHECK_EQ(index.size(), 0u);
  index.find_or_create_many(scratch, 3, rows.hashes(), rows.cached(), rows.compare(), rows.create(),
                            results);
  CHECK_EQ(index.size(), 3u);
  index.check_invariants(rows.cached());
}

TEST(global_positions_survive_tiles_and_cached_hash_rebuild) {
  Index index;
  Index::ProbeState scratch;
  const std::size_t count = 4500;
  std::vector<std::uint64_t> results(count), positions(count);
  std::size_t hash_calls = 0, stored_calls = 0, create_calls = 0;
  auto hash = [&](std::uint32_t row) {
    ++hash_calls;
    return std::uint64_t(row) * 0x9e3779b97f4a7c15ull;
  };
  auto cached = [&](std::uint64_t handle) {
    ++stored_calls;
    return (handle - 1) * 0x9e3779b97f4a7c15ull;
  };
  auto compare = [](std::uint32_t *rows, std::size_t n, std::uint64_t *out,
                    std::uint32_t *rejected) {
    std::size_t reject_count = 0;
    for (std::size_t i = 0; i < n; ++i)
      if (out[rows[i]] != rows[i] + 1) rejected[reject_count++] = rows[i];
    return reject_count;
  };
  index.find_or_create_many(
      scratch, count, hash, cached, compare,
      [&](const std::uint32_t *rows, std::size_t n, std::uint64_t *out) {
        ++create_calls;
        CHECK(n <= Index::maximum_batch_size());
        for (std::size_t i = 0; i < n; ++i) out[i] = rows[i] + 1;
      },
      results.data(), positions.data());
  CHECK_EQ(hash_calls, count);
  CHECK_EQ(stored_calls, 0u);
  CHECK(create_calls >= 3u);
  for (std::size_t i = 0; i < count; ++i) {
    CHECK_EQ(results[i], i + 1);
    CHECK_EQ(index.handle_at(positions[i]), results[i]);
  }
  const auto old_capacity = index.capacity();
  bool threw = false;
  try {
    index.reserve(old_capacity * 2,
                  [](std::uint64_t) -> std::uint64_t { throw std::runtime_error("cached"); });
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  CHECK_EQ(index.capacity(), old_capacity);
  CHECK_EQ(index.size(), count);
  index.reserve(old_capacity * 2, cached);
  CHECK_EQ(stored_calls, count);
  index.check_invariants(cached);
  Index moved(std::move(index));
  CHECK_EQ(index.size(), 0u);
  CHECK_EQ(moved.size(), count);
  index = std::move(moved);
  index.find_many(scratch, count, hash, compare, results.data());
  for (std::size_t i = 0; i < count; ++i) CHECK_EQ(results[i], i + 1);
}
TEST(pointer_handles_and_compare_exception) {
  shoal::unsync::HashIndex<void *> index;
  decltype(index)::ProbeState scratch;
  int objects[3] = {};
  void *results[3];
  auto hash = [](std::uint32_t) { return std::uint64_t{0}; };
  auto stored_hash = [](void *) { return std::uint64_t{0}; };
  bool threw = false;
  try {
    index.find_or_create_many(
        scratch, 3, hash, stored_hash,
        [](std::uint32_t *, std::size_t, void **, std::uint32_t *) -> std::size_t {
          throw std::runtime_error("compare");
        },
        [&](const std::uint32_t *rows, std::size_t n, void **out) {
          for (std::size_t i = 0; i < n; ++i) out[i] = &objects[rows[i]];
        },
        results);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  CHECK_EQ(index.size(), 1u);
  index.check_invariants(stored_hash);
  index.find_or_create_many(
      scratch, 3, hash, stored_hash,
      [&](std::uint32_t *rows, std::size_t n, void **out, std::uint32_t *rejected) {
        std::size_t k = 0;
        for (std::size_t i = 0; i < n; ++i)
          if (out[rows[i]] != &objects[rows[i]]) rejected[k++] = rows[i];
        return k;
      },
      [&](const std::uint32_t *rows, std::size_t n, void **out) {
        for (std::size_t i = 0; i < n; ++i) out[i] = &objects[rows[i]];
      },
      results);
  CHECK_EQ(index.size(), 3u);
  for (unsigned i = 0; i < 3; ++i) CHECK(results[i] == &objects[i]);
  index.find_many(
      scratch, 0, hash,
      [](std::uint32_t *, std::size_t, void **, std::uint32_t *) { return std::size_t{0}; },
      nullptr);
}

namespace {
struct AllocationState {
  std::size_t bytes = 0;
  std::size_t allocations = 0;
  bool fail = false;
};
class IndexAllocator {
 public:
  explicit IndexAllocator(AllocationState *state = nullptr) : state_(state) {}
  void *allocate_zeroed(shoal::Layout layout) noexcept {
    ++state_->allocations;
    if (state_->fail) return nullptr;
    auto memory = shoal::Global{}.allocate_zeroed(layout);
    if (memory) state_->bytes += layout.size;
    return memory;
  }
  void deallocate(void *memory, shoal::Layout layout) noexcept {
    state_->bytes -= layout.size;
    shoal::Global{}.deallocate(memory, layout);
  }

 private:
  AllocationState *state_;
};
}  // namespace
TEST(allocation_refusal_preserves_index_and_reset_reuses_storage) {
  AllocationState allocation;
  {
    shoal::unsync::HashIndex<std::uint64_t, IndexAllocator> index{IndexAllocator(&allocation)};
    decltype(index)::ProbeState scratch;
    Rows rows{{1, 2, 3}, {}, 0, 0};
    std::uint64_t results[3];
    allocation.fail = true;
    bool threw = false;
    try {
      index.find_or_create_many(scratch, 3, rows.hashes(), rows.cached(), rows.compare(),
                                rows.create(), results);
    } catch (const std::bad_alloc &) {
      threw = true;
    }
    CHECK(threw);
    CHECK_EQ(allocation.bytes, 0u);
    CHECK_EQ(rows.stored.size(), 0u);
    allocation.fail = false;
    index.find_or_create_many(scratch, 3, rows.hashes(), rows.cached(), rows.compare(),
                              rows.create(), results);
    const auto bytes = allocation.bytes;
    allocation.fail = true;
    threw = false;
    try {
      index.reserve(1000, rows.cached());
    } catch (const std::bad_alloc &) {
      threw = true;
    }
    CHECK(threw);
    CHECK_EQ(allocation.bytes, bytes);
    CHECK_EQ(index.size(), 3u);
    index.check_invariants(rows.cached());
    const auto allocations = allocation.allocations;
    index.clear_retain_capacity();
    index.find_or_create_many(scratch, 3, rows.hashes(), rows.cached(), rows.compare(),
                              rows.create(), results);
    CHECK_EQ(allocation.allocations, allocations);
    CHECK_EQ(rows.stored.size(), 6u);
    index.clear();
    CHECK_EQ(allocation.bytes, 0u);
    CHECK_EQ(rows.stored.size(), 6u);
  }
  CHECK_EQ(allocation.bytes, 0u);
}
TEST(external_nullable_composite_strings_use_compare_policy) {
  using Key = std::pair<std::optional<std::string>, unsigned>;
  const std::vector<Key> keys{{std::nullopt, 1}, {"", 1},           {"abc", 2},
                              {"abc", 1},        {std::nullopt, 1}, {"abc", 2}};
  std::vector<Key> stored;
  Index index;
  Index::ProbeState scratch;
  std::uint64_t results[6];
  index.find_or_create_many(
      scratch, keys.size(), [](std::uint32_t) { return std::uint64_t{0}; },
      [](std::uint64_t) { return std::uint64_t{0}; },
      [&](std::uint32_t *rows, std::size_t n, std::uint64_t *out, std::uint32_t *rejected) {
        std::size_t k = 0;
        for (std::size_t i = 0; i < n; ++i)
          if (keys[rows[i]] != stored[out[rows[i]] - 1]) rejected[k++] = rows[i];
        return k;
      },
      [&](const std::uint32_t *rows, std::size_t n, std::uint64_t *out) {
        for (std::size_t i = 0; i < n; ++i) {
          stored.push_back(keys[rows[i]]);
          out[i] = stored.size();
        }
      },
      results);
  CHECK_EQ(index.size(), 4u);
  CHECK_EQ(results[0], results[4]);
  CHECK_EQ(results[2], results[5]);
  CHECK(results[0] != results[1]);
  CHECK(results[2] != results[3]);
}

TEST(high_handles_and_atomic_claim_rounds) {
  Index index;
  Index::ProbeState scratch;
  std::uint64_t results[2];
  const auto high = (std::uint64_t{1} << 40) + 1;
  auto hash = [](std::uint32_t row) { return std::uint64_t(row); };
  auto stored = [](std::uint64_t handle) { return handle - high; };
  auto compare = [](std::uint32_t *rows, std::size_t n, std::uint64_t *out,
                    std::uint32_t *rejected) {
    std::size_t k = 0;
    for (std::size_t i = 0; i < n; ++i)
      if (out[rows[i]] != high + rows[i]) rejected[k++] = rows[i];
    return k;
  };
  bool threw = false;
  try {
    index.find_or_create_many(
        scratch, 2, hash, stored, compare,
        [](const std::uint32_t *, std::size_t n, std::uint64_t *out) {
          CHECK_EQ(n, 2u);
          out[0] = high;
          out[1] = 0;
        },
        results);
  } catch (const std::invalid_argument &) {
    threw = true;
  }
  CHECK(threw);
  CHECK_EQ(index.size(), 0u);
  index.check_invariants(stored);
  index.find_or_create_many(
      scratch, 2, hash, stored, compare,
      [](const std::uint32_t *rows, std::size_t n, std::uint64_t *out) {
        for (std::size_t i = 0; i < n; ++i) out[i] = high + rows[i];
      },
      results);
  index.reserve(1000, stored);
  index.find_many(scratch, 2, hash, compare, results);
  CHECK_EQ(results[0], high);
  CHECK_EQ(results[1], high + 1);
  index.check_invariants(stored);
}
TEST(later_round_create_exception_keeps_published_rows_and_allows_retry) {
  Index index;
  Index::ProbeState scratch;
  Rows rows{{1, 2, 3}, {}, 0, 0};
  std::uint64_t results[3];
  bool threw = false;
  auto materialize = rows.create();
  try {
    index.find_or_create_many(
        scratch, 3, rows.hashes(), rows.cached(), rows.compare(),
        [&](const std::uint32_t *new_rows, std::size_t n, std::uint64_t *out) {
          if (!rows.stored.empty()) throw std::runtime_error("later materialization");
          materialize(new_rows, n, out);
        },
        results);
  } catch (const std::runtime_error &) {
    threw = true;
  }
  CHECK(threw);
  CHECK_EQ(index.size(), 1u);
  CHECK_EQ(rows.stored.size(), 1u);
  index.check_invariants(rows.cached());
  index.find_or_create_many(scratch, 3, rows.hashes(), rows.cached(), rows.compare(), rows.create(),
                            results);
  CHECK_EQ(index.size(), 3u);
  CHECK_EQ(rows.stored.size(), 3u);
  for (unsigned i = 0; i < 3; ++i) CHECK_EQ(rows.stored[results[i] - 1], rows.keys[i]);
  index.check_invariants(rows.cached());
}
TEST(immutable_external_index_uses_independent_reader_scratch) {
  Rows rows;
  for (unsigned key = 0; key < 257; ++key) rows.keys.push_back(key);
  Index index;
  Index::ProbeState build_scratch;
  std::vector<std::uint64_t> built(rows.keys.size());
  auto hash_key = [](std::uint64_t key) { return key * 0x9e3779b97f4a7c15ull; };
  index.find_or_create_many(
      build_scratch, rows.keys.size(), [&](std::uint32_t row) { return hash_key(rows.keys[row]); },
      [&](std::uint64_t handle) { return hash_key(rows.stored[handle - 1]); }, rows.compare(),
      rows.create(), built.data());
  std::atomic<unsigned> errors{0};
  std::vector<std::thread> readers;
  const auto &frozen = index;
  for (unsigned thread = 0; thread < 8; ++thread) {
    readers.emplace_back([&, thread] {
      Index::ProbeState scratch;
      std::vector<unsigned> queries(1024);
      std::vector<std::uint64_t> found(queries.size());
      for (std::size_t i = 0; i < queries.size(); ++i) queries[i] = (i + thread) % 514;
      auto compare = [&](std::uint32_t *candidates, std::size_t count, std::uint64_t *handles,
                         std::uint32_t *rejected) {
        std::size_t missing = 0;
        for (std::size_t i = 0; i < count; ++i) {
          const auto row = candidates[i];
          if (queries[row] != rows.stored[handles[row] - 1]) rejected[missing++] = row;
        }
        return missing;
      };
      for (unsigned pass = 0; pass < 16; ++pass) {
        frozen.find_many(
            scratch, queries.size(), [&](std::uint32_t row) { return hash_key(queries[row]); },
            compare, found.data());
        for (std::size_t i = 0; i < queries.size(); ++i) {
          if (queries[i] < 257) {
            if (!found[i] || rows.stored[found[i] - 1] != queries[i]) ++errors;
          } else if (found[i]) {
            ++errors;
          }
        }
      }
    });
  }
  for (auto &reader : readers) reader.join();
  CHECK_EQ(errors.load(), 0u);
}

TEST(move_assignment_releases_each_index_through_its_original_allocator) {
  AllocationState first, second;
  {
    using AllocatedIndex = shoal::unsync::HashIndex<std::uint64_t, IndexAllocator>;
    AllocatedIndex destination{IndexAllocator(&first)}, source{IndexAllocator(&second)};
    AllocatedIndex::ProbeState scratch;
    Rows first_rows{{1}, {}, 0, 0}, second_rows{{2}, {}, 0, 0};
    std::uint64_t result;
    destination.find_or_create_many(scratch, 1, first_rows.hashes(), first_rows.cached(),
                                    first_rows.compare(), first_rows.create(), &result);
    source.find_or_create_many(scratch, 1, second_rows.hashes(), second_rows.cached(),
                               second_rows.compare(), second_rows.create(), &result);
    CHECK(first.bytes != 0);
    CHECK(second.bytes != 0);
    destination = std::move(source);
    CHECK_EQ(first.bytes, 0u);
    CHECK(second.bytes != 0);
    CHECK_EQ(source.size(), 0u);
    destination.find_many(scratch, 1, second_rows.hashes(), second_rows.compare(), &result);
    CHECK_EQ(second_rows.stored[result - 1], 2u);
    destination.clear();
    CHECK_EQ(second.bytes, 0u);
  }
  CHECK_EQ(first.bytes, 0u);
  CHECK_EQ(second.bytes, 0u);
}

TEST(streamed_reserve_preserves_rows_and_rejects_failed_enumeration) {
  Index index;
  Index::ProbeState scratch;
  Rows rows{{1, 2, 3, 4}, {}, 0, ~std::uint64_t{0}};
  std::uint64_t results[4], positions[4];
  index.find_or_create_many(scratch, 4, rows.hashes(), rows.cached(), rows.compare(), rows.create(),
                            results, positions);
  const auto capacity = index.capacity();
  const std::vector<std::uint64_t> physical_order{4, 2, 1, 3};
  std::vector<std::uint64_t> descriptor_order;
  for (std::size_t slot = 0; slot < index.capacity(); ++slot)
    if (const auto handle = index.handle_at(slot)) descriptor_order.push_back(handle);
  CHECK(descriptor_order != physical_order);
  std::vector<std::uint64_t> visited;
  for (unsigned failure = 0; failure < 5; ++failure) {
    bool threw = false;
    try {
      index.reserve_from(capacity * 2, [&](auto visit) {
        for (auto handle : physical_order) {
          if (failure == 0 && handle == 3) continue;
          if (failure == 2 && handle == 1) throw std::runtime_error("enumeration");
          auto enumerated_handle = handle;
          if (handle == 1 && failure == 3) enumerated_handle = 0;
          if (handle == 1 && failure == 4) enumerated_handle = (std::uint64_t{1} << 48) - 1;
          visit(enumerated_handle, rows.hash);
        }
        if (failure == 1) visit(std::uint64_t{5}, rows.hash);
      });
    } catch (const std::exception &) {
      threw = true;
    }
    CHECK(threw);
    CHECK_EQ(index.capacity(), capacity);
    CHECK_EQ(index.size(), 4u);
    for (unsigned row = 0; row < 4; ++row) CHECK_EQ(index.handle_at(positions[row]), results[row]);
    index.check_invariants(rows.cached());
  }
  index.reserve_from(capacity * 2, [&](auto visit) {
    for (auto handle : physical_order) {
      visited.push_back(handle);
      visit(handle, rows.hash);
    }
  });
  CHECK(visited == physical_order);
  CHECK_EQ(index.size(), 4u);
  index.reserve_from(4, [](auto) { throw std::runtime_error("unexpected enumeration"); });
  index.find_many(scratch, 4, rows.hashes(), rows.compare(), results, positions);
  for (unsigned row = 0; row < 4; ++row) {
    CHECK_EQ(rows.stored[results[row] - 1], rows.keys[row]);
    CHECK_EQ(index.handle_at(positions[row]), results[row]);
  }
}

TEST(streamed_reserve_allocation_failure_preserves_positions) {
  AllocationState allocation;
  using AllocatedIndex = shoal::unsync::HashIndex<std::uint64_t, IndexAllocator>;
  AllocatedIndex index{IndexAllocator(&allocation)};
  AllocatedIndex::ProbeState scratch;
  Rows rows{{1, 2}, {}, 0, 0};
  std::uint64_t results[2], positions[2];
  index.find_or_create_many(scratch, 2, rows.hashes(), rows.cached(), rows.compare(), rows.create(),
                            results, positions);
  const auto bytes = allocation.bytes;
  allocation.fail = true;
  bool threw = false, enumerated = false;
  try {
    index.reserve_from(1000, [&](auto visit) {
      enumerated = true;
      visit(std::uint64_t{1}, rows.hash);
      visit(std::uint64_t{2}, rows.hash);
    });
  } catch (const std::bad_alloc &) {
    threw = true;
  }
  CHECK(threw);
  CHECK(!enumerated);
  CHECK_EQ(allocation.bytes, bytes);
  CHECK_EQ(index.size(), 2u);
  for (unsigned row = 0; row < 2; ++row) CHECK_EQ(index.handle_at(positions[row]), results[row]);
}

TEST(probe_scratch_keeps_fingerprints_stops_and_one_position_array) {
  CHECK_EQ(sizeof(Index::ProbeState), 71680u);
}

namespace {
// A join build's rows, after DuckDB's `JoinHashTable`: every row is kept, the
// index holds each key's latest row, and a row's `next` is the handle of the
// row of its key before it (0 at the chain's end). Handles are row number + 1.
// Keys share four homes and five fingerprints, so entries are displaced and
// sit in one another's probe chains.
struct JoinRows {
  struct Row {
    unsigned key;
    std::uint64_t next;
  };
  std::vector<Row> rows;
  Index index;
  Index::ProbeState scratch;

  static std::uint64_t hash_of_key(unsigned key) {
    return (std::uint64_t{key % 5 + 1} << 48) | (key % 4);
  }
  auto stored_hash() const {
    return [this](std::uint64_t handle) { return hash_of_key(rows[handle - 1].key); };
  }
  // Compares keys[row] with the key of the row its result names.
  auto compare(const std::vector<unsigned> &keys) const {
    return [this, &keys](std::uint32_t *candidates, std::size_t count, std::uint64_t *results,
                         std::uint32_t *rejected) {
      std::size_t n = 0;
      for (std::size_t i = 0; i < count; ++i)
        if (rows[results[candidates[i]] - 1].key != keys[candidates[i]]) rejected[n++] = candidates[i];
      return n;
    };
  }
  // Materializes the keys' rows, then indexes them in one batch: a new key's
  // row becomes its entry, and a duplicate's is linked to the head and
  // replaces it, in input order.
  void insert(const std::vector<unsigned> &keys) {
    const std::uint64_t first = rows.size() + 1;
    for (auto key : keys) rows.push_back({key, 0});
    std::vector<std::uint64_t> results(keys.size()), positions(keys.size());
    index.find_or_create_many(
        scratch, keys.size(), [&](std::uint32_t row) { return hash_of_key(keys[row]); }, stored_hash(),
        compare(keys),
        [&](const std::uint32_t *created, std::size_t count, std::uint64_t *handles) {
          for (std::size_t i = 0; i < count; ++i) handles[i] = first + created[i];
        },
        results.data(), positions.data());
    for (std::size_t row = 0; row < keys.size(); ++row) {
      const auto handle = first + row;
      if (results[row] == handle) continue;
      const auto capacity = index.capacity();
      const auto size = index.size();
      rows[handle - 1].next = index.replace_at(positions[row], handle);
      CHECK_EQ(index.handle_at(positions[row]), handle);
      CHECK_EQ(index.capacity(), capacity);
      CHECK_EQ(index.size(), size);
    }
  }
  // Each key's head and its position, by find_many.
  std::pair<std::vector<std::uint64_t>, std::vector<std::uint64_t>> heads(
      const std::vector<unsigned> &keys) {
    std::vector<std::uint64_t> results(keys.size()), positions(keys.size());
    index.find_many(
        scratch, keys.size(), [&](std::uint32_t row) { return hash_of_key(keys[row]); }, compare(keys),
        results.data(), positions.data());
    return {results, positions};
  }
  // Every key's chain, walked from the head find_many returns, holds all its
  // rows, latest first.
  void check_chains() {
    std::vector<unsigned> keys;
    for (const auto &row : rows)
      if (std::find(keys.begin(), keys.end(), row.key) == keys.end()) keys.push_back(row.key);
    CHECK_EQ(index.size(), keys.size());
    index.check_invariants(stored_hash());
    const auto [results, positions] = heads(keys);
    for (std::size_t i = 0; i < keys.size(); ++i) {
      CHECK_EQ(index.handle_at(positions[i]), results[i]);
      std::vector<std::uint64_t> chain, expected;
      for (auto handle = results[i]; handle && chain.size() <= rows.size(); handle = rows[handle - 1].next)
        chain.push_back(handle);
      for (auto handle = rows.size(); handle; --handle)
        if (rows[handle - 1].key == keys[i]) expected.push_back(handle);
      CHECK(chain == expected);
    }
  }
};
}  // namespace

// Every entry is replaced in turn, displaced ones and ones in the middle of
// other keys' probe chains among them: the replaced entry keeps its slot, and
// every key, its neighbours', is still found where it was.
TEST(replace_at_repoints_one_entry_in_place) {
  JoinRows join;
  std::vector<unsigned> keys;
  for (unsigned key = 0; key < 40; ++key) keys.push_back(key);
  join.insert(keys);
  const auto [first_heads, positions] = join.heads(keys);
  std::size_t displaced = 0;
  for (std::size_t i = 0; i < keys.size(); ++i)
    displaced += positions[i] != (JoinRows::hash_of_key(keys[i]) & (join.index.capacity() - 1));
  CHECK(displaced >= 30u);
  for (std::size_t i = 0; i < keys.size(); ++i) {
    join.insert({keys[i]});
    const auto [heads, now] = join.heads(keys);
    CHECK(now == positions);
    for (std::size_t j = 0; j < keys.size(); ++j)
      CHECK_EQ(heads[j], j <= i ? keys.size() + j + 1 : first_heads[j]);
    join.check_chains();
  }
}

// Duplicates come across batches and within one, where a later row's result
// still names the key's first row: the handle replace_at returns is the head
// as it is, the row just before.
TEST(replace_at_prepends_duplicates_to_external_chains) {
  JoinRows join;
  join.insert({7, 1, 7, 2, 7, 7});
  join.check_chains();
  std::vector<unsigned> batch;
  for (unsigned i = 0; i < 3000; ++i) batch.push_back(i % 3 ? 7 : i % 11);
  join.insert(batch);
  join.insert({2, 7, 99});
  join.check_chains();
  const auto [heads, positions] = join.heads({7});
  CHECK_EQ(heads[0], join.rows.size() - 1);
}

TEST(replace_at_after_growth) {
  JoinRows join;
  join.insert({3, 4, 3, 5});
  const auto capacity = join.index.capacity();
  join.index.reserve(capacity * 4, join.stored_hash());
  CHECK(join.index.capacity() > capacity);
  join.check_chains();
  join.insert({4, 3, 4});
  join.check_chains();
  const auto grown = join.index.capacity();
  join.index.reserve_from(grown * 4, [&](auto visit) {
    for (std::uint64_t handle = 1; handle <= join.rows.size(); ++handle) {
      const auto [heads, positions] = join.heads({join.rows[handle - 1].key});
      if (heads[0] == handle) visit(handle, JoinRows::hash_of_key(join.rows[handle - 1].key));
    }
  });
  CHECK(join.index.capacity() > grown);
  join.check_chains();
  join.insert({5, 3, 6});
  join.check_chains();
}

TEST(replace_at_rejects_empty_positions_and_reserved_handles) {
  JoinRows join;
  CHECK_EQ(join.index.capacity(), 0u);
  const auto throws_out_of_range = [&](std::uint64_t position) {
    try {
      join.index.replace_at(position, 1);
    } catch (const std::out_of_range &) {
      return true;
    }
    return false;
  };
  CHECK(throws_out_of_range(0));
  join.insert({1, 2});
  const auto [heads, positions] = join.heads({1, 2});
  std::uint64_t empty = 0;
  while (join.index.handle_at(empty)) ++empty;
  for (auto position : {empty, std::uint64_t{join.index.capacity()}, std::numeric_limits<std::uint64_t>::max()})
    CHECK(throws_out_of_range(position));
  for (auto reserved : {std::uint64_t{0}, (std::uint64_t{1} << 48) - 1, std::uint64_t{1} << 48}) {
    bool threw = false;
    try {
      join.index.replace_at(positions[0], reserved);
    } catch (const std::invalid_argument &) {
      threw = true;
    }
    CHECK(threw);
  }
  CHECK_EQ(join.index.handle_at(empty), 0u);
  CHECK_EQ(join.index.handle_at(positions[0]), heads[0]);
  join.check_chains();
}

TEST_MAIN()
