// Type-stable pages (`tree/latched/pages.rs`): a latched tree's node pages
// stay node pages of their kind for as long as threads share the tree. An
// optimistic reader follows pointers it read without a latch and checks them
// later, so a page a merge emptied can't go back to the allocator: it goes on
// the tree's free list for its kind, keeping its latch word, whose version
// only grows. The pages go back to the allocator when the map is destroyed.
//
// An operation allocates and frees through `NodePages`: it takes every page
// it will allocate before it changes anything, free ones first, so that an
// allocator with none to give fails the operation whole; it keeps the pages
// it frees until it lets go of its nodes.
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>

#include "../../allocator.hpp"
#include "../config.hpp"
#include "../node/latch.hpp"
#include "../node/layout.hpp"
#include "../tree/node_stack.hpp"

namespace shoal::detail {

/// The free pages of one kind, a stack linked through the pages, behind a
/// spin lock: only splits and merges take and give pages.
class FreeList {
 public:
  /// Puts `page`, a node page of this list's kind no thread changes any
  /// more, on the list.
  void push(std::byte* page) noexcept {
    lock();
    *link(page) = top_.load(std::memory_order_relaxed);
    top_.store(page, std::memory_order_relaxed);
    unlock();
  }

  /// The page on top of the list, taken off it, or nullptr.
  std::byte* pop() noexcept {
    // Most allocations find the list empty: they write nothing to it.
    if (top_.load(std::memory_order_relaxed) == nullptr) return nullptr;
    lock();
    std::byte* page = top_.load(std::memory_order_relaxed);
    if (page != nullptr) top_.store(*link(page), std::memory_order_relaxed);
    unlock();
    return page;
  }

 private:
  std::atomic<bool> locked_{false};
  std::atomic<std::byte*> top_{nullptr};

  /// Where a free page keeps the next one: past its header and latch word,
  /// in the first inline-key slots, which a stale reader reads as data.
  static std::byte** link(std::byte* page) noexcept {
    return reinterpret_cast<std::byte**>(page + sizeof(LatchedNodeHeader));
  }

  void lock() noexcept {
    while (locked_.exchange(true, std::memory_order_acquire)) {
      while (locked_.load(std::memory_order_relaxed)) Backoff().spin();
    }
  }
  void unlock() noexcept { locked_.store(false, std::memory_order_release); }
};

/// The free lists of a latched tree's two kinds of node.
class FreeLists {
 public:
  FreeList& list(PageKind kind) noexcept {
    SHOAL_ASSERT(kind != PageKind::Root);
    return kind == PageKind::Leaf ? leaves_ : internals_;
  }

  /// Gives every free page back to `alloc`, once no thread reaches the tree.
  template <class A>
  void deallocate_all(A& alloc, Layout leaf, Layout internal) noexcept {
    while (std::byte* page = leaves_.pop()) AllocatorKinds<A>::deallocate(alloc, PageKind::Leaf, page, leaf);
    while (std::byte* page = internals_.pop()) {
      AllocatorKinds<A>::deallocate(alloc, PageKind::Internal, page, internal);
    }
  }

 private:
  FreeList leaves_;
  FreeList internals_;
};

/// The pages one operation on a latched tree allocates and frees: an
/// allocator of node pages (`MapPages`' interface) over the map's.
///
/// - The operation first takes the pages it will allocate (`reserve`):
///   each off the free list of its kind if there is one, else fresh,
///   zeroed. Allocating one then hands out one of those, latched
///   exclusively until this is destroyed: once linked into the tree, other
///   threads find it held until the operation is done.
/// - Freeing a node page keeps it until this is destroyed, then puts it on
///   its kind's free list; the operation destroys this last, after it lets
///   go of its nodes.
/// A root page comes from, and goes to, the map's allocator directly.
template <class A>
class NodePages {
 public:
  NodePages(FreeLists& free, A& alloc, Layout leaf, Layout internal) noexcept
      : free_(&free), alloc_(&alloc), leaf_(leaf), internal_(internal) {}

  NodePages(const NodePages&) = delete;
  NodePages& operator=(const NodePages&) = delete;

