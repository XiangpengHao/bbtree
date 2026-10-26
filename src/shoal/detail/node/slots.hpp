// Slot arrays inside a node page (`tree/node/slots.rs`). Apart from whole
// pages, this is the only code that moves keys, values and edges within and
// between nodes (D9, D12, D14, D29). Keys and values move as bytes: they are
// trivially relocatable (PORTING.md, P2).
//
// - `Slots<T>`: a fixed-capacity array of slots, side by side or a stride
//   apart.
// - `NodeKeys<K>`: a node's keys lent in place, and the two binary searches
//   over a key list (D19).
// - `KindShape`: one node kind's plan and layout constants.
// - `KeyColumns`: the arrays a node keeps beside its keys, inline keys and
//   hash tags, which every move moves alike.
// - `KeySlots`: a node's keys with their columns.
// - `SeparatorSlots`: an internal node's separators, keys or windows (D49).
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <new>
#include <type_traits>
#include <utility>

#include "../config.hpp"
#include "inline_key_search.hpp"
#include "inline_keys.hpp"
#include "plan.hpp"

namespace shoal::detail {

/// Where `capacity` slots of `T` sit in a page: slot `i` at `stride * i` bytes
/// past `anchor`. The slots of an array of their own sit side by side; the
/// keys and values of a leaf of entries are fields of its entries, an entry's
/// size apart (D29).
///
/// A `Slots` neither owns nor borrows its slots and tracks no length: which
/// slots are initialized is the caller's business. Objects move as bytes; a
/// zero-sized `T` (an empty type, as Rust's `()`) takes no room and moves
/// nothing.
template <class T>
class Slots {
 public:
  static constexpr bool zero_sized = std::is_empty_v<T>;
  static_assert(!zero_sized || std::is_trivially_copyable_v<T>,
                "an empty value type is trivially copyable");

  Slots(void* anchor, std::size_t capacity, std::size_t stride) noexcept
      : anchor_(static_cast<std::byte*>(anchor)), capacity_(capacity), stride_(stride) {}

  std::size_t capacity() const noexcept { return capacity_; }
  std::size_t stride() const noexcept { return stride_; }

  /// Whether the slots sit side by side, with nothing between them.
  bool is_contiguous() const noexcept { return stride_ == sizeof(T); }

  /// A pointer to slot `index`.
  T* slot(std::size_t index) const noexcept {
    SHOAL_ASSERT(index < capacity_);
    return std::launder(reinterpret_cast<T*>(anchor_ + index * stride_));
  }

  /// Slot 0, raw: where a vector load over slots side by side starts.
  const T* data() const noexcept { return reinterpret_cast<const T*>(anchor_); }

  /// Takes the object in slot `index` out; the slot holds nothing after.
  T take(std::size_t index) const noexcept {
    T* source = slot(index);
    T object(std::move(*source));
    source->~T();
    return object;
  }

  /// Puts `object` into slot `index`, which holds nothing.
  void put(std::size_t index, T&& object) const noexcept {
    ::new (static_cast<void*>(slot(index))) T(std::move(object));
  }

  /// Replaces the object in slot `index` with `object`, and returns the old.
  T replace(std::size_t index, T&& object) const noexcept {
    T old = take(index);
    put(index, std::move(object));
    return old;
  }

  /// Writes `object` into slot `index`, first shifting slots `index..len` up
  /// by one. `index <= len < capacity`; slots `index..len` are initialized,
  /// and `index..len + 1` are afterwards.
  void insert(std::size_t index, std::size_t len, T&& object) const noexcept {
    SHOAL_ASSERT(index <= len && len < capacity_);
    if constexpr (!zero_sized) {
      if (is_contiguous()) {
        if (index < len) {
          std::memmove(anchor_ + (index + 1) * sizeof(T), anchor_ + index * sizeof(T),
                       (len - index) * sizeof(T));
        }
      } else {
        for (std::size_t from = len; from > index; --from) move_slot(from - 1, from);
      }
    }
    put(index, std::move(object));
  }

