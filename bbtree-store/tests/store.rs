//! A store against std's `BTreeMap`, on an allocator that counts its pages
//! by kind and may refuse them past a limit.

use std::alloc::Layout;
use std::collections::BTreeMap;
use std::ops::{Bound, ControlFlow};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use bbtree_store::bbtree::{AllocError, Allocator, Global};
use bbtree_store::{BBTreeStore, InsertError};

/// Pages out, by kind: internal nodes, leaves, root pages, heap pages.
#[derive(Default)]
struct Counting {
    internal: AtomicUsize,
    leaf: AtomicUsize,
    root: AtomicUsize,
    heap: AtomicUsize,
    /// At most this many pages of every kind out at once; 0 for any.
    limit: AtomicUsize,
}

impl Counting {
    fn out(&self) -> usize {
        [&self.internal, &self.leaf, &self.root, &self.heap]
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .sum()
    }

    fn take(&self, count: &AtomicUsize, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        let limit = self.limit.load(Ordering::Relaxed);
        if limit != 0 && self.out() >= limit {
            return Err(AllocError);
        }
        count.fetch_add(1, Ordering::Relaxed);
        Global.allocate_zeroed(layout)
    }

    /// Takes a page back, scribbled over first, so that a record read after
    /// its page went reads no key or value it held.
    unsafe fn give(&self, count: &AtomicUsize, page: NonNull<u8>, layout: Layout) {
        count.fetch_sub(1, Ordering::Relaxed);
        // SAFETY: the page came from `take`, from the global allocator, for
        // `layout`, and nothing uses it any more.
        unsafe {
            page.write_bytes(0xA5, layout.size());
            Global.deallocate(page, layout);
        }
    }
}

// SAFETY: every page is the global allocator's, valid and apart until given
// back; counting neither unwinds nor touches a page.
unsafe impl Allocator for Counting {
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        Global.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { Global.deallocate(ptr, layout) }
    }

    fn allocate_internal(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.take(&self.internal, layout)
    }

    unsafe fn deallocate_internal(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { self.give(&self.internal, page, layout) }
    }

    fn allocate_leaf(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.take(&self.leaf, layout)
    }

    unsafe fn deallocate_leaf(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { self.give(&self.leaf, page, layout) }
    }

    fn allocate_root(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.take(&self.root, layout)
    }

    unsafe fn deallocate_root(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { self.give(&self.root, page, layout) }
    }

    fn allocate_heap(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.take(&self.heap, layout)
    }

    unsafe fn deallocate_heap(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { self.give(&self.heap, page, layout) }
    }
}

/// The default node config's heap page size, the leaves'.
const PAGE: usize = 1024;

/// The key numbered `n`: how many bytes `n` takes, then those bytes, then
/// filler, from 2 to 35 bytes, some within an 8-byte inline key and some
/// not. No two numbers share a key.
fn key(n: usize) -> Vec<u8> {
    let bytes = (n as u64).to_be_bytes();
    let skipped = (bytes.iter()).take_while(|&&byte| byte == 0).count().min(7);
    let mut key = vec![(8 - skipped) as u8];
    key.extend_from_slice(&bytes[skipped..]);
    key.resize(key.len() + n % 32, b'k');
    key
}

fn value(n: usize, version: usize, len: usize) -> Vec<u8> {
    (0..len).map(|i| (n + version * 7 + i) as u8).collect()
}

/// A deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

/// The heap bytes of `pages` per byte of the entries' keys and values.
fn heap_per_entry_byte(pages: &Counting, page_size: usize, entry_bytes: usize) -> f64 {
    (pages.heap.load(Ordering::Relaxed) * page_size) as f64 / entry_bytes as f64
}

