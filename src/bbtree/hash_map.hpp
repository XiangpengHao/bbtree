// A shared map over the same paged hash table as unsync::BBHashMap.
// Each operation protects one shard through key comparison and value access.
// Callbacks must not re-enter this map or retain references to its entries.
#pragma once

#include <array>
#include <cstddef>
#include <functional>
#include <mutex>
#include <optional>
#include <shared_mutex>
#include <type_traits>
#include <utility>

#include "detail/hash/table.hpp"
#include "hash.hpp"
#include "insert_error.hpp"

namespace bbtree {

template <class K, class V, class Hasher = Hash<K>, class Equal = std::equal_to<>, class A = Global,
          std::size_t ShardCount = 64>
class BBHashMap {
  static_assert(ShardCount != 0 && (ShardCount & (ShardCount - 1)) == 0 && ShardCount <= 65536,
                "BBHashMap shard count must be a power of two, at most 65536");

  struct alignas(64) Shard {
    mutable std::shared_mutex mutex;
    detail::HashTable<K, V, A> table;
    explicit Shard(A allocator) : table(std::move(allocator)) {}
  };

 public:
  using key_type = K;
  using mapped_type = V;
  using size_type = std::size_t;
  using allocator_type = A;

  /// Hash/equality policies must support simultaneous const calls. Copies
  /// of the allocator share allocation ownership, as in the tree's contract,
  /// and must support concurrent calls from different shards.
  explicit BBHashMap(Hasher hash = Hasher(), Equal equal = Equal(), A allocator = A())
      : BBHashMap(std::move(hash), std::move(equal), std::move(allocator),
                  std::make_index_sequence<ShardCount>{}) {}
  BBHashMap(const BBHashMap &) = delete;
  BBHashMap &operator=(const BBHashMap &) = delete;

  /// Preserves the input entry on allocator refusal. User exceptions propagate;
  /// a policy throwing bad_alloc is not mistaken for an allocator refusal.
  InsertOutcome<std::optional<V>, K, V> insert(K key, V value) const {
    const auto hash = hash_key(key);
    auto &shard = shard_for(hash);
    std::unique_lock<std::shared_mutex> lock(shard.mutex);
    try {
      return InsertOutcome<std::optional<V>, K, V>::done(
          shard.table.insert(hash, std::move(key), std::move(value), equal_));
    } catch (const detail::HashAllocationFailure &failure) {
      return InsertOutcome<std::optional<V>, K, V>::failed(
          InsertError<K, V>(std::move(key), std::move(value), failure.layout()));
    }
  }

  template <class Q, class F>
  auto read(const Q &key, F visit) const {
    using Result = typename detail::Returned<F, const K &, const V &>::type;
    static_assert(!std::is_reference_v<Result> && !std::is_pointer_v<Result>,
                  "shared map callbacks return owned results");
    const auto hash = hash_key(key);
    auto &shard = shard_for(hash);
    std::shared_lock<std::shared_mutex> lock(shard.mutex);
    const auto *entry = shard.table.find(hash, key, equal_);
    if (!entry) return std::optional<Result>{};
    return std::optional<Result>(std::in_place,
                                 detail::Returned<F, const K &, const V &>::call(
                                     visit, entry->key, std::as_const(entry->value)));
  }

  template <class Q, class F>
  auto update(const Q &key, F visit) const {
    using Result = typename detail::Returned<F, const K &, V &>::type;
    static_assert(!std::is_reference_v<Result> && !std::is_pointer_v<Result>,
                  "shared map callbacks return owned results");
    const auto hash = hash_key(key);
    auto &shard = shard_for(hash);
    std::unique_lock<std::shared_mutex> lock(shard.mutex);
    auto *entry = shard.table.find(hash, key, equal_);
    if (!entry) return std::optional<Result>{};
    return std::optional<Result>(
        std::in_place,
        detail::Returned<F, const K &, V &>::call(visit, std::as_const(entry->key), entry->value));
  }

