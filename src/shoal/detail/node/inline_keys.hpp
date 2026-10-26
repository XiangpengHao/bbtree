// Inline keys inside the nodes (OG-D18): what a node stores beside each key,
// and what a search looks for (`tree/node/inline_keys.rs`).
//
// A node's prefix is the bytes its fences share, and so every key in it
// (05-fence-key.md). A key's inline key at that prefix length is the 8 bytes
// of its encoding past the prefix; the node stores the first of them, as many
// as its plan's width.
//
// - `InlineSlot<Width>` is what a node stores an inline key as.
// - `Window` is an inline key with the prefix length it is taken at; it moves
//   back to a shorter prefix without reading the key.
// - `WindowedKey` is a key outside the nodes with its window.
// - `Separator` is a separator outside its internal node.
// - `SearchKey` is what a search looks for.
#pragma once

#include <cstddef>
#include <cstdint>
#include <new>
#include <optional>
#include <type_traits>
#include <utility>

#include "../../inline_key.hpp"
#include "plan.hpp"

namespace shoal::detail {

/// The bytes of a window, whatever a node keeps of it.
inline constexpr std::size_t window_len = sizeof(InlineWord);

/// How a node stores an inline key at `Width` bytes: its top bytes, in an
/// unsigned integer of as many bytes, which orders like them.
template <std::size_t Width>
struct InlineSlot {
  using Word = std::conditional_t<Width == 2, std::uint16_t,
                                  std::conditional_t<Width == 4, std::uint32_t, std::uint64_t>>;
  static constexpr unsigned bits = 8 * sizeof(Word);

  /// The first bytes of `inline_key`, which a node keeps.
  static constexpr Word of_inline_key(InlineWord inline_key) noexcept {
    return static_cast<Word>(inline_key >> (64 - bits));
  }
  /// The inline key these bytes start, zeros after them.
  static constexpr InlineWord inline_key(Word slot) noexcept {
    return bits == 64 ? static_cast<InlineWord>(slot) : static_cast<InlineWord>(slot) << (64 - bits);
  }
};

/// Three-way comparison by `operator<`, as Rust's `Ord::cmp` (D8).
struct Order {
  template <class A, class B>
  static Ordering cmp(const A& a, const B& b) noexcept {
    if (a < b) return Ordering::Less;
    if (b < a) return Ordering::Greater;
    return Ordering::Equal;
  }
};

/// What a lookup searches with for a query `Q` of keys `K`: the query
/// itself, but an arithmetic query of another type than an arithmetic key,
/// which becomes the key type first, so that `map.get(1)` on `uint64_t` keys
/// compares as Rust's `get(&1)` does, and uses the keys' inline keys.
template <class K>
struct Query {
  template <class Q>
  static constexpr bool converts =
      std::is_arithmetic_v<Q> && std::is_arithmetic_v<K> && !std::is_same_v<Q, K>;

  template <class Q>
  using type = std::conditional_t<converts<Q>, K, Q>;

  template <class Q>
  static decltype(auto) of(const Q& query) noexcept {
    if constexpr (converts<Q>) {
      return static_cast<K>(query);
    } else {
      return (query);
    }
  }
};

/// An inline key with the prefix length it is taken at: bytes
/// `prefix_len..prefix_len + 8` of a key's encoding.
class Window {
 public:
  constexpr Window() noexcept : prefix_len_(0), inline_key_(0) {}
  constexpr Window(std::size_t prefix_len, InlineWord inline_key) noexcept
      : prefix_len_(prefix_len), inline_key_(inline_key) {}

  /// What a key without inline keys carries: nothing a node stores.
  static constexpr Window none() noexcept { return Window(); }

  constexpr std::size_t prefix_len() const noexcept { return prefix_len_; }
  constexpr InlineWord inline_key() const noexcept { return inline_key_; }

  constexpr bool operator==(const Window& other) const noexcept {
    return prefix_len_ == other.prefix_len_ && inline_key_ == other.inline_key_;
  }

  /// The same key's window at `fill`'s shorter prefix length, where `fill` is
  /// the window there of a key whose encoding shares this key's bytes up to
  /// this window's prefix length. The bytes that move in come from `fill`, so
  /// no key is read: how a node's prefix shrinks, and how a key moves into a
  /// node with a shorter one. A `fill` at a longer prefix gives wrong inline
  /// keys, never undefined behavior.
  constexpr Window moved_back_to(Window fill) const noexcept {
    const std::size_t shift = prefix_len_ > fill.prefix_len_ ? prefix_len_ - fill.prefix_len_ : 0;
    InlineWord inline_key = 0;
    if (shift == 0) {
      inline_key = inline_key_;
    } else if (shift >= window_len) {
      inline_key = fill.inline_key_;
    } else {
      const unsigned shift_bits = 8 * static_cast<unsigned>(shift);
      const InlineWord filled_bits = ~(~InlineWord{0} >> shift_bits);
      inline_key = (fill.inline_key_ & filled_bits) | (inline_key_ >> shift_bits);
    }
    return Window(fill.prefix_len_, inline_key);
  }