#[test]
fn inserts_reads_replaces_and_removes() {
    let store = BBTreeStore::new();
    assert_eq!(store.get(b"apple"), None);
    assert_eq!(store.insert(b"apple", b"red"), Ok(false));
    assert_eq!(store.insert(b"banana", b"yellow"), Ok(false));
    assert_eq!(store.get(b"apple").as_deref(), Some(&b"red"[..]));
    // A value no longer goes in place; a longer one in a new record.
    assert_eq!(store.insert(b"apple", b"re"), Ok(true));
    assert_eq!(store.get(b"apple").as_deref(), Some(&b"re"[..]));
    assert_eq!(store.insert(b"apple", b"green and red"), Ok(true));
    assert_eq!(store.get(b"apple").as_deref(), Some(&b"green and red"[..]));
    let mut buf = b"x".to_vec();
    assert_eq!(store.get_into(b"banana", &mut buf), Some(6));
    assert_eq!(buf, b"xyellow");
    assert!(store.contains_key(b"banana"));
    assert!(store.remove(b"banana"));
    assert!(!store.remove(b"banana"));
    assert!(!store.contains_key(b"banana"));
    assert_eq!(store.insert(b"", b""), Ok(false));
    assert_eq!(store.get(b"").as_deref(), Some(&b""[..]));
}

/// Keys longer than a quarter page are refused; values of any length go
/// in, those too long for their record to fit in a page's room into
/// overflow pages of their own, all full but for what is left over, which
/// the record keeps if it fits, else a last page. Long values are replaced
/// and removed, and the store drops holding some.
#[test]
fn refuses_long_keys_and_takes_values_of_any_length() {
    let store = BBTreeStore::new();
    let max_key = store.max_key_len();
    assert_eq!(
        store.insert(&vec![1; max_key + 1], b""),
        Err(InsertError::KeyTooLong)
    );
    // A page's room, past its header of a pointer and two `u32`s (1,008
    // bytes with 8-byte pointers), holds a record of nine bytes of lengths
    // and generation, the key, and a pointer-sized link before what the
    // record keeps of a long value; an overflow page's, past its link, the
    // value's bytes (1,016).
    let link = size_of::<*mut u8>();
    let (room, overflow_room) = (PAGE - link - 8, PAGE - link);
    for (key_len, value_len, overflow_pages) in [
        (20, 0, 0),
        (20, room - 9 - 20, 0),
        // Left over: all of it, which doesn't fit with the link.
        (20, room - 9 - 20 + 1, 1),
        (20, overflow_room, 1),
        (20, overflow_room + room - 9 - 20 - link, 1),
        (20, overflow_room + room - 9 - 20 - link + 1, 2),
        (20, 10 * overflow_room + 500, 10),
        (max_key, room - 9 - max_key, 0),
        (max_key, room - 9 - max_key + 1, 1),
        (max_key, 3 * overflow_room + room - 9 - max_key - link, 3),
    ] {
        let pages = Counting::default();
        let store = BBTreeStore::new_in(&pages);
        let (key, value) = (vec![1; key_len], value(key_len, value_len, value_len));
        assert_eq!(store.insert(&key, &value), Ok(false));
        assert_eq!(
            pages.heap.load(Ordering::Relaxed),
            1 + overflow_pages,
            "a {key_len}-byte key's {value_len}-byte value"
        );
        assert_eq!(store.get(&key), Some(value.clone()));
        let mut scanned = Vec::new();
        store.scan::<[u8], ()>(.., |_, found| {
            scanned.push(found.to_vec());
            ControlFlow::Continue(())
        });
        assert_eq!(scanned, [value]);
        drop(store);
        assert_eq!(pages.out(), 0, "every page goes back");
    }
    let pages = Counting::default();
    let store = BBTreeStore::new_in(&pages);
    for n in 0..64 {
        let len = [5_000, 10, 3 * PAGE][n % 3];
        assert_eq!(store.insert(&key(n), &value(n, 0, len)), Ok(false));
    }
    for n in 0..64 {
        let len = [10, 4 * PAGE, 0][n % 3];
        assert_eq!(store.insert(&key(n), &value(n, 1, len)), Ok(true));
        if n % 5 == 0 {
            assert!(store.remove(&key(n)));
        }
    }
    for n in 0..64 {
        let len = [10, 4 * PAGE, 0][n % 3];
        assert_eq!(store.get(&key(n)), (n % 5 != 0).then(|| value(n, 1, len)));
    }
    drop(store);
    assert_eq!(pages.out(), 0, "every page goes back");
}

