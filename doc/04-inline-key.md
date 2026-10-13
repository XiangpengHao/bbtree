# 04 — Inline keys

Status: implemented (D18), on the B+-tree (D17). The problem, the evidence,
the layout and the mechanism ((b) + (d), section 3) are as built; widths per
key type are D31, D32 and D43, prefix stripping is 05-fence-key.md.

## 1. Problem

On keys whose comparison reads the heap or many bytes (`String`, paths,
`[u8; 32]`, memtable keys), bbtree is 0.70–0.95× std at 1M entries. On integer
keys it is 1.15–2.6× faster.

## 2. Cause

Measured on `String` keys: bbtree makes a third fewer comparisons than std
(19 vs 29 per get) but each costs more. Every comparison reads the key's heap
bytes, and in a binary search the next probe depends on that result, so the
misses are serial and half the probes mispredict. std's linear scan keeps
several misses in flight. With std's search and node size, bbtree matches std.

With an inline 8-byte prefix, bbtree's binary search over 1 KB nodes matches
std on get (192 vs 191 ns). So big nodes win only if **each probe stops
touching the heap**.

## 3. Design: what is settled

- **Real keys stay where they are.** Leaves store real `K`, because we hand out
  `&K`. Separator keys live physically in internal nodes (D17).
- **A inline key is a small `Copy` value derived from a key,** with one contract:
  if `inline(a) != inline(b)`, their order equals the order of `a` and `b`. Equal
  inline keys decide nothing, and the search falls back to comparing the full keys.
  - **Refined by prefix stripping** (05-fence-key.md, "The trait"): an inline
    key is a window, 8 bytes at an offset, of an encoding of the key that orders
    like it, so that a node can skip the bytes its keys share.
- **A wrong inline-key implementation gives wrong answers, never undefined
  behaviour.** The search stays in bounds for any comparator (D14).
- **Inline keys are stored beside the keys, as a parallel dense array in the
  node,** not inside a fatter key element; see "Layout" below.
- **Bytes-like keys** (`String`, `str`, `Box<str>`, `Arc<str>`, `Vec<u8>`,
  `Box<[u8]>`, `[u8; N]`, `Bytes`, and the user-key part of memtable keys):
  8-byte windows, big-endian, zero-padded, of which nodes keep 8 bytes for
  heap strings' leaves, 4 for their internal nodes and 2 for byte arrays
  (D43, D32; Q7). A tie falls back to the full comparison.
- **Search runs over the inline keys,** which compare in a register, so the
  search strategy follows the inline key, not `K`. On a tie, the remaining range
  of equal inline keys is searched on full keys. The run is found by a linear
  count over every slot with AVX-512, a branchless binary search otherwise
  (Q6).
- **No node prefetch** (D46). Every hit on a key longer than its inline key,
  and every tie, reads a key line, which sits on the critical path; the
  layouts put it where the search already reads (D29, D32), and internal
  nodes keep only their separators' inline keys, the keys being heads (D49).
- **Both leaves and internal nodes carry inline keys.** Every lookup searches
  both, and in both each probe would otherwise read the heap. Measured: both
  benefit (Q3).
- **Moving the array.** Insert, remove, split and merge move it together with
  the keys, through `Slots` (D14).
- **Cost:** 8 B per entry at the starting width. `String`→`u64` leaf capacity
  drops from 31 to 25 (21 with 16-byte inline keys).
- **No reliance on stable specialization.** As of 2026, the project goal
  "Specialization: minimal implementation" (opened May 2026) covers design work
  and a minimal sound implementation, with stabilization in later years. It
  depends on the next-generation trait solver, targeted for 2026. Concrete-type
  specialization is the subset expected to be sound, but it isn't usable on
  stable for years.
- **Adoption:** existing `BTreeMap` users should benefit without code changes.
  Code written against bbtree doesn't need to switch back to std cleanly.

### Mechanism: a defaulted strategy parameter (b) that recognizes std types (d)

How a key type gets its inline key. "Every type defaults to itself, `String`
overrides" is two overlapping trait implementations, which stable Rust
forbids without specialization, so it's split in two:

