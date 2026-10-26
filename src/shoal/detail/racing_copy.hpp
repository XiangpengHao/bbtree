// A copy an optimistic reader makes of a slot that writers may be changing
// meanwhile (PORTING.md, P7): bytes, which the reader takes for a value only
// once a version check vouches for the read. The tree's leaves and the hash
// maps' entries are read this way.
#pragma once

#include <cstring>
#include <new>

#include "config.hpp"

namespace shoal::detail {

/// A copy of a value of type `T`, read as bytes from a slot a writer may be
/// changing: a `T` only once a version check vouches for the read.
template <class T>
class RacingCopy {
 public:
  SHOAL_RACING_READ void read_from(const void* slot) noexcept {
    std::memcpy(static_cast<void*>(bytes_), slot, sizeof(T));
  }
  /// The value read, once a version vouched for the read.
  const T& assume_read() const noexcept {
    return *std::launder(reinterpret_cast<const T*>(bytes_));
  }

 private:
  alignas(T) unsigned char bytes_[sizeof(T)];
};

}  // namespace shoal::detail
