// The table of a shared hash map's shard (`hash_map.hpp`): open addressing
// with linear probing, the entries in the slots it probes, a tag a slot in
// front of them. A tag is empty or the top seven bits of its entry's hash
// with the high bit set. One block holds it all: a header line with the
// capacity, the tags, then the slots, aligned to cache lines.
//
// A lookup walks the tags, which stay in cache, and reads the one slot whose
// tag matches: a miss reads no slot, a hit its own. The owned map's paged
// table (`table.hpp`) reaches its entries through descriptors and pages
// instead, so that they never move; the shared map hands out no reference
// that outlives a callback, so its entries may move, and live where the
// probe finds them.
//
// Removal shifts the entries after the hole back into it (Knuth's
// Algorithm R): no tombstones, so the table only ever reallocates to grow.
// Growth moves the entries by their cached hashes, running no user code.
//
// Optimistic readers (PORTING.md, P7) read a shard's block without its
// latch: they learn the block's bounds from its header, whichever block the
// pointer they read was, and the shard keeps every block it replaced until
// the map is destroyed (`RetainedBlocks`).
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <new>
#include <optional>
#include <stdexcept>
#include <type_traits>
#include <utility>

#include "../config.hpp"
#include "../racing_copy.hpp"
#include "../target.hpp"
#include "allocation.hpp"

namespace bbtree::detail {

template <class K, class V, class A>
class InlineHashTable {
  static_assert(std::is_nothrow_move_constructible_v<K> && std::is_nothrow_move_constructible_v<V>,
                "BBHashMap keys and values must move without throwing");
  static_assert(std::is_nothrow_destructible_v<K> && std::is_nothrow_destructible_v<V>,
                "BBHashMap keys and values must be destroyed without throwing");

 public:
  struct Entry {
    std::uint64_t hash;
    K key;
    V value;
  };

  explicit InlineHashTable(A allocator) noexcept : allocator_(std::move(allocator)) {}
  InlineHashTable(const InlineHashTable &) = delete;
  InlineHashTable &operator=(const InlineHashTable &) = delete;
  ~InlineHashTable() {
    clear();
    if (block_) allocator_.deallocate(block_, layout(capacity_));
  }

  std::size_t size() const noexcept { return size_; }

  template <class Q, class Equal>
  const Entry *find(std::uint64_t hash, const Q &key, const Equal &equal) const {
    if (capacity_ == 0) return nullptr;
    const auto wanted = tag_of(hash);
    for (auto slot = home(hash); tags()[slot] != empty_tag; slot = next(slot)) {
      const Entry &entry = slots()[slot];
      if (tags()[slot] == wanted && entry.hash == hash && equal(entry.key, key)) return &entry;
    }
    return nullptr;
  }
  template <class Q, class Equal>
  Entry *find(std::uint64_t hash, const Q &key, const Equal &equal) {
    return const_cast<Entry *>(std::as_const(*this).find(hash, key, equal));
  }

  /// The previous value, if `key` was there. Throws `HashAllocationFailure`
  /// if growth was refused, and what `equal` throws, before changing
  /// anything.
  template <class Equal>
  std::optional<V> insert(std::uint64_t hash, K &&key, V &&value, const Equal &equal) {
    if (Entry *stored = find(hash, key, equal)) {
      std::optional<V> previous(std::move(stored->value));
      stored->value.~V();
      ::new (static_cast<void *>(&stored->value)) V(std::move(value));
      return previous;
    }
    if (capacity_ == 0 || size_ + 1 > threshold(capacity_)) grow();
    const auto slot = vacant(hash);
    ::new (static_cast<void *>(slots() + slot)) Entry{hash, std::move(key), std::move(value)};
    tags()[slot] = tag_of(hash);
    ++size_;
    return std::nullopt;
  }

  template <class Q, class Equal>
  std::optional<std::pair<K, V>> remove_entry(std::uint64_t hash, const Q &key, const Equal &equal) {
    Entry *entry = find(hash, key, equal);
    if (!entry) return std::nullopt;
    std::optional<std::pair<K, V>> removed(std::in_place, std::move(entry->key), std::move(entry->value));
    entry->~Entry();
    close_hole(static_cast<std::size_t>(entry - slots()));
    --size_;
    return removed;
  }

  template <class Visit>
  void for_each(Visit visit) const {
    for (std::size_t slot = 0; slot < capacity_; ++slot) {
      if (tags()[slot] != empty_tag) visit(std::as_const(slots()[slot].key), std::as_const(slots()[slot].value));
    }
  }

  /// Destroys the entries, keeping the block for the entries to come.
  void clear() noexcept {
    for (std::size_t slot = 0; slot < capacity_; ++slot) {
      if (tags()[slot] == empty_tag) continue;
      slots()[slot].~Entry();
      tags()[slot] = empty_tag;
    }
    size_ = 0;
  }

