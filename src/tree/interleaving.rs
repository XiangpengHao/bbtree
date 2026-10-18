//! Where a latched tree's threads may interleave: before each access to an
//! atomic that threads share, and in each round of waiting for another
//! thread ([`Interleaving`]).
//!
//! In a normal build, a switch point is nothing and a wait is spin hints.
//! Under Shuttle (`--cfg shuttle`, tests only, `map/shuttle_tests.rs`), each
//! is a point where Shuttle's scheduler may switch to another thread; it
//! runs one thread at a time. Shuttle's own atomics would mark these points
//! themselves. But the latch words live in node pages, and the racing reads
//! load page fields in place (`AtomicU16::from_ptr`), so the tree keeps
//! core's atomics and marks the points itself. With one thread running at a
//! time, every access is sequentially consistent: Shuttle finds bad
//! interleavings, not reorderings a weak memory model allows.
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

    /// One round of waiting for another thread, `spins` spin hints long.
    /// Under Shuttle it is a single yield instead, which tells the scheduler
    /// to run another thread: no spinning frees what this one waits for.
    #[inline(always)]
    pub(in crate::tree) fn wait(spins: u32) {
        #[cfg(all(test, shuttle))]
        if shuttle::current::try_get_current_task().is_some() {
            shuttle::hint::spin_loop();
            return;
        }
        for _ in 0..spins {
            core::hint::spin_loop();
        }
    }
}
