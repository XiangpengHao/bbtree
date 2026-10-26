// Eight-byte probe descriptors: a high 16-bit fingerprint and a low
// 48-bit entry identifier. Empty and deleted are identifier encodings,
// independent of the fingerprint (including an all-zero fingerprint).
//
// Beside them, a byte a slot, its tag (D27): empty, deleted, or occupied with
// the hash's top seven bits. Lookups walk the tags, a ninth of the index's
// bytes, and read a descriptor only where a tag matches: a key that is absent
// reads no descriptor, and a present one only its own. Descriptors stay the
// truth; every write of one writes its tag (`write_descriptor`), except a
// claim's commit, which keeps the occupied tag its claim already wrote: the
// fingerprint, and so the tag, does not change when a claim is published.
//
// One block holds the descriptors, then the tags.
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <stdexcept>
#include <utility>

#include "allocation.hpp"
#include "tag_group.hpp"
#include "../config.hpp"

namespace shoal::detail {

template <class A>
class HashProbeIndex {
 public:
  /// A slot's tag: empty, deleted (or claimed), or occupied with the top
  /// seven bits of its entry's hash.
  static constexpr std::uint8_t empty_tag = 0;
  static constexpr std::uint8_t deleted_tag = 1;
  static constexpr std::uint8_t occupied_tag = 0x80;

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
  /// Whether a descriptor holds the deleted encoding, which a pending claim
  /// shares.
  static bool descriptor_deleted(std::uint64_t descriptor) noexcept {
    return (descriptor & identifier_mask()) == identifier_mask();
  }
  std::uint64_t identifier(std::size_t slot) const noexcept {
    return descriptors_[slot] & identifier_mask();
  }
  bool occupied(std::size_t slot) const noexcept {
    const auto id = descriptors_[slot] & identifier_mask();
    return id != 0 && id != identifier_mask();
  }
  std::uint64_t fingerprint(std::size_t slot) const noexcept { return descriptors_[slot] >> 48; }
  std::uint8_t tag(std::size_t slot) const noexcept { return tags()[slot]; }
  /// Whether the slot's tag is its descriptor's; a pending claim's is not.
  bool tag_agrees(std::size_t slot) const noexcept { return tag(slot) == tag_of(descriptors_[slot]); }
  /// The tag of a slot whose descriptor holds this hash's entry.
  static std::uint8_t wanted_tag(std::uint64_t hash) noexcept {
    return static_cast<std::uint8_t>(occupied_tag | (hash >> 57));
  }
  /// From `slot` on, the first slot that is empty or holds `hash`'s
  /// fingerprint, a claim's included: where a lookup compares a key, if
  /// anywhere. It reads a descriptor only where the tag matches.
  std::size_t probe(std::uint64_t hash, std::size_t slot) const noexcept {
    const auto wanted = wanted_tag(hash);
    slot = empty_or_wanted(wanted, slot);
    while (tag(slot) != empty_tag && fingerprint(slot) != hash >> 48) slot = empty_or_wanted(wanted, next(slot));
    return slot;
  }
  /// The tags step of `probe` for many keys, every key's before any
  /// descriptor is read (D28), so that their waits for memory overlap: each
  /// row's slot moves to the first that is empty or whose tag is its hash's,
  /// and `empty_of(row)` says which, while that tag is in cache. `hash_of(row)`
  /// is a row's hash, `slot_of(row)` a reference to its slot. `confirm` is
  /// the descriptors step, a row at a time in the caller's next loop over
  /// them, by which time a batch's tags have left the nearest cache.
  template <class Row, class HashOf, class SlotOf, class EmptyOf>
  void probe_tags(const Row *rows, std::size_t count, const HashOf &hash_of, const SlotOf &slot_of,
                  const EmptyOf &empty_of) const noexcept {
    for (std::size_t i = 0; i < count; ++i) {
      auto &slot = slot_of(rows[i]);
      const auto found = empty_or_wanted(wanted_tag(hash_of(rows[i])), static_cast<std::size_t>(slot));
      slot = found;
      empty_of(rows[i]) = tags()[found] == empty_tag;
    }
  }
  /// `probe`'s answer from where `probe_tags` stopped: that slot if `empty`,
  /// with no load, or if it holds `hash`'s fingerprint, with its descriptor's
  /// alone; on from it past another, one tag match in a hundred or so,
  /// updating `empty`.
  std::size_t confirm(std::uint64_t hash, std::size_t slot, bool &empty) const noexcept {
    if (empty || fingerprint(slot) == hash >> 48) return slot;
    slot = probe(hash, next(slot));
    empty = tag(slot) == empty_tag;
    return slot;
  }
  /// `probe` from `hash`'s home, for one key at a time: the home tag first,
  /// alone, by a branch (D27). Predicted, the branch lets the home
  /// descriptor's load start beside the tag's, where a slot found in a group
  /// is known only once its tags arrive. A batch overlaps its keys' loads
  /// instead, and the branch would only mispredict there.
  std::size_t probe_home(std::uint64_t hash) const noexcept {
    const auto slot = home(hash);
    const auto first = tag(slot);
    if (first == empty_tag || (first == wanted_tag(hash) && fingerprint(slot) == hash >> 48)) return slot;
    return probe(hash, slot);
  }
  std::size_t home(std::uint64_t hash) const noexcept {
    return static_cast<std::size_t>(hash) & (capacity_ - 1);
  }
  std::size_t next(std::size_t slot) const noexcept { return (slot + 1) & (capacity_ - 1); }