  /// Takes slot `index` out, then shifts slots `index + 1..len` down by one.
  /// `index < len <= capacity`; slots `index..len` are initialized, and
  /// `index..len - 1` afterwards.
  T remove(std::size_t index, std::size_t len) const noexcept {
    SHOAL_ASSERT(index < len && len <= capacity_);
    T removed = take(index);
    if constexpr (!zero_sized) {
      if (is_contiguous()) {
        std::memmove(anchor_ + index * sizeof(T), anchor_ + (index + 1) * sizeof(T),
                     (len - index - 1) * sizeof(T));
      } else {
        for (std::size_t from = index + 1; from < len; ++from) move_slot(from, from - 1);
      }
    }
    return removed;
  }

  /// Moves slots `start..start + count` into slots `dest_start..` of `dest`,
  /// keeping their order. The runs don't overlap: they are in different
  /// pages. The source slots hold nothing afterwards.
  void move_run_to(std::size_t start, std::size_t count, Slots dest,
                   std::size_t dest_start) const noexcept {
    SHOAL_ASSERT(start + count <= capacity_ && dest_start + count <= dest.capacity_);
    if constexpr (!zero_sized) {
      if (is_contiguous() && dest.is_contiguous()) {
        std::memcpy(dest.anchor_ + dest_start * sizeof(T), anchor_ + start * sizeof(T),
                    count * sizeof(T));
      } else {
        for (std::size_t offset = 0; offset < count; ++offset) {
          std::memcpy(dest.anchor_ + (dest_start + offset) * dest.stride_,
                      anchor_ + (start + offset) * stride_, sizeof(T));
        }
      }
    }
  }

  /// Shifts the whole strides of slots `index..len` up by one slot, with the
  /// other fields between them: the entries of a node whose first fields are
  /// these slots (D29). Slot `index`'s stride holds stale bytes afterwards.
  void shift_strides_up(std::size_t index, std::size_t len) const noexcept {
    SHOAL_ASSERT(index <= len && len < capacity_);
    if (index == len) return;
    std::byte* first = anchor_ + index * stride_;
    std::memmove(first + stride_, first, (len - index) * stride_);
  }

  /// Shifts the whole strides of slots `index + 1..len` down by one slot,
  /// over the stride of slot `index`, which holds nothing to drop.
  void shift_strides_down(std::size_t index, std::size_t len) const noexcept {
    SHOAL_ASSERT(index < len && len <= capacity_);
    std::byte* first = anchor_ + index * stride_;
    std::memmove(first, first + stride_, (len - index - 1) * stride_);
  }

  /// Sets every byte of slots side by side to `byte`.
  void fill_bytes(std::uint8_t byte) const noexcept {
    SHOAL_ASSERT(is_contiguous());
    std::memset(anchor_, byte, capacity_ * sizeof(T));
  }

 private:
  std::byte* anchor_;
  std::size_t capacity_;
  std::size_t stride_;

  /// Moves slot `from` into slot `to`, distinct, as bytes.
  void move_slot(std::size_t from, std::size_t to) const noexcept {
    std::memcpy(anchor_ + to * stride_, anchor_ + from * stride_, sizeof(T));
  }
};

/// Where a search found a key, or where it belongs, as Rust's
/// `Result<usize, usize>`.
struct SearchResult {
  std::size_t index;
  bool found;

  static constexpr SearchResult at(std::size_t index) noexcept { return {index, true}; }
  static constexpr SearchResult edge(std::size_t index) noexcept { return {index, false}; }
  SearchResult offset_by(std::size_t start) const noexcept { return {start + index, found}; }
};

/// A node's keys `0..len`, in order, lent in place: side by side, or each the
/// key of an entry in a leaf of entries (D29).
template <class K>
class NodeKeys {
 public:
  NodeKeys(const std::byte* anchor, std::size_t stride, std::size_t len) noexcept
      : anchor_(anchor), stride_(stride), len_(len) {}

