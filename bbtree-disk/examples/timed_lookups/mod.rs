//! What the examples share: seeded random numbers, a map of random `u64`
//! entries, and the time a lookup takes, of keys that hit and keys that
//! miss.

use std::hint::black_box;
use std::time::Instant;

use bbtree::unsync::BBTreeMap;

/// A xorshift64* generator, seeded: the same keys and lookups every run.
pub(crate) struct Numbers(u64);

impl Numbers {
    pub(crate) fn seeded() -> Self {
        Self(0x5eed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Keys to look up: some that all hit, and as many that all miss.
pub(crate) struct TimedLookups<Q> {
    hits: Vec<Q>,
    misses: Vec<Q>,
}

impl TimedLookups<u64> {
    /// A map of `count` random `u64` entries, of even keys, and `count`
    /// lookups of its keys and of odd ones.
    pub(crate) fn of_u64_map(count: usize, numbers: &mut Numbers) -> (BBTreeMap<u64, u64>, Self) {
        let entries: Vec<(u64, u64)> = (0..count)
            .map(|_| {
                let key = numbers.next() & !1;
                (key, key.rotate_left(17))
            })
            .collect();
        let map = entries.iter().copied().collect();
        let hits = (0..count)
            .map(|_| entries[numbers.next() as usize % count].0)
            .collect();
        let misses = (0..count).map(|_| numbers.next() | 1).collect();
        (map, Self::new(hits, misses))
    }
}

impl<Q> TimedLookups<Q> {
    pub(crate) fn new(hits: Vec<Q>, misses: Vec<Q>) -> Self {
        Self { hits, misses }
    }

    /// Times `lookup` over the hits, then over the misses, and prints the
    /// time a lookup of each took.
    pub(crate) fn report(&self, name: &str, mut lookup: impl FnMut(&Q) -> Option<u64>) {
        let (hit_ns, found) = Self::time(&self.hits, &mut lookup);
        let (miss_ns, missed) = Self::time(&self.misses, &mut lookup);
        assert_eq!((found, missed), (self.hits.len(), 0), "{name}");
        println!("{name:<32} hit {hit_ns:>6.1} ns   miss {miss_ns:>6.1} ns");
    }

    /// One pass of `lookup` over `keys`, after an untimed pass over a tenth
    /// of them: nanoseconds per lookup, and the hits.
    fn time(keys: &[Q], mut lookup: impl FnMut(&Q) -> Option<u64>) -> (f64, usize) {
        let warm = keys
            .iter()
            .take(keys.len() / 10)
            .filter_map(&mut lookup)
            .count();
        black_box(warm);
        let start = Instant::now();
        let (hits, sum) = keys.iter().fold((0, 0_u64), |(hits, sum), key| {
            match lookup(black_box(key)) {
                Some(value) => (hits + 1, sum.wrapping_add(value)),
                None => (hits, sum),
            }
        });
        black_box(sum);
        (start.elapsed().as_nanos() as f64 / keys.len() as f64, hits)
    }
}
