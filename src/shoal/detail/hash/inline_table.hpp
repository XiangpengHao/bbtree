// The table of a shared hash map's shard (`hash_map.hpp`): open addressing
// with linear probing, the entries in the slots it probes, a tag a slot in
// front of them (D25). A tag is empty or the top seven bits of its entry's
// hash with the high bit set. One block holds it all: a header line with the
// capacity, the tags, then the slots, aligned to cache lines.
//
// A lookup reads the tags eight at a time, as one word: the first that matches
// and the first that is empty come out of a few bit operations, no branch a
// tag (D27). It reads the one slot whose tag matches: a miss reads no slot, a
// hit its own. The first seven tags are cloned past the last, so that a group
// of eight never wraps. The owned map's paged table (`table.hpp`) reaches its
// entries through descriptors and pages instead, so that they never move; the
// shared map hands out no reference that outlives a callback, so its entries
// may move, and live where the probe finds them.
//
// Removal shifts the entries after the hole back into it (Knuth's Algorithm R,
// D29): no tombstones, so the table only ever reallocates to grow. Growth
// moves the entries by their cached hashes, running no user code.
//
// Optimistic readers (D12, D26) read a shard's block without its
// latch: they learn the block's bounds from its header, whichever block the
// pointer they read was, and the shard keeps every block it replaced until
// the map is destroyed (`RetainedBlocks`).
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
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

namespace shoal::detail {

template <class K, class V, class A>
class InlineHashTable {
  static_assert(std::is_nothrow_move_constructible_v<K> && std::is_nothrow_move_constructible_v<V>,
                "HashMap keys and values must move without throwing");
  static_assert(std::is_nothrow_destructible_v<K> && std::is_nothrow_destructible_v<V>,
                "HashMap keys and values must be destroyed without throwing");

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
    set_tag(tags(), capacity_, slot, tag_of(hash));
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
      set_tag(tags(), capacity_, slot, empty_tag);
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
      SHOAL_CHECK(tags()[slot] == tag_of(hash));
      for (auto visited = home(hash); visited != slot; visited = next(visited))
        SHOAL_CHECK(tags()[visited] != empty_tag);
      ++count;
    }
    SHOAL_CHECK(count == size_);
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
  SHOAL_ALWAYS_INLINE bool racing_find(std::uint64_t hash, const Q &key, const Equal &equal,
                                        RacingCopy<Entry> &found) const {
    const auto view = racing_view();
    return racing_find_from(view, static_cast<std::size_t>(hash) & (view.capacity - 1), hash, key,
                            equal, found);
  }

  /// Zero bytes in an entry's shape, which a bulk read's keys that have no
  /// candidate slot read in its place: so no step branches on whether a key
  /// has one.
  class NoEntry {
   public:
    const Entry *entry() const noexcept { return std::launder(reinterpret_cast<const Entry *>(bytes_)); }

   private:
    alignas(Entry) unsigned char bytes_[sizeof(Entry)]{};
  };

