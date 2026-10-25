// Node pages (`tree/node/page.rs`): the allocation boundary (`FreshPage`),
// where pages come from (`MapPages`), and raw access to an initialized
// node's header and slot arrays (`NodePtr`).
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <new>
#include <utility>

#include "../../allocator.hpp"
#include "../config.hpp"
#include "latch.hpp"
#include "layout.hpp"
#include "plan.hpp"
#include "shared_prefix.hpp"
#include "slots.hpp"

namespace bbtree::detail {

/// Where the tree's operations take pages from and give them back to. Every
/// call names the page's kind. An operation has nothing to do about a failed
/// allocation: the map's allocator ends the program (`MapPages`), and a
/// latched operation that might fail takes its pages before it changes
/// anything (`tree/latched/pages.rs`).
///
/// A page allocator has
///
///     std::byte* allocate_page(PageKind, Layout) noexcept;  // never null
///     void deallocate_page(PageKind, std::byte*, Layout) noexcept;
///
/// `MapPages` is the map's allocator as one.
template <class A>
class MapPages {
 public:
  explicit MapPages(A& alloc) noexcept : alloc_(&alloc) {}

  std::byte* allocate_page(PageKind kind, Layout layout) const noexcept {
    void* page = AllocatorKinds<A>::allocate(*alloc_, kind, layout);
    if (BBTREE_UNLIKELY(page == nullptr)) handle_alloc_error(layout);
    return static_cast<std::byte*>(page);
  }

  /// A page, or nullptr if the allocator has none to give.
  std::byte* try_allocate_page(PageKind kind, Layout layout) const noexcept {
    return static_cast<std::byte*>(AllocatorKinds<A>::allocate(*alloc_, kind, layout));
  }

  void deallocate_page(PageKind kind, std::byte* page, Layout layout) const noexcept {
    AllocatorKinds<A>::deallocate(*alloc_, kind, page, layout);
  }

  A& allocator() const noexcept { return *alloc_; }

  /// Ends the program, as Rust's `handle_alloc_error` does: the tree is in
  /// the middle of a change it can't undo (PORTING.md, P5).
  [[noreturn]] BBTREE_COLD static void handle_alloc_error(Layout layout) noexcept {
    std::fprintf(stderr, "bbtree: memory allocation of %zu bytes failed\n", layout.size);
    std::abort();
  }

 private:
  A* alloc_;
};

/// A tagged pointer to an initialized node of a map with keys `K`, values `V`
/// and strategy `S`, whose lowest bit says whether the node is a leaf. Every
/// page leaves that bit 0, being aligned to at least 64 B. The tag is the
/// only record of a node's kind (D52). Everything it offers is raw: the node
/// handles, `Leaf` and `Internal`, are its users.
template <class K, class V, class S>
class NodePtr {
 public:
  using Shape = NodeShape<K, V, S>;
  using LeafKind = KindShape<K, V, S, true>;
  using InternalKind = KindShape<K, V, S, false>;

  /// The bit that marks a leaf's pointer.
  static constexpr std::uintptr_t leaf_tag = 1;

  /// Uninitialized, as Rust's `MaybeUninit`, so that arrays of node
  /// pointers (node stacks, descent paths) cost nothing to make;
  /// value-initialized, `NodePtr()`, it is null.
  NodePtr() noexcept = default;

  /// The node of kind `Kind` in the page at `page`, tagged with its kind.
  template <class Kind>
  static NodePtr tagged_as(std::byte* page) noexcept {
    const auto address = reinterpret_cast<std::uintptr_t>(page);
    BBTREE_ASSERT((address & leaf_tag) == 0);
    return NodePtr(address | (Kind::is_leaf ? leaf_tag : 0));
  }

  /// The pointer as it is stored, tag and all, and back.
  std::uintptr_t tagged() const noexcept { return tagged_; }
  static NodePtr from_tagged(std::uintptr_t tagged) noexcept { return NodePtr(tagged); }

  bool is_null() const noexcept { return tagged_ == 0; }
  bool is_leaf() const noexcept { return (tagged_ & leaf_tag) != 0; }
  bool operator==(NodePtr other) const noexcept { return tagged_ == other.tagged_; }
  bool operator!=(NodePtr other) const noexcept { return tagged_ != other.tagged_; }

  /// The page's address: the pointer without its tag.
  std::byte* page() const noexcept { return reinterpret_cast<std::byte*>(tagged_ & ~leaf_tag); }