/// A long value whose overflow pages the allocator refuses partway goes
/// in nowhere, with the pages it took given back, the store as it was.
#[test]
fn a_long_value_past_the_budget_takes_no_page() {
    let pages = Counting::default();
    let store = BBTreeStore::new_in(&pages);
    store.insert(b"apple", b"red").expect("no limit");
    store
        .insert(b"banana", &value(0, 0, 2 * PAGE))
        .expect("no limit");
    let out = pages.out();
    pages.limit.store(out + 3, Ordering::Relaxed);
    for key in [&b"apple"[..], b"banana", b"cherry"] {
        assert_eq!(
            store.insert(key, &value(1, 1, 8 * PAGE)),
            Err(InsertError::Alloc(AllocError))
        );
        assert_eq!(pages.out(), out);
    }
    assert_eq!(store.get(b"apple").as_deref(), Some(&b"red"[..]));
    assert_eq!(store.get(b"banana"), Some(value(0, 0, 2 * PAGE)));
    assert_eq!(store.get(b"cherry"), None);
    drop(store);
    assert_eq!(pages.out(), 0, "every page goes back");
}

/// Random inserts, replacements and removals, against std's `BTreeMap`,
/// with values from empty to a few heap pages: leaves split, merge and
/// compact, their records move between leaves' chains, and long values'
/// overflow pages move with their records.
#[test]
fn matches_a_btree_map() {
    let pages = Counting::default();
    let store = BBTreeStore::new_in(&pages);
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let steps = if cfg!(miri) { 2_000 } else { 60_000 };
    for step in 0..steps {
        let n = rng.below(3_000);
        let entry_key = key(n);
        match rng.below(10) {
            0..=5 => {
                let len = match rng.below(16) {
                    0 | 1 => rng.below(PAGE),
                    2 => rng.below(4 * PAGE),
                    3 => 0,
                    _ => rng.below(64),
                };
                let value = value(n, step, len);
                let replaced = store.insert(&entry_key, &value).expect("no limit");
                assert_eq!(replaced, model.insert(entry_key, value).is_some());
            }
            6..=8 => assert_eq!(store.remove(&entry_key), model.remove(&entry_key).is_some()),
            _ => assert_eq!(store.get(&entry_key), model.get(&entry_key).cloned()),
        }
        if step % (steps / 16) == 0 {
            assert_ranges_match(&store, &model, &mut rng);
        }
    }
    for (key, value) in &model {
        assert_eq!(store.get(key).as_ref(), Some(value));
    }
    for key in model.keys() {
        assert!(store.remove(key));
    }
    drop(store);
    assert_eq!(pages.out(), 0, "every page goes back");
}

/// Scans both ways, and copies from both ends meeting, of a random range,
/// against `model`.
fn assert_ranges_match<A>(store: &BBTreeStore<A>, model: &BTreeMap<Vec<u8>, Vec<u8>>, rng: &mut Rng)
where
    A: Allocator + Clone,
{
    let (low, high) = (key(rng.below(3_000)), key(rng.below(3_000)));
    let (low, high) = if low <= high {
        (low, high)
    } else {
        (high, low)
    };
    let bounds = (Bound::Included(&low[..]), Bound::Excluded(&high[..]));
    let expected: Vec<_> = (model.range::<[u8], _>(bounds))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut scanned = Vec::new();
    store.scan::<[u8], ()>(bounds, |key, value| {
        scanned.push((key.to_vec(), value.to_vec()));
        ControlFlow::Continue(())
    });
    assert_eq!(scanned, expected);
    let mut scanned_back = Vec::new();
    store.scan_rev::<[u8], ()>(bounds, |key, value| {
        scanned_back.push((key.to_vec(), value.to_vec()));
        ControlFlow::Continue(())
    });
    scanned_back.reverse();
    assert_eq!(scanned_back, expected);
    let mut copies = store.range::<[u8], _>(bounds);
    let (mut front, mut back) = (Vec::new(), Vec::new());
    loop {
        let entry = if rng.below(2) == 0 {
            copies.next().map(|entry| front.push(entry))
        } else {
            copies.next_back().map(|entry| back.push(entry))
        };
        if entry.is_none() {
            break;
        }
    }
    back.reverse();
    front.extend(back);
    assert_eq!(front, expected);
}

