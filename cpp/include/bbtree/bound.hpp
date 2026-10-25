// The bounds of a range, as Rust's `std::ops::Bound`: a key included,
// excluded, or no bound. A range keeps a pointer to its bounds' keys only
// while it is made.
#pragma once

#include <cstdint>

namespace bbtree {

/// A bound of a range, as Rust's `Bound`: a key included or excluded, or
/// none.
template <class Q>
class Bound {
 public:
  enum class Kind : std::uint8_t { Included, Excluded, Unbounded };

  static Bound included(const Q& key) noexcept { return Bound(Kind::Included, &key); }
  static Bound excluded(const Q& key) noexcept { return Bound(Kind::Excluded, &key); }
  static Bound unbounded() noexcept { return Bound(Kind::Unbounded, nullptr); }

  Kind kind() const noexcept { return kind_; }
  const Q& key() const noexcept { return *key_; }

 private:
  Kind kind_;
  const Q* key_;
  Bound(Kind kind, const Q* key) noexcept : kind_(kind), key_(key) {}
};

}  // namespace bbtree
