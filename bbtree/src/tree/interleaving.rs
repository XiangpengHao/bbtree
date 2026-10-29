//! Where a latched tree's threads may interleave: before each access to an
//! atomic that threads share, before a racing read of a node's header or
//! of the shared prefix ([`Interleaving`]), and in each round of waiting for
//! another thread ([`Backoff`]).
//!
//! In a normal build, a switch point is nothing, and a wait spins, then,
//! with the `std` feature, yields to the OS scheduler. Under Shuttle
//! (`--cfg shuttle`, tests only, `map/shuttle_tests.rs`), each is a point
//! where Shuttle's scheduler may switch to another thread; it runs one
//! thread at a time. Shuttle's own atomics would mark these points
//! themselves. But the latch words live in node pages, as core's atomics,
//! so the tree keeps those and marks the points itself. With one thread
//! running at a time, every access is sequentially consistent: Shuttle
//! finds bad interleavings, not reorderings a weak memory model allows.
//!
//! Outside a Shuttle test, a `--cfg shuttle` build behaves as any other, so
//! the whole suite runs under it.

/// The points where Shuttle may switch threads; see the module
/// documentation.
pub(in crate::tree) struct Interleaving;

impl Interleaving {
    /// Before an access to an atomic that other threads access: another
    /// thread may run first.
    #[inline(always)]
    pub(in crate::tree) fn switch_point() {
        #[cfg(all(test, shuttle))]
        if shuttle::current::try_get_current_task().is_some() {
            // Shuttle models no time: a sleep is a bare switch point, where
            // `yield_now` would also tell its scheduler this thread waits.
            shuttle::thread::sleep(core::time::Duration::ZERO);
        }
    }
}

/// Waiting for another thread to let go of what this one needs, in rounds:
/// spinning twice as long each round, as crossbeam's `Backoff` does, then,
/// with the `std` feature, yielding to the OS scheduler each round. A thread
/// that holds what this one waits for may be preempted, or running a user's
/// closure, and a waiter that only spins keeps its core from it. Without
/// `std`, the rounds keep doubling up to a cap.
pub(in crate::tree) struct Backoff {
    round: u32,
}

impl Backoff {
    /// The rounds that spin with `std`: 1 to 64 spin hints.
    #[cfg(feature = "std")]
    const SPIN_ROUNDS: u32 = 7;
    /// The last round, which every later one repeats: 1024 spin hints
    /// without `std`.
    const MAX_ROUND: u32 = 10;

    pub(in crate::tree) fn new() -> Self {
        Self { round: 0 }
    }

    /// One round of waiting. Under Shuttle it yields instead, which tells
    /// the scheduler to run another thread: no spinning frees what this one
    /// waits for. It yields a random number of times, from once to once
    /// more than the round's doublings, as real waits vary in length.
    /// Threads that wait exactly alike retry in step: removals of
    /// neighboring keys, each holding its leaf and trying its neighbor's,
    /// can collide in every round of a deterministic schedule, which
    /// Shuttle reports as a livelock.
    pub(in crate::tree) fn wait(&mut self) {
        let round = self.round;
        self.round = (round + 1).min(Self::MAX_ROUND);
        #[cfg(all(test, shuttle))]
        if shuttle::current::try_get_current_task().is_some() {
            use shuttle::rand::Rng;
            let yields = 1 + shuttle::rand::thread_rng().gen_range(0..=round + 1);
            for _ in 0..yields {
                shuttle::hint::spin_loop();
            }
            return;
        }
        #[cfg(feature = "std")]
        if round >= Self::SPIN_ROUNDS {
            std::thread::yield_now();
            return;
        }
        for _ in 0..1u32 << round {
            core::hint::spin_loop();
        }
    }
}
