// Eight-byte probe descriptors: a high 16-bit fingerprint and a low
// 48-bit entry identifier. Empty and deleted are identifier encodings,
// independent of the fingerprint (including an all-zero fingerprint).
//
// An array of descriptors begins with a header line that holds its
// capacity, so that an optimistic reader of a shared map, which reaches the
// array through a pointer it read racing, knows the bounds of that array
// whatever array the pointer was (`racing_view`).
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <stdexcept>
#include <utility>

#include "allocation.hpp"
#include "../config.hpp"
#include "../target.hpp"

namespace bbtree::detail {

template <class A>
class HashProbeIndex {
 public:
  explicit HashProbeIndex(A allocator = A()) noexcept : allocator_(std::move(allocator)) {}
  HashProbeIndex(const HashProbeIndex &) = delete;
  HashProbeIndex &operator=(const HashProbeIndex &) = delete;
  HashProbeIndex(HashProbeIndex &&other) noexcept
      : allocator_(std::move(other.allocator_)),
        descriptors_(std::exchange(other.descriptors_, nullptr)),
        capacity_(std::exchange(other.capacity_, 0)),
        size_(std::exchange(other.size_, 0)),
        deleted_(std::exchange(other.deleted_, 0)) {}
  ~HashProbeIndex() { clear(); }

  void swap(HashProbeIndex &other) noexcept {
    using std::swap;
    swap(allocator_, other.allocator_);
    swap(descriptors_, other.descriptors_);
    swap(capacity_, other.capacity_);
    swap(size_, other.size_);
    swap(deleted_, other.deleted_);
  }

  std::size_t capacity() const noexcept { return capacity_; }
  std::size_t size() const noexcept { return size_; }
  bool has_deleted() const noexcept { return deleted_ != 0; }
  std::uint64_t descriptor(std::size_t slot) const noexcept { return descriptors_[slot]; }
  static std::uint64_t descriptor_identifier(std::uint64_t descriptor) noexcept {
    return descriptor & identifier_mask();
  }
  static bool descriptor_matches(std::uint64_t descriptor, std::uint64_t hash) noexcept {
    return descriptor >> 48 == hash >> 48 && (descriptor & identifier_mask()) != identifier_mask();
  }
  std::uint64_t identifier(std::size_t slot) const noexcept {
    return descriptors_[slot] & identifier_mask();
  }
  bool occupied(std::size_t slot) const noexcept {
    const auto id = descriptors_[slot] & identifier_mask();
    return id != 0 && id != identifier_mask();
  }
  std::uint64_t fingerprint(std::size_t slot) const noexcept { return descriptors_[slot] >> 48; }
  std::size_t home(std::uint64_t hash) const noexcept {
    return static_cast<std::size_t>(hash) & (capacity_ - 1);
  }
  std::size_t next(std::size_t slot) const noexcept { return (slot + 1) & (capacity_ - 1); }

  /// The descriptors as an optimistic reader finds them (PORTING.md, P7):
  /// the array the index pointed to and that array's own capacity, zero if
  /// none. Any array the index ever pointed to stays valid while a shared
  /// map's readers may hold it (`RetainedBlocks`).
  struct RacingView {
    const std::uint64_t *descriptors;
    std::size_t capacity;
  };
  BBTREE_RACING_READ RacingView racing_view() const noexcept {
    const std::uint64_t *descriptors = RacingLoad::relaxed(&descriptors_);
    if (!descriptors) return {nullptr, 0};
    return {descriptors, static_cast<std::size_t>(RacingLoad::relaxed(descriptors - 1))};
  }
  BBTREE_RACING_READ static std::uint64_t racing_descriptor(const RacingView &view,
                                                            std::size_t slot) noexcept {
    return RacingLoad::relaxed(view.descriptors + slot);
  }

  template <class Store, class Q, class Equal>
  std::size_t find(std::uint64_t hash, const Q &key, const Store &store, const Equal &equal) const {
    if (capacity_ == 0) return capacity_;
    auto slot = home(hash);
    while (identifier(slot) != 0) {
      if (occupied(slot) && fingerprint(slot) == hash >> 48 &&
          equal(store.at(identifier(slot)).key, key))
        return slot;
      slot = next(slot);
    }
    return capacity_;
  }

  void publish(std::uint64_t hash, std::uint64_t identifier) noexcept {
    publish_at(vacant(hash), hash, identifier);
  }

  std::size_t vacant(std::uint64_t hash) const noexcept {
    auto slot = home(hash);
    while (occupied(slot)) slot = next(slot);
    return slot;
  }

  template <class Store, class Q, class Equal>
  std::size_t locate(std::uint64_t hash, const Q &key, const Store &store,
                     const Equal &equal) const {
    if (capacity_ == 0) return capacity_;
    auto slot = home(hash);
    auto deleted = capacity_;
    while (identifier(slot) != 0) {
      if (occupied(slot)) {
        if (fingerprint(slot) == hash >> 48 && equal(store.at(identifier(slot)).key, key))
          return slot;
      } else if (deleted == capacity_)
        deleted = slot;
      slot = next(slot);
    }
    return deleted == capacity_ ? slot : deleted;
  }

  void publish_at(std::size_t slot, std::uint64_t hash, std::uint64_t identifier) noexcept {
    BBTREE_ASSERT(slot < capacity_ && !occupied(slot));
    if (this->identifier(slot) != 0) --deleted_;
    descriptors_[slot] = (hash & ~identifier_mask()) | identifier;
    ++size_;
  }

