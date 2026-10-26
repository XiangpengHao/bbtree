// A prehashed index of externally owned rows. Keys, cached hashes, and handles
// must remain stable until clear or clear_retain_capacity. Handles occupy 48 bits; zero and 2^48-1
// are reserved. This index does not own rows, keys, or payloads, and uses supplied hashes unchanged
// (without mixing). Positions remain valid throughout a batch call, then until reserve, growth,
// clear, or clear_retain_capacity.
#pragma once

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <stdexcept>
#include <type_traits>
#include <utility>

#include "../detail/hash/probe_index.hpp"

namespace shoal::unsync {

template <class Handle, class A = Global>
class HashIndex {
  static_assert((std::is_integral_v<Handle> && std::is_unsigned_v<Handle> &&
                 sizeof(Handle) <= sizeof(std::uint64_t)) ||
                    (std::is_pointer_v<Handle> &&
                     (std::is_object_v<std::remove_pointer_t<Handle>> ||
                      std::is_void_v<std::remove_pointer_t<Handle>>)),
                "HashIndex requires an unsigned integer or object pointer handle");
  static_assert(std::is_nothrow_move_constructible_v<A> && std::is_nothrow_swappable_v<A>,
                "HashIndex allocator must move and swap without throwing");
  using ProbeIndex = detail::HashProbeIndex<A>;

 public:
  using handle_type = Handle;
  using row_type = std::uint32_t;
  using position_type = std::uint64_t;
  static constexpr std::size_t maximum_batch_size() noexcept { return 2048; }

  class ProbeState {
   public:
    ProbeState() noexcept {}  // Initialize only live prefixes, never the whole scratch.
    ProbeState(const ProbeState &) = delete;
    ProbeState &operator=(const ProbeState &) = delete;

   private:
    friend class HashIndex;
    std::uint16_t fingerprints_[2048];
    position_type slots_[2048];
    row_type pending_[2048], candidates_[2048], rejected_[2048], new_rows_[2048];
    Handle new_handles_[2048];
  };

  explicit HashIndex(A allocator = A()) : index_(std::move(allocator)) {}
  HashIndex(const HashIndex &) = delete;
  HashIndex &operator=(const HashIndex &) = delete;
  HashIndex(HashIndex &&) noexcept = default;
  HashIndex &operator=(HashIndex &&other) noexcept {
    if (this != &other) {
      HashIndex moved(std::move(other));
      swap(moved);
    }
    return *this;
  }
  void swap(HashIndex &other) noexcept { index_.swap(other.index_); }
  std::size_t size() const noexcept { return index_.size(); }
  std::size_t capacity() const noexcept { return index_.capacity(); }
  Handle handle_at(position_type slot) const noexcept {
    SHOAL_ASSERT(slot < capacity());
    return index_.occupied(static_cast<std::size_t>(slot))
               ? decode(index_.identifier(static_cast<std::size_t>(slot)))
               : Handle{};
  }
  void clear() noexcept { index_.clear(); }
  void clear_retain_capacity() noexcept { index_.reset(); }

  template <class StoredHash>
  void reserve(std::size_t count, StoredHash stored_hash) {
    index_.reserve(count, ResidentRows<StoredHash>{index_, stored_hash});
  }

  // On growth, enumerate(visit) must visit every indexed row exactly once as
  // visit(handle, cached_hash), in any order. Handles, hashes, and indexed keys
  // must agree with the existing rows. No enumeration occurs without growth.
  // Enumeration, validation, and allocation failures preserve the prior index.
  template <class Enumerate>
  void reserve_from(std::size_t count, Enumerate enumerate) {
    index_.reserve(count, EnumeratedRows<Enumerate>{size(), enumerate});
  }

