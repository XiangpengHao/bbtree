// Search over a node's inline keys (OG-D18) in explicit vectors
// (`tree/node/vector_count.rs`). The compiler's own count is poor at
// capacities that aren't a multiple of the vector, and on AArch64 widens each
// lane to 64 bits.
//
// - AVX-512 broadcasts the target to every lane: masked loads and two
//   compares per 64 bytes of slots, the masks gathered into a word, one
//   `bzhi` by `len` and one `popcnt`.
// - x86-64 without POPCNT: SSE2 compares; sorted slots make the masks for `<`
//   and `<=` runs of low one-bits whose first zeros give the range (OG-D40).
// - AArch64: NEON counts every width (OG-D56), slots past `len` included,
//   which hold the greatest inline key (invariant 4).
//
// Two equality searches too: an AVX-512 equality mask over 8-byte slots for
// exact-key lookups (OG-D41), and an SSE2 filter of a leaf's hash tags
// (OG-D36).
//
// Every function takes the node kind's capacity as a template argument, a
// constant, so that its loops unroll and its masks fold.
#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>

#include "../target.hpp"

#if defined(SHOAL_ARM64) && defined(SHOAL_MSVC)
#include <arm64_neon.h>
#elif defined(SHOAL_ARM64)
#include <arm_neon.h>
#endif

namespace shoal::detail {

/// A half-open range of indexes, as Rust's `Range<usize>`.
struct IndexRange {
  std::size_t start;
  std::size_t end;

  constexpr std::size_t size() const noexcept { return end - start; }
  constexpr bool empty() const noexcept { return start == end; }
};

/// Which of a lookup's candidates have its hash tag: none, one, two (at these
/// positions), or more.
struct SlotMatches {
  enum class Kind : std::uint8_t { None, One, Two, Many };
  Kind kind;
  std::size_t lower;
  std::size_t upper;

  static constexpr SlotMatches none() noexcept { return {Kind::None, 0, 0}; }
  static constexpr SlotMatches one(std::size_t index) noexcept { return {Kind::One, index, index}; }
  static constexpr SlotMatches two(std::size_t lower, std::size_t upper) noexcept {
    return {Kind::Two, lower, upper};
  }
  static constexpr SlotMatches many() noexcept { return {Kind::Many, 0, 0}; }
};


/// The slots equal to a target, as a bit mask over the capacity: an exact-key
/// lookup over 8-byte slots needs no ordered count.
class U64EqualityMask {
 public:
  explicit U64EqualityMask(std::uint64_t bits) noexcept : bits_(bits) {}

  /// The mask of `Capacity` slots, where AVX-512F has vectors for it.
  template <std::size_t Capacity>
  static inline std::optional<U64EqualityMask> matching(const std::uint64_t* slots,
                                                        std::uint64_t target) noexcept {
#if defined(SHOAL_AVX512F)
    if constexpr (Capacity <= 64) {
      constexpr std::size_t lanes = 8;
      std::uint64_t bits = 0;
      const __m512i broadcast = _mm512_set1_epi64(static_cast<long long>(target));
      for (std::size_t start = 0; start < Capacity; start += lanes) {
        const std::size_t count = Capacity - start < lanes ? Capacity - start : lanes;
        const __mmask8 in_slots = static_cast<__mmask8>((1u << count) - 1);
        const __m512i chunk = _mm512_maskz_loadu_epi64(in_slots, slots + start);
        bits |= static_cast<std::uint64_t>(_mm512_mask_cmpeq_epi64_mask(in_slots, chunk, broadcast))
                << start;
      }
      return U64EqualityMask(bits);
    } else {
      return std::nullopt;
    }
#else
    (void)slots;
    (void)target;
    return std::nullopt;
#endif
  }

  /// The live matching hull, or `nullopt` when no live bit matches.
  std::optional<IndexRange> equal_range(std::size_t len) const noexcept {
    const std::uint64_t bits = Bits::keep_low(bits_, static_cast<unsigned>(len < 64 ? len : 64));
    if (bits == 0) return std::nullopt;
    std::size_t end = 64 - Bits::leading_zeros(bits);
    if (end > len) end = len;
    std::size_t start = Bits::trailing_zeros(bits);
    if (start > end) start = end;
    return IndexRange{start, end};
  }

