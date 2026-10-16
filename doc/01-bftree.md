# 01 — Learning from bf-tree

Source: [microsoft/bf-tree](https://github.com/microsoft/bf-tree) @ `ca61307` (2026-08-19).
Scope: node layout, in-node search, split, traversal, testing.
Out of scope: concurrency, mapping table, mini-pages, circular buffer, WAL, snapshots.

The one difference that shapes everything below: bf-tree keys and values are
byte strings (`&[u8]`). bbtree's are generic `K: Ord` and `V`; we hand out
`&K`/`&V`, cannot copy keys (no `K: Clone`), and std's iterators never call
`Ord`.

## 1. Node format

Both node kinds share one shape: a fixed 32 B header, then metadata growing up
from the front and payload growing down from the back. The gap between them
is the free space (`remaining_size`).

```
 0        32                                                      node_size
 | header | meta 0 | meta 1 | ... | meta n-1 |   free   | payload ... payload |
            ───────── grows → ──────────────              ── ← grows ─────────
```

bf-tree needs per-record metadata (offsets, lengths) because records are
variable-length. bbtree's K and V are fixed-size, so a node is simply arrays:
the keys, then the values (decisions D9).

### Shared header word: `NodeMeta` (`src/nodes/node_meta.rs`)

6 bytes: `encoded: u16`, `node_size: u16`, `remaining_size: u16`.
`encoded` packs the record count and four flags:

| bits  | meaning                                                     |
|-------|-------------------------------------------------------------|
| 15    | `has_fence` (leaf has low/high fence slots)                 |
| 14    | `should_split` (split this node on the next descent)        |
| 13    | `children_is_leaf` (inner only: what the children are)      |
| 12    | `cache_only` leaf                                           |
| 0–11  | record count, so ≤ 4096 records per page                    |

### Leaf (`src/nodes/leaf_node.rs`)

- Header: `NodeMeta`, `prefix_len`, plus buffer-management and durability
  fields; const-asserted to 32 B.
- 8 B metadata per record: payload offset, key length, value length, and the
  first 2 key bytes after the common prefix ("preview"). Binary search
  compares preview bytes first and touches the payload only on a tie.
- Insert: binary search, memmove the metadata entries after the position by
  8 B, write the key suffix and value at the low end of the free gap.

### Inner (`src/nodes/inner_node.rs`)

- Always 4096 B, allocated page-aligned: `Layout::from_size_align(4096, 4096)`.
- 8 B metadata per separator: offset, key length, first 4 key bytes (not
  repeated in the payload). Payload: rest of the key + child id (8 B, read
  with `read_unaligned` because the payload is byte-packed).
- Slot 0 holds a dummy key plus the leftmost child, so the node is a uniform
  array of (separator, child) pairs.
- The parent's `children_is_leaf` flag says what its children are; children
  carry no kind tag.

## 2. Split and traversal

- Leaf split balances bytes, not counts (variable-length records). Inner split
  is by count; the middle key moves up.
- No parent pointers and no sibling pointers. A descent holds only the
  immediate parent. If a split does not fit in the parent, the parent is
  flagged `should_split` and the operation restarts, splitting flagged nodes
  top-down on the way back down (`traverse_to_leaf` in `src/tree.rs`).
- A range scan moves to the next leaf by re-descending from the root with the
  current leaf's high fence key.

## 3. Testing discipline

- `const _: () = assert!(size_of::<LeafNode>() == 32)` and similar for every
  on-page struct.
- Debug-only recomputation of derived state: `current_lowest_offset()` is
  `count * 8 + remaining_size`, and under `debug_assertions` it is re-derived
  by scanning every slot.
- Configuration limits are checked once, when the tree is built, so that any
  insert is guaranteed to finish with at most one split. They are never
  checked during operations.
- Differential fuzzing against `std::collections::BTreeSet<Vec<u8>>`
  (`fuzz/fuzz_targets/bf_tree.rs`): insert, read, delete, scan. `make_key`
  maps a `u16` to a small, highly colliding key space, so duplicates, deletes
  of present keys, and splits are frequent.

## 4. What bbtree takes

| bf-tree | bbtree |
|---|---|
| Metadata grows from the front, payload from the back | Keys, then values, each an array (decisions D9). |
| Count + flags packed in one `u16`; header const-asserted | Header is `len: u16` and a head flag, plus prefix lengths in nodes with inline keys; layouts const-asserted. |
| Node kind decided by the parent (`children_is_leaf`) | Same idea per edge: a pointer's lowest bit tags a leaf, so the parent's edge tells the child's kind before it is read; the tree's pointer to the root page tells the root's (decisions D52). |
| Root id swapped for a new root, its kind in a bit (`ROOT_IS_LEAF_MASK`) | The root page stays the root: growing or shrinking the tree moves a node into or out of it (decisions D52). |
| Inner nodes page-aligned | All nodes aligned to their own size, 1 KB or more, so none crosses a page (decisions D15), from the map's allocator (D8). |
| No parent pointers | Same. Iterators, entries and cursors carry a node stack instead (decisions D11). |
| Configuration limits checked once, at construction | Const-asserted per `(K, V)`: minimum capacity, so an insert needs at most one split per level and the exact allocation count is known before mutating. |
| Debug cross-checks; const-asserted layouts | Same. |
| Differential fuzzing with a small, colliding key space | Same, plus a tiny-capacity mode so splits happen with few keys. |

Not taken:

- **Stored fence keys.** They would need `K: Clone`. bbtree strips what a
  node's fences share without storing them, from windows of an
  order-preserving encoding (decisions D26).
- **Preview bytes from `Ord`.** Generic `Ord` gives only a comparison, no
  bytes to copy; keys with an `InlineKey` encoding get them as inline keys
  (decisions D18).
- **Byte-packed payload read with `read_unaligned`.** We hand out `&K`/`&V`,
  which must be aligned.
- **Re-descending from the root to reach the next leaf.** It would call `Ord`
  inside `next()`, which std's iterators never do. A node stack avoids it.