  // The header.

  std::size_t len() const noexcept { return header()->len; }
  bool has_head() const noexcept { return header()->has_head; }
  void set_has_head(bool has_head) const noexcept { header()->has_head = has_head; }

  void set_len(std::size_t len) const noexcept {
    header()->len = static_cast<std::uint16_t>(len);
  }

  /// The prefix length of a node with inline keys; 0 for one without.
  std::size_t prefix_len() const noexcept {
    if constexpr (Shape::has_inline_keys) {
      return reinterpret_cast<const InlineKeyNodeHeader*>(page())->prefix_len;
    } else {
      return 0;
    }
  }

  /// Sets the prefix length of a node with inline keys; nothing without.
  void set_prefix_len(std::size_t prefix_len) const noexcept {
    if constexpr (Shape::has_inline_keys) {
      BBTREE_ASSERT(prefix_len <= InlineKeyNodeHeader::max_prefix_len);
      reinterpret_cast<InlineKeyNodeHeader*>(page())->prefix_len =
          static_cast<std::uint16_t>(prefix_len);
    }
  }

  /// The node's latch word, in a latched tree.
  NodeLatch& latch() const noexcept {
    static_assert(Shape::latched);
    return NodeLatch::at(page() + offsetof(LatchedNodeHeader, latch));
  }

  /// `len` and `prefix_len` of a latched tree's node, while a writer may be
  /// changing them, for an optimistic read: each read once, relaxed.
  BBTREE_RACING_READ std::pair<std::size_t, std::size_t> load_racing_header() const noexcept {
    auto* header = reinterpret_cast<InlineKeyNodeHeader*>(page());
    return {RacingLoad::relaxed(&header->header.len),
            RacingLoad::relaxed(&header->prefix_len)};
  }

  /// `has_head` of a latched tree's leaf, while a writer may change it.
  BBTREE_RACING_READ bool load_racing_has_head() const noexcept {
    return RacingLoad::relaxed(&header()->has_head);
  }

  /// Where a leaf's annex is: right after its node header.
  typename S::annex* leaf_annex() const noexcept {
    return reinterpret_cast<typename S::annex*>(page() + Shape::leaf_header().annex_offset());
  }

  // Making and moving nodes.

  /// Makes the page at `page` an empty node of kind `Kind`, whatever it held:
  /// no keys, no head, a new annex, every inline-key slot the greatest
  /// (invariant 4). An internal node still needs its first edge. In a latched
  /// tree, the latch word stays as it is.
  template <class Kind>
  static NodePtr initialize(std::byte* page) noexcept {
    const NodePtr node = tagged_as<Kind>(page);
    if constexpr (Shape::has_inline_keys) {
      auto* header = reinterpret_cast<InlineKeyNodeHeader*>(page);
      header->header.len = 0;
      header->header.has_head = false;
      header->prefix_len = 0;
    } else {
      auto* header = reinterpret_cast<NodeHeader*>(page);
      header->len = 0;
      header->has_head = false;
    }
    if constexpr (Kind::is_leaf && !std::is_empty_v<typename S::annex>) {
      ::new (static_cast<void*>(node.leaf_annex())) typename S::annex();
    }
    KeyColumns<Kind>(page).initialize();
    return node;
  }

  /// Moves this node, of kind `Kind`, into the page at `dest`, a page for a
  /// node of that kind: every byte of its layout copied raw, but a latched
  /// page's latch word, which stays the destination's (its version only
  /// grows). Returns the node's pointer there.
  template <class Kind>
  NodePtr move_node_to(std::byte* dest) const noexcept {
    BBTREE_ASSERT(is_leaf() == Kind::is_leaf);
    constexpr std::size_t size = Kind::page.size;
    const std::byte* source = page();
    if constexpr (Shape::latched) {
      constexpr std::size_t latch = offsetof(LatchedNodeHeader, latch);
      constexpr std::size_t latch_end = latch + sizeof(NodeLatch);
      std::memcpy(dest, source, latch);
      std::memcpy(dest + latch_end, source + latch_end, size - latch_end);
    } else {
      std::memcpy(dest, source, size);
    }
    return tagged_as<Kind>(dest);
  }

