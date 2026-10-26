// Entries live in owned pages. Rebuilding the probe index or its page
// directory never moves an entry; deleted slots form an identifier free list.
#pragma once

#include <cstddef>
#include <cstdint>
#include <limits>
#include <new>
#include <stdexcept>
#include <type_traits>
#include <utility>

#include "allocation.hpp"
#include "../config.hpp"

namespace shoal::detail {

template <class K, class V, class A>
class HashEntryStore {
 public:
  struct Entry {
    std::uint64_t hash;
    K key;
    V value;
  };

  class InsertedEntry {
   public:
    InsertedEntry(std::size_t identifier, Entry *entry) noexcept
        : identifier_(identifier), entry_(entry) {}
    std::size_t identifier() const noexcept { return identifier_; }
    Entry &entry() const noexcept { return *entry_; }

   private:
    std::size_t identifier_;
    Entry *entry_;
  };

 private:
  union Slot {
    Entry entry;
    std::size_t next_free;
    Slot() noexcept {}
    ~Slot() {}
  };

  // Up to 64 entries per page, approximately 4 KiB for larger entries.
  // An oversized entry gets its own page with its natural alignment.
  static constexpr std::size_t slots_per_page() noexcept {
    return sizeof(Slot) > 4096 ? 1 : (4096 / sizeof(Slot) < 64 ? 4096 / sizeof(Slot) : 64);
  }

  struct Page {
    std::uint64_t occupied = 0;
    Slot slots[slots_per_page()];
  };

 public:
  explicit HashEntryStore(A allocator = A()) noexcept : allocator_(std::move(allocator)) {}
  HashEntryStore(const HashEntryStore &) = delete;
  HashEntryStore &operator=(const HashEntryStore &) = delete;
  HashEntryStore(HashEntryStore &&other) noexcept : HashEntryStore(std::move(other.allocator_)) {
    take(other);
  }
  ~HashEntryStore() { clear(); }

  void swap(HashEntryStore &other) noexcept {
    using std::swap;
    swap(allocator_, other.allocator_);
    swap(pages_, other.pages_);
    swap(page_count_, other.page_count_);
    swap(directory_capacity_, other.directory_capacity_);
    swap(free_head_, other.free_head_);
    swap(next_unused_, other.next_unused_);
    swap(size_, other.size_);
  }

  const A &allocator() const noexcept { return allocator_; }
  std::size_t size() const noexcept { return size_; }

  InsertedEntry insert(std::uint64_t hash, K &&key, V &&value) {
    const bool reuse = free_head_ != 0;
    if (!reuse && next_unused_ == page_count_ * slots_per_page()) add_page();
    const auto identifier = reuse ? free_head_ : next_unused_ + 1;
    auto &page = *pages_[ordinal(identifier) / slots_per_page()];
    const auto offset = ordinal(identifier) % slots_per_page();
    auto &slot = page.slots[offset];
    const auto next = reuse ? slot.next_free : 0;
    ::new (static_cast<void *>(&slot.entry)) Entry{hash, std::move(key), std::move(value)};
    page.occupied |= std::uint64_t{1} << offset;
    if (reuse)
      free_head_ = next;
    else
      ++next_unused_;
    ++size_;
    return {identifier, &slot.entry};
  }

  Entry &at(std::size_t identifier) noexcept {
    SHOAL_ASSERT(live(identifier));
    return pages_[ordinal(identifier) / slots_per_page()]
        ->slots[ordinal(identifier) % slots_per_page()]
        .entry;
  }
  const Entry &at(std::size_t identifier) const noexcept {
    SHOAL_ASSERT(live(identifier));
    return pages_[ordinal(identifier) / slots_per_page()]
        ->slots[ordinal(identifier) % slots_per_page()]
        .entry;
  }

  bool live(std::size_t identifier) const noexcept {
    if (identifier == 0 || ordinal(identifier) / slots_per_page() >= page_count_) return false;
    return (pages_[ordinal(identifier) / slots_per_page()]->occupied &
            (std::uint64_t{1} << (ordinal(identifier) % slots_per_page()))) != 0;
  }

  void erase(std::size_t identifier) noexcept {
    auto &entry = at(identifier);
    entry.~Entry();
    auto &page = *pages_[ordinal(identifier) / slots_per_page()];
    const auto offset = ordinal(identifier) % slots_per_page();
    ::new (static_cast<void *>(&page.slots[offset].next_free)) std::size_t(free_head_);
    page.occupied &= ~(std::uint64_t{1} << offset);
    free_head_ = identifier;
    --size_;
  }