  /// One key's optimistic lookup in steps, which a bulk read takes for all
  /// its keys before the next step, so that the keys' waits for memory
  /// overlap. No step branches on whether the key is there: half the keys
  /// of a lookup mix are absent, and each mispredicted branch would also
  /// throw away the loads of the keys after it. The eight tags from the
  /// key's home slot; the first of them that matches before an empty one,
  /// and that slot's cached hash, or `NoEntry`'s; the entry copied and the
  /// keys compared. A key whose first match was another key's, or whose
  /// eight tags held neither its own nor an empty one, finishes as
  /// `racing_find` does.
  class RacingLookup {
   public:
    SHOAL_ALWAYS_INLINE void load_tags(const InlineHashTable &table, std::uint64_t hash) noexcept {
      view_ = table.racing_view();
      home_ = static_cast<std::size_t>(hash) & (view_.capacity - 1);
      group_ = view_.capacity == 0 ? 0 : racing_group(view_, home_);
    }
    SHOAL_ALWAYS_INLINE void load_hash(std::uint64_t hash, const NoEntry &none) noexcept {
      const TagGroup group(group_, tag_of(hash));
      candidate_ = group.first_match() < group.first_empty();
      overflow_ = group.first_empty() == group_width && group.first_match() == group_width;
      slot_ = (home_ + group.first_match()) & (view_.capacity - 1);
      entry_ = candidate_ ? slot_in(view_, slot_) : none.entry();
      entry_hash_ = racing_hash(entry_);
    }
    template <class Q, class Equal>
    SHOAL_ALWAYS_INLINE bool finish(const InlineHashTable &table, std::uint64_t hash, const Q &key,
                                     const Equal &equal, RacingCopy<Entry> &found) const {
      found.read_from(entry_);
      const bool hit = candidate_ & (entry_hash_ == hash) & equal(found.assume_read().key, key);
      if (SHOAL_UNLIKELY(overflow_ | (candidate_ & !hit))) {
        const auto from = overflow_ ? home_ + group_width : slot_ + 1;
        return table.racing_find_from(view_, from & (view_.capacity - 1), hash, key, equal, found);
      }
      return hit;
    }

   private:
    RacingView view_;
    std::size_t home_;
    std::size_t slot_;
    std::uint64_t group_;
    const Entry *entry_;
    std::uint64_t entry_hash_;
    bool candidate_;
    bool overflow_;
  };

 private:
  static constexpr std::uint8_t empty_tag = 0;
  static constexpr std::uint8_t occupied_tag = 0x80;
  /// The header line before the tags; its first word is the capacity.
  static constexpr std::size_t header_bytes = 64;
  static constexpr std::size_t block_align = alignof(Entry) > 64 ? alignof(Entry) : 64;
  static constexpr std::size_t group_width = 8;

  A allocator_;
  unsigned char *block_ = nullptr;
  std::size_t capacity_ = 0;
  std::size_t size_ = 0;

