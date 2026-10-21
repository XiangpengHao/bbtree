//! `BBTreeStore` under Shuttle, as `BBTreeMap` is (`map/shuttle_tests.rs`):
//! threads insert, replace, read and remove keys of one key range, with
//! values from empty to over half a heap page, so that their leaves split
//! and compact their heaps while other threads read them, and pages freed
//! from one leaf's heap come back as another's, or as leaves.
//!
//! `RUSTFLAGS="--cfg shuttle" cargo test --release --features store --lib shuttle_tests`

use std::collections::BTreeMap;
use std::sync::Arc;
use std::vec::Vec;

use shuttle::rand::{Rng, thread_rng};
use shuttle::thread;

use super::BBTreeStore;
use crate::tree::test_support::Exploration;

const THREADS: usize = 3;

fn key(n: usize) -> Vec<u8> {
    std::format!("key:{n:04}").into_bytes()
}

fn value(n: usize, version: usize, len: usize) -> Vec<u8> {
    (0..len).map(|i| (n * 31 + version * 7 + i) as u8).collect()
}

/// Each thread owns the keys equal to its number modulo [`THREADS`], so
/// that its model is exact, while the threads' keys share leaves.
#[test]
fn threads_match_their_models() {
    Exploration {
        random: 200,
        pct: 200,
    }
    .run(|| {
        let store = Arc::new(BBTreeStore::new());
        let mut models: Vec<BTreeMap<Vec<u8>, Vec<u8>>> = Vec::new();
        for thread in 0..THREADS {
            let mut model = BTreeMap::new();
            for i in 0..30 {
                let n = i * THREADS + thread;
                let value = value(n, 0, 40);
                store
                    .insert(&key(n), &value)
                    .expect("the global allocator gives");
                model.insert(key(n), value);
            }
            models.push(model);
        }
        let handles: Vec<_> = (models.into_iter().enumerate())
            .map(|(thread, mut model)| {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    let mut rng = thread_rng();
                    for version in 1..=12 {
                        let n = rng.gen_range(0..40) * THREADS + thread;
                        let key = key(n);
                        match rng.gen_range(0..4) {
                            0 | 1 => {
                                let len = [0, 20, 200, 600][rng.gen_range(0..4)];
                                let value = value(n, version, len);
                                let replaced = store
                                    .insert(&key, &value)
                                    .expect("the global allocator gives");
                                assert_eq!(replaced, model.insert(key, value).is_some());
                            }
                            2 => assert_eq!(store.remove(&key), model.remove(&key).is_some()),
                            _ => assert_eq!(store.get(&key), model.get(&key).cloned()),
                        }
                    }
                    model
                })
            })
            .collect();
        let mut all = BTreeMap::new();
        for handle in handles {
            all.extend(handle.join().expect("no thread panics"));
        }
        let mut store = Arc::into_inner(store).expect("the threads are done");
        for (key, value) in &all {
            assert_eq!(store.get(key).as_ref(), Some(value));
        }
        store.check_invariants(all.len());
    });
}