  // Pending external-row claims share the deleted encoding but never contribute
  // to tombstone accounting. They must be committed or released before return.
  void claim_empty(std::size_t slot, std::uint64_t hash) noexcept {
    BBTREE_ASSERT(identifier(slot) == 0);
    descriptors_[slot] = (hash & ~identifier_mask()) | identifier_mask();
  }
  void commit_claim(std::size_t slot, std::uint64_t identifier) noexcept {
    BBTREE_ASSERT(this->identifier(slot) == identifier_mask());
    descriptors_[slot] = (descriptors_[slot] & ~identifier_mask()) | identifier;
    ++size_;
  }
  void release_claim(std::size_t slot) noexcept {
    BBTREE_ASSERT(identifier(slot) == identifier_mask());
    descriptors_[slot] = 0;
  }

  void erase(std::size_t slot) noexcept {
    BBTREE_ASSERT(occupied(slot));
    descriptors_[slot] = identifier_mask();
    --size_;
    ++deleted_;
  }

  template <class Store>
  void reserve(std::size_t count, const Store &store) {
    auto target = capacity_ == 0 ? std::size_t{16} : capacity_;
    while (count > threshold(target)) {
      if (target > std::numeric_limits<std::size_t>::max() / sizeof(std::uint64_t) / 2)
        throw std::length_error("BBHashMap probe index exhausted");
      target *= 2;
    }
    if (count != 0 && target != capacity_) rebuild(target, store);
  }

  template <class Store>
  bool prepare_insert(const Store &store) {
    if (capacity_ == 0 || size_ + deleted_ >= threshold(capacity_)) {
      if (capacity_ != 0 && size_ < capacity_ / 2)
        rebuild(capacity_, store);
      else
        reserve(capacity_ == 0 ? 1 : threshold(capacity_) + 1, store);
      return true;
    }
    return false;
  }

  void clear() noexcept {
    if (descriptors_) allocator_.deallocate(descriptors_ - header_words, layout(capacity_));
    descriptors_ = nullptr;
    capacity_ = size_ = deleted_ = 0;
  }

  template <class Store>
  bool prepare_many(std::size_t additional, const Store &store) {
    if (additional == 0) return false;
    if (additional > std::numeric_limits<std::size_t>::max() - size_)
      throw std::length_error("BBHashMap batch size exhausted");
    const auto previous_capacity = capacity_;
    reserve(size_ + additional, store);
    if (capacity_ != previous_capacity) return true;
    if (size_ + deleted_ + additional > threshold(capacity_)) {
      rebuild(capacity_, store);
      return true;
    }
    return false;
  }

  void reset() noexcept {
    for (std::size_t slot = 0; slot < capacity_; ++slot) descriptors_[slot] = 0;
    size_ = deleted_ = 0;
  }

 private:
  A allocator_;
  std::uint64_t *descriptors_ = nullptr;
  std::size_t capacity_ = 0;
  std::size_t size_ = 0;
  std::size_t deleted_ = 0;

  /// The header line before the descriptors; its last word is the capacity.
  static constexpr std::size_t header_words = 64 / sizeof(std::uint64_t);

  static constexpr std::uint64_t identifier_mask() noexcept { return (std::uint64_t{1} << 48) - 1; }
  static std::size_t threshold(std::size_t capacity) noexcept { return capacity - capacity / 4; }
  static Layout layout(std::size_t capacity) noexcept {
    return {(header_words + capacity) * sizeof(std::uint64_t), 64};
  }

  template <class Store>
  void rebuild(std::size_t capacity, const Store &store) {
    if (capacity == capacity_) return rebuild_in_place(store);
    HashProbeIndex replacement(allocator_);
    auto *block =
        static_cast<std::uint64_t *>(HashAllocation::allocate(allocator_, layout(capacity)));
    ::new (static_cast<void *>(block + header_words - 1)) std::uint64_t(capacity);
    replacement.descriptors_ = block + header_words;
    replacement.capacity_ = capacity;
    for (std::size_t slot = 0; slot < capacity; ++slot)
      ::new (static_cast<void *>(replacement.descriptors_ + slot)) std::uint64_t(0);
    store.for_each([&](std::uint64_t identifier, const auto &entry) {
      const auto slot = replacement.vacant(entry.hash);
      replacement.descriptors_[slot] = (entry.hash & ~identifier_mask()) | identifier;
    });
    replacement.size_ = size_;
    // A racing reader reaches the array through the pointer swapped in: its
    // header and descriptors are stored before the pointer.
    std::atomic_thread_fence(std::memory_order_release);
    swap(replacement);
  }

  // At the same capacity, drop the tombstones where they lie, allocating
  // nothing: the entries' cached hashes place them again. Only the entry
  // store rebuilds at the same capacity, and its walk runs no user code.
  template <class Store>
  void rebuild_in_place(const Store &store) {
    for (std::size_t slot = 0; slot < capacity_; ++slot) descriptors_[slot] = 0;
    store.for_each([&](std::uint64_t identifier, const auto &entry) {
      descriptors_[vacant(entry.hash)] = (entry.hash & ~identifier_mask()) | identifier;
    });
    deleted_ = 0;
  }
};

}  // namespace bbtree::detail