  std::size_t len() const noexcept { return len_; }

  const K& operator[](std::size_t index) const noexcept {
    SHOAL_ASSERT(index < len_);
    return *std::launder(reinterpret_cast<const K*>(anchor_ + index * stride_));
  }

  NodeKeys slice(std::size_t start, std::size_t end) const noexcept {
    SHOAL_CHECK(start <= end && end <= len_);
    return NodeKeys(anchor_ + start * stride_, stride_, end - start);
  }

 private:
  const std::byte* anchor_;
  std::size_t stride_;
  std::size_t len_;
};

/// The two binary searches over a key list, a node's own keys or the head
/// keys an internal node's separators lead to (D19). A list has `len()`,
/// `operator[]` and `slice`.
struct KeySearcher {
  /// Key `i` if it equals `query`, otherwise the edge where it belongs, by a
  /// binary search with no early exit, each step a conditional move. In
  /// bounds whatever `operator<` answers.
  template <class List, class Q>
  SHOAL_ALWAYS_INLINE static SearchResult branchless(List keys, const Q& query) noexcept {
    std::size_t size = keys.len();
    if (size == 0) return SearchResult::edge(0);
    std::size_t base = 0;
    while (size > 1) {
      const std::size_t half = size / 2;
      const std::size_t mid = base + half;
      const bool probe_is_greater = query < keys[mid];
#if defined(__clang__)
      base = __builtin_unpredictable(probe_is_greater) ? base : mid;
#else
      base = probe_is_greater ? base : mid;
#endif
      size -= half;
    }
    switch (Order::cmp(query, keys[base])) {
      case Ordering::Equal: return SearchResult::at(base);
      case Ordering::Greater: return SearchResult::edge(base + 1);
      default: return SearchResult::edge(base);
    }
  }

  /// Key `i` if `cmp_key(key i)` says it is the one searched for, otherwise
  /// the edge where it belongs, by a binary search that stops at an equal
  /// key. `cmp_key(probe)` orders the query against `probe`. In bounds
  /// whatever it answers.
  template <class List, class CmpKey>
  SHOAL_ALWAYS_INLINE static SearchResult early_exit(List keys, CmpKey cmp_key) noexcept {
    std::size_t low = 0, high = keys.len();
    while (low < high) {
      const std::size_t mid = low + (high - low) / 2;
      switch (cmp_key(keys[mid])) {
        case Ordering::Less: high = mid; break;
        case Ordering::Equal: return SearchResult::at(mid);
        case Ordering::Greater: low = mid + 1; break;
      }
    }
    return SearchResult::edge(low);
  }

  /// `keys` searched for `query` as `how` says, comparing with `cmp_key` in
  /// the early-exit search.
  template <KeySearch How, class List, class Q, class CmpKey>
  SHOAL_ALWAYS_INLINE static SearchResult search(List keys, const Q& query,
                                                  CmpKey cmp_key) noexcept {
    if constexpr (How == KeySearch::Branchless) {
      return branchless(keys, query);
    } else {
      return early_exit(keys, cmp_key);
    }
  }
};

/// One node kind's constants: its plan, its key area and its columns, for a
/// map with keys `K`, values `V` and strategy `S`.
template <class K, class V, class S, bool IsLeaf>
struct KindShape {
  using Shape = NodeShape<K, V, S>;
  static constexpr bool is_leaf = IsLeaf;
  static constexpr NodePlan plan = IsLeaf ? Shape::leaf_plan : Shape::internal_plan;
  static constexpr KeyArea keys =
      IsLeaf ? Shape::leaf_layout.keys : Shape::internal_layout.keys;
  static constexpr std::size_t capacity = keys.capacity;
  static constexpr std::size_t width = plan.inline_width;
  static constexpr bool has_inline_keys = width != 0;
  static constexpr std::size_t hash_tags_offset =
      IsLeaf && plan.hash_tags ? Shape::leaf_layout.hash_tags_offset : 0;
  static constexpr bool has_hash_tags = hash_tags_offset != 0;
  static constexpr Layout page = IsLeaf ? Shape::leaf_layout.page : Shape::internal_layout.page;
  static constexpr std::size_t min_len = Shape::min_len(capacity);

