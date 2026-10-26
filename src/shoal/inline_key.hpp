// Inline keys (design/04-inline-key.md, D18): register-sized windows of a
// key's encoding that nodes store beside their keys and search first. A
// search then reads the full keys, and the heap bytes behind them, only where
// inline keys tie.
//
// `InlineKey<K>` says whether keys `K` have inline keys and how (PORTING.md,
// P1). Its primary template says they have none: the map then compares full
// keys with `operator<`. It is specialized for what Rust's `Auto` recognizes:
// byte strings (`std::string`, `std::string_view`, `std::vector<uint8_t>`,
// `std::vector<char>`, `std::array<uint8_t, 16 | 20 | 32>`) and the 64- and
// 128-bit integers. A key type of your own opts in by specializing it,
// deriving from `InlineKeyBase`.
#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <string>
#include <string_view>
#include <type_traits>
#include <vector>

#include "detail/config.hpp"
#include "detail/target.hpp"

namespace shoal {

/// An inline key: 8 bytes of a key's encoding, big-endian, so that inline keys
/// compare like the bytes they come from. Nodes keep the first 2, 4 or 8.
using InlineWord = std::uint64_t;

/// How many bytes of each inline key the nodes keep. Narrower inline keys pack
/// more of a node's inline keys into each cache line but tie more often.
enum class InlineWidth : std::uint8_t { Two = 2, Four = 4, Eight = 8 };

/// How one key orders against another, as Rust's `Ordering`.
enum class Ordering : signed char { Less = -1, Equal = 0, Greater = 1 };

/// A byte string's bytes, as the nodes read them.
struct ByteView {
  const std::uint8_t* data;
  std::size_t size;
};

/// The windows and comparisons of a byte string, ordered like its bytes, as
/// the nodes compute them (`proof/search_bytes.rs`, `proof/inline_key.rs`):
/// every load of a fixed size, with no call to `memcmp`. A key that wraps a
/// byte string takes its inline key's functions from here.
class ByteWindows {
 public:
  explicit ByteWindows(ByteView bytes) noexcept : bytes_(bytes) {}

  ByteView bytes() const noexcept { return bytes_; }
  const std::uint8_t* data() const noexcept { return bytes_.data; }
  std::size_t size() const noexcept { return bytes_.size; }

  /// Bytes `offset..offset + 8`, big-endian, zero past the end.
  SHOAL_ALWAYS_INLINE InlineWord inline_key_at(std::size_t offset) const noexcept {
    const std::size_t len = bytes_.size;
    if (len < 8) return short_inline_key_at(offset);
    const std::size_t last_start = len - 8;
    if (offset <= last_start) return load_be64(bytes_.data + offset);
    const std::size_t shift = min(offset - last_start, 8);
    return shift == 8 ? 0 : load_be64(bytes_.data + last_start) << (8 * shift);
  }

  /// The byte string's hash tag: its length, saturated at 255 (D36).
  std::uint8_t hash_tag() const noexcept {
    return bytes_.size < 255 ? static_cast<std::uint8_t>(bytes_.size) : 255;
  }

  /// How the bytes, padded with zeros, order against `prefix` in its first
  /// `prefix_len` bytes (D50): whole 32-byte chunks for equality first, then a
  /// window at a time.
  SHOAL_ALWAYS_INLINE Ordering cmp_prefix(const std::uint8_t* prefix, std::size_t prefix_len) const noexcept {
    constexpr std::size_t chunk = 32;
    std::size_t offset = 0;
    while (prefix_len - offset >= chunk && offset + chunk <= bytes_.size &&
           std::memcmp(bytes_.data + offset, prefix + offset, chunk) == 0) {
      offset += chunk;
    }
    const ByteWindows prefix_windows(ByteView{prefix, prefix_len});
    while (offset < prefix_len) {
      const std::size_t remaining = prefix_len - offset;
      const unsigned dropped_bits = 8 * static_cast<unsigned>(remaining >= 8 ? 0 : 8 - remaining);
      const InlineWord query = drop_low(inline_key_at(offset), dropped_bits);
      const InlineWord shared = drop_low(prefix_windows.inline_key_at(offset), dropped_bits);
      if (query != shared) return query < shared ? Ordering::Less : Ordering::Greater;
      offset += 8;
    }
    return Ordering::Equal;
  }

  /// How the bytes order against `shared`, the bytes every key of a tree
  /// starts with: as `cmp_prefix`, but a byte string ending inside them goes
  /// first, as every byte string of the tree is at least as long.
  SHOAL_ALWAYS_INLINE Ordering cmp_shared(const std::uint8_t* shared, std::size_t shared_len) const noexcept {
    const Ordering ordering = cmp_prefix(shared, shared_len);
    if (ordering == Ordering::Equal && bytes_.size < shared_len) return Ordering::Less;
    return ordering;
  }