- **(b) An opt-in trait.** Anyone can implement it, newtypes included, and the
  map gets one extra type parameter with a default.
- **(d) A default that recognizes std byte-string keys** by type identity, so
  existing code gets inline keys with no change.

Sketch:

```rust
pub type InlineWord = u64;  // 8 bytes; u128 for 16

pub trait InlineKey: Ord {
    /// Bytes offset..offset + 8 of an encoding that orders like the key,
    /// big-endian, zero past its end (05-fence-key.md, "The trait").
    fn inline_key_at(&self, offset: usize) -> InlineWord;
}
impl InlineKey for String {
    fn inline_key_at(&self, offset: usize) -> InlineWord { window(self.as_bytes(), offset) }
}

pub struct Auto;
pub struct WithInlineKey;

pub struct BBTreeMap<K, V, A = Global, S = Auto> { /* … */ }
```

User code:

```rust
let names: BBTreeMap<String, u64> = BBTreeMap::new();      // inline keys

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct UserName(String);
impl InlineKey for UserName {
    fn inline_key_at(&self, offset: usize) -> InlineWord { self.0.inline_key_at(offset) }
}
let plain: BBTreeMap<UserName, u64> = BBTreeMap::new();                     // no inline keys
let opted_in: BBTreeMap<UserName, u64, WithInlineKey> =
    BBTreeMap::with_strategy(WithInlineKey);                                 // inline keys
```

Opting in goes through `with_strategy`, not an annotated `new()`: a `new()`
generic over `S` breaks plain `BBTreeMap::new()` with "type annotations
needed", because default type parameters don't drive inference. `new()` stays
`Auto`-only, as std's is `Global`-only. Until the allocator parameter lands, the
strategy is the third parameter.

