// Descriptor discovery and payload comparisons run in separate loops. Only
// false fingerprint matches survive to another round. Discovery walks the
// index's tags, which stay in cache, before any descriptor. Larger bounded tiles
// overlap payload waits across more queries without software prefetching.
#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

#include "../../hash.hpp"
#include "entry_store.hpp"
#include "probe_index.hpp"

namespace bbtree::detail {

template <class K, class V, class A>
class HashLookupBatch {
  using Index = HashProbeIndex<A>;
  using Entry = typename HashEntryStore<K, V, A>::Entry;

 public:
  static constexpr std::size_t maximum_width() noexcept { return 128; }

  template <class Q, class Hasher, class Equal>
  static void find_many(const HashProbeIndex<A>& index, const HashEntryStore<K, V, A>& entries,
                        const Q* keys, std::size_t count, const V** values, const Hasher& hash,
                        const Equal& equal) {
    find_many_impl(
        index, entries, keys, count, values,
        [&](const Q& key, std::size_t) { return HashBits::mix(hash(key)); }, equal, nullptr);
  }

  template <class Q, class Equal>
  BBTREE_ALWAYS_INLINE static void find_hashed(const Index& index,
                                               const HashEntryStore<K, V, A>& entries,
                                               const Q* keys, const std::uint64_t* hashes,
                                               std::size_t count, const V** values,
                                               const Equal& equal,
                                               std::size_t* positions = nullptr) {
    BBTREE_ASSERT(count <= maximum_width());
    find_many_impl(
        index, entries, keys, count, values, [&](const Q&, std::size_t row) { return hashes[row]; },
        equal, positions);
  }

 private:
  template <class Q, class HashAt, class Equal>
  BBTREE_ALWAYS_INLINE static void find_many_impl(const Index& index,
                                                  const HashEntryStore<K, V, A>& entries,
                                                  const Q* keys, std::size_t count,
                                                  const V** values, const HashAt& hash_at,
                                                  const Equal& equal, std::size_t* positions) {
    while (count != 0) {
      std::array<std::uint64_t, 128> hashes;
      std::array<std::size_t, 128> local_slots;
      auto* slots = positions ? positions : local_slots.data();
      std::array<std::uint8_t, 128> pending, candidates;
      std::array<const Entry*, 128> payloads;
      const auto width = count < hashes.size() ? count : hashes.size();
      if (index.capacity() == 0) {
        for (std::size_t row = 0; row < width; ++row) values[row] = nullptr;
      } else {
        for (std::size_t row = 0; row < width; ++row) {
          hashes[row] = hash_at(keys[row], row);
          slots[row] = index.home(hashes[row]);
          pending[row] = static_cast<std::uint8_t>(row);
        }
        auto remaining = width;
        while (remaining != 0) {
          // Tags first (in cache), then the descriptors and payloads of the
          // slots whose tags match: an absent key reads no descriptor.
          std::size_t candidate_count = 0;
          for (std::size_t lane = 0; lane < remaining; ++lane) {
            const auto row = pending[lane];
            auto slot = index.probe(hashes[row], slots[row]);
            while (index.tag(slot) != Index::empty_tag &&
                   !Index::descriptor_matches(index.descriptor(slot), hashes[row]))
              slot = index.probe(hashes[row], index.next(slot));
            slots[row] = slot;
            if (index.tag(slot) == Index::empty_tag)
              values[row] = nullptr;
            else {
              candidates[candidate_count] = row;
              payloads[candidate_count++] = &entries.at(index.identifier(slot));
            }
          }
          remaining = 0;
          for (std::size_t candidate = 0; candidate < candidate_count; ++candidate) {
            const auto row = candidates[candidate];
            const auto* payload = payloads[candidate];
            if (equal(payload->key, keys[row]))
              values[row] = &payload->value;
            else {
              slots[row] = index.next(slots[row]);
              pending[remaining++] = row;
            }
          }
        }
      }
      count -= width;
      keys += width;
      values += width;
    }
  }
};

}  // namespace bbtree::detail