  using Slot = InlineSlot<has_inline_keys ? width : 8>;
  using Word = typename Slot::Word;
  using InlineKeys = NodeInlineKeys<has_inline_keys ? width : 8, capacity>;
};

/// What the columns of a node hold for one key.
struct KeyCells {
  /// The first bytes of the key's window, which the inline keys keep.
  InlineWord inline_key;
  /// The key's hash tag, which a node that keeps them stores.
  std::uint8_t hash_tag;
};

/// The arrays a node of kind `Kind` keeps beside its keys, one slot per key:
/// its inline keys (D18) and, in a leaf whose plan keeps them, hash tags
/// (D36). Each operation mirrors the `Slots` operation of its name.
template <class Kind>
class KeyColumns {
 public:
  using Word = typename Kind::Word;
  static constexpr std::size_t capacity = Kind::capacity;

  explicit KeyColumns(std::byte* page) noexcept : page_(page) {}

  Word* inline_key_slots() const noexcept {
    return reinterpret_cast<Word*>(page_ + Kind::keys.inline_keys_offset);
  }
  std::uint8_t* hash_tag_slots() const noexcept {
    return reinterpret_cast<std::uint8_t*>(page_ + Kind::hash_tags_offset);
  }

  /// Inline key `index`, as kept.
  InlineWord read_inline_key(std::size_t index) const noexcept {
    SHOAL_ASSERT(index < capacity);
    return Kind::Slot::inline_key(inline_key_slots()[index]);
  }

  void write_inline_key(std::size_t index, InlineWord inline_key) const noexcept {
    SHOAL_ASSERT(index < capacity);
    inline_key_slots()[index] = Kind::Slot::of_inline_key(inline_key);
  }

  /// Fills every slot of a node that holds no keys yet: each inline key with
  /// the greatest (invariant 4), each hash tag with 0.
  void initialize() const noexcept {
    if constexpr (Kind::has_inline_keys) {
      std::memset(inline_key_slots(), 0xff, capacity * sizeof(Word));
    }
    if constexpr (Kind::has_hash_tags) std::memset(hash_tag_slots(), 0, capacity);
  }

  /// Gives slots `from..to`, which the node no longer holds keys in, the
  /// greatest inline key again (invariant 4). Their hash tags stay stale: a
  /// search masks out every slot outside its candidates (invariant 5).
  void mark_vacant(std::size_t from, std::size_t to) const noexcept {
    if constexpr (Kind::has_inline_keys) {
      for (std::size_t index = from; index < to; ++index) inline_key_slots()[index] = static_cast<Word>(~Word{0});
    }
  }

  void write(std::size_t index, KeyCells cells) const noexcept {
    if constexpr (Kind::has_inline_keys) write_inline_key(index, cells.inline_key);
    if constexpr (Kind::has_hash_tags) hash_tag_slots()[index] = cells.hash_tag;
  }

  void insert(std::size_t index, std::size_t len, KeyCells cells) const noexcept {
    if constexpr (Kind::has_inline_keys) {
      inline_keys().insert(index, len, Kind::Slot::of_inline_key(cells.inline_key));
    }
    if constexpr (Kind::has_hash_tags) tags().insert(index, len, std::uint8_t(cells.hash_tag));
  }

  void remove(std::size_t index, std::size_t len) const noexcept {
    if constexpr (Kind::has_inline_keys) inline_keys().remove(index, len);
    if constexpr (Kind::has_hash_tags) tags().remove(index, len);
  }

