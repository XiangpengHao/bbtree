mod string_keys;

use bbtree::unsync::BBTreeMap;
use std::{collections::BTreeMap, hint::black_box, time::Instant};
use string_keys::StringKeys;

trait DiagnosticMap<K>: Default {
    fn insert(&mut self, key: K, value: u64);
    fn get(&self, key: &K) -> Option<&u64>;
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a u64)>
    where
        K: 'a;
    fn len(&self) -> usize;
}

impl<K: Ord> DiagnosticMap<K> for BBTreeMap<K, u64> {
    fn insert(&mut self, key: K, value: u64) {
        self.insert(key, value);
    }
    fn get(&self, key: &K) -> Option<&u64> {
        self.get(key)
    }
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a u64)>
    where
        K: 'a,
    {
        self.iter()
    }
    fn len(&self) -> usize {
        self.len()
    }
}

impl<K: Ord> DiagnosticMap<K> for BTreeMap<K, u64> {
    fn insert(&mut self, key: K, value: u64) {
        self.insert(key, value);
    }
    fn get(&self, key: &K) -> Option<&u64> {
        self.get(key)
    }
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a u64)>
    where
        K: 'a,
    {
        self.iter()
    }
    fn len(&self) -> usize {
        self.len()
    }
}

struct Workload<K> {
    keys: Vec<K>,
    probes: Vec<K>,
    operation: String,
    expected_checksum: u64,
}

impl<K: Copy + Ord> Workload<K> {
    fn mix(mut value: u64) -> u64 {
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn new(size: usize, operation: String, key: impl Fn(usize) -> K) -> Self {
        let keys: Vec<_> = (0..size).map(&key).collect();
        if operation == "iterate_next" {
            return Self {
                keys,
                probes: Vec::new(),
                operation,
                expected_checksum: (1..=size)
                    .fold(0_u64, |sum, value| sum.wrapping_add(value as u64)),
            };
        }
        let count = if operation == "random_insert" {
            size / 4
        } else {
            size
        };
        let mut expected_checksum = 0_u64;
        let probes = (0..count)
            .map(|index| {
                if operation == "get_hit" {
                    let rank = ((u128::from(Self::mix(index as u64 ^ 0xcafe_f00d)) * size as u128)
                        >> 64) as usize;
                    expected_checksum = expected_checksum.wrapping_add(rank as u64 + 1);
                    keys[rank]
                } else {
                    key(size + index)
                }
            })
            .collect();
        Self {
            keys,
            probes,
            operation,
            expected_checksum,
        }
    }

    fn run(&self, key_kind: &str, backend: &str) {
        match backend {
            "bbtree-unsync" => self.measure::<BBTreeMap<K, u64>>(key_kind, backend),
            "std-btree" => self.measure::<BTreeMap<K, u64>>(key_kind, backend),
            _ => panic!("unknown backend: {backend}"),
        }
    }

    fn measure<M: DiagnosticMap<K>>(&self, key_kind: &str, backend: &str) {
        let mut map = M::default();
        for (index, key) in self.keys.iter().enumerate() {
            map.insert(*key, index as u64 + 1);
        }
        assert_eq!(map.len(), self.keys.len());
        let count = if self.operation == "iterate_next" {
            self.keys.len()
        } else {
            self.probes.len()
        };
        let mut checksum = 0_u64;
        let start = Instant::now();
        if self.operation == "random_insert" {
            for key in &self.probes {
                map.insert(black_box(*key), 1);
            }
        } else if self.operation == "iterate_next" {
            let mut entries = map.iter();
            // Measure the per-entry cursor, without Iterator::fold's
            // specialized traversal hiding the cost of repeated next.
            #[allow(clippy::while_let_on_iterator)]
            while let Some((_, value)) = entries.next() {
                checksum = checksum.wrapping_add(*value);
            }
        } else {
            for key in &self.probes {
                checksum = checksum.wrapping_add(map.get(black_box(key)).copied().unwrap_or(0));
            }
        }
        let elapsed = start.elapsed();
        black_box(checksum);
        let expected_len = self.keys.len()
            + if self.operation == "random_insert" {
                self.probes.len()
            } else {
                0
            };
        assert_eq!(map.len(), expected_len);
        assert_eq!(checksum, self.expected_checksum);
        if self.operation == "random_insert" {
            // Check every inserted value after stopping the timer. The timed
            // insertion loop and its reported checksum remain unchanged.
            let inserted_checksum = self.probes.iter().fold(0_u64, |sum, key| {
                sum.wrapping_add(*map.get(key).expect("inserted key is present"))
            });
            assert_eq!(inserted_checksum, self.probes.len() as u64);
        }
        println!(
            "{key_kind},{backend},{},{},{},{:.3},{checksum}",
            self.operation,
            self.keys.len(),
            count,
            elapsed.as_nanos() as f64 / count as f64
        );
    }
}

impl Workload<u64> {
    fn key(index: usize) -> u64 {
        Self::mix(index as u64)
    }
}

impl Workload<u128> {
    fn key(index: usize) -> u128 {
        let high = Self::mix(index as u64);
        let low = Self::mix(index as u64 ^ 0x9e37_79b9_7f4a_7c15);
        (u128::from(high) << 64) | u128::from(low)
    }
}

impl Workload<[u8; 32]> {
    fn key(index: usize) -> [u8; 32] {
        let mut key = [0; 32];
        for (word, bytes) in key.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            let salt = (word as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            bytes.copy_from_slice(&Self::mix((index as u64).wrapping_add(salt)).to_be_bytes());
        }
        key
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let key_kind = args
        .next()
        .expect("key: hash32, u64, u128, u128-dense, borrowed-json or borrowed-path");
    let backend = args.next().expect("backend: bbtree-unsync or std-btree");
    let operation = args
        .next()
        .expect("operation: get_hit, get_miss, random_insert, iterate_next");
    assert!(["get_hit", "get_miss", "random_insert", "iterate_next"].contains(&operation.as_str()));
    let size = args.next().map_or(2_000_000, |size| size.parse().unwrap());
    assert!(size > 1_000_000);
    assert!(args.next().is_none(), "unexpected extra argument");
    match key_kind.as_str() {
        "u64" => Workload::new(size, operation, Workload::<u64>::key).run(&key_kind, &backend),
        "u128" => Workload::new(size, operation, Workload::<u128>::key).run(&key_kind, &backend),
        "u128-dense" => {
            assert_ne!(operation, "random_insert", "dense case measures reads only");
            // Ordered even keys share leading bytes; absent odd probes stay
            // inside their range, so they also exercise internal tied runs.
            Workload::new(size, operation, |index| {
                if index < size {
                    2 * index as u128
                } else {
                    2 * (index - size) as u128 + 1
                }
            })
            .run(&key_kind, &backend);
        }
        "hash32" => {
            Workload::new(size, operation, Workload::<[u8; 32]>::key).run(&key_kind, &backend)
        }
        "borrowed-json" | "borrowed-path" => {
            // The owner outlives both the borrowed workload and its map.
            let strings = StringKeys::new(&key_kind, size, &operation);
            Workload::new(size, operation, |index| strings.key(index)).run(&key_kind, &backend);
        }
        _ => panic!("unknown key: {key_kind}"),
    }
}