  /// Sets the length of a node of kind `Kind`; the inline-key slots a shorter
  /// length leaves get the greatest inline key (invariant 4).
  template <class Kind>
  void set_len_refilling(std::size_t len) const noexcept {
    BBTREE_ASSERT(len <= Kind::capacity);
    const std::size_t old_len = this->len();
    if (len < old_len) KeyColumns<Kind>(page()).mark_vacant(len, old_len);
    set_len(len);
  }

  // The slot arrays.

  template <class Kind>
  KeySlots<K, Kind> key_slots() const noexcept {
    return KeySlots<K, Kind>(page(), prefix_len());
  }

  SeparatorSlots<K, InternalKind> separator_slots() const noexcept {
    return SeparatorSlots<K, InternalKind>(page(), prefix_len());
  }

  template <class Kind>
  KeyColumns<Kind> key_columns() const noexcept {
    return KeyColumns<Kind>(page());
  }

  /// A leaf's head key slot, in entry 0 before the head (D49).
  K* head_key_slot() const noexcept {
    static_assert(Shape::has_inline_keys, "a leaf has a head key only with inline keys");
    return std::launder(reinterpret_cast<K*>(page() + Shape::leaf_layout.head_key_offset));
  }

  /// A leaf's value slots: value `i` beside key `i` (D29).
  Slots<V> value_slots() const noexcept {
    return Slots<V>(page() + Shape::leaf_layout.values_offset(), Shape::leaf_capacity,
                    Shape::leaf_layout.value_stride);
  }

  /// A leaf's head and value slots as one array: slot 0 the head, slot
  /// `i + 1` value `i` (D17).
  Slots<V> head_and_value_slots() const noexcept {
    return Slots<V>(page() + Shape::leaf_layout.head_offset, Shape::leaf_capacity + 1,
                    Shape::leaf_layout.value_stride);
  }

  /// An internal node's `capacity + 1` edges.
  Slots<NodePtr> edge_slots() const noexcept {
    return Slots<NodePtr>(page() + Shape::internal_layout.edges_offset,
                          Shape::internal_capacity + 1, Shape::internal_layout.edge_stride);
  }

  /// Edge `index`'s raw slot, for an optimistic reader's relaxed load.
  std::uintptr_t* edge_word(std::size_t index) const noexcept {
    return reinterpret_cast<std::uintptr_t*>(page() + Shape::internal_layout.edges_offset +
                                             index * Shape::internal_layout.edge_stride);
  }

 private:
  std::uintptr_t tagged_;

  explicit NodePtr(std::uintptr_t tagged) noexcept : tagged_(tagged) {}

  NodeHeader* header() const noexcept { return reinterpret_cast<NodeHeader*>(page()); }
};

/// An allocated page of kind `Kind` (`PageKind`) that holds no node: the only
/// way to make a node, and what a node turns back into. It owns its page; the
/// allocator it came from is the map's, which every operation that frees it
/// is given.
template <class K, class V, class S, PageKind Kind>
class FreshPage {
 public:
  using Shape = NodeShape<K, V, S>;

  /// A page's layout: its kind's; a root page's room for the larger kind,
  /// aligned as it, then the root area, what every key shares (D50, D52).
  static constexpr Layout layout() noexcept {
    if constexpr (Kind == PageKind::Leaf) {
      return Shape::leaf_layout.page;
    } else if constexpr (Kind == PageKind::Internal) {
      return Shape::internal_layout.page;
    } else {
      return Layout{Shape::root_area_offset + sizeof(SharedPrefix), Shape::root_page_align};
    }
  }

  explicit FreshPage(std::byte* page) noexcept : page_(page) {
    BBTREE_ASSERT(reinterpret_cast<std::uintptr_t>(page) % layout().align == 0);
  }

  template <class Pages>
  static FreshPage allocate(const Pages& pages) noexcept {
    return FreshPage(pages.allocate_page(Kind, layout()));
  }

  template <class Pages>
  void deallocate(const Pages& pages) && noexcept {
    pages.deallocate_page(Kind, page_, layout());
  }

  std::byte* into_raw() && noexcept { return page_; }
  std::byte* raw() const noexcept { return page_; }

 private:
  std::byte* page_;
};

/// A fresh page whose kind is known only at run time: what freeing a node of
/// either kind gives back.
struct AnyFreshPage {
  PageKind kind;
  std::byte* page;
  Layout layout;

  template <class Pages>
  void deallocate(const Pages& pages) const noexcept {
    pages.deallocate_page(kind, page, layout);
  }
};

}  // namespace bbtree::detail
