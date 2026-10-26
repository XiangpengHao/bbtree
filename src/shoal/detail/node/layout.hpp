// Compile-time byte layout of the node pages of each kind, without and with
// inline keys of each width (`tree/node/layout.rs`).
//
//     leaf      | header | k0 k1 … k_n | head v0 v1 … v_n |
//     internal  | header | k0 k1 … k_n | e0 e1 … e_n+1     |
//
//     with inline keys:
//     leaf      | header | w0 … w_n | (hk head) (k0 v0) (k1 v1) … (k_n v_n) |
//     internal  | header | w0 … w_n | e0 e1 … e_n+1                         |
//               0        inline_keys_offset
//
// - Every layout has a header at offset 0, and one more trailing slot than
//   keys: a leaf's head and values, or an internal node's edges. Trailing slot
//   0 is the head, or edge 0, and slot `i + 1` goes with key `i`.
// - Without inline keys, the keys are an array of their own, at the same
//   offset in both kinds, and the trailing slots an array after them.
// - With inline keys, `capacity` inline keys follow the header, from offset 8
//   (16 in a latched tree). Leaves pair their keys and values in entries;
//   entry 0 holds the head and its separator's key (OG-D49), entry `i + 1` key
//   `i` and value `i`. Power-of-two entries up to a cache line start at a
//   multiple of their size (OG-D34). Internal nodes keep no keys, only inline
//   keys and edges.
// - A leaf whose plan keeps hash tags has a byte per key after its inline
//   keys, where its layout has room for them (OG-D36).
// - Each kind is the smallest multiple of its page size that holds 12 keys.
//
// Every layout is computed at compile time, and every invariant the node code
// relies on is checked then: a layout that breaks one fails to compile.
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <stdexcept>

#include "../../allocator.hpp"
#include "../../inline_key.hpp"

namespace shoal::detail {

/// The header at the start of every node page.
struct NodeHeader {
  /// The number of keys.
  std::uint16_t len;
  /// Whether a leaf holds a head (OG-D17); its head slot is initialized
  /// exactly when this is set, and so is its head key slot with inline keys
  /// (OG-D49).
  bool has_head;
};

/// The header of a node with inline keys: the plain header, then how many
/// leading bytes the node's fences share, which its inline keys skip.
struct InlineKeyNodeHeader {
  NodeHeader header;
  std::uint16_t prefix_len;

  static constexpr std::size_t max_prefix_len = UINT16_MAX;
};

/// The header of a node of a latched tree: the inline-key header, then the
/// node's latch word, which only atomics access.
struct LatchedNodeHeader {
  InlineKeyNodeHeader header;
  std::atomic<std::uint64_t> latch;
};

static_assert(sizeof(NodeHeader) == 4 && alignof(NodeHeader) == 2);
static_assert(sizeof(InlineKeyNodeHeader) == 6 && offsetof(InlineKeyNodeHeader, prefix_len) == 4);
static_assert(sizeof(LatchedNodeHeader) == 16 && offsetof(LatchedNodeHeader, latch) == 8);

/// Where the keys of one kind of node sit, and how many fit.
struct KeyArea {
  /// The most keys the node holds.
  std::size_t capacity;
  /// Where `capacity` inline keys start, in a layout with inline keys; 0
  /// without (no header ends at 0).
  std::size_t inline_keys_offset;
  /// Where key 0 is; in an internal node that keeps no keys, where they would
  /// start, which nothing reads.
  std::size_t keys_offset;
  /// Bytes from one key to the next: a key's size, or an entry's with inline
  /// keys; 0 in an internal node that keeps no keys.
  std::size_t key_stride;

  constexpr bool has_inline_keys() const { return inline_keys_offset != 0; }
};

/// The layout of a leaf's page.
struct LeafLayout {
  Layout page;
  KeyArea keys;
  /// Where the head is, trailing slot 0; value `i` is trailing slot `i + 1`.
  std::size_t head_offset;
  /// Bytes from the head to value 0, and from one value to the next.
  std::size_t value_stride;
  /// Where a dense array of hash tags is, or 0 for none.
  std::size_t hash_tags_offset;
  /// Where the head's key is, in entry 0 before the head, with inline keys;
  /// 0 without.
  std::size_t head_key_offset;