  /// A unique live position, if there is one.
  SlotMatches unique(std::size_t len) const noexcept {
    const std::uint64_t bits = Bits::keep_low(bits_, static_cast<unsigned>(len < 64 ? len : 64));
    if (bits == 0) return SlotMatches::none();
    if ((bits & (bits - 1)) == 0) return SlotMatches::one(Bits::trailing_zeros(bits));
    return SlotMatches::many();
  }

 private:
  std::uint64_t bits_;
};

/// The hash tags equal to a target among a lookup's candidates, in SSE2
/// vectors whose byte masks fit one word. Tags are unsorted: this finds
/// matches, not a range.
struct U8EqualitySearch {
  template <std::size_t Capacity>
  static inline std::optional<SlotMatches> matching(const std::uint8_t* slots,
                                                    IndexRange candidates,
                                                    std::uint8_t target) noexcept {
#if defined(SHOAL_X86_64)
    constexpr std::size_t lanes = 16;
    if constexpr (Capacity < lanes || Capacity > 64) {
      (void)slots, (void)candidates, (void)target;
      return std::nullopt;
    } else {
      std::uint64_t bits = 0;
      const __m128i broadcast = _mm_set1_epi8(static_cast<char>(target));
      for (std::size_t offset = 0; offset < Capacity; offset += lanes) {
        const std::size_t start = offset < Capacity - lanes ? offset : Capacity - lanes;
        const __m128i chunk = _mm_loadu_si128(reinterpret_cast<const __m128i*>(slots + start));
        const std::uint64_t equal =
            static_cast<std::uint32_t>(_mm_movemask_epi8(_mm_cmpeq_epi8(chunk, broadcast)));
        bits |= equal << start;
      }
      const std::size_t start = candidates.start < Capacity ? candidates.start : Capacity;
      const std::size_t end = candidates.end < Capacity ? candidates.end : Capacity;
      const std::uint64_t from_start = start >= 64 ? 0 : ~std::uint64_t{0} << start;
      const std::uint64_t to_end = end == 0 ? 0 : ~std::uint64_t{0} >> (64 - end);
      bits &= from_start & to_end;
      if (bits == 0) return SlotMatches::none();
      const std::uint64_t remaining = bits & (bits - 1);
      if (remaining == 0) return SlotMatches::one(Bits::trailing_zeros(bits));
      if ((remaining & (remaining - 1)) == 0) {
        return SlotMatches::two(Bits::trailing_zeros(bits), Bits::trailing_zeros(remaining));
      }
      return SlotMatches::many();
    }
#else
    (void)slots, (void)candidates, (void)target;
    return std::nullopt;
#endif
  }
};

#if defined(SHOAL_ARM64)
/// A slot width's NEON lanes: 16-byte vectors of `lanes` slots. A node's
/// capacity is below 2^16, so a lane's count and their sum fit any width.
template <class W>
struct NeonLanes;

template <>
struct NeonLanes<std::uint16_t> {
  static constexpr std::size_t lanes = 8;
  using Vector = uint16x8_t;
  static Vector splat(std::uint16_t value) noexcept { return vdupq_n_u16(value); }
  static Vector load(const std::uint16_t* first) noexcept { return vld1q_u16(first); }
  static Vector positions(std::size_t first) noexcept {
    static const std::uint16_t indexes[lanes] = {0, 1, 2, 3, 4, 5, 6, 7};
    return vaddq_u16(vld1q_u16(indexes), splat(static_cast<std::uint16_t>(first)));
  }
  static Vector below(Vector slots, Vector target) noexcept { return vcgtq_u16(target, slots); }
  static Vector equal(Vector slots, Vector target) noexcept { return vceqq_u16(slots, target); }
  static Vector bit_and(Vector a, Vector b) noexcept { return vandq_u16(a, b); }
  static Vector subtract(Vector a, Vector b) noexcept { return vsubq_u16(a, b); }
  static std::size_t sum(Vector lanes_) noexcept { return vaddvq_u16(lanes_); }
};

template <>
struct NeonLanes<std::uint32_t> {
  static constexpr std::size_t lanes = 4;
  using Vector = uint32x4_t;
  static Vector splat(std::uint32_t value) noexcept { return vdupq_n_u32(value); }
  static Vector load(const std::uint32_t* first) noexcept { return vld1q_u32(first); }
  static Vector positions(std::size_t first) noexcept {
    static const std::uint32_t indexes[lanes] = {0, 1, 2, 3};
    return vaddq_u32(vld1q_u32(indexes), splat(static_cast<std::uint32_t>(first)));
  }
  static Vector below(Vector slots, Vector target) noexcept { return vcgtq_u32(target, slots); }
  static Vector equal(Vector slots, Vector target) noexcept { return vceqq_u32(slots, target); }
  static Vector bit_and(Vector a, Vector b) noexcept { return vandq_u32(a, b); }
  static Vector subtract(Vector a, Vector b) noexcept { return vsubq_u32(a, b); }
  static std::size_t sum(Vector lanes_) noexcept { return vaddvq_u32(lanes_); }
};

template <>
struct NeonLanes<std::uint64_t> {
  static constexpr std::size_t lanes = 2;
  using Vector = uint64x2_t;
  static Vector splat(std::uint64_t value) noexcept { return vdupq_n_u64(value); }
  static Vector load(const std::uint64_t* first) noexcept { return vld1q_u64(first); }
  static Vector positions(std::size_t first) noexcept {
    static const std::uint64_t indexes[lanes] = {0, 1};
    return vaddq_u64(vld1q_u64(indexes), splat(first));
  }
  static Vector below(Vector slots, Vector target) noexcept { return vcgtq_u64(target, slots); }
  static Vector equal(Vector slots, Vector target) noexcept { return vceqq_u64(slots, target); }
  static Vector bit_and(Vector a, Vector b) noexcept { return vandq_u64(a, b); }
  static Vector subtract(Vector a, Vector b) noexcept { return vsubq_u64(a, b); }
  static std::size_t sum(Vector lanes_) noexcept { return vaddvq_u64(lanes_); }
};
#endif

/// The count of a node's inline keys below and equal to a target, in the
/// build target's explicit vectors, for slots of `W`.
template <class W>
struct VectorCount {
  /// The positions of the first `len` of the `Capacity` slots equal to
  /// `target`: those below it, then those equal. Exactly the equal ones for
  /// sorted slots, a range within `0..len` for any. `nullopt` where the
  /// target has no such vectors, or the compiler's count is faster.
  template <std::size_t Capacity>
  static inline std::optional<IndexRange> equal_range(const W* slots, std::size_t len,
                                                      W target) noexcept {
#if defined(SHOAL_ARM64)
    return neon_equal_range<Capacity>(slots, len, target);
#elif defined(SHOAL_AVX512BW)
    if constexpr (sizeof(W) == 8 && (Capacity < 28 || Capacity > 31)) {
      return std::nullopt;
    } else {
      return avx512_equal_range<Capacity>(slots, len, target);
    }
#elif defined(SHOAL_X86_64) && !defined(SHOAL_POPCNT)
    if constexpr (sizeof(W) == 8) {
      return std::nullopt;
    } else {
      return sse2_equal_range<Capacity>(slots, len, target);
    }
#else
    (void)slots, (void)len, (void)target;
    return std::nullopt;
#endif
  }

