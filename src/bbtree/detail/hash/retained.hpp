// Blocks a shared hash map's shard lets go of while threads share it. An
// optimistic reader follows the pointer to the shard's block without a latch
// and checks what it read only afterwards, as the latched tree's readers do
// (`latched/pages.hpp`): so a block the shard replaced must stay readable
// until the map is destroyed. Only growth replaces it, doubling, so what the
// shard keeps is smaller than what it uses; `BBHashMap::clear` keeps the
// block it has.
#pragma once

#include <array>
#include <cstddef>
#include <utility>

#include "../../allocator.hpp"
#include "../config.hpp"

namespace bbtree::detail {

template <class A>
class RetainedBlocks {
 public:
  explicit RetainedBlocks(A allocator) noexcept : allocator_(std::move(allocator)) {}
  RetainedBlocks(const RetainedBlocks &) = delete;
  RetainedBlocks &operator=(const RetainedBlocks &) = delete;
  ~RetainedBlocks() {
    for (std::size_t index = 0; index < count_; ++index)
      allocator_.deallocate(blocks_[index].block, blocks_[index].layout);
  }

  /// From now on, blocks let go of go back to the allocator: no reader is
  /// left, as the map is being destroyed.
  void stop_retaining() noexcept { retaining_ = false; }

  /// Keeps `block` until destruction while readers may hold it; false if
  /// the caller should deallocate it now.
  bool retain(void *block, Layout layout) noexcept {
    if (!retaining_) return false;
    BBTREE_CHECK(count_ < blocks_.size());
    blocks_[count_++] = {block, layout};
    return true;
  }

 private:
  struct Block {
    void *block;
    Layout layout;
  };

  A allocator_;
  // Each block is one doubling of a shard's table, from 16 slots: fewer than
  // 64 before its size overflows.
  std::array<Block, 64> blocks_{};
  std::size_t count_ = 0;
  bool retaining_ = true;
};

/// The allocator a shard's table allocates through: `A`, whose
/// deallocations wait in the shard's `RetainedBlocks` while shared.
template <class A>
class RetainingAllocator {
 public:
  RetainingAllocator(A allocator, RetainedBlocks<A> *retained) noexcept
      : allocator_(std::move(allocator)), retained_(retained) {}

  void *allocate_zeroed(Layout layout) noexcept { return allocator_.allocate_zeroed(layout); }
  void deallocate(void *block, Layout layout) noexcept {
    if (!retained_->retain(block, layout)) allocator_.deallocate(block, layout);
  }

 private:
  A allocator_;
  RetainedBlocks<A> *retained_;
};

}  // namespace bbtree::detail
