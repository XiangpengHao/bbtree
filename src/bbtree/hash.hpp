// Hash policy shared by the owned and concurrent hash maps. Byte strings
// hash their borrowed view, so owning keys and borrowed queries agree.
#pragma once

#include <cstdint>
#include <functional>
#include <string>
#include <string_view>

namespace bbtree {

template <class K>
struct Hash : std::hash<K> {};

template <class Char, class Traits, class Alloc>
struct Hash<std::basic_string<Char, Traits, Alloc>> {
  std::size_t operator()(std::basic_string_view<Char, Traits> key) const
      noexcept(noexcept(std::hash<std::basic_string_view<Char, Traits>>{}(key))) {
    return std::hash<std::basic_string_view<Char, Traits>>{}(key);
  }
};

namespace detail {

// Avalanche the policy's result before taking disjoint high and low bits.
// This distributes identity integer hashes too. It adds no entropy or
// collision resistance; applications needing those supply a keyed policy.
struct HashBits {
  static std::uint64_t mix(std::uint64_t hash) noexcept {
    hash ^= hash >> 30;
    hash *= 0xbf58476d1ce4e5b9ull;
    hash ^= hash >> 27;
    hash *= 0x94d049bb133111ebull;
    return hash ^ (hash >> 31);
  }
};

}  // namespace detail
}  // namespace bbtree