  ~NodePages() {
    SHOAL_ASSERT(reserved_len_ == 0);
    for (std::size_t index = 0; index < allocated_len_; ++index) {
      latch_of(allocated_[index].page).unlock_exclusive(allocated_[index].version);
    }
    for (std::size_t index = 0; index < freed_len_; ++index) {
      free_->list(freed_[index].kind).push(freed_[index].page);
    }
  }

  /// Takes `leaves` leaf pages and `internals` internal pages, every page
  /// the operation will allocate, before it changes anything; or, if the
  /// allocator has none to give, takes none and returns the layout it was
  /// asked for, in `refused`.
  bool reserve(std::size_t leaves, std::size_t internals, Layout& refused) noexcept {
    SHOAL_ASSERT(reserved_len_ == 0);
    for (std::size_t count = 0; count < leaves + internals; ++count) {
      const PageKind kind = count < leaves ? PageKind::Leaf : PageKind::Internal;
      std::byte* page = take(kind);
      if (page == nullptr) {
        unreserve();
        refused = layout(kind);
        return false;
      }
      reserved_[reserved_len_++] = Taken{page, kind, Version(0)};
    }
    return true;
  }

  /// Puts back the pages `reserve` took: the operation allocates none.
  void unreserve() noexcept {
    for (std::size_t index = 0; index < reserved_len_; ++index) {
      free_->list(reserved_[index].kind).push(reserved_[index].page);
    }
    reserved_len_ = 0;
  }

  /// Whether the operation freed the page at `page`.
  bool has_freed(const std::byte* page) const noexcept {
    for (std::size_t index = 0; index < freed_len_; ++index) {
      if (freed_[index].page == page) return true;
    }
    return false;
  }

  // The page allocator the tree's operations take.

  std::byte* allocate_page(PageKind kind, Layout requested) const noexcept {
    SHOAL_ASSERT(requested == layout(kind));
    std::byte* page = take_reserved(kind);
    SHOAL_ASSERT(page != nullptr);
    if (page == nullptr) {
      page = take(kind);
      if (page == nullptr) MapPages<A>::handle_alloc_error(requested);
    }
    // A thread that reached a free page through a stale pointer may hold it a
    // moment, and waits for nothing meanwhile.
    const Version version = latch_of(page).lock_exclusive();
    allocated_[allocated_len_++] = Taken{page, kind, version};
    return page;
  }

  void deallocate_page(PageKind kind, std::byte* page, Layout) const noexcept {
    freed_[freed_len_++] = Taken{page, kind, Version(0)};
  }

 private:
  struct Taken {
    std::byte* page;
    PageKind kind;
    Version version;
  };

  static constexpr std::size_t max_taken = max_depth + 2;

  FreeLists* free_;
  A* alloc_;
  Layout leaf_;
  Layout internal_;
  // The node pages an operation took: reserved and not allocated yet; those
  // it allocated, each latched at a version; those it freed.
  mutable Taken reserved_[max_taken];
  mutable std::size_t reserved_len_ = 0;
  mutable Taken allocated_[max_taken];
  mutable std::size_t allocated_len_ = 0;
  mutable Taken freed_[max_taken];
  mutable std::size_t freed_len_ = 0;

  Layout layout(PageKind kind) const noexcept { return kind == PageKind::Leaf ? leaf_ : internal_; }

  static NodeLatch& latch_of(std::byte* page) noexcept {
    return NodeLatch::at(page + offsetof(LatchedNodeHeader, latch));
  }

  /// A page for a new node of `kind`: off its free list, as it was; or fresh,
  /// zeroed, its latch unheld and every edge a reader may read initialized.
  std::byte* take(PageKind kind) const noexcept {
    if (std::byte* page = free_->list(kind).pop()) return page;
    return static_cast<std::byte*>(AllocatorKinds<A>::allocate(*alloc_, kind, layout(kind)));
  }

  std::byte* take_reserved(PageKind kind) const noexcept {
    for (std::size_t index = 0; index < reserved_len_; ++index) {
      if (reserved_[index].kind == kind) {
        std::byte* page = reserved_[index].page;
        reserved_[index] = reserved_[--reserved_len_];
        return page;
      }
    }
    return nullptr;
  }
};

}  // namespace shoal::detail
