#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <type_traits>
#include <utility>

#include "../config.hpp"
#include "batch.hpp"

namespace bbtree::detail {

template <class K, class V, class A>
class HashTable {
  static_assert(std::is_nothrow_move_constructible_v<K> && std::is_nothrow_move_constructible_v<V>,
                "BBHashMap keys and values must move without throwing");
  static_assert(std::is_nothrow_destructible_v<K> && std::is_nothrow_destructible_v<V>,
                "BBHashMap keys and values must be destroyed without throwing");
  static_assert(std::is_nothrow_move_constructible_v<A> && std::is_nothrow_swappable_v<A>,
                "BBHashMap allocators must move and swap without throwing");
  using Store = HashEntryStore<K, V, A>;

 public:
  explicit HashTable(A allocator = A()) : index_(allocator), entries_(std::move(allocator)) {}
  HashTable(HashTable &&) noexcept = default;
  HashTable(const HashTable &) = delete;
  HashTable &operator=(const HashTable &) = delete;

  void swap(HashTable &other) noexcept {
    index_.swap(other.index_);
    entries_.swap(other.entries_);
  }
  std::size_t size() const noexcept { return entries_.size(); }
  const A &allocator() const noexcept { return entries_.allocator(); }
  template <class Visit>
  void for_each_hashed(Visit visit) const {
    entries_.for_each(
        [&](std::size_t, const auto &entry) { visit(entry.key, entry.value, entry.hash); });
  }
  void reserve(std::size_t count) { index_.reserve(count, entries_); }
  void clear() noexcept {
    index_.clear();
    entries_.clear();
  }
  void clear_retain_capacity() noexcept {
    index_.reset();
    entries_.reset();
  }

  template <class Q, class Hasher, class Equal>
  void get_many(const Q *keys, std::size_t count, const V **values, const Hasher &hash,
                const Equal &equal) const {
    HashLookupBatch<K, V, A>::find_many(index_, entries_, keys, count, values, hash, equal);
  }

  template <class Q, class Hasher, class Equal, class Init>
  void get_or_insert_many(const Q *keys, std::size_t count, V **values, const Hasher &hash,
                          const Equal &equal, Init &initialize) {
    auto create = [&](std::size_t, const Q &key) -> decltype(auto) { return initialize(key); };
    auto output = [&](std::size_t row, const Q &, V &value, bool) { values[row] = &value; };
    apply_many(keys, count, hash, equal, create, output);
  }

  template <class Q, class Hasher, class Equal, class Init, class Update>
  void update_or_insert_many(const Q *keys, std::size_t count, const Hasher &hash,
                             const Equal &equal, Init &initialize, Update &update) {
    auto output = [&](std::size_t row, const Q &key, V &value, bool inserted) {
      if (!inserted) update(row, key, value);
    };
    apply_many(keys, count, hash, equal, initialize, output);
  }

  template <class Q, class Equal>
  const typename Store::Entry *find(std::uint64_t hash, const Q &key, const Equal &equal) const {
    const auto slot = index_.find(hash, key, entries_, equal);
    return slot == index_.capacity() ? nullptr : &entries_.at(index_.identifier(slot));
  }
  template <class Q, class Equal>
  typename Store::Entry *find(std::uint64_t hash, const Q &key, const Equal &equal) {
    return const_cast<typename Store::Entry *>(std::as_const(*this).find(hash, key, equal));
  }

  template <class Equal>
  std::optional<V> insert(std::uint64_t hash, K &&key, V &&value, const Equal &equal) {
    const auto slot = index_.locate(hash, key, entries_, equal);
    if (index_.capacity() != 0 && index_.occupied(slot)) {
      auto *stored = &entries_.at(index_.identifier(slot));
      std::optional<V> previous(std::move(stored->value));
      stored->value.~V();
      ::new (static_cast<void *>(&stored->value)) V(std::move(value));
      return previous;
    }
    emplace_vacant(slot, hash, std::move(key), std::move(value));
    return std::nullopt;
  }

  template <class Q, class Equal>
  std::optional<std::pair<K, V>> remove_entry(std::uint64_t hash, const Q &key,
                                              const Equal &equal) {
    const auto slot = index_.find(hash, key, entries_, equal);
    if (slot == index_.capacity()) return std::nullopt;
    const auto identifier = index_.identifier(slot);
    auto &stored = entries_.at(identifier);
    std::optional<std::pair<K, V>> previous(std::in_place, std::move(stored.key),
                                            std::move(stored.value));
    index_.erase(slot);
    entries_.erase(identifier);
    return previous;
  }

  template <bool Const>
  class Iterator {
    using Storage = std::conditional_t<Const, const Store, Store>;

