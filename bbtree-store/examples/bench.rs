//! A store's insert, get and scan times at one thread, inserts and gets at
//! eight, and the bytes its pages take, by kind; then the heap's bytes
//! under churn, as values change size and entries go and come back. One
//! store at a time.
//!
//! `cargo run --release -p bbtree-store --example bench -- [entries]`

use std::alloc::Layout;
use std::hint::black_box;
use std::ops::ControlFlow;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use bbtree_store::BBTreeStore;
use bbtree_store::bbtree::{AllocError, Allocator, Global};

#[global_allocator]
static MIMALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Bytes out, nodes' and heap pages'.
#[derive(Default)]
struct Counting {
    nodes: AtomicUsize,
    heap: AtomicUsize,
}

// SAFETY: every page is the global allocator's; counting touches none.
unsafe impl Allocator for Counting {
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.nodes.fetch_add(layout.size(), Ordering::Relaxed);
        Global.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.nodes.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { Global.deallocate(ptr, layout) }
    }

    fn allocate_heap(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.heap.fetch_add(layout.size(), Ordering::Relaxed);
        Global.allocate_zeroed(layout)
    }

    unsafe fn deallocate_heap(&self, page: NonNull<u8>, layout: Layout) {
        self.heap.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { Global.deallocate(page, layout) }
    }
}

type Store = BBTreeStore<&'static Counting>;

/// Entry `i`'s key: 20 bytes, in an order apart from `i`'s.
fn key(i: u64) -> [u8; 20] {
    let mut key = [b'k'; 20];
    key[..8].copy_from_slice(&i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes());
    key
}

/// Entry `i`'s value: 32 bytes.
fn value(i: u64) -> [u8; 32] {
    [i as u8; 32]
}

fn ns_per(start: Instant, count: u64) -> f64 {
    start.elapsed().as_nanos() as f64 / count as f64
}

fn per_entry(count: &AtomicUsize, entries: u64) -> f64 {
    count.load(Ordering::Relaxed) as f64 / entries as f64
}

fn one_thread(store: &Store, counting: &Counting, entries: u64) {
    let start = Instant::now();
    for i in 0..entries {
        store.insert(&key(i), &value(i)).expect("no budget");
    }
    let insert = ns_per(start, entries);

    let mut buf = Vec::new();
    let start = Instant::now();
    for i in 0..entries {
        buf.clear();
        assert!(
            store
                .get_into(&key(i.wrapping_mul(7919) % entries), &mut buf)
                .is_some()
        );
    }
    let get = ns_per(start, entries);

    let start = Instant::now();
    let mut bytes = 0;
    store.scan::<[u8], ()>(.., |key, value| {
        bytes += key.len() + value.len();
        ControlFlow::Continue(())
    });
    black_box(bytes);
    let scan = ns_per(start, entries);

    let (nodes, heap) = (
        per_entry(&counting.nodes, entries),
        per_entry(&counting.heap, entries),
    );
    println!(
        "1 thread:  insert {insert:.0} ns, get {get:.0} ns, scan {scan:.1} ns an entry; \
         nodes {nodes:.1} + heap {heap:.1} = {:.1} bytes an entry",
        nodes + heap
    );
}

/// Churn over the store's `entries` keys: each op replaces a value with one
/// of 16 to 64 bytes, or removes the entry, or puts a removed one back.
fn churn(store: &Store, counting: &Counting, entries: u64) {
    let mut present = vec![true; entries as usize];
    let mut live_bytes: usize = entries as usize * (20 + 32 + 9);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let start = Instant::now();
    for round in 1..=4 {
        for _ in 0..entries {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let i = state % entries;
            let len = 16 + (state >> 40) as usize % 49;
            let slot = &mut present[i as usize];
            if *slot && state >> 60 < 4 {
                let mut old = Vec::new();
                store.get_into(&key(i), &mut old);
                assert!(store.remove(&key(i)));
                live_bytes -= 20 + 9 + old.len();
                *slot = false;
            } else {
                let mut old = Vec::new();
                let had = store.get_into(&key(i), &mut old).is_some();
                store
                    .insert(&key(i), &vec![i as u8; len])
                    .expect("no budget");
                live_bytes += 20 + 9 + len;
                if had {
                    live_bytes -= 20 + 9 + old.len();
                }
                *slot = true;
            }
        }
        let heap = counting.heap.load(Ordering::Relaxed);
        println!(
            "churn round {round}: heap {:.2} bytes a live record byte, {:.0} ns an op",
            heap as f64 / live_bytes as f64,
            ns_per(start, entries * round)
        );
    }
}

fn threads(entries: u64) {
    let threads = 8;
    let counting: &'static Counting = Box::leak(Box::default());
    let store = Store::new_in(counting);
    let start = Instant::now();
    thread::scope(|scope| {
        for t in 0..threads {
            let store = &store;
            scope.spawn(move || {
                for i in (t..entries).step_by(threads as usize) {
                    store.insert(&key(i), &value(i)).expect("no budget");
                }
            });
        }
    });
    let insert = ns_per(start, entries);
    let start = Instant::now();
    thread::scope(|scope| {
        for t in 0..threads {
            let store = &store;
            scope.spawn(move || {
                let mut buf = Vec::new();
                for i in (t..entries).step_by(threads as usize) {
                    buf.clear();
                    let key = key(i.wrapping_mul(7919) % entries);
                    assert!(store.get_into(&key, &mut buf).is_some());
                }
            });
        }
    });
    let get = ns_per(start, entries);
    println!("8 threads: insert {insert:.0} ns, get {get:.0} ns an entry, wall time");
}

fn main() {
    let entries: u64 = std::env::args()
        .nth(1)
        .map_or(10_000_000, |n| n.parse().expect("a count"));
    println!("{entries} entries of 20-byte keys and 32-byte values");
    let counting: &'static Counting = Box::leak(Box::default());
    let store = Store::new_in(counting);
    one_thread(&store, counting, entries);
    churn(&store, counting, entries);
    drop(store);
    threads(entries);
}
