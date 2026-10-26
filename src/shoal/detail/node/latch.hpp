// Latches for a tree that threads share (design/09-concurrency.md,
// `tree/node/latch.rs`): a word in every node's header. One word serves both
// ways the tree is latched, so the root page, which changes kind, keeps one
// word through the change:
//
// - **Internal nodes, optimistically.** A reader reads the version, reads the
//   node, and checks that the version is unchanged. A writer takes the node
//   exclusively from the version it read, never waiting, and bumps the
//   version as it lets go: optimistic lock coupling.
// - **Leaves, pessimistically.** Shared for reads, exclusive for changes,
//   spinning with backoff. Letting go exclusively bumps the version too, so a
//   lookup can also read a leaf optimistically (D57).
//
// The word, from the low bit: the exclusive bit, a count of shared holders,
// and the version. A version is the word without its count, read while the
// exclusive bit is clear.
#pragma once

#include <atomic>
#include <cstdint>

#include "../config.hpp"
#include "../target.hpp"

namespace shoal::detail {

/// What an optimistic reader saw of a node: its latch word without the
/// shared count, unlatched exclusively. Equal versions of one node mean that
/// nothing changed it in between.
class Version {
 public:
  /// Uninitialized, for arrays of held nodes that cost nothing to make.
  Version() noexcept = default;
  constexpr explicit Version(std::uint64_t word) noexcept : word_(word) {}
  constexpr std::uint64_t word() const noexcept { return word_; }
  constexpr bool operator==(Version other) const noexcept { return word_ == other.word_; }
  constexpr bool operator!=(Version other) const noexcept { return word_ != other.word_; }
  /// Whether a thread held the node exclusively when this was read: then it
  /// vouches for nothing (`NodeLatch::load_version_relaxed`).
  constexpr bool held() const noexcept;

  /// The version a node moves to when let go after being held exclusively
  /// since this one.
  constexpr Version next() const noexcept;

 private:
  std::uint64_t word_;
};

/// Spinning while another thread holds a latch: twice as long each round, up
/// to a cap.
class Backoff {
 public:
  void spin() noexcept {
    for (unsigned spin = 0; spin < (1u << round_); ++spin) SpinWait::relax();
    if (round_ < max_round) ++round_;
  }

 private:
  static constexpr unsigned max_round = 10;
  unsigned round_ = 0;
};

/// A node's latch word; see the header comment. Only atomics access it.
class NodeLatch {
 public:
  static constexpr std::uint64_t exclusive_bit = 1;
  static constexpr std::uint64_t shared_unit = 1 << 1;
  /// Up to 2^20 - 1 shared holders at once.
  static constexpr std::uint64_t shared_mask = ((std::uint64_t{1} << 20) - 1) << 1;
  /// 43 bits of version.
  static constexpr std::uint64_t version_unit = std::uint64_t{1} << 21;

  /// The latch word at `word`, in a latched page's header.
  static NodeLatch& at(void* word) noexcept { return *static_cast<NodeLatch*>(word); }

  /// The node's version, to read it optimistically, once no thread holds it
  /// exclusively; spins until then.
  SHOAL_ALWAYS_INLINE Version read_version() const noexcept {
    const std::uint64_t word = word_.load(std::memory_order_acquire);
    if (SHOAL_LIKELY((word & exclusive_bit) == 0)) return Version(word & ~shared_mask);
    return read_version_contended();
  }

  /// Whether the node is still at `version`: what the caller read of it since
  /// is what it held. A fence keeps the reads before the check.
  SHOAL_ALWAYS_INLINE bool is_unchanged(Version version) const noexcept {
    std::atomic_thread_fence(std::memory_order_acquire);
    return (word_.load(std::memory_order_relaxed) & ~shared_mask) == version.word();
  }

  /// Takes the node exclusively if it is still at `version`, with no shared
  /// holder; otherwise fails at once.
  inline bool try_lock_at(Version version) noexcept {
    std::uint64_t expected = version.word();
    // Acquire-release, not acquire with a release fence after (Rust's
    // spelling): the acquire half keeps writes made under the latch from
    // being seen before the latch taken, which is what the fence paired
    // with a reader's acquire fence to say; one `casal`, no `dmb`.
    return word_.compare_exchange_strong(expected, version.word() | exclusive_bit,
                                         std::memory_order_acq_rel,
                                         std::memory_order_relaxed);
  }