How `Auto` recognizes std types inside the tree. `type_id_of` is `TypeId`
with lifetimes erased (dtolnay's `typeid` trick, vendored):

```rust
fn is_byte_string<T: ?Sized>() -> bool {
    let id = type_id_of::<T>();
    id == TypeId::of::<String>() || id == TypeId::of::<str>() || id == TypeId::of::<Vec<u8>>()
}

if is_byte_string::<K>() && is_byte_string::<Q>() {
    let target = prefix(unsafe { byte_string(key) });
    // search the inline-key array for `target`
} else {
    self.search_index(key)
}
```

- **Drop-in compatibility.** `BBTreeMap<K, V>` means what std's `BTreeMap<K, V>`
  means; `A` sits in std's allocator slot, and `S` is defaulted. Every method is
  identical for every `S`.
- **Both sides must be recognized.** `Auto` uses inline keys only when **both** `K`
  and `Q` are listed types, because `K: Borrow<Q>` allows a user type with its
  own `Ord` as `Q`. Every listed type orders by bytes. A newtype has its own
  `TypeId`, so it is never misclassified; it takes today's path unless it opts
  in.
- **Integers too, since D31.** `Auto` also recognizes `u64`, `i64`, `u128`
  and `i128`, and uses their own `InlineKey` impls, as `WithInlineKey` does;
  a lookup uses them when `Q` is `K`.
- **The choice costs nothing at run time.** `TypeId::of` is a const fn since
  1.91, but comparing `TypeId`s in a const context is still unstable
  (`const_cmp`), so `is_byte_string` is a plain function that the optimizer folds
  after monomorphization.
  - The node layout keeps compile-time checks: the layouts with and without a
    inline-key array are both consts, each asserted; the folded check only picks
    one.
  - **To confirm in the prototype's assembly:** the fold for the lifetime-erased
    version.
- **Where the unsafe goes:** the casts and the vendored `typeid` trick, about 30
  lines, in the modules that already allow `unsafe` (D14).
- **The inline-key path itself** (array, search, moves) is shared by `Auto` and
  `WithInlineKey`; `S` only decides where inline keys come from.
- **When specialization is stable, we switch to it.** See the TODO at the end.

### Layout: parallel array, not a fat key element

For `String` → `u64` in a 1 KB leaf (25 entries with 8-byte inline keys):

```
parallel array:  | hdr | i0 i1 … i24 | k0 k1 … k24 | free | … v1 v0 | head |
                       └─ 200 B ─┘└─── 600 B ───┘
fat element:     | hdr | (i0,k0) (i1,k1) … (i24,k24) | free | … v1 v0 | head |
                       └──────────── 800 B ─────────┘
```

Both cost 8 bytes per entry. The parallel array wins on what the search
touches:

- **Search lines:** 4 instead of 13 while inline keys decide. Hits and ties
  still read key lines (section 3).
- **Search loop:** runs over a dense `[u64]`, which can be vectorized.
- **Hot set:** internal nodes keep only inline-key arrays and edges hot, about 4×
  less, so more upper levels stay in L1/L2.
- **`keys()`** stays a contiguous `&[K]`.

It costs one extra cache line (the `String` header) on a tie and for the final
equality check, and one more array to shift on insert and remove. With unique
prefixes that's about one line per get, against many saved. The fat element
wins only for heavily tied keys, where the inline key decides nothing anyway
(Q2).

## 4. Open questions

**Q1. (resolved)** How a key type gets its inline key: (b) + (d), see
"Mechanism" in section 3.

**Q2. Keys with long shared prefixes.** Handled by per-node prefix stripping;
see 05-fence-key.md.

**Q3. (resolved) Both node kinds benefit; internal nodes most.** Wiki titles
at 10M, `get_hit`, one run each with `--perf` (counters are reliable, times ±15%):

| build | ns | instructions | DRAM fills |
|---|---|---|---|
| no inline keys | 1073 | 1158 | 83.5 |
| inline keys | 704 | 803 | 42.2 |
| inline keys in internal nodes only | 748 | 997 | 46.2 |

- **Split of the gain:** of the 369 ns saved, about 325 come from internal
  nodes and 44 from leaves.
- **Cost:** 68.8 → 80.5 bytes per entry (+17%). Nodes hold 25 entries instead
  of 31, so a 10M map is one level taller (4 → 5).

**Q4. (resolved) No length in the inline key.** The inline key is the prefix
only.
- **Internal nodes** need ordering comparisons, which the prefix provides.
- **Leaves** also need equality, to confirm a hit. For std string and byte
  keys, the length is already in the node, inside the key's own header (`ptr`,
  `len`), so a leaf can settle equality for keys no longer than the inline
  width from the prefix plus that length, without reading the heap.

**Q5. (resolved)** The name is **inline key**: the `InlineKey` trait.

**Q7. (resolved) Width: 8-byte windows, of which each key type keeps 2, 4 or
8 bytes** (D18), by what a tie costs (05-fence-key.md, "Width"). Real key sets tie on
8-byte prefixes far more than the synthetic benchmark keys, which almost never
tie. Fraction of sorted neighbours sharing the first 8 / 16 bytes
(`~/.cache/bbtree-bench`, see each `SOURCE.md`):

| keys | 8-byte tie | 16-byte tie | keys ≤ 8 B |
|---|---|---|---|
| crates.io names (347k) | 36% | 5% | 27% |
| Wikipedia titles (19M) | 70% | 20% | 8% |
| JSON keys (1.2k distinct) | 24% | 1% | 67% |
| Linux paths (87k) | 99% | 89% | 0% |

- **Ties grow with map size.** Wikipedia's 8-byte tie rate is 4% at 1k keys,
  44% at 1M and 65% at 10M.
- **Paths:** see 05-fence-key.md.

**Q6. (resolved) Search over the inline-key array: a linear count with
AVX-512, a branchless binary search otherwise.**
- **The count:** one pass over every slot up to the capacity counts the
  inline keys below the target and those equal to it, masked by `len`, so
  the run is `below..below + equal`. Its trip count is a constant per node
  kind, so for 21 slots each count compiles to three AVX-512 compares and
  no loop, branch or dependent load. It reads slots past `len`, so those
  hold the greatest inline key.
- **The search:** a branchless binary search for the run's start, then a
  scan for its end. The scan replaced a second binary search, whose exit
  depended on where the first one ended: on hits it mispredicted about 0.5
  times per lookup, late, as the leaf arrived from DRAM, which flushed the
  next lookup's overlapped work (LBR and cycle profiles, hash32 at 1M).
