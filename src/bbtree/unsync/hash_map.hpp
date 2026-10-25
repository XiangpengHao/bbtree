// An owning, single-threaded hash map. The probe index moves independently
// of paged entries: inserts/reserve preserve references, erase invalidates
// only the removed entry, and clear/assignment invalidate everything.
#pragma once

#include <functional>
#include <optional>
#include <type_traits>
#include <utility>

#include "../detail/hash/table.hpp"
#include "../hash.hpp"

namespace bbtree::unsync {

template <class K, class V, class Hasher = Hash<K>, class Equal = std::equal_to<>, class A = Global>
class BBHashMap {
  static_assert(std::is_nothrow_move_constructible_v<Hasher> &&
                    std::is_nothrow_swappable_v<Hasher> &&
                    std::is_nothrow_move_constructible_v<Equal> &&
                    std::is_nothrow_swappable_v<Equal>,
                "BBHashMap policies must move and swap without throwing");

 public:
  using key_type = K;
  using mapped_type = V;
  using size_type = std::size_t;
  using allocator_type = A;

  explicit BBHashMap(Hasher hash = Hasher(), Equal equal = Equal(), A allocator = A())
      : hash_(std::move(hash)), equal_(std::move(equal)), table_(std::move(allocator)) {}
  BBHashMap(BBHashMap &&) noexcept = default;
  BBHashMap(const BBHashMap &other) : BBHashMap(other.hash_, other.equal_, other.allocator()) {
    reserve(other.size());
    for (auto [key, value] : other) insert(key, value);
  }
  BBHashMap &operator=(const BBHashMap &other) {
    if (this != &other) {
      BBHashMap copy(other);
      swap(copy);
    }
    return *this;
  }
  BBHashMap &operator=(BBHashMap &&other) noexcept {
    if (this != &other) {
      BBHashMap moved(std::move(other));
      swap(moved);
    }
    return *this;
  }
  void swap(BBHashMap &other) noexcept {
    using std::swap;
    swap(hash_, other.hash_);
    swap(equal_, other.equal_);
    table_.swap(other.table_);
  }

  size_type size() const noexcept { return table_.size(); }
  bool empty() const noexcept { return size() == 0; }
  const A &allocator() const noexcept { return table_.allocator(); }

  /// Allocation failure throws std::bad_alloc; entries remain unchanged.
  /// Hash/equality exceptions propagate before changing any entry.
  /// Takes its arguments by reference: MSVC can't pass an over-aligned
  /// key or value by value.
  template <class KeyArg = K, class ValueArg = V,
            std::enable_if_t<std::is_constructible_v<K, KeyArg &&> && std::is_constructible_v<V, ValueArg &&>,
                             int> = 0>
  std::optional<V> insert(KeyArg &&key, ValueArg &&value) {
    K owned_key(std::forward<KeyArg>(key));
    V owned_value(std::forward<ValueArg>(value));
    const auto hash = hash_key(owned_key);
    return table_.insert(hash, std::move(owned_key), std::move(owned_value), equal_);
  }

  template <class Q>
  const V *get(const Q &key) const {
    const auto *entry = table_.find(hash_key(key), key, equal_);
    return entry ? &entry->value : nullptr;
  }
  template <class Q>
  V *get_mut(const Q &key) {
    auto *entry = table_.find(hash_key(key), key, equal_);
    return entry ? &entry->value : nullptr;
  }
  template <class Q>
  std::optional<std::pair<const K &, const V &>> get_key_value(const Q &key) const {
    const auto *entry = table_.find(hash_key(key), key, equal_);
    if (entry) return std::pair<const K &, const V &>{entry->key, entry->value};
    return std::nullopt;
  }
  template <class Q>
  bool contains_key(const Q &key) const {
    return get(key) != nullptr;
  }
  template <class Q>
  std::optional<std::pair<K, V>> remove_entry(const Q &key) {
    return table_.remove_entry(hash_key(key), key, equal_);
  }
  template <class Q>
  std::optional<V> remove(const Q &key) {
    auto entry = remove_entry(key);
    if (entry) return std::move(entry->second);
    return std::nullopt;
  }

  /// Results are in input order. Inputs and outputs must be valid, disjoint
  /// arrays; zero count accepts null pointers. Exceptions leave a partial output.
  template <class Q>
  void get_many(const Q *keys, size_type count, const V **values) const {
    table_.get_many(keys, count, values, hash_, equal_);
  }

  /// Finds or creates each key, initializing only newly inserted entries.
  /// Duplicate keys share a value pointer. Inputs are borrowed; missing keys
  /// are copied into K. Earlier inserts remain on exception. The initializer
  /// must not re-enter this map. Returned pointers survive subsequent growth.
  template <class Q, class Init>
  void get_or_insert_many(const Q *keys, size_type count, V **values, Init initialize) {
    table_.get_or_insert_many(keys, count, values, hash_, equal_, initialize);
  }

  /// For each input in order, initialize(row, query) constructs a missing
  /// value, otherwise update(row, query, value) modifies the existing value.
  /// Duplicates observe preceding inputs' changes. Queries are borrowed and
  /// hashed once. Callbacks must not re-enter the map; completed changes remain
  /// if a callback throws, including modifications made by a throwing update.
  template <class Q, class Init, class Update>
  void update_or_insert_many(const Q *keys, size_type count, Init initialize, Update update) {
    table_.update_or_insert_many(keys, count, hash_, equal_, initialize, update);
  }

  /// Moves inputs in order, with exactly the semantics of separate inserts,
  /// including duplicate keys. On exception, earlier inserts remain committed.
  void insert_many(K *keys, V *values, size_type count, std::optional<V> *previous) {
    for (size_type index = 0; index < count; ++index) {
      auto result = insert(std::move(keys[index]), std::move(values[index]));
      previous[index].reset();
      if (result) previous[index].emplace(std::move(*result));
    }
  }

  /// Reserves index capacity for `count` live entries; payload pages remain
  /// lazy. Rebuilds use cached hashes, invoking neither hash nor equality.
  void reserve(size_type count) { table_.reserve(count); }
  void clear() noexcept { table_.clear(); }
  /// Destroys every entry but retains the index and pages for another build.
  /// Invalidates all references and iterators; performs no allocations.
  void clear_retain_capacity() noexcept { table_.clear_retain_capacity(); }
  /// Visits (key, value, cached routing hash) in page order without hashing.
  /// The mixed hash is local to this map's hash policy, not a persistent ID.
  /// The callback must not structurally mutate the map. Exceptions propagate.
  template <class Visit>
  void for_each_hashed(Visit visit) const {
    table_.for_each_hashed(visit);
  }
  auto begin() noexcept { return table_.begin(); }
  auto end() noexcept { return table_.end(); }
  auto begin() const noexcept { return table_.begin(); }
  auto end() const noexcept { return table_.end(); }
  void check_invariants() const noexcept { table_.check_invariants(); }

 private:
  Hasher hash_;
  Equal equal_;
  detail::HashTable<K, V, A> table_;

  template <class Q>
  std::uint64_t hash_key(const Q &key) const {
    return detail::HashBits::mix(hash_(key));
  }
};

}  // namespace bbtree::unsync
