// A map's entry for in-place changes (`unsync/map/entry.rs`): `Entry`,
// `OccupiedEntry` and `VacantEntry`, as Rust's `BTreeMap::entry` gives them.
//
// An occupied entry is a node stack at its key, in a leaf or at a separator
// whose value is a leaf's head (D17); a vacant one is a node stack at the
// leaf edge where its key goes, or, for a map without a tree, no stack yet.
// Every comparison of keys happens when the map makes the entry: the entry's
// operations compare none. References an entry hands out stay valid until
// the map changes otherwise.
#pragma once

#include <cstddef>
#include <optional>
#include <utility>
#include <variant>

#include "../detail/tree/remove.hpp"
#include "../detail/tree/tree.hpp"

namespace shoal::unsync {

template <class K, class V, class Config, class A>
class TreeMap;

template <class K, class V, class S, class A>
class VacantEntry;

/// An entry the map holds.
template <class K, class V, class S, class A>
class OccupiedEntry {
  using Stack = detail::MutStack<K, V, S>;
  using Pages = detail::MapPages<A>;

 public:
  const K& key() const noexcept { return stack_.key(); }
  const V& get() const noexcept { return stack_.value(); }
  V& get_mut() noexcept { return stack_.value(); }

  /// Puts `value` in place of the entry's value and returns the old one.
  V insert(V value) noexcept { return std::exchange(stack_.value(), std::move(value)); }

  /// Removes the entry from the map and returns it, its key as stored.
  std::pair<K, V> remove_entry() && noexcept { return stack_.remove(pages_, *length_); }

  /// Removes the entry from the map and returns its value.
  V remove() && noexcept { return std::move(*this).remove_entry().second; }

 private:
  template <class, class, class, class>
  friend class TreeMap;
  friend class VacantEntry<K, V, S, A>;

  Stack stack_;
  std::size_t* length_;
  Pages pages_;

  OccupiedEntry(Stack stack, std::size_t& length, Pages pages) noexcept
      : stack_(stack), length_(&length), pages_(pages) {}
};

/// The place of a key the map lacks.
template <class K, class V, class S, class A>
class VacantEntry {
  using Stack = detail::MutStack<K, V, S>;
  using Pages = detail::MapPages<A>;
  using Occupied = OccupiedEntry<K, V, S, A>;

 public:
  const K& key() const noexcept { return key_; }

  /// The key the entry would have inserted.
  K into_key() && noexcept { return std::move(key_); }

  /// Inserts the key with `value` and returns the value, in the map.
  V& insert(V value) && noexcept { return std::move(*this).insert_entry(std::move(value)).get_mut(); }

  /// Inserts the key with `value` and returns the entry it makes.
  Occupied insert_entry(V value) && noexcept {
    if (!has_tree_) {
      // A new tree: an empty root leaf, which a search compares no keys in.
      stack_.tree().root_page_or_new(pages_);
      stack_.search(detail::SearchKey<K, K>(key_));
    }
    stack_.insert(std::move(key_), std::move(value), pages_, *length_);
    stack_.finish_insertion();
    return Occupied(stack_, *length_, pages_);
  }

 private:
  template <class, class, class, class>
  friend class TreeMap;

  K key_;
  Stack stack_;
  std::size_t* length_;
  Pages pages_;
  bool has_tree_;

  VacantEntry(K&& key, Stack stack, std::size_t& length, Pages pages, bool has_tree) noexcept
      : key_(std::move(key)), stack_(stack), length_(&length), pages_(pages), has_tree_(has_tree) {}
};

/// A key's entry: occupied if the map holds the key, vacant if not.
template <class K, class V, class S, class A>
class Entry {
  using Occupied = OccupiedEntry<K, V, S, A>;
  using Vacant = VacantEntry<K, V, S, A>;

 public:
  explicit Entry(Occupied occupied) noexcept : state_(std::move(occupied)) {}
  explicit Entry(Vacant vacant) noexcept : state_(std::move(vacant)) {}

  bool is_occupied() const noexcept { return std::holds_alternative<Occupied>(state_); }

  const K& key() const noexcept {
    return is_occupied() ? std::get<Occupied>(state_).key() : std::get<Vacant>(state_).key();
  }

  /// The entry's value, `value` inserted first if vacant.
  V& or_insert(V value) && noexcept {
    if (is_occupied()) return std::get<Occupied>(state_).get_mut();
    return std::get<Vacant>(std::move(state_)).insert(std::move(value));
  }

  /// The entry's value, `make()` inserted first if vacant.
  template <class Make>
  V& or_insert_with(Make make) && {
    if (is_occupied()) return std::get<Occupied>(state_).get_mut();
    return std::get<Vacant>(std::move(state_)).insert(make());
  }

  /// The entry's value, `make(key)` inserted first if vacant.
  template <class Make>
  V& or_insert_with_key(Make make) && {
    if (is_occupied()) return std::get<Occupied>(state_).get_mut();
    Vacant& vacant = std::get<Vacant>(state_);
    V value = make(vacant.key());
    return std::move(vacant).insert(std::move(value));
  }

  /// The entry's value, a default one inserted first if vacant.
  V& or_default() && { return std::move(*this).or_insert_with([] { return V(); }); }

  /// Runs `change(value)` on the value if occupied; returns the entry.
  template <class Change>
  Entry and_modify(Change change) && {
    if (is_occupied()) change(std::get<Occupied>(state_).get_mut());
    return std::move(*this);
  }

  /// Sets the entry's value to `value`, inserting the key if vacant, and
  /// returns the occupied entry.
  Occupied insert_entry(V value) && noexcept {
    if (is_occupied()) {
      Occupied& occupied = std::get<Occupied>(state_);
      occupied.insert(std::move(value));
      return std::move(occupied);
    }
    return std::get<Vacant>(std::move(state_)).insert_entry(std::move(value));
  }

  /// The entry, which is occupied.
  Occupied into_occupied() && noexcept { return std::get<Occupied>(std::move(state_)); }

  /// The entry, which is vacant.
  Vacant into_vacant() && noexcept { return std::get<Vacant>(std::move(state_)); }

 private:
  std::variant<Vacant, Occupied> state_;
};

}  // namespace shoal::unsync