  void move_run_to(std::size_t start, std::size_t count, KeyColumns dest,
                   std::size_t dest_start) const noexcept {
    if constexpr (Kind::has_inline_keys) {
      inline_keys().move_run_to(start, count, dest.inline_keys(), dest_start);
    }
    if constexpr (Kind::has_hash_tags) tags().move_run_to(start, count, dest.tags(), dest_start);
  }

 private:
  std::byte* page_;

  Slots<Word> inline_keys() const noexcept {
    return Slots<Word>(inline_key_slots(), capacity, sizeof(Word));
  }
  Slots<std::uint8_t> tags() const noexcept {
    return Slots<std::uint8_t>(hash_tag_slots(), capacity, 1);
  }
};

/// A node's key slots and the columns beside them, for a node of kind `Kind`
/// (a leaf, or an internal node that keeps keys). Every operation moves a key
/// and its cells together, so that inline key `i` is always key `i`'s. The
/// node stores its keys' windows at its prefix length; a key written in must
/// come with its window there.
template <class K, class Kind>
class KeySlots {
 public:
  KeySlots(std::byte* page, std::size_t prefix_len) noexcept
      : page_(page), prefix_len_(prefix_len) {}

  Slots<K> keys() const noexcept {
    return Slots<K>(page_ + Kind::keys.keys_offset, Kind::capacity, Kind::keys.key_stride);
  }
  KeyColumns<Kind> columns() const noexcept { return KeyColumns<Kind>(page_); }
  std::size_t prefix_len() const noexcept { return prefix_len_; }

  /// Key `index`'s window at the node's prefix length, as the node keeps it.
  Window window(std::size_t index) const noexcept {
    if constexpr (Kind::has_inline_keys) {
      return Window(prefix_len_, columns().read_inline_key(index));
    } else {
      return Window::none();
    }
  }

  /// Takes key `index` and its window out.
  WindowedKey<K> read(std::size_t index) const noexcept {
    const Window key_window = window(index);
    return WindowedKey<K>(keys().take(index), key_window);
  }

  /// Writes `key` and its cells into slot `index`, which holds nothing.
  void write(std::size_t index, WindowedKey<K>&& key) const noexcept {
    const KeyCells cells = cells_of(key);
    keys().put(index, std::move(key.key()));
    columns().write(index, cells);
  }

  /// Replaces key `index` and its cells, and returns the old key.
  WindowedKey<K> replace(std::size_t index, WindowedKey<K>&& key) const noexcept {
    WindowedKey<K> old = read(index);
    write(index, std::move(key));
    return old;
  }

  void insert(std::size_t index, std::size_t len, WindowedKey<K>&& key) const noexcept {
    const KeyCells cells = cells_of(key);
    keys().insert(index, len, std::move(key.key()));
    columns().insert(index, len, cells);
  }

  /// `insert` in a node of entries whose first fields are the keys (D29): it
  /// shifts entries `index..len` up whole, in one copy, then writes the key.
  /// The caller writes what pairs with it.
  void insert_shifting_entries(std::size_t index, std::size_t len,
                               WindowedKey<K>&& key) const noexcept {
    const KeyCells cells = cells_of(key);
    keys().shift_strides_up(index, len);
    keys().put(index, std::move(key.key()));
    columns().insert(index, len, cells);
  }

  WindowedKey<K> remove(std::size_t index, std::size_t len) const noexcept {
    const Window key_window = window(index);
    K key = keys().remove(index, len);
    columns().remove(index, len);
    return WindowedKey<K>(std::move(key), key_window);
  }

  /// `remove` in a node of entries whose first fields are the keys (D29): it
  /// takes key `index` out, then shifts entries `index + 1..len` down whole.
  /// The caller has taken out what pairs with key `index` first.
  WindowedKey<K> remove_shifting_entries(std::size_t index, std::size_t len) const noexcept {
    const Window key_window = window(index);
    K key = keys().take(index);
    columns().remove(index, len);
    keys().shift_strides_down(index, len);
    return WindowedKey<K>(std::move(key), key_window);
  }