- **Measured** (2026-10-08, 1M entries, bbtree, one run each), `get_hit`
  ns before → after:

  | build | hash32 | str-json | str-16 | wiki titles |
  |---|---|---|---|---|
  | AVX-512 (count) | 227 → 157 | 302 → 233 | 339 → 243 | 365 → 303 |
  | baseline x86-64 (search) | 231 → 167 | 302 → 254 | 325 → 270 | 370 → 330 |

  Misses and inserts stayed within noise (±5%) on baseline, and stayed
  within noise or gained up to 10% with AVX-512.
- **Why the split:** each way trades extra work against mispredicts, and
  which wins depends on what the CPU can compare in vectors. Hash32
  `get_hit` at 1M, instructions / mispredicts / ns per lookup:

  | way | AVX-512 | baseline x86-64 |
  |---|---|---|
  | count | 441 / 0.07 / 157 | 1477 / 0.07 / 246 |
  | early-exit scan | 565 / 6.3 / 179 | 565 / 6.3 / 178 |
  | binary search, then scan | not measured | 476 / 1.9 / 167 |

  - **The early-exit scan** (stop at the first inline key not below the
    target) is scalar, so it costs the same in both builds: it mispredicts
    about once per node, where it stops. With AVX-512 it is 6–18% slower
    than the count on every get.
  - **The count** never mispredicts, and with AVX-512 its work is three
    compares. Baseline x86-64 has no 64-bit vector compare, so there it is
    an emulated sequence, about 200 more instructions per node: gets 7–54%
    slower than before it.
  - **The search** probes about 5 of 21 slots, without the emulation, and
    mispredicts less than the scan: the fastest of the three without
    AVX-512.
- **Open: AVX2.** It has only a signed 64-bit vector compare, so the count
  needs a fix-up per compare; one run of count against search was mixed,
  which is not enough to settle it. It takes the search for now.

## 5. How we'll know it worked

- **Cells:** the `keys` suite (`str-json`, `str-16`, `str-path`, `hash32`,
  `memtable16`, `memtable24`), plus `tiny` and `memtable`, against std's
  `BTreeMap` and `hashbrown`.
- **Success:**
  - at least std's speed on get and insert for `str-json` and `str-16` at 1M
    and 10M;
  - no regression on integer keys;
  - memory per entry reported alongside.
- **Paths** were expected to stay near parity until prefix stripping
  (05-fence-key.md). Measured: they regress, to 1.2–1.7× std's time.
  - **Cause:** when every inline key in a node ties, the inline search is pure
    overhead, about 100 instructions per level, before the full-key search.
  - **Result so far:** before the prefetch fix, `str-json` and `str-16` gets
    ran at 0.61–0.74× std's time at 1M and 10M.
  - **With prefix stripping** (D26), `str-path` runs at 0.66–0.76× its time
    before at 1M (05-fence-key.md, "Measured").

## TODO

- [ ] **Track Rust specialization; when it is stable, switch to it.**
  - **The target design:** a blanket `impl<K: Ord> InlineKey for K` meaning "no
    inline key", overridden for `String`, `str`, `Vec<u8>`, … and by users for
    their own types. Then:
    - `Auto`'s type-identity list goes away;
    - `WithInlineKey` goes away; implementing `InlineKey` is enough, with no
      change of map type;
    - the `S` parameter can go away.
  - **What to watch:**
    - the "always applicable" / concrete-type subset, which is what this needs;
    - the 2026 project goal "Specialization: minimal implementation"
      ([rust-lang/goals#652](https://github.com/rust-lang/goals/issues/652));
    - the tracking issue
      ([rust-lang/rust#31844](https://github.com/rust-lang/rust/issues/31844));
    - the next-generation trait solver it depends on.
  - **Last checked:** 2026-10-07. Design work only; stabilization in later years.
- [ ] **AVX2: count or search?** It takes the search; one run was mixed
  (Q6). Settle it if AVX2 builds matter.