 private:
  std::size_t prefix_len_;
  InlineWord inline_key_;
};

/// A key outside the nodes, with its window for the node it goes into. In a
/// map whose keys have no inline keys, the window is `Window::none()`.
template <class K>
class WindowedKey {
 public:
  WindowedKey(K&& key, Window window) noexcept : key_(std::move(key)), window_(window) {}

  /// `key`, with its window at `prefix_len`, for a node with that prefix.
  static WindowedKey at(K&& key, std::size_t prefix_len) noexcept {
    Window window;
    if constexpr (KeyInliner<K>::enabled) {
      window = Window(prefix_len, KeyInliner<K>::inline_key_at(key, prefix_len));
    }
    return WindowedKey(std::move(key), window);
  }

  K& key() noexcept { return key_; }
  const K& key() const noexcept { return key_; }
  Window window() const noexcept { return window_; }
  K into_key() noexcept { return std::move(key_); }

  /// The key with its window moved back to `fill`'s prefix length.
  WindowedKey moved_back_to(Window fill) && noexcept {
    return WindowedKey(std::move(key_), window_.moved_back_to(fill));
  }

 private:
  K key_;
  Window window_;
};

/// A separator outside its internal node (OG-D17): on its way up from a split,
/// between internal nodes, or into a merged leaf. Without inline keys
/// (`WithKey`), internal nodes keep their separator keys, and a separator is
/// its key. With inline keys (OG-D49), it is the key's window alone; the key
/// stays a leaf head.
template <class K, bool WithKey>
class Separator;

template <class K>
class Separator<K, true> {
 public:
  explicit Separator(WindowedKey<K>&& key) noexcept
      : key_(std::move(key.key())), window_(key.window()) {}

  Window window() const noexcept { return window_; }
  Separator moved_back_to(Window fill) && noexcept {
    return Separator(WindowedKey<K>(std::move(key_), window_.moved_back_to(fill)));
  }
  WindowedKey<K> into_windowed_key() && noexcept { return WindowedKey<K>(std::move(key_), window_); }
  const K& key() const noexcept { return key_; }

 private:
  K key_;
  Window window_;
};

template <class K>
class Separator<K, false> {
 public:
  explicit Separator(Window window) noexcept : window_(window) {}

  Window window() const noexcept { return window_; }
  Separator moved_back_to(Window fill) && noexcept { return Separator(window_.moved_back_to(fill)); }

 private:
  Window window_;
};

/// What a search looks for: `key`, of a type `Q` that keys `K` compare with.
/// Where they can, nodes compare its window at each node's prefix with their
/// inline keys:
/// - for byte-string keys, if `Q` is a byte string too, by its bytes;
/// - for keys with their own impl, if `Q` is `K` itself.
/// Otherwise the search compares full keys with `operator<`.
template <class K, class Q>
class SearchKey {
  using KInliner = KeyInliner<K>;
  using QInliner = KeyInliner<Q>;

 public:
  /// Whether the query compares by its bytes, as byte-string keys do.
  static constexpr bool by_bytes = KInliner::is_byte_string && QInliner::is_byte_string;
  /// Whether the query compares by its own impl, as `K` does.
  static constexpr bool by_impl =
      KInliner::is_impl && std::is_same_v<std::remove_cv_t<K>, std::remove_cv_t<Q>>;
  /// Whether the nodes compare the query's windows with their inline keys.
  static constexpr bool has_inline_target = by_bytes || by_impl;

  explicit SearchKey(const Q& key) noexcept : key_(&key) {
    if constexpr (by_bytes) bytes_ = QInliner::bytes(key);
  }

  const Q& key() const noexcept { return *key_; }

  /// The query's bytes, if the nodes compare it by them. A query that holds
  /// its bytes is read again from the key, so that a fixed array's length
  /// stays a constant.
  ByteView query_bytes() const noexcept {
    if constexpr (QInliner::holds_its_bytes) {
      return QInliner::bytes(*key_);
    } else {
      return bytes_;
    }
  }

  /// The query's window at a node's `prefix_len`; only with
  /// `has_inline_target`.
  SHOAL_ALWAYS_INLINE Window inline_target(std::size_t prefix_len) const noexcept {
    static_assert(has_inline_target);
    if constexpr (by_bytes) {
      return Window(prefix_len, ByteWindows(query_bytes()).inline_key_at(prefix_len));
    } else {
      return Window(prefix_len, KInliner::inline_key_at(*key_, prefix_len));
    }
  }