  constexpr std::size_t values_offset() const { return head_offset + value_stride; }
  constexpr bool has_hash_tags() const { return hash_tags_offset != 0; }
};

/// The layout of an internal node's page.
struct InternalLayout {
  Layout page;
  KeyArea keys;
  /// Where edge 0 is.
  std::size_t edges_offset;
  /// Bytes from one edge to the next.
  std::size_t edge_stride;
};

/// What a node's header holds besides its plain fields: a latch word in a
/// latched tree, and after it a leaf's annex, of `annex_size` bytes.
struct HeaderShape {
  bool latched;
  std::size_t annex_size;
  std::size_t annex_align;

  constexpr std::size_t node_header_size() const {
    return latched ? sizeof(LatchedNodeHeader) : sizeof(InlineKeyNodeHeader);
  }
  /// Where the annex starts: after the node header, aligned.
  constexpr std::size_t annex_offset() const {
    return (node_header_size() + annex_align - 1) / annex_align * annex_align;
  }
  /// The whole header's size, its annex's included.
  constexpr std::size_t size() const {
    return annex_size == 0 ? node_header_size() : annex_offset() + annex_size;
  }
};

/// What a node holds after each of its keys, and once more: a leaf's values
/// and head, or an internal node's edges.
struct TrailingSlot {
  std::size_t size;
  std::size_t align;
  /// Inline-key leaves pair keys with values; internal edges stay dense.
  bool pair_with_key;

  constexpr bool paired(std::size_t inline_key_size) const {
    return pair_with_key && inline_key_size > 0;
  }
};

/// One kind's layout, before it is named a leaf's or an internal node's.
struct KindLayout {
  Layout page;
  KeyArea keys;
  std::size_t trailing_offset;
  std::size_t trailing_stride;
};

/// The layout arithmetic of `tree/node/layout.rs`, for keys of `key_size`
/// bytes aligned to `key_align`, every function evaluated at compile time.
class NodeLayoutMath {
 public:
  /// Entries up to this size start at a multiple of it (OG-D34).
  static constexpr std::size_t cache_line_size = 64;
  /// Every node kind holds at least this many keys (OG-D13).
  static constexpr std::size_t min_capacity = 12;
  /// Upper bound on any node's capacity: indexes are `u16`, and this keeps
  /// search work bounded in nodes of nearly zero-sized slots.
  static constexpr std::size_t max_capacity = 1023;

  constexpr NodeLayoutMath(std::size_t key_size, std::size_t key_align)
      : key_size_(key_size), key_align_(key_align) {}

  static constexpr std::size_t next_multiple_of(std::size_t value, std::size_t step) {
    return (value + step - 1) / step * step;
  }

  static constexpr bool is_power_of_two(std::size_t value) {
    return value != 0 && (value & (value - 1)) == 0;
  }

  static constexpr std::size_t max(std::size_t a, std::size_t b) { return a > b ? a : b; }
  static constexpr std::size_t min(std::size_t a, std::size_t b) { return a < b ? a : b; }

  /// `size`, a page size of a node config, once checked: a power of two, and
  /// at least `min`.
  static constexpr std::size_t page_size(std::size_t size, std::size_t min_size) {
    require(is_power_of_two(size) && size >= min_size,
            "a NodeConfig page size is a power of two, at least its minimum");
    return size;
  }

  /// Where the inline keys start in a node that has them: after its header.
  static constexpr std::size_t inline_keys_offset(HeaderShape header) {
    return next_multiple_of(header.size(), alignof(InlineWord));
  }

