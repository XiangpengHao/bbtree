//! Per-operation latency for `bbtree-scaling --latency` (`scaling.rs`): a
//! clock cheap enough to read around every operation, and a histogram of
//! what it read.

use std::time::{Duration, Instant};

/// A clock for timing one operation alone: it reads the processor's
/// counter, and fences the reads so that the operation is done between
/// them, its loads included. Without the fences an out-of-order core reads
/// the end before a lookup's cache misses come back, and half of all
/// lookups looked free. The time-stamp counter on x86-64, the virtual
/// counter on aarch64, else nanoseconds since the clock was made; with the
/// nanoseconds per tick, measured against `Instant` when made. The fences
/// cost a few nanoseconds, the same for every map, and keep consecutive
/// operations from overlapping, which is what a latency means.
#[derive(Clone, Copy)]
pub(crate) struct TickClock {
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    made: Instant,
    nanos_per_tick: f64,
}

impl TickClock {
    /// The clock, calibrated over `calibration`.
    pub(crate) fn calibrated(calibration: Duration) -> Self {
        let mut clock = Self {
            #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
            made: Instant::now(),
            nanos_per_tick: 1.0,
        };
        let (start, start_ticks) = (Instant::now(), clock.start());
        while start.elapsed() < calibration {}
        let ticks = clock.stop() - start_ticks;
        clock.nanos_per_tick = start.elapsed().as_nanos() as f64 / ticks as f64;
        clock
    }

    /// The time an operation starts, in ticks: read after everything
    /// before it is done, and before anything after it starts.
    #[inline(always)]
    pub(crate) fn start(&self) -> u64 {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: every x86-64 CPU has SSE2's `lfence` and the time-stamp
        // counter.
        unsafe {
            use core::arch::x86_64::{_mm_lfence, _rdtsc};
            _mm_lfence();
            let ticks = _rdtsc();
            _mm_lfence();
            ticks
        }
        #[cfg(target_arch = "aarch64")]
        {
            let ticks: u64;
            // SAFETY: `isb` and reading the virtual counter, which Linux and
            // macOS let user code read, touch no memory.
            unsafe {
                core::arch::asm!("isb", "mrs {ticks}, cntvct_el0", "isb", ticks = out(reg) ticks,
                    options(nostack, nomem));
            }
            ticks
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            self.made.elapsed().as_nanos() as u64
        }
    }

    /// The time an operation ends, in ticks: read once its instructions
    /// are done and its loads have come back.
    #[inline(always)]
    pub(crate) fn stop(&self) -> u64 {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: every x86-64 CPU this runs on has `rdtscp`, which waits
        // for the instructions before it and their loads.
        unsafe {
            use core::arch::x86_64::{__rdtscp, _mm_lfence};
            let mut processor = 0;
            let ticks = __rdtscp(&mut processor);
            _mm_lfence();
            ticks
        }
        #[cfg(target_arch = "aarch64")]
        {
            let ticks: u64;
            // SAFETY: `dsb` waits for the memory accesses before it, `isb`
            // for the instructions; then the counter, as in `start`.
            unsafe {
                core::arch::asm!("dsb ish", "isb", "mrs {ticks}, cntvct_el0", "isb",
                    ticks = out(reg) ticks, options(nostack));
            }
            ticks
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            self.made.elapsed().as_nanos() as u64
        }
    }

    /// `ticks` in nanoseconds.
    pub(crate) fn nanos(self, ticks: u64) -> f64 {
        ticks as f64 * self.nanos_per_tick
    }
}

/// How many times each latency, in ticks, was seen: exactly below
/// [`Self::EXACT`], then in 512 buckets per power of two, each within 0.2%
/// of what it holds; and the largest exactly.
pub(crate) struct LatencyHistogram {
    counts: Vec<u64>,
    total: u64,
    max: u64,
}

impl LatencyHistogram {
    /// Latencies below this many ticks have a bucket each.
    const EXACT: u64 = 1024;
    /// Buckets per power of two past [`Self::EXACT`].
    const PER_OCTAVE: u64 = 512;

    pub(crate) fn new() -> Self {
        let octaves = u64::BITS as u64 - Self::EXACT.trailing_zeros() as u64;
        Self {
            counts: vec![0; (Self::EXACT + octaves * Self::PER_OCTAVE) as usize],
            total: 0,
            max: 0,
        }
    }

    /// The bucket of `ticks`.
    #[inline(always)]
    fn bucket(ticks: u64) -> usize {
        if ticks < Self::EXACT {
            return ticks as usize;
        }
        let octave =
            u64::from(63 - ticks.leading_zeros()) - u64::from(Self::EXACT.trailing_zeros());
        let shift = octave + 1;
        let mantissa = (ticks >> shift) - Self::PER_OCTAVE;
        (Self::EXACT + octave * Self::PER_OCTAVE + mantissa) as usize
    }

    /// The least latency bucket `bucket` holds.
    fn lower_bound(bucket: usize) -> u64 {
        let bucket = bucket as u64;
        if bucket < Self::EXACT {
            return bucket;
        }
        let (octave, mantissa) = (
            (bucket - Self::EXACT) / Self::PER_OCTAVE,
            (bucket - Self::EXACT) % Self::PER_OCTAVE,
        );
        (Self::PER_OCTAVE + mantissa) << (octave + 1)
    }

    #[inline(always)]
    pub(crate) fn record(&mut self, ticks: u64) {
        self.counts[Self::bucket(ticks)] += 1;
        self.total += 1;
        self.max = self.max.max(ticks);
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        for (count, other) in self.counts.iter_mut().zip(&other.counts) {
            *count += other;
        }
        self.total += other.total;
        self.max = self.max.max(other.max);
    }

    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    /// The least latency, in ticks, that `fraction` of the operations took
    /// no longer than, to the bucket's width.
    pub(crate) fn quantile(&self, fraction: f64) -> u64 {
        let rank = ((self.total as f64 * fraction).ceil() as u64).max(1);
        let mut seen = 0;
        for (bucket, &count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return Self::lower_bound(bucket);
            }
        }
        self.max
    }

    pub(crate) fn max(&self) -> u64 {
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::LatencyHistogram;

    #[test]
    fn buckets_hold_their_lower_bound_and_stay_within_a_fifth_of_a_percent() {
        for ticks in (0..5_000).chain([1 << 20, (1 << 33) + 12_345, u64::MAX / 3]) {
            let bucket = LatencyHistogram::bucket(ticks);
            let lower = LatencyHistogram::lower_bound(bucket);
            assert!(
                lower <= ticks && ticks - lower <= ticks / 500 + 1,
                "{ticks}: {lower}"
            );
            assert_eq!(LatencyHistogram::bucket(lower), bucket);
        }
    }

    #[test]
    fn quantiles_are_of_what_was_recorded() {
        let mut histogram = LatencyHistogram::new();
        for ticks in 1..=1_000 {
            histogram.record(ticks);
        }
        assert_eq!(histogram.quantile(0.5), 500);
        assert_eq!(histogram.quantile(0.99), 990);
        assert_eq!(histogram.max(), 1_000);
    }
}