  /// Lets go of the node, held exclusively since `version`, at the next one.
  inline void unlock_exclusive(Version version) noexcept {
    word_.store(version.next().word(), std::memory_order_release);
  }

  /// The node's version if no thread holds it exclusively now.
  inline bool try_read_version(Version& version) const noexcept {
    const std::uint64_t word = word_.load(std::memory_order_acquire);
    if ((word & exclusive_bit) != 0) return false;
    version = Version(word & ~shared_mask);
    return true;
  }

  /// The node's word without its shared count, by a relaxed load: a version
  /// unless a thread held the node exclusively (`Version::held`). For a
  /// reader of many nodes, which fences once after reading all their words
  /// and before reading the nodes, so that one node's wait for memory does
  /// not hold up the next one's, and keeps what it read of a node only if
  /// the node was not held and is still at that word after another fence.
  inline Version load_version_relaxed() const noexcept {
    return Version(word_.load(std::memory_order_relaxed) & ~shared_mask);
  }

  /// Takes the node exclusively if no thread holds it in either mode; the
  /// version to let go at, or false without waiting.
  inline bool try_lock_exclusive(Version& version) noexcept {
    const std::uint64_t word = word_.load(std::memory_order_relaxed);
    if ((word & (exclusive_bit | shared_mask)) != 0 || !try_lock_at(Version(word))) return false;
    version = Version(word);
    return true;
  }

  /// Takes the node exclusively once no thread holds it in either mode;
  /// returns the version to let go at.
  inline Version lock_exclusive() noexcept {
    const std::uint64_t word = word_.load(std::memory_order_relaxed);
    if ((word & (exclusive_bit | shared_mask)) == 0 && try_lock_at(Version(word))) {
      return Version(word);
    }
    return lock_exclusive_contended();
  }

  /// Takes the node shared, once no thread holds it exclusively.
  inline void lock_shared() noexcept {
    const std::uint64_t word = word_.load(std::memory_order_relaxed);
    if ((word & exclusive_bit) == 0 && try_add_shared_holder(word)) return;
    lock_shared_contended();
  }

  inline void unlock_shared() noexcept {
    word_.fetch_sub(shared_unit, std::memory_order_release);
  }

  /// The raw word, for tests.
  std::uint64_t load_word() const noexcept { return word_.load(std::memory_order_relaxed); }

 private:
  std::atomic<std::uint64_t> word_;

  SHOAL_COLD Version read_version_contended() const noexcept {
    Backoff backoff;
    for (;;) {
      backoff.spin();
      const std::uint64_t word = word_.load(std::memory_order_acquire);
      if ((word & exclusive_bit) == 0) return Version(word & ~shared_mask);
    }
  }

  SHOAL_COLD Version lock_exclusive_contended() noexcept {
    Backoff backoff;
    for (;;) {
      backoff.spin();
      const std::uint64_t word = word_.load(std::memory_order_relaxed);
      if ((word & (exclusive_bit | shared_mask)) == 0 && try_lock_at(Version(word))) {
        return Version(word);
      }
    }
  }

  bool try_add_shared_holder(std::uint64_t word) noexcept {
    return word_.compare_exchange_weak(word, word + shared_unit, std::memory_order_acquire,
                                       std::memory_order_relaxed);
  }

  SHOAL_COLD void lock_shared_contended() noexcept {
    Backoff backoff;
    for (;;) {
      const std::uint64_t word = word_.load(std::memory_order_relaxed);
      if ((word & exclusive_bit) == 0) {
        SHOAL_ASSERT((word & shared_mask) != shared_mask);
        if (try_add_shared_holder(word)) return;
      }
      backoff.spin();
    }
  }
};

static_assert(sizeof(NodeLatch) == 8 && std::atomic<std::uint64_t>::is_always_lock_free);

constexpr Version Version::next() const noexcept { return Version(word_ + NodeLatch::version_unit); }
constexpr bool Version::held() const noexcept { return (word_ & NodeLatch::exclusive_bit) != 0; }

}  // namespace shoal::detail