  // Returns the next live identifier after `previous`, or zero at the end.
  // A bitmap skips empty pages without reading their entry storage.
  std::size_t next(std::size_t previous) const noexcept {
    auto page_index = previous / slots_per_page();
    auto offset = previous % slots_per_page();
    for (; page_index < page_count_; ++page_index, offset = 0) {
      auto remaining = pages_[page_index]->occupied >> offset;
      if (remaining == 0) continue;
      while ((remaining & 1) == 0) {
        remaining >>= 1;
        ++offset;
      }
      return page_index * slots_per_page() + offset + 1;
    }
    return 0;
  }

  // Visit a full page as a linear run. Resolve its directory pointer and
  // bitmap once instead of reconstructing an identifier for every next().
  template <class Visit>
  void for_each(Visit visit) const {
    constexpr auto full = ~std::uint64_t{0} >> (64 - slots_per_page());
    for (std::size_t page_index = 0; page_index < page_count_; ++page_index) {
      const auto &page = *pages_[page_index];
      const auto first = page_index * slots_per_page() + 1;
      if (page.occupied == full) {
        for (std::size_t slot = 0; slot < slots_per_page(); ++slot)
          visit(first + slot, page.slots[slot].entry);
      } else {
        auto bits = page.occupied;
        std::size_t slot = 0;
        while (bits != 0) {
          while ((bits & 1) == 0) {
            bits >>= 1;
            ++slot;
          }
          visit(first + slot, page.slots[slot].entry);
          bits >>= 1;
          ++slot;
        }
      }
    }
  }

  void reset() noexcept {
    destroy_entries();
    for (std::size_t index = 0; index < page_count_; ++index) pages_[index]->occupied = 0;
    free_head_ = next_unused_ = size_ = 0;
  }

  void clear() noexcept {
    destroy_entries();
    for (std::size_t index = 0; index < page_count_; ++index) {
      pages_[index]->~Page();
      allocator_.deallocate(pages_[index], Layout{sizeof(Page), alignof(Page)});
    }
    if (pages_) allocator_.deallocate(pages_, directory_layout(directory_capacity_));
    pages_ = nullptr;
    page_count_ = directory_capacity_ = free_head_ = next_unused_ = size_ = 0;
  }

 private:
  A allocator_;
  Page **pages_ = nullptr;
  std::size_t page_count_ = 0;
  std::size_t directory_capacity_ = 0;
  std::size_t free_head_ = 0;
  std::size_t next_unused_ = 0;
  std::size_t size_ = 0;

  static std::size_t ordinal(std::size_t identifier) noexcept { return identifier - 1; }
  static Layout directory_layout(std::size_t capacity) noexcept {
    return {capacity * sizeof(Page *), alignof(Page *)};
  }

  void take(HashEntryStore &other) noexcept {
    pages_ = std::exchange(other.pages_, nullptr);
    page_count_ = std::exchange(other.page_count_, 0);
    directory_capacity_ = std::exchange(other.directory_capacity_, 0);
    free_head_ = std::exchange(other.free_head_, 0);
    next_unused_ = std::exchange(other.next_unused_, 0);
    size_ = std::exchange(other.size_, 0);
  }

  void add_page() {
    // The all-ones 48-bit identifier is the probe index's tombstone.
    constexpr auto identifier_limit = (std::uint64_t{1} << 48) - 2;
    constexpr auto native_limit = std::numeric_limits<std::size_t>::max() - 1;
    constexpr auto limit = native_limit < identifier_limit ? native_limit : identifier_limit;
    if (page_count_ >= limit / slots_per_page())
      throw std::length_error("HashMap entry identifier exhausted");
    if (page_count_ == directory_capacity_) grow_directory();
    void *memory = HashAllocation::allocate(allocator_, Layout{sizeof(Page), alignof(Page)});
    auto *page = ::new (memory) Page();
    pages_[page_count_++] = page;
  }

  void destroy_entries() noexcept {
    if constexpr (!std::is_trivially_destructible_v<Entry>) {
      for (auto identifier = next(0); identifier != 0; identifier = next(identifier))
        at(identifier).~Entry();
    }
  }

  void grow_directory() {
    if (directory_capacity_ > std::numeric_limits<std::size_t>::max() / sizeof(Page *) / 2)
      throw std::length_error("HashMap page directory exhausted");
    const auto capacity = directory_capacity_ == 0 ? 4 : directory_capacity_ * 2;
    auto **directory =
        static_cast<Page **>(HashAllocation::allocate(allocator_, directory_layout(capacity)));
    for (std::size_t index = 0; index < capacity; ++index)
      ::new (static_cast<void *>(directory + index))
          Page *(index < page_count_ ? pages_[index] : nullptr);
    if (pages_) allocator_.deallocate(pages_, directory_layout(directory_capacity_));
    pages_ = directory;
    directory_capacity_ = capacity;
  }
};

}  // namespace shoal::detail