  template <class Store, class Q, class Equal>
  std::size_t find(std::uint64_t hash, const Q &key, const Store &store, const Equal &equal) const {
    if (capacity_ == 0) return capacity_;
    for (auto slot = probe_home(hash); tag(slot) != empty_tag; slot = probe(hash, next(slot)))
      if (equal(store.at(identifier(slot)).key, key)) return slot;
    return capacity_;
  }

  void publish(std::uint64_t hash, std::uint64_t identifier) noexcept {
    publish_at(vacant(hash), hash, identifier);
  }

  std::size_t vacant(std::uint64_t hash) const noexcept {
    return scan_tags(
        home(hash), [](std::uint8_t tag) { return !(tag & occupied_tag); }, &TagGroup::first_unoccupied);
  }

  template <class Store, class Q, class Equal>
  std::size_t locate(std::uint64_t hash, const Q &key, const Store &store,
                     const Equal &equal) const {
    if (capacity_ == 0) return capacity_;
    const auto wanted = wanted_tag(hash);
    auto slot = home(hash);
    auto deleted = capacity_;
    for (std::uint8_t tag; (tag = this->tag(slot)) != empty_tag; slot = next(slot)) {
      if (tag == wanted) {
        if (fingerprint(slot) == hash >> 48 && equal(store.at(identifier(slot)).key, key))
          return slot;
      } else if (tag == deleted_tag && deleted == capacity_)
        deleted = slot;
    }
    return deleted == capacity_ ? slot : deleted;
  }

  void publish_at(std::size_t slot, std::uint64_t hash, std::uint64_t identifier) noexcept {
    SHOAL_ASSERT(slot < capacity_ && !occupied(slot));
    if (this->identifier(slot) != 0) --deleted_;
    write_descriptor(slot, (hash & ~identifier_mask()) | identifier);
    ++size_;
  }

  // Pending external-row claims share the deleted encoding but never contribute
  // to tombstone accounting. They must be committed or released before return.
  void claim_empty(std::size_t slot, std::uint64_t hash) noexcept {
    SHOAL_ASSERT(identifier(slot) == 0);
    descriptors_[slot] = (hash & ~identifier_mask()) | identifier_mask();
    tags()[slot] = wanted_tag(hash);
  }
  void commit_claim(std::size_t slot, std::uint64_t identifier) noexcept {
    SHOAL_ASSERT(this->identifier(slot) == identifier_mask());
    descriptors_[slot] = (descriptors_[slot] & ~identifier_mask()) | identifier;
    ++size_;
  }
  /// Repoints an occupied slot at another entry of the same fingerprint; like
  /// a commit, it keeps the tag, which the fingerprint alone decides.
  void replace_identifier(std::size_t slot, std::uint64_t identifier) noexcept {
    SHOAL_ASSERT(occupied(slot) && identifier != 0 && identifier < identifier_mask());
    descriptors_[slot] = (descriptors_[slot] & ~identifier_mask()) | identifier;
  }
  void release_claim(std::size_t slot) noexcept {
    SHOAL_ASSERT(identifier(slot) == identifier_mask());
    write_descriptor(slot, 0);
  }

  void erase(std::size_t slot) noexcept {
    SHOAL_ASSERT(occupied(slot));
    write_descriptor(slot, identifier_mask());
    --size_;
    ++deleted_;
  }

