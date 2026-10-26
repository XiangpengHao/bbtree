// What an insert into a shared `TreeMap` returns (`allocator.rs`,
// `InsertError`): what it did, or, if its allocator had no page to give, the
// entry back, with the map as it was (OG-D8).
#pragma once

#include <optional>
#include <type_traits>
#include <utility>

#include "allocator.hpp"

namespace shoal {

/// An insert whose allocator had no page to give: the map is as it was, and
/// the entry comes back.
template <class K, class V>
class InsertError {
 public:
  InsertError(K&& key, V&& value, Layout layout) noexcept
      : key_(std::move(key)), value_(std::move(value)), layout_(layout) {}

  K& key() noexcept { return key_; }
  V& value() noexcept { return value_; }
  /// The layout of the page the allocator was asked for.
  Layout layout() const noexcept { return layout_; }
  std::pair<K, V> into_entry() && noexcept { return {std::move(key_), std::move(value_)}; }

 private:
  K key_;
  V value_;
  Layout layout_;
};

/// What an insert did, `T`, or the entry back if it couldn't allocate.
template <class T, class K, class V>
class InsertOutcome {
 public:
  static InsertOutcome done(T&& output) noexcept { return InsertOutcome(std::move(output)); }
  static InsertOutcome failed(InsertError<K, V>&& error) noexcept { return InsertOutcome(std::move(error)); }

  bool ok() const noexcept { return output_.has_value(); }
  explicit operator bool() const noexcept { return ok(); }
  T& value() noexcept { return *output_; }
  const T& value() const noexcept { return *output_; }
  InsertError<K, V>& error() noexcept { return *error_; }

 private:
  std::optional<T> output_;
  std::optional<InsertError<K, V>> error_;

  explicit InsertOutcome(T&& output) noexcept : output_(std::move(output)) {}
  explicit InsertOutcome(InsertError<K, V>&& error) noexcept : error_(std::move(error)) {}
};

/// What a leaf closure of a shared map's insert returns
/// (`update_or_insert_with_leaf`): its result, or the layout it had no memory
/// for, which refuses the insert, leaving the map as it was and the entry to
/// come back in an `InsertError` (Rust's `Result<R, Layout>`).
template <class R>
class LeafOutcome {
 public:
  static LeafOutcome done(R value) noexcept { return LeafOutcome(std::move(value), Layout{0, 0}); }
  static LeafOutcome refused(Layout layout) noexcept { return LeafOutcome(std::nullopt, layout); }

  bool ok() const noexcept { return value_.has_value(); }
  R& value() noexcept { return *value_; }
  Layout layout() const noexcept { return layout_; }

 private:
  std::optional<R> value_;
  Layout layout_;

  LeafOutcome(std::optional<R> value, Layout layout) noexcept : value_(std::move(value)), layout_(layout) {}
};

/// What a closure returning `void` returns, where a result is kept.
struct Unit {
  friend constexpr bool operator==(Unit, Unit) noexcept { return true; }
};

namespace detail {

/// The result of calling `F` with `Args`, `Unit` for `void`, and how to
/// call it to get one.
template <class F, class... Args>
struct Returned {
  using Raw = std::invoke_result_t<F, Args...>;
  using type = std::conditional_t<std::is_void_v<Raw>, Unit, Raw>;

  template <class G, class... Actual>
  static type call(G&& f, Actual&&... args) {
    if constexpr (std::is_void_v<Raw>) {
      std::forward<G>(f)(std::forward<Actual>(args)...);
      return Unit{};
    } else {
      return std::forward<G>(f)(std::forward<Actual>(args)...);
    }
  }
};

}  // namespace detail
}  // namespace shoal