  static std::uint8_t tag_of(std::uint64_t hash) noexcept {
    return static_cast<std::uint8_t>(occupied_tag | (hash >> 57));
  }
  static std::size_t threshold(std::size_t capacity) noexcept { return capacity - capacity / 4; }
  static std::size_t slots_offset(std::size_t capacity) noexcept {
    return (header_bytes + capacity + group_width - 1 + block_align - 1) / block_align * block_align;
  }
  /// Writes a tag, and its clone past the last tag if it is among the first
  /// seven.
  static void set_tag(std::uint8_t *tags, std::size_t capacity, std::size_t slot, std::uint8_t tag) noexcept {
    tags[slot] = tag;
    if (slot < group_width - 1) tags[capacity + slot] = tag;
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
      set_tag(tags(), capacity_, hole, tags()[slot]);
      hole = slot;
    }
    set_tag(tags(), capacity_, hole, empty_tag);
  }

  void grow() {
    const auto capacity = capacity_ == 0 ? std::size_t{16} : capacity_ * 2;
    if (capacity > (std::numeric_limits<std::size_t>::max() - header_bytes) / (sizeof(Entry) + 1) / 2)
      throw std::length_error("HashMap shard exhausted");
    auto *block = static_cast<unsigned char *>(HashAllocation::allocate(allocator_, layout(capacity)));
    ::new (static_cast<void *>(block)) std::uint64_t(capacity);
    std::uint8_t *tags = tags_of(block);
    Entry *slots = slots_of(block, capacity);
    for (std::size_t slot = 0; slot < capacity + group_width - 1; ++slot)
      ::new (static_cast<void *>(tags + slot)) std::uint8_t(empty_tag);
    for (std::size_t slot = 0; slot < capacity_; ++slot) {
      if (this->tags()[slot] == empty_tag) continue;
      Entry &entry = this->slots()[slot];
      const auto target = vacant_in(tags, capacity, entry.hash);
      ::new (static_cast<void *>(slots + target)) Entry(std::move(entry));
      set_tag(tags, capacity, target, this->tags()[slot]);
      entry.~Entry();
    }
    // A racing reader reaches the block through `block_`: the header, tags
    // and slots are stored before the pointer.
    std::atomic_thread_fence(std::memory_order_release);
    unsigned char *previous = std::exchange(block_, block);
    const auto previous_capacity = std::exchange(capacity_, capacity);
    if (previous) allocator_.deallocate(previous, layout(previous_capacity));
  }

  SHOAL_RACING_READ RacingView racing_view() const noexcept {
    const unsigned char *block = RacingLoad::relaxed(&block_);
    if (!block) return {nullptr, 0};
    return {block, static_cast<std::size_t>(RacingLoad::relaxed(reinterpret_cast<const std::uint64_t *>(block)))};
  }
  static const Entry *slot_in(const RacingView &view, std::size_t slot) noexcept {
    return reinterpret_cast<const Entry *>(view.block + slots_offset(view.capacity)) + slot;
  }
  /// Eight tags as a word, the first at the low byte, and where in them the
  /// first tag equal to a wanted one and the first empty one are: a zero
  /// byte test (`(x - 0x01..) & ~x & 0x80..`), whose lowest flag is always
  /// a true zero byte.
  class TagGroup {
   public:
    TagGroup(std::uint64_t tags, std::uint8_t wanted) noexcept
        : matches_(zero_bytes(tags ^ (wanted * low_bits))), empties_(zero_bytes(tags)) {}
    /// Index of the first match, `group_width` if none.
    std::size_t first_match() const noexcept { return Bits::trailing_zeros(matches_) / 8; }
    std::size_t first_empty() const noexcept { return Bits::trailing_zeros(empties_) / 8; }
    /// The matches before the first empty tag, as flags (bit 7 of a byte).
    std::uint64_t matches_before_empty() const noexcept {
      return empties_ == 0 ? matches_ : matches_ & ((empties_ & (~empties_ + 1)) - 1);
    }

   private:
    static constexpr std::uint64_t low_bits = 0x0101010101010101ull;
    static constexpr std::uint64_t high_bits = 0x8080808080808080ull;
    static std::uint64_t zero_bytes(std::uint64_t x) noexcept { return (x - low_bits) & ~x & high_bits; }

    std::uint64_t matches_;
    std::uint64_t empties_;
  };

  SHOAL_RACING_READ static std::uint64_t racing_group(const RacingView &view, std::size_t slot) noexcept {
    std::uint64_t group;
    std::memcpy(&group, view.block + header_bytes + slot, sizeof(group));
    return group;
  }
  SHOAL_RACING_READ static std::uint64_t racing_hash(const Entry *entry) noexcept {
    return RacingLoad::relaxed(&entry->hash);
  }
  /// From `slot` on, eight tags at a time, each slot whose tag matches
  /// before an empty one; it gives up after a lap, which only a block a
  /// writer is changing could need.
  template <class Q, class Equal>
  SHOAL_ALWAYS_INLINE bool racing_find_from(const RacingView &view, std::size_t slot, std::uint64_t hash,
                                             const Q &key, const Equal &equal, RacingCopy<Entry> &found) const {
    static_assert(std::is_trivially_copyable_v<Entry>, "racing reads copy entries as bytes");
    const auto wanted = tag_of(hash);
    for (std::size_t probes = 0; probes < view.capacity; probes += group_width) {
      const TagGroup group(racing_group(view, slot), wanted);
      for (auto matches = group.matches_before_empty(); matches != 0; matches &= matches - 1) {
        const Entry *entry = slot_in(view, (slot + Bits::trailing_zeros(matches) / 8) & (view.capacity - 1));
        if (racing_hash(entry) != hash) continue;
        found.read_from(entry);
        if (equal(found.assume_read().key, key)) return true;
      }
      if (group.first_empty() < group_width) return false;
      slot = (slot + group_width) & (view.capacity - 1);
    }
    return false;
  }
};

}  // namespace shoal::detail
