// A shared hash map in shards, each an inline-slot table (D25)
// (`detail/hash/inline_table.hpp`) behind a latch the tree's nodes use too
// (`node/latch.hpp`).
// Writers hold their shard's latch exclusively, bumping its version. Readers
// of keys and values that copy as bytes write nothing: they read the version,
// look the key up without the latch, copying the entry they find, and check
// the version after, as the tree's optimistic readers do (D12, D26);
// user code sees only that copy. Readers of other types hold the latch shared.
// Callbacks must not re-enter this map or retain references to its entries.
#pragma once

#include <array>
#include <atomic>
#include <cstddef>
#include <functional>
#include <optional>
#include <type_traits>
#include <utility>

#include "detail/hash/retained.hpp"
#include "detail/hash/inline_table.hpp"
#include "detail/node/latch.hpp"
#include "detail/racing_copy.hpp"
#include "hash.hpp"
#include "insert_error.hpp"

namespace shoal {

template <class K, class V, class Hasher = Hash<K>, class Equal = std::equal_to<>, class A = Global,
          std::size_t ShardCount = 64>
class HashMap {
  static_assert(ShardCount != 0 && (ShardCount & (ShardCount - 1)) == 0 && ShardCount <= 65536,
                "HashMap shard count must be a power of two, at most 65536");

  using Table = detail::InlineHashTable<K, V, detail::RetainingAllocator<A>>;
  using Entry = typename Table::Entry;

  /// Readers that copy entries as bytes, and so never hold a latch.
  static constexpr bool reads_racing = std::is_trivially_copyable_v<K> && std::is_trivially_copyable_v<V>;
  /// Optimistic reads of a key before it waits for its shard shared.
  static constexpr int racing_attempts = 4;
  /// Keys a bulk read reads optimistically before checking their versions.
  static constexpr std::size_t batch_width = 64;

  struct alignas(64) Shard {
    detail::NodeLatch latch{};
    // Before the table, so that it outlives the table's last deallocations.
    detail::RetainedBlocks<A> retained;
    Table table;
    explicit Shard(A allocator)
        : retained(allocator), table(detail::RetainingAllocator<A>(std::move(allocator), &retained)) {}
    ~Shard() { retained.stop_retaining(); }
  };

  /// A shard held exclusively, for the lifetime of the guard.
  class ExclusiveShard {
   public:
    explicit ExclusiveShard(Shard &shard) noexcept : shard_(shard), version_(shard.latch.lock_exclusive()) {}
    ~ExclusiveShard() { shard_.latch.unlock_exclusive(version_); }
    ExclusiveShard(const ExclusiveShard &) = delete;
    ExclusiveShard &operator=(const ExclusiveShard &) = delete;

   private:
    Shard &shard_;
    detail::Version version_;
  };

  /// A shard held shared, for the lifetime of the guard.
  class SharedShard {
   public:
    explicit SharedShard(Shard &shard) noexcept : shard_(shard) { shard.latch.lock_shared(); }
    ~SharedShard() { shard_.latch.unlock_shared(); }
    SharedShard(const SharedShard &) = delete;
    SharedShard &operator=(const SharedShard &) = delete;

   private:
    Shard &shard_;
  };

 public:
  using key_type = K;
  using mapped_type = V;
  using size_type = std::size_t;
  using allocator_type = A;

  /// Hash/equality policies must support simultaneous const calls. Copies
  /// of the allocator share allocation ownership, as in the tree's contract,
  /// and must support concurrent calls from different shards.
  explicit HashMap(Hasher hash = Hasher(), Equal equal = Equal(), A allocator = A())
      : HashMap(std::move(hash), std::move(equal), std::move(allocator),
                  std::make_index_sequence<ShardCount>{}) {}
  HashMap(const HashMap &) = delete;
  HashMap &operator=(const HashMap &) = delete;

  /// Preserves the input entry on allocator refusal. User exceptions propagate;
  /// a policy throwing bad_alloc is not mistaken for an allocator refusal.
  InsertOutcome<std::optional<V>, K, V> insert(K key, V value) const {
    const auto hash = hash_key(key);
    auto &shard = shard_for(hash);
    ExclusiveShard held(shard);
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
    if constexpr (reads_racing) {
      for (int attempt = 0; attempt < racing_attempts; ++attempt) {
        const detail::Version version = shard.latch.read_version();
        detail::RacingCopy<Entry> found;
        const bool hit = shard.table.racing_find(hash, key, equal_, found);
        if (!shard.latch.is_unchanged(version)) continue;
        if (!hit) return std::optional<Result>{};
        const Entry &entry = found.assume_read();
        return std::optional<Result>(
            std::in_place, detail::Returned<F, const K &, const V &>::call(visit, entry.key, entry.value));
      }
    }
    SharedShard held(shard);
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
    ExclusiveShard held(shard);
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
    ExclusiveShard held(shard);
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
    ExclusiveShard held(shard);
    const auto *entry = shard.table.find(hash, key, equal_);
    if (!entry || !condition(entry->key, std::as_const(entry->value))) return std::nullopt;
    return shard.table.remove_entry(hash, key, equal_);
  }

