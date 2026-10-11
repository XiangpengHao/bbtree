#![forbid(unsafe_code)]

use super::BBTreeMap;
use crate::InlineKey;
use crate::node::integer_edge_model::{LookupTrace, TreeShape};
use core::fmt::Debug;

trait IntegerKey: Copy + Ord + Debug + InlineKey {
    fn random(rank: usize) -> Self;
    fn dense(rank: usize) -> Self;
}

impl IntegerKey for u64 {
    fn random(rank: usize) -> Self {
        SampleRng::mix(rank as u64)
    }
    fn dense(rank: usize) -> Self {
        rank as u64
    }
}

impl IntegerKey for u128 {
    fn random(rank: usize) -> Self {
        let upper = SampleRng::mix(rank as u64);
        let lower = SampleRng::mix(rank as u64 ^ 0x9e37_79b9_7f4a_7c15);
        (u128::from(upper) << 64) | u128::from(lower)
    }
    fn dense(rank: usize) -> Self {
        rank as u128
    }
}

struct SampleRng {
    state: u64,
}

impl SampleRng {
    fn new(stream: u64) -> Self {
        Self {
            state: Self::mix(stream),
        }
    }
    fn mix(mut x: u64) -> u64 {
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
    fn below(&mut self, bound: usize) -> usize {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        ((u128::from(Self::mix(self.state)) * bound as u128) >> 64) as usize
    }
    fn shuffle(&mut self, order: &mut [usize]) {
        for index in (1..order.len()).rev() {
            order.swap(index, self.below(index + 1));
        }
    }
}

pub(crate) struct Diagnostic;

impl Diagnostic {
    fn run<K: IntegerKey>(kind: &str, entries: usize, samples: usize, ordered: bool) {
        let mut order: Vec<_> = (0..entries).collect();
        if !ordered {
            SampleRng::new(1).shuffle(&mut order);
        }
        let mut map = BBTreeMap::new();
        for rank in order {
            let key = if ordered {
                K::dense(2 * rank)
            } else {
                K::random(rank)
            };
            assert_eq!(map.insert(key, rank as u64 + 1), None);
        }
        assert_eq!(map.len(), entries);
        let root = map.root.as_ref().unwrap().borrow();
        println!(
            "kind={kind} entries={entries} samples={samples} root_height={} mode=structural_not_timing",
            root.height()
        );
        let mut shape = TreeShape::new::<K>();
        shape.visit(root);
        shape.print();
        for hits in [true, false] {
            let mut rng = SampleRng::new(2);
            let mut trace = LookupTrace::new::<K>();
            for _ in 0..samples {
                let rank = rng.below(entries);
                let key = if ordered {
                    K::dense(2 * rank + usize::from(!hits))
                } else {
                    K::random(rank + if hits { 0 } else { entries })
                };
                let expected = hits.then_some(rank as u64 + 1);
                let actual = map.get(&key).copied();
                assert_eq!(actual, expected, "{kind} query={key:?}");
                assert_eq!(trace.lookup(root, &key), actual, "{kind} query={key:?}");
            }
            trace.print(if hits { "hit" } else { "miss" }, samples);
        }
    }

    pub(crate) fn main() {
        let mut args = std::env::args().skip(1);
        let kind = args
            .next()
            .expect("u64-random|u128-random|u64-ordered|u128-ordered");
        let entries: usize = args.next().map_or(2_000_000, |s| s.parse().unwrap());
        let samples: usize = args.next().map_or(100_000, |s| s.parse().unwrap());
        assert!(entries > 1_000_000 && entries <= usize::MAX / 2 && samples > 0);
        match kind.as_str() {
            "u64-random" => Self::run::<u64>(&kind, entries, samples, false),
            "u128-random" => Self::run::<u128>(&kind, entries, samples, false),
            "u64-ordered" => Self::run::<u64>(&kind, entries, samples, true),
            "u128-ordered" => Self::run::<u128>(&kind, entries, samples, true),
            _ => panic!("unknown key kind: {kind}"),
        }
    }
}