  /// How the bytes order against `stored`, given that their first `equal_len`
  /// bytes are equal when padded with zeros: 8 bytes at a time from there,
  /// as big-endian words, then by length. A string that ends within the
  /// equal bytes isn't read at all.
  SHOAL_ALWAYS_INLINE Ordering cmp_from(ByteView stored, std::size_t equal_len) const noexcept {
    const std::size_t query_len = bytes_.size;
    const std::size_t both_len = min(query_len, stored.size);
    if (equal_len < both_len && both_len < 8) {
      const InlineWord query = inline_key_at(0);
      const InlineWord other = ByteWindows(stored).inline_key_at(0);
      if (query != other) return query < other ? Ordering::Less : Ordering::Greater;
      return compare_lengths(query_len, stored.size);
    }
    std::size_t compared = equal_len;
    while (compared < both_len) {
      const std::size_t word_end = min(compared + 8, both_len);
      const InlineWord query = load_be64(bytes_.data + word_end - 8);
      const InlineWord other = load_be64(stored.data + word_end - 8);
      if (query != other) return query < other ? Ordering::Less : Ordering::Greater;
      compared = word_end;
    }
    return compare_lengths(query_len, stored.size);
  }

  /// Whether the bytes equal `other`'s.
  SHOAL_ALWAYS_INLINE bool equals(ByteView other) const noexcept {
    return bytes_.size == other.size &&
           (bytes_.size == 0 || std::memcmp(bytes_.data, other.data, bytes_.size) == 0);
  }

  static InlineWord load_be64(const std::uint8_t* bytes) noexcept {
    InlineWord word;
    std::memcpy(&word, bytes, sizeof(word));
    return to_big_endian(word);
  }

 private:
  ByteView bytes_;

  static constexpr std::size_t min(std::size_t a, std::size_t b) noexcept { return a < b ? a : b; }

  static constexpr InlineWord drop_low(InlineWord word, unsigned bits) noexcept {
    return bits >= 64 ? 0 : word >> bits;
  }

  static constexpr Ordering compare_lengths(std::size_t a, std::size_t b) noexcept {
    return a < b ? Ordering::Less : (b < a ? Ordering::Greater : Ordering::Equal);
  }

  static InlineWord to_big_endian(InlineWord word) noexcept {
    return detail::ByteOrder::big_endian64(word);
  }

  static std::uint32_t load_be32(const std::uint8_t* bytes) noexcept {
    std::uint32_t word;
    std::memcpy(&word, bytes, sizeof(word));
    return detail::ByteOrder::big_endian32(word);
  }

  /// `inline_key_at` of fewer than 8 bytes: they padded to a word, shifted.
  SHOAL_ALWAYS_INLINE InlineWord short_inline_key_at(std::size_t offset) const noexcept {
    const std::uint8_t* bytes = bytes_.data;
    const std::size_t len = bytes_.size;
    InlineWord word;
    if (len >= 4) {
      word = (static_cast<InlineWord>(load_be32(bytes)) << 32) |
             (static_cast<InlineWord>(load_be32(bytes + len - 4)) << (8 * (8 - len)));
    } else if (len >= 2) {
      const InlineWord first = (static_cast<InlineWord>(bytes[0]) << 8) | bytes[1];
      word = (first << 48) | (static_cast<InlineWord>(bytes[len - 1]) << (8 * (8 - len)));
    } else if (len == 1) {
      word = static_cast<InlineWord>(bytes[0]) << 56;
    } else {
      return 0;
    }
    const std::size_t shift = min(offset, 8);
    return shift == 8 ? 0 : word << (8 * shift);
  }
};

/// The primary template: keys without inline keys.
template <class K, class = void>
struct InlineKey {
  static constexpr bool enabled = false;
};

/// What an inline-key impl of a key type of your own derives from, with
/// itself as `Self`: every hook but `inline_key_at` and `width` defaulted.
///
/// The contract: every key has an encoding such that if `a`'s sorts before
/// `b`'s, both padded with zeros, then `a < b`; `inline_key_at(key, offset)`
/// is that encoding's bytes `offset..offset + 8`, big-endian, zero past its
/// end. A wrong impl gives wrong answers, never undefined behavior. Hooks must
/// not throw (PORTING.md, P4).
template <class K, class Self>
struct InlineKeyBase {
  static constexpr bool enabled = true;
  static constexpr bool is_byte_string = false;
  /// How many bytes of each inline key the nodes keep.
  static constexpr InlineWidth width = InlineWidth::Eight;
  /// Whether leaves keep each key's `Self::hash_tag`, where they have room.
  static constexpr bool has_hash_tag = false;
  /// Whether ties compare with `Self::cmp_from`, past the bytes known equal;
  /// internal nodes then keep 4 bytes of 8-byte inline keys (D43).
  static constexpr bool has_cmp_from = false;

  static std::uint8_t hash_tag(const K&) noexcept { return 0; }