  /// A leaf's layout, with inline keys of `inline_key_size` bytes (none if
  /// 0), with hash tags if `hash_tags` and there is room. Where tags fit only
  /// by also taking the head key's place, the leaf gives up an entry to keep
  /// them; where they never fit, it keeps none (OG-D36).
  constexpr LeafLayout compute_leaf(std::size_t page_size, std::size_t inline_key_size,
                                    bool hash_tags, HeaderShape header,
                                    TrailingSlot value) const {
    const KindLayout layout = kind_layout(page_size, header, inline_key_size, 0, value, true);
    if (inline_key_size == 0 || !hash_tags) return leaf_of(layout, 0, value);
    const std::size_t tags_start = columns_end(header, inline_key_size, layout.keys.capacity);
    const std::size_t tags_end = tags_start + layout.keys.capacity;
    if (tags_end <= entries_offset(layout, value)) return leaf_of(layout, tags_start, value);
    if (tags_end <= layout.trailing_offset) {
      const KindLayout tagged = kind_layout(page_size, header, inline_key_size, 1, value, true);
      return leaf_of(tagged, columns_end(header, inline_key_size, tagged.keys.capacity), value);
    }
    return leaf_of(layout, 0, value);
  }

  /// An internal node's layout: its keys, if it keeps them, and its edges.
  /// With inline keys it keeps no keys (OG-D49), on pages of
  /// `inline_page_size`; without, of `plain_page_size`.
  constexpr InternalLayout compute_internal(std::size_t inline_page_size,
                                            std::size_t plain_page_size,
                                            std::size_t inline_key_size, HeaderShape header,
                                            TrailingSlot edge) const {
    const bool keeps_keys = inline_key_size == 0;
    const std::size_t page = keeps_keys ? plain_page_size : inline_page_size;
    const KindLayout layout = kind_layout(page, header, inline_key_size, 0, edge, keeps_keys);
    return InternalLayout{layout.page, layout.keys, layout.trailing_offset,
                          layout.trailing_stride};
  }

 private:
  std::size_t key_size_;
  std::size_t key_align_;

  static constexpr void require(bool condition, const char* what) {
    if (!condition) throw std::logic_error(what);
  }

  constexpr LeafLayout leaf_of(KindLayout layout, std::size_t hash_tags_offset,
                               TrailingSlot value) const {
    const std::size_t head_key_offset =
        layout.keys.has_inline_keys() ? entries_offset(layout, value) : 0;
    return LeafLayout{layout.page,           layout.keys,      layout.trailing_offset,
                      layout.trailing_stride, hash_tags_offset, head_key_offset};
  }

  /// Where entry 0 is in a leaf of paired entries: its key's place, which the
  /// head's key takes (OG-D49), then the head.
  constexpr std::size_t entries_offset(KindLayout layout, TrailingSlot value) const {
    return layout.trailing_offset - entry_trailing_offset(value);
  }

  /// A node is aligned to its page size, and to its keys, if it keeps them,
  /// and trailing slots.
  constexpr std::size_t node_align(std::size_t page_size, std::size_t trailing_align,
                                   bool keeps_keys) const {
    return max(page_size, max(keeps_keys ? key_align_ : 1, trailing_align));
  }

  /// Where the keys, or the entries, could start, before rounding up to their
  /// alignment: after the header and the arrays beside the keys.
  static constexpr std::size_t columns_end(HeaderShape header, std::size_t column_size,
                                           std::size_t capacity) {
    return column_size > 0 ? inline_keys_offset(header) + capacity * column_size
                           : sizeof(NodeHeader);
  }

  /// Where an entry's trailing slot is: after its key, aligned.
  constexpr std::size_t entry_trailing_offset(TrailingSlot trailing) const {
    return next_multiple_of(key_size_, trailing.align);
  }

  constexpr std::size_t entry_align(TrailingSlot trailing) const {
    return max(key_align_, trailing.align);
  }

  /// An entry's size: its key, then its trailing slot, as a struct of them.
  constexpr std::size_t entry_size(TrailingSlot trailing) const {
    return next_multiple_of(entry_trailing_offset(trailing) + trailing.size,
                            entry_align(trailing));
  }