  // hash_at(row_type global_row) returns the precomputed uint64_t hash, at most
  // once per query. compare(row_type *candidates, size_t count, Handle *results,
  // row_type *rejected) returns the rejected count. Results use global input row
  // indices and already contain candidate handles. Compare may reorder candidates
  // but must output only a subset of the original rows in rejected. Compare must
  // treat results as read-only. Accepted rows retain their candidate result;
  // rejected rows probe onward. Misses produce a
  // zero/null handle and UINT64_MAX position. Zero count accepts null outputs.
  // Counts above UINT32_MAX throw before mutation. Callbacks must not reenter or
  // mutate index state; indexed keys/hashes/handles must remain stable, while
  // externally owned payloads may change. Concurrent const lookups require
  // independent ProbeStates and callback/output storage.
  // Equivalent keys must supply identical hashes. Output arrays must be disjoint
  // from scratch, index storage, query data, and each other; rejected rows must
  // be distinct. Outputs belong exclusively to this call: callbacks must not
  // mutate results or positions through any alias. Compare may reorder candidates.
  template <class HashAt, class Compare>
  void find_many(ProbeState &state, std::size_t count, HashAt hash_at, Compare compare,
                 Handle *results, position_type *positions = nullptr) const {
    validate_count(count);
    for (std::size_t base = 0; base < count;) {
      const auto width = std::min(maximum_batch_size(), count - base);
      auto *working_positions = positions ? positions + base : state.slots_;
      initialize(state, working_positions, base, width, hash_at);
      auto pending = width;
      while (pending) {
        std::size_t candidates = 0;
        for (std::size_t i = 0; i < pending; ++i) {
          const auto row = state.pending_[i];
          auto &slot = working_positions[row - base];
          const auto fingerprint = state.fingerprints_[row - base];
          while (capacity() && index_.descriptor(slot) && index_.fingerprint(slot) != fingerprint)
            slot = index_.next(slot);
          if (!capacity() || !index_.descriptor(slot)) {
            results[row] = Handle{};
            if (positions) positions[row] = missing_position();
          } else {
            results[row] = decode(index_.identifier(slot));
            state.candidates_[candidates++] = row;
          }
        }
        pending = reject_candidates(state, working_positions, base, candidates, compare, results);
      }
      base += width;
    }
  }

  // stored_hash(Handle) returns the cached uint64_t hash for reserve/rebuild or
  // invariant checking. create(const row_type *new_rows, size_t count,
  // Handle *new_handles) materializes one batch and returns handles in row order.
  // Each returned handle must identify its requested key, with stored_hash(handle)
  // equal to hash_at(new_row). All output handles must be initialized in row order.
  // All returned handles are validated before publishing any in that round.
  // On callback/validation failure unpublished claims roll back; earlier rounds
  // remain committed and outputs are partial/unspecified. The caller must clean
  // up its own partially materialized external rows. Compare failures leave all
  // published handles indexed. Rebuild failures preserve the prior index.
  template <class HashAt, class StoredHash, class Compare, class Create>
  void find_or_create_many(ProbeState &state, std::size_t count, HashAt hash_at,
                           StoredHash stored_hash, Compare compare, Create create, Handle *results,
                           position_type *positions = nullptr) {
    validate_count(count);
    if (count > std::numeric_limits<std::size_t>::max() - size())
      throw std::length_error("HashIndex size exhausted");
    reserve(size() + count, stored_hash);
    for (std::size_t base = 0; base < count;) {
      const auto width = std::min(maximum_batch_size(), count - base);
      auto *working_positions = positions ? positions + base : state.slots_;
      initialize(state, working_positions, base, width, hash_at);
      auto pending = width;
      while (pending) {
        PendingClaims claims(index_, state.new_rows_, working_positions, base);
        std::size_t candidates = 0;
        for (std::size_t i = 0; i < pending; ++i) {
          const auto row = state.pending_[i];
          auto &slot = working_positions[row - base];
          const auto fingerprint = state.fingerprints_[row - base];
          while (index_.descriptor(slot) && index_.fingerprint(slot) != fingerprint)
            slot = index_.next(slot);
          if (!index_.descriptor(slot)) {
            index_.claim_empty(slot, std::uint64_t{fingerprint} << 48);
            claims.add(row);
          } else
            state.candidates_[candidates++] = row;
        }
        if (claims.count()) {
          create(state.new_rows_, claims.count(), state.new_handles_);
          validate_handles(state.new_handles_, claims.count());
          for (std::size_t i = 0; i < claims.count(); ++i) {
            const auto row = state.new_rows_[i];
            index_.commit_claim(working_positions[row - base], encode(state.new_handles_[i]));
            results[row] = state.new_handles_[i];
          }
          claims.published();
        }
        for (std::size_t i = 0; i < candidates; ++i) {
          const auto row = state.candidates_[i];
          const auto slot = working_positions[row - base];
          results[row] = decode(index_.identifier(slot));
        }
        pending = reject_candidates(state, working_positions, base, candidates, compare, results);
      }
      base += width;
    }
  }

  template <class StoredHash>
  void check_invariants(StoredHash stored_hash) const {
    std::size_t occupied = 0;
    for (std::size_t slot = 0; slot < capacity(); ++slot) {
      if (!index_.descriptor(slot)) continue;
      if (!index_.occupied(slot)) throw std::logic_error("HashIndex unpublished claim");
      ++occupied;
      const auto hash = stored_hash(decode(index_.identifier(slot)));
      if (index_.fingerprint(slot) != hash >> 48)
        throw std::logic_error("HashIndex fingerprint mismatch");
      auto probe = index_.home(hash);
      while (probe != slot) {
        if (!index_.identifier(probe)) throw std::logic_error("HashIndex broken probe chain");
        probe = index_.next(probe);
      }
    }
    if (occupied != size()) throw std::logic_error("HashIndex size mismatch");
  }