  /// Moves keys `start..start + count`, with their cells, into `dest`, a node
  /// of the same map and kind at this node's prefix length.
  void move_run_to(std::size_t start, std::size_t count, KeySlots dest,
                   std::size_t dest_start) const noexcept {
    SHOAL_ASSERT(prefix_len_ == dest.prefix_len_);
    keys().move_run_to(start, count, dest.keys(), dest_start);
    columns().move_run_to(start, count, dest.columns(), dest_start);
  }

 private:
  std::byte* page_;
  std::size_t prefix_len_;

  /// What the columns hold for `key`, whose window is at this node's prefix.
  KeyCells cells_of(const WindowedKey<K>& key) const noexcept {
    SHOAL_ASSERT(!Kind::has_inline_keys || key.window().prefix_len() == prefix_len_);
    std::uint8_t hash_tag = 0;
    if constexpr (Kind::has_hash_tags) hash_tag = KeyInliner<K>::hash_tag(key.key());
    return KeyCells{key.window().inline_key(), hash_tag};
  }
};

/// An internal node's separators (D17): its keys without inline keys, their
/// windows with them (D49). Every operation mirrors the `KeySlots` operation
/// of its name, on separators.
template <class K, class Kind>
class SeparatorSlots {
 public:
  static constexpr bool with_keys = !Kind::has_inline_keys;
  using Sep = Separator<K, with_keys>;

  SeparatorSlots(std::byte* page, std::size_t prefix_len) noexcept
      : page_(page), prefix_len_(prefix_len) {}

  /// Separator `index`'s window at the node's prefix length, as kept.
  Window window(std::size_t index) const noexcept {
    if constexpr (with_keys) {
      return Window::none();
    } else {
      return Window(prefix_len_, columns().read_inline_key(index));
    }
  }

  Sep read(std::size_t index) const noexcept {
    if constexpr (with_keys) {
      return Sep(key_slots().read(index));
    } else {
      return Sep(window(index));
    }
  }

  void write(std::size_t index, Sep&& separator) const noexcept {
    if constexpr (with_keys) {
      key_slots().write(index, std::move(separator).into_windowed_key());
    } else {
      columns().write(index, cells_of(separator.window()));
    }
  }

  Sep replace(std::size_t index, Sep&& separator) const noexcept {
    Sep old = read(index);
    write(index, std::move(separator));
    return old;
  }

  void insert(std::size_t index, std::size_t len, Sep&& separator) const noexcept {
    if constexpr (with_keys) {
      key_slots().insert(index, len, std::move(separator).into_windowed_key());
    } else {
      columns().insert(index, len, cells_of(separator.window()));
    }
  }

  Sep remove(std::size_t index, std::size_t len) const noexcept {
    if constexpr (with_keys) {
      return Sep(key_slots().remove(index, len));
    } else {
      const Window separator_window = window(index);
      columns().remove(index, len);
      return Sep(separator_window);
    }
  }

  void move_run_to(std::size_t start, std::size_t count, SeparatorSlots dest,
                   std::size_t dest_start) const noexcept {
    SHOAL_ASSERT(prefix_len_ == dest.prefix_len_);
    if constexpr (with_keys) {
      key_slots().move_run_to(start, count, dest.key_slots(), dest_start);
    } else {
      columns().move_run_to(start, count, dest.columns(), dest_start);
    }
  }

 private:
  std::byte* page_;
  std::size_t prefix_len_;

  KeyColumns<Kind> columns() const noexcept { return KeyColumns<Kind>(page_); }
  KeySlots<K, Kind> key_slots() const noexcept { return KeySlots<K, Kind>(page_, prefix_len_); }

  KeyCells cells_of(Window separator_window) const noexcept {
    SHOAL_ASSERT(separator_window.prefix_len() == prefix_len_);
    return KeyCells{separator_window.inline_key(), 0};
  }
};

}  // namespace shoal::detail
