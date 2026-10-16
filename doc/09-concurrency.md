# 09 — Concurrency: BCTree, closures instead of handles

Status: design (2026-10-15), not built. `BBTreeMap` stays single-threaded
(D4).

## Scope

A concurrent B-tree differs enough in design and cost that `BBTreeMap`
stays single-threaded, and the concurrent tree is a separate type, BCTree
(working name). The two share the node code; how is open.

BCTree is about single-entry concurrency: latches that keep the tree
consistent while threads read and write it, one entry per operation. No
transactions, no atomicity across entries.

## Requirements

1. Users never take a latch by hand.
2. Users never hold an epoch, guard or handle. BCTree's API stays close to
   `BBTreeMap`'s.
3. A single thread never deadlocks with itself. Two threads may deadlock
   only where the semantics ask for it.

## API: closures, not handles

An operation that exposes a key or value runs the caller's closure while
the entry's leaf is latched, unlatches, and returns what the closure
returned. No reference, guard or handle outlives the call, so no latch
does either.

```rust
impl<K: Ord, V> BCTreeMap<K, V> {
    /// Runs `f` on the entry while its leaf is latched shared.
    fn read<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>;
    /// Runs `f` on the entry while its leaf is latched exclusive.
    fn update<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &mut V) -> R) -> Option<R>;
    /// std's `Entry` API within one exclusive latch: insert or modify.
    fn entry<R>(&self, key: K, f: impl FnOnce(Entry<'_, K, V>) -> R) -> R;
    /// Visits the range one leaf at a time until `f` breaks.
    fn scan<Q, R>(&self, range: impl RangeBounds<Q>,
                  f: impl FnMut(&K, &V) -> ControlFlow<R>) -> Option<R>;

    fn get_cloned<Q>(&self, key: &Q) -> Option<V> where V: Clone {
        self.read(key, |_, v| v.clone())
    }
}
```

The types keep references from escaping: `impl FnOnce(&K, &V) -> R` is
`for<'a> FnOnce(&'a K, &'a V) -> R`, and `R` is fixed outside `'a`, so
`map.read(&k, |_, v| v)` doesn't compile. `std::thread::scope` and
`LocalKey::with` rely on the same rule. scc's `HashMap` (`read_sync`,
`update_sync`, `iter_sync`) and DashMap (`view`, `alter`) offer the same
shape.

One entry at a time: an entry can't be held across calls, there is no
`get_mut` handle, and an operation on two entries isn't atomic.

Rejected:

- **A handle that holds the leaf latch**, like DashMap's `Ref`. Its docs
  warn on `insert`, `remove`, `get_mut` and others: "May deadlock if called
  when holding any sort of reference into the map". That breaks
  requirement 3, and a held latch also blocks the leaf's splits.
- **A per-entry lock under an intention lock on the leaf.** A split needs
  the leaf exclusive, which conflicts with every intention lock, so a
  thread that holds an entry and then inserts into the same leaf can wait
  on itself; and a split moves entries to another leaf, so their locks
  can't live in it.

## Latching

- **Inner nodes: optimistic.** A version word per node: a reader reads the
  node, then checks the version is unchanged; a writer takes the node by
  compare-and-swap on the version. bf-tree does this
  (`src/utils/inner_lock.rs`: `try_read`, `check_version`, `upgrade`).
- **Leaves: pessimistic.** A spin latch with backoff and a scheduler hint:
  shared for `read` and `scan`, exclusive for `update`, `entry`, `insert`
  and `remove`.
- **Descent:** optimistic through the inner nodes, checking each node's
  version after reading its child pointer; then latch the leaf and check
  its parent's version. Any failed check restarts from the root.

An optimistic read may touch only plain bytes, loaded as atomics. Reading
memory that a writer is changing is a data race, undefined behavior in
Rust even when the version check later rejects the read, and user code
(`Ord`, `Borrow`) must never see a half-written key. Inner nodes hold only
inline keys (11), so an optimistic descent runs no user code. A tie reads
the key at a leaf's head (11); how it does so safely is open.

## Scans

`scan` latches one leaf at a time. Between leaves it unlatches and resumes
by searching for the first key after the last one it returned. A scan is
not a snapshot: a change behind the cursor is missed and one ahead of it
is seen. A key present for the whole scan is visited exactly once.

## Panics

Whatever is simplest. The latch guard unlatches on unwind; there is no
poisoning; a value that a panicking `update` closure left half-changed
stays as it is.

## Open

- **Re-entry:** a closure that calls into the same map. Deferred. Known
  deadlocks: a shared latch taken again on the same leaf while a writer
  waits between the two; any call on the leaf from inside `update`; two
  threads in closures on leaves A and B that each call into the other's
  leaf.
- **Ties:** how an optimistic descent reads a leaf head's key.
- **Splits and merges:** the latching protocol.
- **Memory reclamation:** what keeps a node an optimistic reader is reading
  from being freed (epochs, or type-stable nodes).

## Testing

Shuttle, for randomized schedules of the concurrent paths.