  /// The equality mask an exact-key lookup over 8-byte slots takes.
  template <std::size_t Capacity>
  static inline std::optional<U64EqualityMask> equality_mask(const W* slots, W target) noexcept {
    if constexpr (sizeof(W) == 8) {
      return U64EqualityMask::matching<Capacity>(slots, target);
    } else {
      (void)slots, (void)target;
      return std::nullopt;
    }
  }

 private:
#if defined(SHOAL_ARM64)
  template <std::size_t Capacity>
  static inline std::optional<IndexRange> neon_equal_range(const W* slots, std::size_t len,
                                                           W target) noexcept {
    using Lanes = NeonLanes<W>;
    constexpr std::size_t lanes = Lanes::lanes;
    if constexpr (Capacity < lanes) {
      return std::nullopt;
    } else {
      constexpr std::size_t tail = Capacity % lanes;
      constexpr std::size_t whole = Capacity - tail;
      const auto broadcast = Lanes::splat(target);
      auto below = Lanes::splat(0);
      auto equal = Lanes::splat(0);
      for (std::size_t start = 0; start < whole; start += lanes) {
        const auto chunk = Lanes::load(slots + start);
        below = Lanes::subtract(below, Lanes::below(chunk, broadcast));
        equal = Lanes::subtract(equal, Lanes::equal(chunk, broadcast));
      }
      if constexpr (tail != 0) {
        constexpr std::size_t first = Capacity - lanes;
        const auto chunk = Lanes::load(slots + first);
        // The lanes the whole vectors didn't count: `whole..`.
        const auto fresh =
            Lanes::below(Lanes::splat(static_cast<W>(whole - 1)), Lanes::positions(first));
        below = Lanes::subtract(below, Lanes::bit_and(Lanes::below(chunk, broadcast), fresh));
        equal = Lanes::subtract(equal, Lanes::bit_and(Lanes::equal(chunk, broadcast), fresh));
      }
      std::size_t below_count = Lanes::sum(below);
      const std::size_t equal_count = Lanes::sum(equal);
      if (below_count > len) below_count = len;
      std::size_t end = below_count + equal_count;
      if (end > len) end = len;
      return IndexRange{below_count, end};
    }
  }
#endif

#if defined(SHOAL_AVX512BW)
  template <std::size_t Capacity>
  static inline std::optional<IndexRange> avx512_equal_range(const W* slots, std::size_t len,
                                                             W target) noexcept {
    constexpr std::size_t lanes = 64 / sizeof(W);
    constexpr std::size_t word_lanes = 64;
    std::size_t below = 0, equal = 0;
    for (std::size_t word_start = 0; word_start < Capacity; word_start += word_lanes) {
      std::uint64_t below_bits = 0, equal_bits = 0;
      for (std::size_t offset = 0; offset < word_lanes && word_start + offset < Capacity;
           offset += lanes) {
        const std::size_t start = word_start + offset;
        const std::size_t count = Capacity - start < lanes ? Capacity - start : lanes;
        const std::uint64_t in_slots = count >= 64 ? ~std::uint64_t{0} : (std::uint64_t{1} << count) - 1;
        std::uint64_t below_mask, equal_mask;
        if constexpr (sizeof(W) == 2) {
          const __m512i chunk = _mm512_maskz_loadu_epi16(static_cast<__mmask32>(in_slots), slots + start);
          const __m512i broadcast = _mm512_set1_epi16(static_cast<short>(target));
          below_mask = _mm512_mask_cmplt_epu16_mask(static_cast<__mmask32>(in_slots), chunk, broadcast);
          equal_mask = _mm512_mask_cmpeq_epu16_mask(static_cast<__mmask32>(in_slots), chunk, broadcast);
        } else if constexpr (sizeof(W) == 4) {
          const __m512i chunk = _mm512_maskz_loadu_epi32(static_cast<__mmask16>(in_slots), slots + start);
          const __m512i broadcast = _mm512_set1_epi32(static_cast<int>(target));
          below_mask = _mm512_mask_cmplt_epu32_mask(static_cast<__mmask16>(in_slots), chunk, broadcast);
          equal_mask = _mm512_mask_cmpeq_epu32_mask(static_cast<__mmask16>(in_slots), chunk, broadcast);
        } else {
          const __m512i chunk = _mm512_maskz_loadu_epi64(static_cast<__mmask8>(in_slots), slots + start);
          const __m512i broadcast = _mm512_set1_epi64(static_cast<long long>(target));
          below_mask = _mm512_mask_cmplt_epu64_mask(static_cast<__mmask8>(in_slots), chunk, broadcast);
          equal_mask = _mm512_mask_cmpeq_epu64_mask(static_cast<__mmask8>(in_slots), chunk, broadcast);
        }
        below_bits |= below_mask << offset;
        equal_bits |= equal_mask << offset;
      }
      const std::size_t remaining = len > word_start ? len - word_start : 0;
      const unsigned live = static_cast<unsigned>(remaining < word_lanes ? remaining : word_lanes);
      below += Bits::count_ones(Bits::keep_low(below_bits, live));
      equal += Bits::count_ones(Bits::keep_low(equal_bits, live));
    }
    return IndexRange{below, below + equal};
  }
#endif

#if defined(SHOAL_X86_64) && !defined(SHOAL_AVX512BW) && !defined(SHOAL_POPCNT)
  template <std::size_t Capacity>
  static inline std::optional<IndexRange> sse2_equal_range(const W* slots, std::size_t len,
                                                           W target) noexcept {
    constexpr std::size_t slot_bytes = sizeof(W);
    constexpr std::size_t lanes = 16 / slot_bytes;
    constexpr std::size_t unpacked_max_slots = 64 / slot_bytes;
    constexpr bool packed = Capacity > unpacked_max_slots;
    constexpr std::size_t max_slots = packed ? 2 * unpacked_max_slots : unpacked_max_slots;
    if constexpr (Capacity < lanes || Capacity > max_slots) {
      return std::nullopt;
    } else {
      constexpr std::size_t bits_per_slot = packed ? slot_bytes / 2 : slot_bytes;
      std::uint64_t below_bits = 0, through_bits = 0;
      constexpr W sign_bit = static_cast<W>(W{1} << (8 * slot_bytes - 1));
      const __m128i flip_sign = set1(sign_bit);
      const __m128i broadcast = set1(static_cast<W>(target ^ sign_bit));
      if constexpr (packed) {
        for (std::size_t offset = 0; offset < Capacity; offset += 2 * lanes) {
          const std::size_t start = offset < Capacity - 2 * lanes ? offset : Capacity - 2 * lanes;
          const __m128i first = _mm_xor_si128(
              _mm_loadu_si128(reinterpret_cast<const __m128i*>(slots + start)), flip_sign);
          const __m128i second = _mm_xor_si128(
              _mm_loadu_si128(reinterpret_cast<const __m128i*>(slots + start + lanes)), flip_sign);
          const __m128i lower = pack(greater(broadcast, first), greater(broadcast, second));
          const __m128i above = pack(greater(first, broadcast), greater(second, broadcast));
          below_bits |= static_cast<std::uint64_t>(static_cast<std::uint32_t>(_mm_movemask_epi8(lower)))
                        << (start * bits_per_slot);
          through_bits |=
              static_cast<std::uint64_t>(static_cast<std::uint16_t>(~_mm_movemask_epi8(above)))
              << (start * bits_per_slot);
        }
      } else {
        for (std::size_t offset = 0; offset < Capacity; offset += lanes) {
          const std::size_t start = offset < Capacity - lanes ? offset : Capacity - lanes;
          const __m128i chunk = _mm_xor_si128(
              _mm_loadu_si128(reinterpret_cast<const __m128i*>(slots + start)), flip_sign);
          const std::uint64_t lower =
              static_cast<std::uint32_t>(_mm_movemask_epi8(greater(broadcast, chunk)));
          const std::uint64_t equal =
              static_cast<std::uint32_t>(_mm_movemask_epi8(equals(broadcast, chunk)));
          below_bits |= lower << (start * slot_bytes);
          through_bits |= (lower | equal) << (start * slot_bytes);
        }
      }
      std::size_t below = Bits::trailing_ones(below_bits) / bits_per_slot;
      std::size_t through = Bits::trailing_ones(through_bits) / bits_per_slot;
      if (below > len) below = len;
      if (through > len) through = len;
      return IndexRange{below, through};
    }
  }

  static __m128i set1(W value) noexcept {
    if constexpr (sizeof(W) == 2) return _mm_set1_epi16(static_cast<short>(value));
    else return _mm_set1_epi32(static_cast<int>(value));
  }
  static __m128i greater(__m128i a, __m128i b) noexcept {
    if constexpr (sizeof(W) == 2) return _mm_cmpgt_epi16(a, b);
    else return _mm_cmpgt_epi32(a, b);
  }
  static __m128i equals(__m128i a, __m128i b) noexcept {
    if constexpr (sizeof(W) == 2) return _mm_cmpeq_epi16(a, b);
    else return _mm_cmpeq_epi32(a, b);
  }
  static __m128i pack(__m128i a, __m128i b) noexcept {
    if constexpr (sizeof(W) == 2) return _mm_packs_epi16(a, b);
    else return _mm_packs_epi32(a, b);
  }
#endif
};

}  // namespace shoal::detail
