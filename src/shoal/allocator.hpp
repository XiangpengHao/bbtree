// Where a map's memory comes from (OG-D8): an allocator of pages, each of a
// kind and of the one layout every page of that kind has. By default every
// kind comes from `allocate_zeroed`; an allocator that tells the kinds apart,
// to budget, pool or place them apart, defines the kinds' own functions.
#pragma once

#include <cstddef>
#include <cstring>
#include <new>
#include <type_traits>
#include <utility>

namespace shoal {

/// The size and alignment of a block, as `core::alloc::Layout`.
struct Layout {
  std::size_t size;
  std::size_t align;

  friend constexpr bool operator==(Layout a, Layout b) {
    return a.size == b.size && a.align == b.align;
  }
  friend constexpr bool operator!=(Layout a, Layout b) { return !(a == b); }
};

/// What a page is for, which tells which of an allocator's functions hands it
/// out and takes it back.
enum class PageKind { Internal, Leaf, Root };

/// The global allocator: aligned `operator new`, zeroed. It refuses blocks of
/// size zero, which a map never asks for.
///
/// An allocator is any class with
///
///     void* allocate_zeroed(Layout) noexcept;          // nullptr if none
///     void deallocate(void*, Layout) noexcept;
///
/// and optionally `allocate_internal`/`deallocate_internal`,
/// `allocate_leaf`/`deallocate_leaf` and `allocate_root`/`deallocate_root`
/// of the same signatures, which then serve their kind. A page comes back
/// to the function of its kind, with the layout it was asked for with, to
/// the allocator it came from or a copy of it: a copy is the same allocator.
/// No function throws: a map calls them in the middle of changing its nodes.
struct Global {
  void* allocate_zeroed(Layout layout) noexcept {
    if (layout.size == 0) return nullptr;
    void* block = ::operator new(layout.size, std::align_val_t(layout.align), std::nothrow);
    if (block != nullptr) std::memset(block, 0, layout.size);
    return block;
  }

  void deallocate(void* block, Layout layout) noexcept {
    ::operator delete(block, std::align_val_t(layout.align));
  }
};

namespace detail {

/// Which of an allocator's functions serve each page kind: its own for the
/// kind if it has them, `allocate_zeroed` and `deallocate` otherwise.
template <class A>
class AllocatorKinds {
  template <class T, class = void>
  struct has_internal : std::false_type {};
  template <class T>
  struct has_internal<T, std::void_t<decltype(std::declval<T&>().allocate_internal(Layout{}))>>
      : std::true_type {};
  template <class T, class = void>
  struct has_leaf : std::false_type {};
  template <class T>
  struct has_leaf<T, std::void_t<decltype(std::declval<T&>().allocate_leaf(Layout{}))>>
      : std::true_type {};
  template <class T, class = void>
  struct has_root : std::false_type {};
  template <class T>
  struct has_root<T, std::void_t<decltype(std::declval<T&>().allocate_root(Layout{}))>>
      : std::true_type {};

 public:
  /// A zeroed page of `kind` for `layout`, or nullptr if the allocator has
  /// none to give.
  static void* allocate(A& alloc, PageKind kind, Layout layout) noexcept {
    switch (kind) {
      case PageKind::Internal:
        if constexpr (has_internal<A>::value) return alloc.allocate_internal(layout);
        break;
      case PageKind::Leaf:
        if constexpr (has_leaf<A>::value) return alloc.allocate_leaf(layout);
        break;
      case PageKind::Root:
        if constexpr (has_root<A>::value) return alloc.allocate_root(layout);
        break;
    }
    return alloc.allocate_zeroed(layout);
  }

  /// Gives `page`, of `kind`, back.
  static void deallocate(A& alloc, PageKind kind, void* page, Layout layout) noexcept {
    switch (kind) {
      case PageKind::Internal:
        if constexpr (has_internal<A>::value) return alloc.deallocate_internal(page, layout);
        break;
      case PageKind::Leaf:
        if constexpr (has_leaf<A>::value) return alloc.deallocate_leaf(page, layout);
        break;
      case PageKind::Root:
        if constexpr (has_root<A>::value) return alloc.deallocate_root(page, layout);
        break;
    }
    alloc.deallocate(page, layout);
  }
};

}  // namespace detail
}  // namespace shoal