  /// Visits shards one at a time. Each shard is consistent while held
  /// shared; the complete count/traversal is not an atomic map-wide snapshot.
  size_type size() const {
    size_type count = 0;
    for (auto &shard : shards_) {
      SharedShard held(shard);
      count += shard.table.size();
    }
    return count;
  }
  bool empty() const { return size() == 0; }

  template <class Visit>
  void for_each(Visit visit) const {
    for (auto &shard : shards_) {
      SharedShard held(shard);
      shard.table.for_each(visit);
    }
  }

  /// Clears shards one at a time. Concurrent inserts into an already cleared
  /// shard can survive; callers needing an empty map must exclude writers.
  /// A shard keeps its memory for the entries to come, and gives it back
  /// when the map is destroyed: an optimistic reader may still be reading it.
  void clear() const {
    for (auto &shard : shards_) {
      ExclusiveShard held(shard);
      shard.table.clear();
    }
  }

  /// Shared batches have separate-operation semantics. Keys and values that
  /// copy as bytes are read optimistically `batch_width` at a time, their
  /// versions checked once all are read, so that their memory waits overlap;
  /// a key whose shard changed meanwhile is read again on its own (D28).
  template <class Q>
  void get_many_copied(const Q *keys, size_type count, std::optional<V> *values) const {
    if constexpr (reads_racing) {
      for (size_type first = 0; first < count; first += batch_width) {
        const size_type width = count - first < batch_width ? count - first : batch_width;
        get_batch_copied(keys + first, width, values + first);
      }
    } else {
      for (size_type index = 0; index < count; ++index) {
        auto result = get_copied(keys[index]);
        values[index].reset();
        if (result) values[index].emplace(std::move(*result));
      }
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
      SharedShard held(shard);
      shard.table.check_invariants();
    }
  }

 private:
  Hasher hash_;
  Equal equal_;
  mutable std::array<Shard, ShardCount> shards_;

  template <std::size_t... Indices>
  HashMap(Hasher hash, Equal equal, A allocator, std::index_sequence<Indices...>)
      : hash_(std::move(hash)),
        equal_(std::move(equal)),
        shards_{{Shard((void(Indices), allocator))...}} {}

  /// Up to `batch_width` keys read optimistically, then checked.
  template <class Q>
  void get_batch_copied(const Q *keys, size_type width, std::optional<V> *values) const {
    std::array<std::uint64_t, batch_width> hashes;
    std::array<Shard *, batch_width> shards;
    std::array<detail::Version, batch_width> versions;
    std::array<typename Table::RacingLookup, batch_width> lookups;
    // The lanes whose tags don't show their key absent, and what their
    // entries held, by their place among them.
    std::array<std::uint8_t, batch_width> matched;
    std::array<detail::RacingCopy<Entry>, batch_width> found;
    std::array<bool, batch_width> hits;
    const typename Table::NoEntry none;
    for (size_type lane = 0; lane < width; ++lane) {
      hashes[lane] = hash_key(keys[lane]);
      shards[lane] = &shard_for(hashes[lane]);
      // A shard a writer holds now is read all the same, as safely, and
      // turned away below.
      versions[lane] = shards[lane]->latch.load_version_relaxed();
    }
    // Fences, rather than acquire loads, order the versions before the
    // entries and the entries before the checks: once a bulk read, so that
    // the keys' waits for memory overlap.
    std::atomic_thread_fence(std::memory_order_acquire);
    for (size_type lane = 0; lane < width; ++lane) lookups[lane].load_tags(shards[lane]->table, hashes[lane]);
    // Every lane is written and the count moves by whether it matched: a
    // key its tags show absent stops here, without a branch.
    size_type matched_count = 0;
    for (size_type lane = 0; lane < width; ++lane) {
      matched[matched_count] = static_cast<std::uint8_t>(lane);
      matched_count += lookups[lane].match(hashes[lane]);
    }
    for (size_type index = 0; index < matched_count; ++index) lookups[matched[index]].load_hash(none);
    for (size_type index = 0; index < matched_count; ++index) {
      const auto lane = matched[index];
      hits[index] = lookups[lane].finish(shards[lane]->table, hashes[lane], keys[lane], equal_, found[index]);
    }
    for (size_type lane = 0; lane < width; ++lane) values[lane].reset();
    for (size_type index = 0; index < matched_count; ++index) {
      // A miss copied `NoEntry`, or another key's entry: its value is
      // dropped, without a branch on whether the key was there.
      auto &value = values[matched[index]];
      value.emplace(found[index].assume_read().value);
      if (!hits[index]) value.reset();
    }
    std::atomic_thread_fence(std::memory_order_acquire);
    for (size_type lane = 0; lane < width; ++lane) {
      if (SHOAL_UNLIKELY(versions[lane].held() || shards[lane]->latch.load_version_relaxed() != versions[lane]))
        values[lane] = get_copied(keys[lane]);
    }
  }

  template <class Q>
  std::uint64_t hash_key(const Q &key) const {
    return detail::HashBits::of<Hasher>(hash_(key));
  }
  Shard &shard_for(std::uint64_t hash) const noexcept {
    // Middle bits leave the fingerprint and the low 32 placement bits intact.
    return shards_[(hash >> 32) & (ShardCount - 1)];
  }
};

}  // namespace shoal
