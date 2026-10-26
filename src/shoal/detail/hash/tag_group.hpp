// Eight one-byte tags read as one word (D27), as both hash tables probe
// them: an empty tag is zero, an occupied one has bit 7 set. A zero-byte
// test, `(x - 0x01..) & ~x & 0x80..`, flags bit 7 of every byte that is
// zero; it may also flag a byte above a true zero, but its lowest flag is
// always a true zero byte, which is the only one read.
#pragma once

#include <cstddef>
#include <cstdint>

#include "../target.hpp"

namespace shoal::detail {

/// Eight tags as a word, the first at the low byte, and where in them the
/// first tag equal to a wanted one and the first empty one are.
class TagGroup {
 public:
  static constexpr std::size_t width = 8;

  /// `tags` as eight bytes were copied into it, the first from the lowest
  /// address.
  TagGroup(std::uint64_t tags, std::uint8_t wanted) noexcept
      : matches_(zero_bytes(in_slot_order(tags) ^ (wanted * low_bits))),
        empties_(zero_bytes(in_slot_order(tags))) {}
  /// Index of the first match, `width` if none.
  std::size_t first_match() const noexcept { return Bits::trailing_zeros(matches_) / 8; }
  std::size_t first_empty() const noexcept { return Bits::trailing_zeros(empties_) / 8; }
  std::size_t first_match_or_empty() const noexcept {
    return Bits::trailing_zeros(matches_ | empties_) / 8;
  }
  /// The matches before the first empty tag, as flags (bit 7 of a byte).
  std::uint64_t matches_before_empty() const noexcept {
    return empties_ == 0 ? matches_ : matches_ & ((empties_ & (~empties_ + 1)) - 1);
  }
  /// Index of the first tag of `tags` with bit 7 clear, which holds no
  /// entry, `width` if none.
  static std::size_t first_unoccupied(std::uint64_t tags) noexcept {
    return Bits::trailing_zeros(~in_slot_order(tags) & high_bits) / 8;
  }

 private:
  static constexpr std::uint64_t low_bits = 0x0101010101010101ull;
  static constexpr std::uint64_t high_bits = 0x8080808080808080ull;
  static std::uint64_t zero_bytes(std::uint64_t x) noexcept { return (x - low_bits) & ~x & high_bits; }
  /// The first tag at the low byte, whatever the target's byte order.
  static std::uint64_t in_slot_order(std::uint64_t tags) noexcept { return ByteOrder::little_endian64(tags); }

  std::uint64_t matches_;
  std::uint64_t empties_;
};

}  // namespace shoal::detail