 private:
  ProbeIndex index_;
  static constexpr position_type missing_position() noexcept {
    return std::numeric_limits<position_type>::max();
  }
  static std::uint64_t encode(Handle handle) noexcept {
    if constexpr (std::is_pointer_v<Handle>)
      return reinterpret_cast<std::uintptr_t>(handle);
    else
      return static_cast<std::uint64_t>(handle);
  }
  static Handle decode(std::uint64_t handle) noexcept {
    if constexpr (std::is_pointer_v<Handle>)
      return reinterpret_cast<Handle>(static_cast<std::uintptr_t>(handle));
    else
      return static_cast<Handle>(handle);
  }
  static void validate_handles(const Handle *handles, std::size_t count) {
    bool invalid = false;
    for (std::size_t i = 0; i < count; ++i)
      invalid |= encode(handles[i]) - 1 >= (std::uint64_t{1} << 48) - 2;
    if (invalid) throw std::invalid_argument("HashIndex handle must be in [1, 2^48-2]");
  }
  static void validate_handle(Handle handle) {
    const auto encoded = encode(handle);
    if (!encoded || encoded >= (std::uint64_t{1} << 48) - 1)
      throw std::invalid_argument("HashIndex handle must be in [1, 2^48-2]");
  }
  static void validate_count(std::size_t count) {
    if (count > std::numeric_limits<row_type>::max())
      throw std::length_error("HashIndex input row range exhausted");
  }
  template <class StoredHash>
  struct ResidentRows {
    const ProbeIndex &index;
    StoredHash &stored_hash;
    template <class Visitor>
    void for_each(Visitor visitor) const {
      struct CachedHash {
        std::uint64_t hash;
      };
      for (std::size_t slot = 0; slot < index.capacity(); ++slot)
        if (index.occupied(slot)) {
          const auto id = index.identifier(slot);
          visitor(id, CachedHash{stored_hash(decode(id))});
        }
    }
  };
  template <class Enumerate>
  struct EnumeratedRows {
    std::size_t expected_count;
    Enumerate &enumerate;
    template <class Visitor>
    void for_each(Visitor visitor) const {
      struct CachedHash {
        std::uint64_t hash;
      };
      std::size_t visited = 0;
      enumerate([&](Handle handle, std::uint64_t hash) {
        if (visited == expected_count)
          throw std::invalid_argument("HashIndex enumeration has extra rows");
        validate_handle(handle);
        ++visited;
        visitor(encode(handle), CachedHash{hash});
      });
      if (visited != expected_count)
        throw std::invalid_argument("HashIndex enumeration omitted rows");
    }
  };
  class PendingClaims {
   public:
    PendingClaims(ProbeIndex &index, row_type *rows, position_type *positions, std::size_t base)
        : index_(index), rows_(rows), positions_(positions), base_(base) {}
    ~PendingClaims() {
      for (std::size_t i = 0; i < count_; ++i) index_.release_claim(positions_[rows_[i] - base_]);
    }
    void add(row_type row) noexcept { rows_[count_++] = row; }
    std::size_t count() const noexcept { return count_; }
    void published() noexcept { count_ = 0; }

   private:
    ProbeIndex &index_;
    row_type *rows_;
    position_type *positions_;
    std::size_t base_;
    std::size_t count_ = 0;
  };
  template <class HashAt>
  void initialize(ProbeState &state, position_type *positions, std::size_t base, std::size_t width,
                  HashAt &hash_at) const {
    for (std::size_t i = 0; i < width; ++i) {
      const auto hash = hash_at(static_cast<row_type>(base + i));
      state.fingerprints_[i] = static_cast<std::uint16_t>(hash >> 48);
      positions[i] = capacity() ? index_.home(hash) : 0;
      state.pending_[i] = static_cast<row_type>(base + i);
    }
  }
  template <class Compare>
  std::size_t reject_candidates(ProbeState &state, position_type *positions, std::size_t base,
                                std::size_t count, Compare &compare, Handle *results) const {
    if (!count) return 0;
    const auto rejected = compare(state.candidates_, count, results, state.rejected_);
    SHOAL_ASSERT(rejected <= count);
    for (std::size_t i = 0; i < rejected; ++i) {
      const auto row = state.rejected_[i];
      positions[row - base] = index_.next(positions[row - base]);
      state.pending_[i] = row;
    }
    return rejected;
  }
};

}  // namespace shoal::unsync