  /// The minimum bytes a node of `capacity` keys needs.
  constexpr std::size_t bytes_needed(HeaderShape header, std::size_t column_size,
                                     TrailingSlot trailing, bool keeps_keys,
                                     std::size_t capacity) const {
    const std::size_t keys_start = columns_end(header, column_size, capacity);
    if (trailing.paired(column_size)) {
      return next_multiple_of(keys_start, entry_align(trailing)) +
             (capacity + 1) * entry_size(trailing);
    }
    if (keeps_keys) {
      return next_multiple_of(next_multiple_of(keys_start, key_align_) + capacity * key_size_,
                              trailing.align) +
             (capacity + 1) * trailing.size;
    }
    return next_multiple_of(keys_start, trailing.align) + (capacity + 1) * trailing.size;
  }

  /// The most bytes the inline keys of a node of pages of `page_size` take.
  static constexpr std::size_t max_inline_keys_size(std::size_t page_size) {
    return max(page_size, 1024);
  }

  static constexpr std::size_t capacity_cap(std::size_t page_size, std::size_t inline_key_size) {
    return inline_key_size == 0
               ? max_capacity
               : min(max_capacity, max_inline_keys_size(page_size) / inline_key_size);
  }

  /// The largest capacity, up to `cap`, whose `bytes_needed` fit in
  /// `node_size` bytes.
  constexpr std::size_t capacity_in(HeaderShape header, std::size_t node_size, std::size_t cap,
                                    std::size_t column_size, TrailingSlot trailing,
                                    bool keeps_keys) const {
    const std::size_t size_per_key =
        column_size + (trailing.paired(column_size) ? entry_size(trailing)
                       : keeps_keys                 ? key_size_ + trailing.size
                                                    : trailing.size);
    std::size_t capacity = size_per_key == 0 ? cap : min(cap, node_size / size_per_key);
    while (bytes_needed(header, column_size, trailing, keeps_keys, capacity) > node_size) {
      --capacity;
    }
    return capacity;
  }

  /// The smallest node, in steps of `page_size`, that holds `min_capacity`
  /// keys, with as many keys as fit in it.
  constexpr KindLayout kind_layout(std::size_t page_size, HeaderShape header,
                                   std::size_t inline_key_size, std::size_t hash_tag_size,
                                   TrailingSlot trailing, bool keeps_keys) const {
    const std::size_t column_size = inline_key_size + hash_tag_size;
    const std::size_t align = node_align(page_size, trailing.align, keeps_keys);
    const std::size_t size = next_multiple_of(
        bytes_needed(header, column_size, trailing, keeps_keys, min_capacity), align);
    const std::size_t capacity = capacity_in(header, size, capacity_cap(page_size, inline_key_size),
                                             column_size, trailing, keeps_keys);
    const std::size_t keys_start = columns_end(header, column_size, capacity);
    std::size_t keys_offset = 0, key_stride = 0, trailing_offset = 0, trailing_stride = 0;
    if (trailing.paired(inline_key_size)) {
      const std::size_t entry = entry_size(trailing);
      const std::size_t entries_align = is_power_of_two(entry) && entry <= cache_line_size
                                            ? entry
                                            : entry_align(trailing);
      const std::size_t entries = next_multiple_of(keys_start, entries_align);
      keys_offset = entries + entry;
      key_stride = entry;
      trailing_offset = entries + entry_trailing_offset(trailing);
      trailing_stride = entry;
    } else if (keeps_keys) {
      keys_offset = next_multiple_of(keys_start, key_align_);
      key_stride = key_size_;
      trailing_offset = next_multiple_of(keys_offset + capacity * key_size_, trailing.align);
      trailing_stride = trailing.size;
    } else {
      keys_offset = keys_start;
      key_stride = 0;
      trailing_offset = next_multiple_of(keys_start, trailing.align);
      trailing_stride = trailing.size;
    }
    const KindLayout layout{
        Layout{size, align},
        KeyArea{capacity, inline_key_size > 0 ? inline_keys_offset(header) : 0, keys_offset,
                key_stride},
        trailing_offset, trailing_stride};
    check_invariants(layout, page_size, header, inline_key_size, column_size, trailing,
                     keeps_keys);
    return layout;
  }