  /// Orders the query against `stored` from their first byte.
  SHOAL_ALWAYS_INLINE Ordering cmp_full(const K& stored) const noexcept {
    if constexpr (by_bytes) {
      return ByteWindows(query_bytes()).cmp_from(KInliner::bytes(stored), 0);
    } else {
      return Order::cmp(*key_, stored);
    }
  }

  /// Whether the query equals `stored`, comparing their bytes all at once: an
  /// exact-key lookup's lone candidate (OG-D38). Only `by_bytes`.
  SHOAL_ALWAYS_INLINE bool equals_whole_bytes(const K& stored) const noexcept {
    static_assert(by_bytes);
    return ByteWindows(query_bytes()).equals(KInliner::bytes(stored));
  }

  /// Whether the query, a byte string, ends within the inline key a node of
  /// `Width` bytes keeps at `prefix_len`: then a stored key with an equal
  /// inline key that shares the prefix is this key if their lengths are.
  template <std::size_t Width>
  bool ends_within_window(std::size_t prefix_len) const noexcept {
    if constexpr (by_bytes && Width != 0) {
      return query_bytes().size <= prefix_len + Width;
    } else {
      return false;
    }
  }

  /// Whether the query has a hash tag the nodes compare with theirs.
  static constexpr bool has_hash_tag = by_bytes || (by_impl && KInliner::impl_has_hash_tag);

  /// The query's hash tag (OG-D36); only with `has_hash_tag`.
  std::uint8_t hash_tag() const noexcept {
    if constexpr (by_bytes) {
      return ByteWindows(query_bytes()).hash_tag();
    } else {
      return KInliner::hash_tag(*key_);
    }
  }

  /// How the query orders against every key of a tree whose encodings all
  /// start with `shared` (OG-D50): before or after them if it differs in those
  /// bytes or, as a byte string, ends inside them; `Equal` if it has them.
  /// `nullopt` if the nodes can't compare its encoding.
  inline std::optional<Ordering> cmp_shared_prefix(const std::uint8_t* shared,
                                                   std::size_t shared_len) const noexcept {
    if constexpr (by_bytes) {
      return ByteWindows(query_bytes()).cmp_shared(shared, shared_len);
    } else if constexpr (by_impl) {
      return InlineKey<K>::cmp_prefix(*key_, shared, shared_len);
    } else {
      return std::nullopt;
    }
  }

  /// How the query orders against `stored`, a key of a node keeping `Width`
  /// bytes of each inline key, whose inline key equals the query's `target`
  /// in them. Byte strings compare from past the bytes known equal, as do
  /// keys whose impl has `cmp_from`; others by `operator<`.
  template <std::size_t Width>
  SHOAL_ALWAYS_INLINE Ordering cmp_tied(Window target, const K& stored) const noexcept {
    if constexpr (Width != 0 && by_bytes) {
      return ByteWindows(query_bytes()).cmp_from(KInliner::bytes(stored), target.prefix_len() + Width);
    } else if constexpr (Width != 0 && by_impl && KInliner::impl_has_cmp_from) {
      return InlineKey<K>::cmp_from(*key_, stored, target.prefix_len() + Width);
    } else {
      return Order::cmp(*key_, stored);
    }
  }

  /// The first line read to form the next search window, for a batch to
  /// prefetch; nullptr if none.
  const void* window_line(std::size_t prefix_len) const noexcept {
    if constexpr (by_bytes) {
      const ByteView bytes = query_bytes();
      if (prefix_len >= bytes.size) return nullptr;
      const std::size_t last = bytes.size >= window_len ? bytes.size - window_len : 0;
      return bytes.data + (prefix_len < last ? prefix_len : last);
    } else {
      return nullptr;
    }
  }

  /// The first stored byte `cmp_tied` reads of `stored` in a node of `Width`
  /// bytes at `prefix_len`, for a batch to prefetch; nullptr if none.
  template <std::size_t Width>
  const void* tied_bytes_line(std::size_t prefix_len, const K& stored) const noexcept {
    if constexpr (by_bytes && KInliner::is_heap_byte_string && Width != 0) {
      const ByteView query = query_bytes();
      const std::size_t equal_len = prefix_len + Width;
      if (query.size <= equal_len) return nullptr;
      const ByteView bytes = KInliner::bytes(stored);
      const std::size_t both_len = query.size < bytes.size ? query.size : bytes.size;
      if (equal_len >= both_len) return nullptr;
      std::size_t end = equal_len + window_len < both_len ? equal_len + window_len : both_len;
      const std::size_t offset = end >= window_len ? end - window_len : 0;
      return bytes.data + offset;
    } else {
      return nullptr;
    }
  }

 private:
  const Q* key_;
  /// The query's bytes when both types are byte strings.
  ByteView bytes_{nullptr, 0};
};

}  // namespace shoal::detail