  template <class Q>
  std::optional<V> get_copied(const Q &key) const {
    return read(key, [](const K &, const V &value) { return V(value); });
  }
  template <class Q>
  std::optional<V> get_cloned(const Q &key) const {
    return get_copied(key);
  }
  template <class Q>
  bool contains_key(const Q &key) const {
    return read(key, [](const K &, const V &) {}).has_value();
  }
  template <class Q>
  std::optional<std::pair<K, V>> remove_entry(const Q &key) const {
    const auto hash = hash_key(key);
    auto &shard = shard_for(hash);
    std::unique_lock<std::shared_mutex> lock(shard.mutex);
    return shard.table.remove_entry(hash, key, equal_);
  }
  template <class Q>
  std::optional<V> remove(const Q &key) const {
    auto entry = remove_entry(key);
    if (entry) return std::move(entry->second);
    return std::nullopt;
  }
  template <class Q, class Condition>
  std::optional<std::pair<K, V>> remove_if(const Q &key, Condition condition) const {
    const auto hash = hash_key(key);
    auto &shard = shard_for(hash);
    std::unique_lock<std::shared_mutex> lock(shard.mutex);
    const auto *entry = shard.table.find(hash, key, equal_);
    if (!entry || !condition(entry->key, std::as_const(entry->value))) return std::nullopt;
    return shard.table.remove_entry(hash, key, equal_);
  }

  /// Visits shards one at a time. Each shard is consistent under its read
  /// lock; the complete count/traversal is not an atomic map-wide snapshot.
  size_type size() const {
    size_type count = 0;
    for (auto &shard : shards_) {
      std::shared_lock<std::shared_mutex> lock(shard.mutex);
      count += shard.table.size();
    }
    return count;
  }
  bool empty() const { return size() == 0; }

  template <class Visit>
  void for_each(Visit visit) const {
    for (auto &shard : shards_) {
      std::shared_lock<std::shared_mutex> lock(shard.mutex);
      for (auto [key, value] : std::as_const(shard.table)) visit(key, value);
    }
  }

  /// Clears shards one at a time. Concurrent inserts into an already cleared
  /// shard can survive; callers needing an empty map must exclude writers.
  void clear() const {
    for (auto &shard : shards_) {
      std::unique_lock<std::shared_mutex> lock(shard.mutex);
      shard.table.clear();
    }
  }

  /// Shared batches take one lock per key, with separate-operation semantics.
  template <class Q>
  void get_many_copied(const Q *keys, size_type count, std::optional<V> *values) const {
    for (size_type index = 0; index < count; ++index) {
      auto result = get_copied(keys[index]);
      values[index].reset();
      if (result) values[index].emplace(std::move(*result));
    }
  }
  template <class Q, class Visit>
  void read_many(const Q *keys, size_type count, Visit visit) const {
    for (size_type index = 0; index < count; ++index)
      read(keys[index], [&](const K &key, const V &value) { visit(index, key, value); });
  }
  void insert_many(K *keys, V *values, size_type count, std::optional<V> *previous,
                   bool *failed) const {
    for (size_type index = 0; index < count; ++index) {
      auto result = insert(std::move(keys[index]), std::move(values[index]));
      failed[index] = !result.ok();
      previous[index].reset();
      if (result.ok()) {
        if (result.value()) previous[index].emplace(std::move(*result.value()));
      } else {
        keys[index].~K();
        ::new (static_cast<void *>(keys + index)) K(std::move(result.error().key()));
        values[index].~V();
        ::new (static_cast<void *>(values + index)) V(std::move(result.error().value()));
      }
    }
  }

  void check_invariants() const {
    for (auto &shard : shards_) {
      std::shared_lock<std::shared_mutex> lock(shard.mutex);
      shard.table.check_invariants();
    }
  }

 private:
  Hasher hash_;
  Equal equal_;
  mutable std::array<Shard, ShardCount> shards_;

  template <std::size_t... Indices>
  BBHashMap(Hasher hash, Equal equal, A allocator, std::index_sequence<Indices...>)
      : hash_(std::move(hash)),
        equal_(std::move(equal)),
        shards_{{Shard((void(Indices), allocator))...}} {}

  template <class Q>
  std::uint64_t hash_key(const Q &key) const {
    return detail::HashBits::mix(hash_(key));
  }
  Shard &shard_for(std::uint64_t hash) const noexcept {
    // Middle bits leave the fingerprint and the low 32 placement bits intact.
    return shards_[(hash >> 32) & (ShardCount - 1)];
  }
};

}  // namespace bbtree
