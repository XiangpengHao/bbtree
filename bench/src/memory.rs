//! Memory: the base allocator, the counting allocator that memory mode wraps
//! around it, and the process's resident set.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};

pub use mimalloc::MiMalloc as BaseAllocator;

/// What each cell measures, which the binary decides by its global
/// allocator.
#[derive(Clone, Copy)]
pub(crate) enum RunMode {
    /// Time, on the plain base allocator.
    Timing,
    /// The memory a cell's maps hold, from the counting allocator.
    Memory(&'static CountingAllocator),
}

impl RunMode {
    pub(crate) fn new(counting_allocator: Option<&'static CountingAllocator>) -> Self {
        counting_allocator.map_or(Self::Timing, Self::Memory)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Timing => "timing",
            Self::Memory(_) => "memory",
        }
    }

    /// The global allocator, for report headers.
    pub(crate) fn allocator_description(self) -> &'static str {
        match self {
            Self::Timing => "mimalloc",
            Self::Memory(_) => "mimalloc, wrapped in a live-byte counter",
        }
    }
}

/// The base allocator, counting the bytes it has live.
pub struct CountingAllocator {
    base: BaseAllocator,
    live_bytes: AtomicUsize,
}

impl CountingAllocator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            base: BaseAllocator,
            live_bytes: AtomicUsize::new(0),
        }
    }

    /// The bytes that dropping `value` frees, which are the bytes it held
    /// on the heap, including what it owns indirectly.
    pub(crate) fn bytes_freed_by_dropping<T>(&self, value: T) -> usize {
        let before = self.live_bytes.load(Ordering::Relaxed);
        drop(value);
        before - self.live_bytes.load(Ordering::Relaxed)
    }
}

impl Default for CountingAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: every method forwards to the base allocator with its own
// arguments and returns what the base returned; the counter is bookkeeping.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `alloc`'s contract, which is the base's.
        let pointer = unsafe { self.base.alloc(layout) };
        if !pointer.is_null() {
            self.live_bytes.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let pointer = unsafe { self.base.alloc_zeroed(layout) };
        if !pointer.is_null() {
            self.live_bytes.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller got `pointer` from this allocator, so from the
        // base, with `layout`.
        unsafe { self.base.dealloc(pointer, layout) };
        self.live_bytes.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as for `dealloc`, and the caller upholds `realloc`'s
        // contract on `new_size`.
        let new_pointer = unsafe { self.base.realloc(pointer, layout, new_size) };
        if !new_pointer.is_null() {
            self.live_bytes.fetch_add(new_size, Ordering::Relaxed);
            self.live_bytes.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        new_pointer
    }
}

/// The process's resident set, from `/proc/self/status`; zero where that
/// isn't available.
pub(crate) struct ResidentSet {
    current_bytes: u64,
    peak_bytes: u64,
}

impl ResidentSet {
    pub(crate) fn read() -> Self {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        Self {
            current_bytes: Self::field_bytes(&status, "VmRSS:"),
            peak_bytes: Self::field_bytes(&status, "VmHWM:"),
        }
    }

    pub(crate) fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    pub(crate) fn peak_bytes(&self) -> u64 {
        self.peak_bytes
    }

    /// Reads a `Name:   1234 kB` line.
    fn field_bytes(status: &str, name: &str) -> u64 {
        (status.lines())
            .find_map(|line| line.strip_prefix(name))
            .and_then(|value| value.trim().strip_suffix(" kB"))
            .and_then(|kilobytes| kilobytes.parse::<u64>().ok())
            .map_or(0, |kilobytes| kilobytes * 1024)
    }
}
