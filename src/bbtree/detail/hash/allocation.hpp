#pragma once

#include <new>

#include "../../allocator.hpp"

namespace bbtree::detail {

// Distinguishes our allocator's refusal from exceptions thrown by user
// hashing/equality, so the shared map returns only real allocation refusals.
class HashAllocationFailure : public std::bad_alloc {
 public:
  explicit HashAllocationFailure(Layout layout) noexcept : layout_(layout) {}
  Layout layout() const noexcept { return layout_; }

 private:
  Layout layout_;
};

struct HashAllocation {
  template <class A>
  static void *allocate(A &allocator, Layout layout) {
    void *memory = allocator.allocate_zeroed(layout);
    if (!memory) throw HashAllocationFailure(layout);
    return memory;
  }
};

}  // namespace bbtree::detail