  // Every entry is tagged with its hash and reachable from its home slot
  // without crossing an empty one, and they number `size()`.
  void check_invariants() const noexcept {
    std::size_t count = 0;
    for (std::size_t slot = 0; slot < capacity_; ++slot) {
      if (tags()[slot] == empty_tag) continue;
      const auto hash = slots()[slot].hash;
      BBTREE_CHECK(tags()[slot] == tag_of(hash));
      for (auto visited = home(hash); visited != slot; visited = next(visited))
        BBTREE_CHECK(tags()[visited] != empty_tag);
      ++count;
    }
    BBTREE_CHECK(count == size_);
  }

  /// The block as an optimistic reader finds it: the block the table pointed
  /// to and that block's own capacity, zero if none.
  struct RacingView {
    const unsigned char *block;
    std::size_t capacity;
  };

  /// Looks `key` up as an optimistic reader: reads tags and the matching
  /// slot without a latch, copying the entry into `found`, and runs user
  /// code (`equal`) only on that copy. What it returns means something only
  /// once the shard's version vouches for the read; whatever writers do
  /// meanwhile, it stays within the block it reads, and stops.
  template <class Q, class Equal>
  BBTREE_ALWAYS_INLINE bool racing_find(std::uint64_t hash, const Q &key, const Equal &equal,
                                        RacingCopy<Entry> &found) const {
    const auto view = racing_view();
    return racing_find_from(view, static_cast<std::size_t>(hash) & (view.capacity - 1), hash, key,
                            equal, found);
  }

  /// One key's optimistic lookup in steps, which a bulk read takes for all
  /// its keys before the next step, so that the keys' waits for memory
  /// overlap. A step branches only on what an earlier step loaded: a branch
  /// on a load still in flight, mispredicted, would throw away the loads of
  /// the keys after it. The tag at the key's home slot; the tags from there
  /// to the first that is empty or its own, and that slot's hash; the entry
  /// copied and the keys compared. A key whose slot holds another hash
  /// finishes as `racing_find` does. The bulk read keeps each key's hash.
  class RacingLookup {
   public:
    BBTREE_ALWAYS_INLINE void load_tag(const InlineHashTable &table, std::uint64_t hash) noexcept {
      view_ = table.racing_view();
      slot_ = static_cast<std::size_t>(hash) & (view_.capacity - 1);
      tag_ = view_.capacity == 0 ? empty_tag : racing_tag(view_, slot_);
    }
    BBTREE_ALWAYS_INLINE void load_hash(std::uint64_t hash) noexcept {
      const auto wanted = tag_of(hash);
      if (tag_ == empty_tag) return;
      if (tag_ != wanted) {
        slot_ = racing_probe(view_, hash, slot_);
        tag_ = racing_tag(view_, slot_);
        if (tag_ != wanted) return;
      }
      slot_hash_ = racing_hash(slot_in(view_, slot_));
    }
    template <class Q, class Equal>
    BBTREE_ALWAYS_INLINE bool finish(const InlineHashTable &table, std::uint64_t hash, const Q &key,
                                     const Equal &equal, RacingCopy<Entry> &found) const {
      // No matching tag before an empty one: the key is absent.
      if (tag_ != tag_of(hash)) return false;
      if (slot_hash_ == hash) {
        found.read_from(slot_in(view_, slot_));
        if (equal(found.assume_read().key, key)) return true;
      }
      return table.racing_find_from(view_, (slot_ + 1) & (view_.capacity - 1), hash, key, equal,
                                    found);
    }

   private:
    RacingView view_;
    std::size_t slot_;
    std::uint64_t slot_hash_;
    std::uint8_t tag_;
  };

 private:
  static constexpr std::uint8_t empty_tag = 0;
  static constexpr std::uint8_t occupied_tag = 0x80;
  /// The header line before the tags; its first word is the capacity.
  static constexpr std::size_t header_bytes = 64;
  static constexpr std::size_t block_align = alignof(Entry) > 64 ? alignof(Entry) : 64;

  A allocator_;
  unsigned char *block_ = nullptr;
  std::size_t capacity_ = 0;
  std::size_t size_ = 0;

  static std::uint8_t tag_of(std::uint64_t hash) noexcept {
    return static_cast<std::uint8_t>(occupied_tag | (hash >> 57));
  }
  static std::size_t threshold(std::size_t capacity) noexcept { return capacity - capacity / 4; }
  static std::size_t slots_offset(std::size_t capacity) noexcept {
    return (header_bytes + capacity + block_align - 1) / block_align * block_align;
  }
  static Layout layout(std::size_t capacity) noexcept {
    return {slots_offset(capacity) + capacity * sizeof(Entry), block_align};
  }

  static std::uint8_t *tags_of(unsigned char *block) noexcept { return block + header_bytes; }
  static Entry *slots_of(unsigned char *block, std::size_t capacity) noexcept {
    return reinterpret_cast<Entry *>(block + slots_offset(capacity));
  }
  /// The first empty slot from `hash`'s home in a block of `capacity`.
  static std::size_t vacant_in(const std::uint8_t *tags, std::size_t capacity, std::uint64_t hash) noexcept {
    auto slot = static_cast<std::size_t>(hash) & (capacity - 1);
    while (tags[slot] != empty_tag) slot = (slot + 1) & (capacity - 1);
    return slot;
  }