  /// How `key`'s encoding, padded with zeros, orders against `prefix` in its
  /// first `prefix_len` bytes: by default a window at a time.
  static SHOAL_ALWAYS_INLINE Ordering cmp_prefix(const K& key, const std::uint8_t* prefix,
                                                  std::size_t prefix_len) noexcept {
    const ByteWindows prefix_windows(ByteView{prefix, prefix_len});
    const std::size_t words = prefix_len / 8;
    for (std::size_t index = 0; index < words; ++index) {
      const InlineWord window = Self::inline_key_at(key, 8 * index);
      const InlineWord shared = prefix_windows.inline_key_at(8 * index);
      if (window != shared) return window < shared ? Ordering::Less : Ordering::Greater;
    }
    const std::size_t tail = prefix_len % 8;
    if (tail == 0) return Ordering::Equal;
    const unsigned dropped_bits = 8 * static_cast<unsigned>(8 - tail);
    const InlineWord window = Self::inline_key_at(key, 8 * words) >> dropped_bits;
    const InlineWord shared = prefix_windows.inline_key_at(8 * words) >> dropped_bits;
    if (window != shared) return window < shared ? Ordering::Less : Ordering::Greater;
    return Ordering::Equal;
  }
};

/// A byte string's inline keys: windows of its bytes; its hash tag its
/// length; ties compared from where keys differ (D36, D43). `Bytes::of(key)`
/// gives the bytes. A byte string that holds its bytes, a fixed array, keeps 2
/// bytes of each inline key in both node kinds and compares a lone candidate
/// whole (D32, D38); one that points to them keeps 8 in leaves, 4 inside.
template <class K, bool HoldsItsBytes>
struct ByteStringInlineKey {
  static constexpr bool enabled = true;
  static constexpr bool is_byte_string = true;
  static constexpr bool holds_its_bytes = HoldsItsBytes;
};

// `std::char_traits<char>` orders strings as their bytes, unsigned: as their
// inline keys do. A `std::vector<char>` orders by `char`, signed on most
// targets, so it is not a byte string here.

template <class Allocator>
struct InlineKey<std::basic_string<char, std::char_traits<char>, Allocator>>
    : ByteStringInlineKey<std::basic_string<char, std::char_traits<char>, Allocator>, false> {
  static ByteView bytes(const std::basic_string<char, std::char_traits<char>, Allocator>& key) noexcept {
    return {reinterpret_cast<const std::uint8_t*>(key.data()), key.size()};
  }
};

template <>
struct InlineKey<std::string_view> : ByteStringInlineKey<std::string_view, false> {
  static ByteView bytes(std::string_view key) noexcept {
    return {reinterpret_cast<const std::uint8_t*>(key.data()), key.size()};
  }
};

template <class Allocator>
struct InlineKey<std::vector<std::uint8_t, Allocator>>
    : ByteStringInlineKey<std::vector<std::uint8_t, Allocator>, false> {
  static ByteView bytes(const std::vector<std::uint8_t, Allocator>& key) noexcept {
    return {key.data(), key.size()};
  }
};

template <std::size_t N>
struct InlineKey<std::array<std::uint8_t, N>, std::enable_if_t<N == 16 || N == 20 || N == 32>>
    : ByteStringInlineKey<std::array<std::uint8_t, N>, true> {
  static ByteView bytes(const std::array<std::uint8_t, N>& key) noexcept {
    return {key.data(), N};
  }
};

/// The 64-bit integers, whichever of `long` and `long long` they are: their
/// big-endian bytes, a signed one's with the sign bit flipped so that
/// negatives sort first. Nodes keep 2 of them, past the bytes a node's keys
/// share (D31).
template <class T>
struct InlineKey<T, std::enable_if_t<std::is_integral_v<T> && sizeof(T) == 8>>
    : InlineKeyBase<T, InlineKey<T>> {
  static constexpr InlineWidth width = InlineWidth::Two;
  static constexpr bool is_std_integer = true;
  static InlineWord inline_key_at(T key, std::size_t offset) noexcept {
    std::uint64_t bits = static_cast<std::uint64_t>(key);
    if constexpr (std::is_signed_v<T>) bits ^= std::uint64_t{1} << 63;
    return offset < 8 ? bits << (8 * offset) : 0;
  }
};

#if defined(__SIZEOF_INT128__)
template <>
struct InlineKey<unsigned __int128>
    : InlineKeyBase<unsigned __int128, InlineKey<unsigned __int128>> {
  static constexpr InlineWidth width = InlineWidth::Two;
  static constexpr bool is_std_integer = true;
  static InlineWord inline_key_at(unsigned __int128 key, std::size_t offset) noexcept {
    return offset < 16 ? static_cast<InlineWord>((key << (8 * offset)) >> 64) : 0;
  }
};

template <>
struct InlineKey<__int128> : InlineKeyBase<__int128, InlineKey<__int128>> {
  static constexpr InlineWidth width = InlineWidth::Two;
  static constexpr bool is_std_integer = true;
  static InlineWord inline_key_at(__int128 key, std::size_t offset) noexcept {
    const unsigned __int128 flipped =
        static_cast<unsigned __int128>(key) ^ (static_cast<unsigned __int128>(1) << 127);
    return offset < 16 ? static_cast<InlineWord>((flipped << (8 * offset)) >> 64) : 0;
  }
};
#endif

}  // namespace shoal