/// Inserts past the allocator's limit fail, the store holding what it
/// held. Removals take holes back without a page, by compacting, so that
/// most new records of the removed ones' size fit again: not all, as the
/// room a leaf's chain frees stays that leaf's.
#[test]
fn a_spent_budget_refuses_inserts_and_changes_nothing() {
    let pages = Counting::default();
    pages.limit.store(64, Ordering::Relaxed);
    let store = BBTreeStore::new_in(&pages);
    let mut model = BTreeMap::new();
    let mut refused = 0;
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for step in 0..3_000 {
        let n = rng.below(2_000);
        let (key, value) = (key(n), value(n, step, 100));
        match store.insert(&key, &value) {
            Ok(replaced) => assert_eq!(replaced, model.insert(key, value).is_some()),
            Err(InsertError::Alloc(_)) => refused += 1,
            Err(error) => panic!("{error}"),
        }
        assert!(pages.out() <= 64);
    }
    assert!(refused > 0, "the budget runs out");
    for (key, value) in &model {
        assert_eq!(store.get(key).as_ref(), Some(value));
    }
    let removed: Vec<Vec<u8>> = model.keys().step_by(2).cloned().collect();
    for key in &removed {
        assert!(store.remove(key));
        model.remove(key);
    }
    let mut taken_back = 0;
    for key in &removed {
        let value = value(0, 1, 100);
        if store.insert(key, &value).is_ok() {
            model.insert(key.clone(), value);
            taken_back += 1;
        }
    }
    assert!(
        taken_back * 2 > removed.len(),
        "{taken_back} of {} removed keys back in",
        removed.len()
    );
    for (key, value) in &model {
        assert_eq!(store.get(key).as_ref(), Some(value));
    }
    drop(store);
    assert_eq!(pages.out(), 0, "every page goes back");
}

/// Under churn, replacing and removing entries at random over a fixed key
/// set, the holes go back: the heap stays under twice what the records
/// take, as a leaf compacts once half its chain is holes, and keeps a head
/// page with room.
#[test]
fn holes_go_back_under_churn() {
    let pages = Counting::default();
    let store = BBTreeStore::new_in(&pages);
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let entry_bytes = |model: &BTreeMap<Vec<u8>, Vec<u8>>| -> usize {
        model
            .iter()
            .map(|(key, value)| 9 + key.len() + value.len())
            .sum()
    };
    for round in 0..8 {
        for step in 0..20_000 {
            let n = rng.below(4_000);
            let entry_key = key(n);
            if rng.below(4) == 0 {
                store.remove(&entry_key);
                model.remove(&entry_key);
            } else {
                let value = value(n, step, 16 + rng.below(96));
                store.insert(&entry_key, &value).expect("no limit");
                model.insert(entry_key, value);
            }
        }
        let ratio = heap_per_entry_byte(&pages, PAGE, entry_bytes(&model));
        assert!(
            ratio < 2.0,
            "round {round}: {ratio:.2} heap bytes a record byte"
        );
    }
    for (key, value) in &model {
        assert_eq!(store.get(key).as_ref(), Some(value));
    }
    drop(store);
    assert_eq!(pages.out(), 0, "every page goes back");
}

/// Threads insert, read and remove entries apart, some of whose values
/// take overflow pages.
#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs threads")]
fn threads_share_a_store() {
    let (threads, per_thread) = (4, 5_000);
    let value_len = |n: usize| {
        if n.is_multiple_of(16) {
            PAGE + n % (3 * PAGE)
        } else {
            n % 200
        }
    };
    let store = Arc::new(BBTreeStore::new());
    let handles: Vec<_> = (0..threads)
        .map(|thread| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                for i in 0..per_thread {
                    let n = i * threads + thread;
                    let (key, value) = (key(n), value(n, 0, value_len(n)));
                    store.insert(&key, &value).expect("no limit");
                    assert_eq!(store.get(&key), Some(value));
                    if i % 3 == 0 {
                        assert!(store.remove(&key));
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no thread panics");
    }
    for n in 0..threads * per_thread {
        let present = (n / threads) % 3 != 0;
        assert_eq!(
            store.get(&key(n)),
            present.then(|| value(n, 0, value_len(n)))
        );
    }
}