  /// What every layout needs: whole pages, a capacity in bounds, the header,
  /// inline keys and hash tags, keys and trailing slots in place, in that
  /// order, aligned, in the page and clear of each other.
  constexpr void check_invariants(KindLayout layout, std::size_t page_size, HeaderShape header,
                                  std::size_t inline_key_size, std::size_t column_size,
                                  TrailingSlot trailing, bool keeps_keys) const {
    const Layout page = layout.page;
    const KeyArea keys = layout.keys;
    const std::size_t size = page.size, align = page.align, capacity = keys.capacity;
    require(is_power_of_two(align), "a node is aligned to a power of two");
    require(size % page_size == 0 && size % align == 0, "a node is whole pages");
    require(capacity >= min_capacity, "a node holds at least 12 keys");
    require(capacity <= max_capacity, "a node holds at most 1023 keys");
    require(align % alignof(NodeHeader) == 0, "the header is aligned");
    const std::size_t keys_start = columns_end(header, column_size, capacity);
    if (!keys.has_inline_keys()) {
      require(keys_start == sizeof(NodeHeader) && !header.latched && header.annex_size == 0,
              "a node without inline keys has the plain header");
    } else {
      require(align % alignof(InlineKeyNodeHeader) == 0, "the header is aligned");
      require(keys.inline_keys_offset >= sizeof(InlineKeyNodeHeader), "inline keys after header");
      if (header.latched) {
        require(align % alignof(LatchedNodeHeader) == 0, "the latch is aligned");
        require(keys.inline_keys_offset >= sizeof(LatchedNodeHeader), "inline keys after latch");
      }
      if (header.annex_size > 0) {
        require(align % header.annex_align == 0, "the annex is aligned");
        require(header.annex_offset() % header.annex_align == 0, "the annex is aligned");
        require(keys.inline_keys_offset >= header.size(), "inline keys after the annex");
      }
      require(align % alignof(InlineWord) == 0, "inline keys are aligned");
      require(keys.inline_keys_offset % alignof(InlineWord) == 0, "inline keys are aligned");
      require(capacity * inline_key_size <= max_inline_keys_size(page_size), "inline keys fit");
      require(keys.inline_keys_offset + capacity * column_size == keys_start, "columns are dense");
    }
    const std::size_t key_stride = keys.key_stride, trailing_stride = layout.trailing_stride;
    require(align % trailing.align == 0 && layout.trailing_offset % trailing.align == 0,
            "trailing slots are aligned");
    require(trailing_stride >= trailing.size && trailing_stride % trailing.align == 0,
            "trailing slots are a stride apart");
    if (keeps_keys) {
      require(align % key_align_ == 0 && keys.keys_offset % key_align_ == 0, "keys are aligned");
      require(key_stride >= key_size_ && key_stride % key_align_ == 0, "keys are a stride apart");
    } else {
      require(key_stride == 0 && keys.has_inline_keys(), "a node without keys has inline keys");
    }
    require(keys.keys_offset + capacity * key_stride <= size, "keys end in the page");
    require(layout.trailing_offset + capacity * trailing_stride + trailing.size <= size,
            "trailing slots end in the page");
    if (trailing.paired(inline_key_size)) {
      const std::size_t slot_offset = entry_trailing_offset(trailing);
      const std::size_t entries = layout.trailing_offset - slot_offset;
      require(keys_start <= entries, "entries after the columns");
      require(key_stride == entry_size(trailing) && trailing_stride == key_stride,
              "entries are a stride apart");
      require(keys.keys_offset == entries + key_stride, "key 0 is entry 1's");
      require(slot_offset >= key_size_ && slot_offset + trailing.size <= key_stride,
              "an entry's slot follows its key");
      if (is_power_of_two(key_stride) && key_stride <= cache_line_size) {
        require(keys.keys_offset % key_stride == 0, "entries do not cross cache lines");
      }
    } else if (keeps_keys) {
      require(key_stride == key_size_ && trailing_stride == trailing.size, "dense arrays");
      require(keys_start <= keys.keys_offset, "keys after the columns");
      require(keys.keys_offset + capacity * key_size_ <= layout.trailing_offset,
              "trailing slots after the keys");
    } else {
      require(trailing_stride == trailing.size, "dense edges");
      require(keys_start <= layout.trailing_offset, "edges after the inline keys");
    }
  }
};

}  // namespace shoal::detail