  template <class Store>
  void reserve(std::size_t count, const Store &store) {
    auto target = capacity_ == 0 ? std::size_t{16} : capacity_;
    while (count > threshold(target)) {
      if (target > std::numeric_limits<std::size_t>::max() / sizeof(std::uint64_t) / 2)
        throw std::length_error("HashMap probe index exhausted");
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
    if (descriptors_) allocator_.deallocate(descriptors_, layout(capacity_));
    descriptors_ = nullptr;
    capacity_ = size_ = deleted_ = 0;
  }

  template <class Store>
  bool prepare_many(std::size_t additional, const Store &store) {
    if (additional == 0) return false;
    if (additional > std::numeric_limits<std::size_t>::max() - size_)
      throw std::length_error("HashMap batch size exhausted");
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
    // Zero is an empty descriptor and an empty tag.
    if (descriptors_) std::memset(descriptors_, 0, layout(capacity_).size);
    size_ = deleted_ = 0;
  }

 private:
  A allocator_;
  std::uint64_t *descriptors_ = nullptr;
  std::size_t capacity_ = 0;
  std::size_t size_ = 0;
  std::size_t deleted_ = 0;

  static constexpr std::uint64_t identifier_mask() noexcept { return (std::uint64_t{1} << 48) - 1; }
  static std::size_t threshold(std::size_t capacity) noexcept { return capacity - capacity / 4; }
  static Layout layout(std::size_t capacity) noexcept {
    return {capacity * (sizeof(std::uint64_t) + 1), 64};
  }
  /// The tags, after the descriptors in the block.
  std::uint8_t *tags() const noexcept { return reinterpret_cast<std::uint8_t *>(descriptors_ + capacity_); }
  static std::uint8_t tag_of(std::uint64_t descriptor) noexcept {
    const auto identifier = descriptor & identifier_mask();
    if (identifier == 0) return empty_tag;
    if (identifier == identifier_mask()) return deleted_tag;
    return static_cast<std::uint8_t>(occupied_tag | (descriptor >> 57));
  }
  void write_descriptor(std::size_t slot, std::uint64_t descriptor) noexcept {
    descriptors_[slot] = descriptor;
    tags()[slot] = tag_of(descriptor);
  }
  /// From `slot` on, the first slot whose tag is empty or `wanted`.
  std::size_t empty_or_wanted(std::uint8_t wanted, std::size_t slot) const noexcept {
    return scan_tags(
        slot, [wanted](std::uint8_t tag) { return tag == empty_tag || tag == wanted; },
        [wanted](std::uint64_t tags) { return TagGroup(tags, wanted).first_match_or_empty(); });
  }
  /// From `slot` on, the first slot whose tag `stops`, a group of tags at a
  /// time (`first_stop` gives the first in a group, `TagGroup::width` if
  /// none); the last few before the end, where a group would cross it, one
  /// at a time. Some tag must stop it.
  template <class Stops, class FirstStop>
  std::size_t scan_tags(std::size_t slot, const Stops &stops, const FirstStop &first_stop) const noexcept {
    SHOAL_ASSERT(capacity_ != 0);
    const std::uint8_t *tags = this->tags();
    for (;;) {
      if (capacity_ - slot >= TagGroup::width) {
        std::uint64_t group;
        std::memcpy(&group, tags + slot, sizeof(group));
        const auto found = first_stop(group);
        if (found < TagGroup::width) return slot + found;
        slot = (slot + TagGroup::width) & (capacity_ - 1);
      } else {
        for (; slot < capacity_; ++slot)
          if (stops(tags[slot])) return slot;
        slot = 0;
      }
    }
  }

  template <class Store>
  void rebuild(std::size_t capacity, const Store &store) {
    if (capacity == capacity_) return rebuild_in_place(store);
    HashProbeIndex replacement(allocator_);
    replacement.descriptors_ =
        static_cast<std::uint64_t *>(HashAllocation::allocate(allocator_, layout(capacity)));
    replacement.capacity_ = capacity;
    // The block comes zeroed (`HashAllocation`): every descriptor and tag
    // empty.
    store.for_each([&](std::uint64_t identifier, const auto &entry) {
      replacement.write_descriptor(replacement.vacant(entry.hash), (entry.hash & ~identifier_mask()) | identifier);
    });
    replacement.size_ = size_;
    swap(replacement);
  }

  // At the same capacity, drop the tombstones where they lie, allocating
  // nothing: the entries' cached hashes place them again. Only the entry
  // store rebuilds at the same capacity, and its walk runs no user code.
  template <class Store>
  void rebuild_in_place(const Store &store) {
    if (descriptors_) std::memset(descriptors_, 0, layout(capacity_).size);
    store.for_each([&](std::uint64_t identifier, const auto &entry) {
      write_descriptor(vacant(entry.hash), (entry.hash & ~identifier_mask()) | identifier);
    });
    deleted_ = 0;
  }
};

}  // namespace shoal::detail