  std::uint8_t *tags() const noexcept { return tags_of(block_); }
  Entry *slots() const noexcept { return slots_of(block_, capacity_); }
  std::size_t home(std::uint64_t hash) const noexcept {
    return static_cast<std::size_t>(hash) & (capacity_ - 1);
  }
  std::size_t next(std::size_t slot) const noexcept { return (slot + 1) & (capacity_ - 1); }
  std::size_t vacant(std::uint64_t hash) const noexcept { return vacant_in(tags(), capacity_, hash); }

  // Algorithm R: an entry after the hole whose home is not cyclically in
  // (hole, its slot] moves into the hole, which moves to where it was.
  void close_hole(std::size_t hole) noexcept {
    for (auto slot = next(hole); tags()[slot] != empty_tag; slot = next(slot)) {
      const auto distance_home = (slot - home(slots()[slot].hash)) & (capacity_ - 1);
      const auto distance_hole = (slot - hole) & (capacity_ - 1);
      if (distance_home < distance_hole) continue;
      ::new (static_cast<void *>(slots() + hole)) Entry(std::move(slots()[slot]));
      slots()[slot].~Entry();
      tags()[hole] = tags()[slot];
      hole = slot;
    }
    tags()[hole] = empty_tag;
  }

  void grow() {
    const auto capacity = capacity_ == 0 ? std::size_t{16} : capacity_ * 2;
    if (capacity > (std::numeric_limits<std::size_t>::max() - header_bytes) / (sizeof(Entry) + 1) / 2)
      throw std::length_error("BBHashMap shard exhausted");
    auto *block = static_cast<unsigned char *>(HashAllocation::allocate(allocator_, layout(capacity)));
    ::new (static_cast<void *>(block)) std::uint64_t(capacity);
    std::uint8_t *tags = tags_of(block);
    Entry *slots = slots_of(block, capacity);
    for (std::size_t slot = 0; slot < capacity; ++slot)
      ::new (static_cast<void *>(tags + slot)) std::uint8_t(empty_tag);
    for (std::size_t slot = 0; slot < capacity_; ++slot) {
      if (this->tags()[slot] == empty_tag) continue;
      Entry &entry = this->slots()[slot];
      const auto target = vacant_in(tags, capacity, entry.hash);
      ::new (static_cast<void *>(slots + target)) Entry(std::move(entry));
      tags[target] = this->tags()[slot];
      entry.~Entry();
    }
    // A racing reader reaches the block through `block_`: the header, tags
    // and slots are stored before the pointer.
    std::atomic_thread_fence(std::memory_order_release);
    unsigned char *previous = std::exchange(block_, block);
    const auto previous_capacity = std::exchange(capacity_, capacity);
    if (previous) allocator_.deallocate(previous, layout(previous_capacity));
  }

  BBTREE_RACING_READ RacingView racing_view() const noexcept {
    const unsigned char *block = RacingLoad::relaxed(&block_);
    if (!block) return {nullptr, 0};
    return {block, static_cast<std::size_t>(RacingLoad::relaxed(reinterpret_cast<const std::uint64_t *>(block)))};
  }
  BBTREE_RACING_READ static std::uint8_t racing_tag(const RacingView &view, std::size_t slot) noexcept {
    return RacingLoad::relaxed(view.block + header_bytes + slot);
  }
  static const Entry *slot_in(const RacingView &view, std::size_t slot) noexcept {
    return reinterpret_cast<const Entry *>(view.block + slots_offset(view.capacity)) + slot;
  }
  BBTREE_RACING_READ static std::uint64_t racing_hash(const Entry *entry) noexcept {
    return RacingLoad::relaxed(&entry->hash);
  }
  /// From `slot` on, the first slot that is empty or whose tag is `hash`'s;
  /// it gives up after a lap, which only a block a writer is changing could
  /// need.
  BBTREE_RACING_READ static std::size_t racing_probe(const RacingView &view, std::uint64_t hash,
                                                     std::size_t slot) noexcept {
    const auto wanted = tag_of(hash);
    for (std::size_t probes = 0; probes < view.capacity; ++probes) {
      const auto tag = RacingLoad::relaxed(view.block + header_bytes + slot);
      if (tag == empty_tag || tag == wanted) return slot;
      slot = (slot + 1) & (view.capacity - 1);
    }
    return slot;
  }

  template <class Q, class Equal>
  BBTREE_ALWAYS_INLINE bool racing_find_from(const RacingView &view, std::size_t slot, std::uint64_t hash,
                                             const Q &key, const Equal &equal, RacingCopy<Entry> &found) const {
    static_assert(std::is_trivially_copyable_v<Entry>, "racing reads copy entries as bytes");
    const auto wanted = tag_of(hash);
    for (std::size_t probes = 0; probes < view.capacity; ++probes) {
      slot = racing_probe(view, hash, slot);
      if (racing_tag(view, slot) != wanted) return false;
      const Entry *entry = slot_in(view, slot);
      if (racing_hash(entry) == hash) {
        found.read_from(entry);
        if (found.assume_read().hash == hash && equal(found.assume_read().key, key)) return true;
      }
      slot = (slot + 1) & (view.capacity - 1);
    }
    return false;
  }
};

}  // namespace bbtree::detail