   public:
    using item = std::pair<const K &, std::conditional_t<Const, const V &, V &>>;
    Iterator(Storage *store, std::size_t identifier) noexcept
        : store_(store), identifier_(identifier) {}
    item operator*() const noexcept {
      auto &entry = store_->at(identifier_);
      return {entry.key, entry.value};
    }
    Iterator &operator++() noexcept {
      identifier_ = store_->next(identifier_);
      return *this;
    }
    bool operator==(const Iterator &other) const noexcept {
      return store_ == other.store_ && identifier_ == other.identifier_;
    }
    bool operator!=(const Iterator &other) const noexcept { return !(*this == other); }

   private:
    Storage *store_;
    std::size_t identifier_;
  };

  Iterator<false> begin() noexcept { return {&entries_, entries_.next(0)}; }
  Iterator<false> end() noexcept { return {&entries_, 0}; }
  Iterator<true> begin() const noexcept { return {&entries_, entries_.next(0)}; }
  Iterator<true> end() const noexcept { return {&entries_, 0}; }

  // No user hashing/equality: prove descriptor ownership, fingerprint and
  // reachability from each cached hash, with exactly one descriptor per entry.
  void check_invariants() const noexcept {
    BBTREE_CHECK(index_.size() == entries_.size());
    std::size_t count = 0;
    for (std::size_t slot = 0; slot < index_.capacity(); ++slot) {
      if (!index_.occupied(slot)) continue;
      const auto identifier = index_.identifier(slot);
      BBTREE_CHECK(entries_.live(identifier));
      const auto hash = entries_.at(identifier).hash;
      BBTREE_CHECK(index_.fingerprint(slot) == hash >> 48);
      auto visited = index_.home(hash);
      while (visited != slot) {
        BBTREE_CHECK(index_.identifier(visited) != 0);
        BBTREE_CHECK(index_.identifier(visited) != identifier);
        visited = index_.next(visited);
      }
      ++count;
    }
    BBTREE_CHECK(count == entries_.size());
  }

 private:
  HashProbeIndex<A> index_;
  Store entries_;

  template <class Q, class Hasher, class Equal, class Init, class Visit>
  void apply_many(const Q *keys, std::size_t count, const Hasher &hash, const Equal &equal,
                  Init &initialize, Visit &visit) {
    using Batch = HashLookupBatch<K, V, A>;
    std::size_t consumed = 0;
    while (count != 0) {
      std::array<std::uint64_t, Batch::maximum_width()> hashes;
      std::array<const V *, Batch::maximum_width()> found;
      std::array<std::size_t, Batch::maximum_width()> positions;
      const auto width = count < hashes.size() ? count : hashes.size();
      for (std::size_t row = 0; row < width; ++row) hashes[row] = HashBits::mix(hash(keys[row]));
      Batch::find_hashed(index_, entries_, keys, hashes.data(), width, found.data(), equal,
                         positions.data());
      std::size_t missing = 0;
      for (std::size_t row = 0; row < width; ++row) missing += found[row] == nullptr;
      // Without tombstones, a cached empty slot proves absence until an earlier
      // row occupies it. Duplicate/colliding rows then search again. Reserving
      // for all misses prevents later inserts from invalidating cached slots.
      const bool cached_positions =
          !index_.has_deleted() && !index_.prepare_many(missing, entries_);
      for (std::size_t row = 0; row < width; ++row) {
        V *stored = const_cast<V *>(found[row]);
        bool inserted = false;
        if (!stored) {
          std::size_t slot;
          if (cached_positions && index_.descriptor(positions[row]) == 0) {
            // Still empty: the earlier lookup already proved absence. No
            // tombstone decoding or second occupancy check is necessary.
            slot = positions[row];
          } else {
            slot = index_.locate(hashes[row], keys[row], entries_, equal);
            if (index_.occupied(slot)) stored = &entries_.at(index_.identifier(slot)).value;
          }
          if (!stored) {
            K key(keys[row]);
            V value(initialize(consumed + row, keys[row]));
            stored = cached_positions
                         ? emplace_at(slot, hashes[row], std::move(key), std::move(value))
                         : emplace_vacant(slot, hashes[row], std::move(key), std::move(value));
            inserted = true;
          }
        }
        visit(consumed + row, keys[row], *stored, inserted);
      }
      keys += width;
      consumed += width;
      count -= width;
    }
  }

  BBTREE_ALWAYS_INLINE V *emplace_vacant(std::size_t slot, std::uint64_t hash, K &&key, V &&value) {
    if (index_.prepare_insert(entries_)) slot = index_.vacant(hash);
    return emplace_at(slot, hash, std::move(key), std::move(value));
  }

  BBTREE_ALWAYS_INLINE V *emplace_at(std::size_t slot, std::uint64_t hash, K &&key, V &&value) {
    const auto inserted = entries_.insert(hash, std::move(key), std::move(value));
    index_.publish_at(slot, hash, inserted.identifier());
    return &inserted.entry().value;
  }
};

}  // namespace bbtree::detail
