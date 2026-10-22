//! The post's "How it works" (design/14-blogpost.md): the cost model's
//! three principles, in order, then the principles as tabs: minimize cache
//! misses, then instructions, then cache coherence traffic, with their
//! figures (`post_figure.rs`). The footnotes the post cites close it.

use dioxus::prelude::*;

use crate::outline::{PostHeading, SectionHeading};
use crate::part_tabs::{PartTabList, PartTabs, SectionPanel};
use crate::post::Bbtree;
use crate::post_figure::SvgFigure;
use crate::rust_code::RustCode;

const SEARCH_SPACE: Asset = asset!("/assets/figures/search-space.svg");
const STRING_KEYS: Asset = asset!("/assets/figures/string-keys.svg");
const INLINE_KEYS: Asset = asset!("/assets/figures/inline-keys.svg");
const PREFIX_KEYS: Asset = asset!("/assets/figures/prefix-keys.svg");
const INTEGER_KEYS: Asset = asset!("/assets/figures/integer-keys.svg");
const CACHE_LINES: Asset = asset!("/assets/figures/cache-lines.svg");
const BATCH_LANES: Asset = asset!("/assets/figures/batch-lanes.svg");
const KEY_COUNT: Asset = asset!("/assets/figures/key-count.svg");
const INSTRUCTIONS: Asset = asset!("/assets/figures/instructions.svg");
const VERSION_LOCK: Asset = asset!("/assets/figures/version-lock.svg");
const CONCURRENT_GETS: Asset = asset!("/assets/figures/concurrent-gets.svg");

/// The post's code, as doc 14 gives it.
const INLINE_KEY_TRAIT: &str = r"pub trait InlineKey: Ord {
    /// How many bytes of each inline key the nodes keep: 2, 4 or 8.
    const WIDTH: InlineWidth = InlineWidth::Eight;

    /// Bytes `offset..offset + 8` of the key's encoding, big-endian,
    /// zero past its end, which orders as the keys do.
    fn inline_key_at(&self, offset: usize) -> InlineWord;
}

// A compound key, ordered by customer, then order: its encoding is
// both fields' bytes, one after the other.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct OrderKey {
    customer: u32,
    order: u32,
}

impl InlineKey for OrderKey {
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        let mut bytes = [0; 16];
        bytes[..4].copy_from_slice(&self.customer.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.order.to_be_bytes());
        let start = offset.min(8);
        u64::from_be_bytes(bytes[start..start + 8].try_into().unwrap())
    }
}

let orders: BBTreeMap<OrderKey, u64, _> =
    BBTreeMap::with_strategy(WithInlineKey);";

const GET_MANY: &str = r"fn get_many<const N: usize>(&self, keys: [&K; N]) -> [Option<V>; N] {
    let lanes = keys.map(|key| self.search(key));
    block_on(join_all(lanes))
}";

const SEARCH: &str = r"async fn search(&self, key: &K) -> Option<V> {
    let mut node = self.root();
    while node.is_internal() {
        let edge = node.search(key);
        node = node.child(edge);
        prefetch(node.header()).await;
    }
    node.get(key)
}";

const CHILD_INDEX: &str = r"fn child_index(inline_keys: &[u16; 32], target: u16) -> usize {
    let keys = _mm512_loadu_epi16(inline_keys.as_ptr().cast());
    let smaller =
        _mm512_cmplt_epu16_mask(keys, _mm512_set1_epi16(target as i16));
    smaller.count_ones() as usize
}";

#[component]
pub(crate) fn HowItWorks() -> Element {
    let tabs = PartTabs::use_new(PostHeading::HowItWorks);
    rsx! {
        SectionHeading { heading: PostHeading::HowItWorks }
        p { "Every performance optimization should be guided by a cost model." }
        p {
            "B-tree optimizations are simple, three easy pieces"
            NoteRef { note: Note::Ostp }
            ": we just need to remember three principles, in order:"
        }
        ol {
            li {
                "The number of "
                strong { "chained cache lines" }
                " touched. A cache miss costs about 100 ns."
            }
            li {
                "The number of "
                strong { "instructions" }
                " executed. An instruction costs about 0.5 ns."
            }
            li {
                "The amount of "
                strong { "cache coherence traffic" }
                ", especially important for concurrent access. It forces a cache miss, and costs a little more than 100 ns."
            }
        }
        PartTabList { tabs }
        SectionPanel { tabs, section: PostHeading::CacheMisses, MinimizeCacheMisses {} }
        SectionPanel { tabs, section: PostHeading::Instructions, MinimizeInstructions {} }
        SectionPanel { tabs, section: PostHeading::Coherence, MinimizeCoherenceTraffic {} }
    }
}

#[component]
fn MinimizeCacheMisses() -> Element {
    rsx! {
        p {
            "With this cost model, it's clear that the most important optimization is to minimize the number of cache misses. To put it another way, for each cache miss, we want to narrow down the search space as much as we can."
        }
        SectionHeading { heading: PostHeading::ThoughtExperiment }
        p {
            "Say we have a sorted array of "
            code { "u64" }
            "s, and we want to find a target "
            code { "u64" }
            "."
        }
        p { "With binary search, each cache miss narrows down the search space by half." }
        p {
            "With linear search, each cache miss narrows down the search space by "
            code { "cache_line_size / item_size" }
            " items, which is 64 / 8 = 8."
        }
        p {
            "So the break-even point is about 16 items, where binary search costs the same as linear search: two cache misses to find the target."
        }
        p {
            "But there's a catch: 1. the CPU prefetcher may fetch the next cache lines on its own, which gives linear search an even bigger advantage; 2. linear search is simpler, allowing much more parallelism in the CPU pipeline. So the actual break-even point is usually higher."
        }
        SvgFigure {
            src: SEARCH_SPACE,
            loops: true,
            alt: "Binary search and linear search over 64 sorted u64s, a cache miss at a time: binary search finds the target in 3 misses, linear search in 6.",
            caption: rsx! {
                "64 sorted "
                code { "u64" }
                "s, 8 to a cache line. Each step is one cache miss: binary search halves what is left, linear search rules out one line. Here binary search needs 3 misses and linear search 6; at 16 items, both need at most 2."
            },
        }
        SectionHeading { heading: PostHeading::ApplyToBTrees }
        p {
            "In a B-tree, this decides the node size"
            NoteRef { note: Note::CacheConscious }
            ". If we size nodes so that each holds about 16–32 items, we can always do linear search, which lets us do nice things like SIMD (in later sections)."
        }
        p {
            strong { class: "run-in",
                "What if the keys are Strings?"
            }
            " "
            code { "String" }
            "s are different, because their bytes are stored on the heap, not inline in the node. This changes our equation completely. We no longer get 8 keys from one cache miss; instead, each key we load is its own cache miss. So we almost always want to do binary search over "
            code { "String" }
            "s."
        }
        SvgFigure {
            src: STRING_KEYS,
            loops: true,
            alt: "A node of eight String keys holds only pointers; a binary search for Grace_Hopper follows three of them to the heap.",
            caption: rsx! {
                "A node of "
                code { "String" }
                "s holds each key's pointer, length and capacity; its bytes are on the heap. Every comparison of a binary search follows a pointer: 3 heap reads to find one key of 8."
            },
        }
        p {
            strong { class: "run-in",
                "How does "
                Bbtree {}
                " do it?"
            }
            " Heap keys are very common, so "
            Bbtree {}
            " must handle them well. "
            Bbtree {}
            " introduces "
            code { "inline_keys" }
            ", an idea databases have used for decades"
            NoteRefs { notes: vec![Note::GraefeLarson, Note::Graefe] }
            ", similar to German strings"
            NoteRef { note: Note::Umbra }
            " and "
            code { "StringView" }
            " in Arrow"
            NoteRef { note: Note::StringView }
            "."
        }
        p {
            "With fixed-size inline keys, we get back the benefit of one cache miss loading 8 (or more) keys. And we only load the full key when the comparison ties."
        }
        SvgFigure {
            src: INLINE_KEYS,
            loops: true,
            alt: "The same node with the first 8 bytes of each key inline, one cache line: one compare of all of them leaves one candidate, read once from the heap.",
            caption: rsx! {
                "The same node with inline keys: the first 8 bytes of every key, 64 bytes, one cache line. One compare of all of them leaves a single candidate, and only it is read from the heap."
            },
        }
        p {
            strong { class: "run-in", "What about keys with long prefixes?" }
            " Many keys, like URLs and paths, have long prefixes. If we just take the first 8 bytes as the inline key, we may compare nothing at all. "
            Bbtree {}
            " addresses this with "
            code { "prefix_keys" }
            ": the common prefix of all keys in a node"
            NoteRef { note: Note::PrefixBTree }
            ". The beauty of "
            code { "prefix_keys" }
            " is that no node has to store them. Every key in a node sorts between the two separators on either side of the node in its parent. So if those two separators share their first 20 bytes, every key in the node starts with the same 20 bytes."
        }
        SvgFigure {
            src: PREFIX_KEYS,
            loops: true,
            alt: "Two separators in a parent share their first 20 bytes; the child's keys between them all start with those bytes, so its 8-byte window slides past them to where the keys differ.",
            caption: rsx! {
                "Neighboring Wikipedia titles. The parent's two separators share their first 20 bytes, so every key of the child does too. Over the first 8 bytes, the keys all tie; past the shared 20, they all differ."
            },
        }
        p {
            "So each node only stores the length of its prefix, and keeps the window of each key right after it, which makes inline keys far more effective."
        }
        p {
            strong { class: "run-in",
                "How does this apply to other key types, and how does it work with Rust's type system?"
            }
        }
        p {
            "It's clear that "
            code { "String" }
            " and "
            code { "Vec<u8>" }
            " benefit from this optimization. But integers like "
            code { "u64" }
            " and "
            code { "u128" }
            " can too; we just need to mind the endianness: stored big-endian, their bytes sort in the same order as the numbers."
        }
        SvgFigure {
            src: INTEGER_KEYS,
            alt: "Four u64 keys of one leaf as big-endian bytes: the prefix the leaf skips, its 2-byte inline key, and the rest.",
            caption: rsx! {
                "Four keys of one leaf of a tree of 10M random "
                code { "u64" }
                "s, as bytes: the prefix its separators share, which the leaf skips; the 2 bytes it keeps as each inline key; and the rest, read only on a tie."
            },
        }
        p {
            "For other key types, like compound keys in structs, users can implement the "
            code { "InlineKey" }
            " trait, and when they do, we use the inline keys wherever we can."
        }
        RustCode { code: INLINE_KEY_TRAIT }
        p {
            "The trickiest part is Rust's type system. If we required every key type to implement "
            code { "InlineKey" }
            ", "
            Bbtree {}
            " couldn't be a drop-in replacement for std's "
            code { "BTreeMap" }
            "/"
            code { "BTreeSet" }
            ". Nightly Rust has "
            code { "specialization" }
            NoteRef { note: Note::Specialization }
            " for exactly this. For stable Rust, we specialize the common types by hand: "
            code { "u64" }
            ", "
            code { "i64" }
            ", "
            code { "u128" }
            ", "
            code { "i128" }
            ", "
            code { "String" }
            ", "
            code { "str" }
            ", "
            code { "&str" }
            ", "
            code { "Vec<u8>" }
            ", "
            code { "&[u8]" }
            ", "
            code { "Box<str>" }
            ", "
            code { "Box<[u8]>" }
            ", "
            code { "[u8; 16]" }
            ", "
            code { "[u8; 20]" }
            " and "
            code { "[u8; 32]" }
            ". So without any extra work, the built-in types are optimized, and users can still implement the trait for their own types to get the full benefit."
        }
        SectionHeading { heading: PostHeading::CacheMissResults }
        p { "After these optimizations, we can show how many cache lines a lookup touches." }
        SvgFigure {
            src: CACHE_LINES,
            alt: "Cache lines brought in from memory per lookup, std's BTreeMap and bbtree, for four kinds of key.",
            caption: rsx! {
                "Cache lines brought in from memory per lookup that finds its key, 10M entries, one thread, Ryzen 9 9900X; lower is better."
            },
        }
        SectionHeading { heading: PostHeading::BatchMode }
        p {
            "So far we have focused on maximizing the value of a cache miss: packing as much information as possible into one cache line."
        }
        p { "There's a more advanced optimization: hiding the cost of cache misses." }
        p {
            "The simplest way to hide latency is software prefetching: while working on one key, we prefetch the next one. This usually works well, but it isn't enough for B-trees."
        }
        p {
            "This is because a "
            strong { "B-tree's memory accesses are fundamentally chained" }
            ": you don't know which node to fetch next until you have searched the current one."
        }
        p {
            "But B-trees are often read or built in batches, which lets us play a trick with Rust's async state machines. A batch is one call with many keys, such as "
            code { "BBTreeMap" }
            "'s "
            a {
                href: "https://docs.rs/bbtree/latest/bbtree/struct.BBTreeMap.html#method.get_many_copied",
                target: "_blank",
                rel: "noopener",
                code { "get_many_copied" }
            }
            " and "
            a {
                href: "https://docs.rs/bbtree/latest/bbtree/struct.BBTreeMap.html#method.insert_many",
                target: "_blank",
                rel: "noopener",
                code { "insert_many" }
            }
            ": an array of keys in, an array of answers out."
        }
        p {
            "The high-level idea is simple: in Rust we often treat an IO operation as a "
            code { "Future" }
            ", and "
            code { "await" }
            " it. In "
            Bbtree {}
            ", we treat a cache miss as a "
            code { "Future" }
            ", and "
            code { "await" }
            " it too. Database research has interleaved lookups like this for years, by hand and with coroutines"
            NoteRef { note: Note::Interleaving }
            "."
        }
        p { "The code looks roughly like this (they run in a single thread):" }
        RustCode { code: GET_MANY }
        p {
            "And "
            code { "search" }
            " looks like this:"
        }
        RustCode { code: SEARCH }
        SvgFigure {
            src: BATCH_LANES,
            loops: true,
            alt: "Four u64 keys looked up together, a task each, on one core: the tasks take turns on the core, and while a task's line is on its way, the core runs the others.",
            caption: rsx! {
                "Four "
                code { "u64" }
                " keys looked up together on one core, each key's search a task, from bbtree's own run of the batch; time is counted in turns. In its turn, a task reads the line it prefetched, steps down the tree, prefetches the next line and yields; while that line is on its way, the core runs the other tasks."
            },
        }
        p { "But we need to be very careful:" }
        ol {
            li {
                "A cache miss costs 100 ns, so our "
                code { "Future" }
                " must be as lightweight as possible. This means we can't use "
                code { "tokio" }
                " or any general-purpose async runtime, because their overhead is already higher than a cache miss. "
                Bbtree {}
                " polls its lanes with a tiny executor of its own: no allocation, no runtime."
            }
            li {
                "Each async function in Rust is a state machine generated at compile time. We want this state machine to be as small as possible, so that switching between states is cheap."
            }
            li {
                "There's no way to know whether a cache line is already in cache, so we have to guess. In "
                Bbtree {}
                " that's not so hard: once the leaves outgrow the L2 cache, a leaf is almost always a cache miss; smaller trees look their keys up one at a time."
            }
        }
    }
}

#[component]
fn MinimizeInstructions() -> Element {
    rsx! {
        p {
            "In general, minimizing instructions means reducing CPU time, which means two things: 1. reducing the instruction count; 2. reducing branches."
        }
        p {
            "A mispredicted branch costs about 20 cycles (the time of 20 instructions), about 10% of a cache miss."
        }
        SectionHeading { heading: PostHeading::SearchAgain }
        p {
            "With inline keys, searching a node means searching a small array of small integers: 2 bytes a key for integers and UUIDs, 4 to 8 for strings. Now let's compare their instructions and branches over a node of 32 keys:"
        }
        ol {
            li {
                "Binary search: about 5 steps, each a compare and a branch, and the CPU typically guesses wrong half of the time. A branchless binary search"
                NoteRef { note: Note::Layouts }
                " avoids the guess, but then each step waits for the one before."
            }
            li {
                "Linear search, without its early exit, becomes a count: compare the target with every key, and count the keys smaller than it; that number is the child to follow. It does more compares, but none branches or waits for another, a perfect fit for the CPU pipeline."
            }
        }
        SvgFigure {
            src: KEY_COUNT,
            loops: true,
            alt: "Looking up 45: binary search compares 58, then 27, then 40, each chosen by the one before; the count compares all eight slots at once. Both reach child 4.",
            caption: rsx! {
                "Looking up 45 among seven inline keys, all in one cache line: neither search takes another miss. The count compares all of them at once: four are smaller, so it follows child 4. Binary search reaches child 4 after three compares, each waiting for the one before; over 32 keys, it takes about five."
            },
        }
        SectionHeading { heading: PostHeading::SimdSearch }
        p {
            "With SIMD, searching a node takes a handful of instructions"
            NoteRef { note: Note::KarySearch }
            ". One AVX-512 instruction compares the target with 32 two-byte keys at once; its result is a 32-bit mask, a bit a key, and the number of ones in the mask, its popcount, is the answer:"
        }
        RustCode { code: CHILD_INDEX }
        p { "The function has only three steps, all branchless and super fast:" }
        ol {
            li {
                code { "_mm512_loadu_epi16" }
                " loads the inline keys into a SIMD register."
            }
            li {
                code { "_mm512_cmplt_epu16_mask" }
                " compares them with the target and returns a mask, a bit for each smaller key, in key order (e.g., "
                code { "1111100000000000" }
                ")."
            }
            li {
                code { "smaller.count_ones()" }
                " counts the ones in the mask, which is the index of the child to follow. x86 does this in one instruction, "
                code { "popcnt" }
                "."
            }
        }
        p {
            "We also implement the same search for ARM in NEON, so Apple Silicon and AWS Graviton work too. On x86, AVX-512 needs "
            code { "-C target-cpu=native" }
            " (Rust 1.89 or later)."
        }
        SectionHeading { heading: PostHeading::Compiler }
        p {
            "We often hear \"write simple code and let the compiler optimize it\". The extreme version is FastLanes"
            NoteRef { note: Note::FastLanes }
            ", whose scalar code the compiler automatically vectorizes (generates SIMD code for)."
        }
        p {
            "Here the compiler is not your friend. A node holds few keys, a few dozen at most, and their number is usually not a power of two. The compiler often considers it not worth vectorizing, and sometimes can't prove it safe, so we write the SIMD by hand, and handle the boundary cases very carefully."
        }
        p {
            "On an M1 Pro, the compiler's own count widened each compare's result to 64 bits and added them up one at a time. We wrote the count in NEON by hand and changed nothing else: the same tree, the same nodes, the same cache lines. A lookup of a "
            code { "u64" }
            " key in a map of 10M entries went from 1,678 to 407 instructions, and from 390 to 174 ns."
            NoteRef { note: Note::NeonCount }
        }
        p {
            "Vectorization isn't the only problem: sometimes the compiler generates bad code because it doesn't know the workload. For example, to compare two "
            code { "&[u8]" }
            ", Rust calls "
            code { "memcmp" }
            ", a function call that isn't inlined. But in "
            Bbtree {}
            ", we know the keys are usually short, so instead of calling "
            code { "memcmp" }
            ", we load the bytes as integers and compare the integers. Arrow does the same for "
            code { "StringView" }
            NoteRefs { notes: vec![Note::StringView, Note::ArrowCompare] }
            "."
        }
        SectionHeading { heading: PostHeading::InstructionResults }
        SvgFigure {
            src: INSTRUCTIONS,
            alt: "Instructions per lookup, std's BTreeMap and bbtree, for four kinds of key.",
            caption: rsx! {
                "Instructions per lookup that finds its key, 10M entries, one thread, Ryzen 9 9900X; lower is better."
            },
        }
    }
}

#[component]
fn MinimizeCoherenceTraffic() -> Element {
    rsx! {
        p {
            "Cache coherence traffic happens when one core writes to a cache line that other cores hold. To make sure every core sees a consistent view of memory, CPU caches implement a cache coherence protocol"
            NoteRef { note: Note::Primer }
            ". At a very high level, a write to a cache line invalidates all copies of that line in other cores."
        }
        p {
            "This causes two problems: 1. more cache misses; 2. more importantly, it puts a ceiling on the B-tree's concurrency."
        }
        p {
            "On modern hardware, a contended cache line supports at most about 10 M op/s. So if we simply put a "
            code { "Mutex" }
            " over the B-tree, we reach at most 10 M op/s, while "
            Bbtree {}
            " reaches 180 M op/s at 96 threads."
        }
        p {
            "Note that a "
            code { "RwLock" }
            " won't help: even a read lock must "
            strong { "increment" }
            " the lock's reader count, which is a write to its cache line."
        }
        p {
            "So how does "
            Bbtree {}
            " solve this?"
        }
        p {
            "The key idea is a version lock"
            NoteRefs { notes: vec![Note::Olfit, Note::Olc] }
            ". Every node carries a version: a reader reads the version, reads the node, and reads the version again. If the version hasn't changed, what it read is consistent; otherwise, it retries."
        }
        SvgFigure {
            src: VERSION_LOCK,
            loops: true,
            alt: "A node's version, a writer and two readers, time running down: reader 1 keeps its read, reader 2 overlaps the change and retries.",
            caption: rsx! {
                "Time runs down. Readers only read. Reader 1 finished before the writer changed anything, so its read stands. Reader 2 overlapped the change, finds version 8, and starts over."
            },
        }
        p { "This allows very high concurrency, because a read causes no write traffic at all." }
        p { "But there are two things we need to be careful about." }
        SectionHeading { heading: PostHeading::ProblemReferences }
        p {
            "The value may change, or even be freed, while the user holds the reference, so it's unsafe to use."
        }
        p {
            "We can't return a clone of "
            code { "V" }
            " either, because "
            code { "V" }
            " may not be "
            code { "Clone" }
            "."
        }
        p {
            "Here we use a trick similar to Bf-Tree's"
            NoteRef { note: Note::BfTree }
            ": only inner nodes are optimistic; leaves are not. This works because (1) contention usually happens at inner nodes, (2) values are stored only in leaves, and (3) leaves are only 20% of the nodes on a lookup's path (assuming a tree of 5 levels). For "
            code { "Copy" }
            " values, "
            code { "get_copied" }
            " goes one step further: it reads the leaf optimistically too, copies the value out, and checks the version after, like a seqlock."
        }
        SectionHeading { heading: PostHeading::ProblemReclamation }
        p {
            "The second problem is more subtle: if one thread is reading a node while another thread frees it, the reader may segfault."
        }
        p {
            "The usual solution is epoch-based memory reclamation"
            NoteRef { note: Note::Ebr }
            ", e.g., "
            code { "crossbeam-epoch" }
            "."
        }
        p {
            "But then we either leak the epoch to the user, which is ugly and error-prone, or pin an epoch on every access (many libraries do this), which is expensive: every access is a global read and a private write."
        }
        p {
            "In "
            Bbtree {}
            ", we do it differently: a freed node never goes back to the allocator while the map lives. Instead, the map keeps it on a free list and reuses it for the next node it needs. The node keeps its version through both steps: freeing it bumps the version, reusing it bumps the version again, and a version only grows. So a late reader still reads a node, not arbitrary memory, finds a higher version than it saw, and retries, without any segfault."
        }
        SectionHeading { heading: PostHeading::CoherenceResults }
        SvgFigure {
            src: CONCURRENT_GETS,
            alt: "Gets of u64 keys as threads are added to 96: bbtree's lines climb nearly straight, above DashMap's and far above the other ordered maps'.",
            caption: rsx! {
                "Gets of "
                code { "u64" }
                " keys, half of them misses, 10M entries, all threads together, a thread a core, on AWS's c8a.metal-24xl (96 cores); the Concurrent card above has the other keys and workloads."
            },
        }
        p {
            Bbtree {}
            " is wildly concurrent: its get throughput grows 83–90× as the core count grows to 96, 86–94% of linear scaling. At this thread count, "
            Bbtree {}
            " is even faster than "
            a {
                href: "https://github.com/xacrimon/dashmap",
                target: "_blank",
                rel: "noopener",
                "DashMap"
            }
            ", a "
            em { "hashtable" }
            "."
        }
    }
}

/// A work the post cites, in the order it first cites them.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Note {
    Gray,
    FreeSpace,
    ClickBench,
    Ostp,
    CacheConscious,
    GraefeLarson,
    Graefe,
    Umbra,
    StringView,
    PrefixBTree,
    Specialization,
    Interleaving,
    Layouts,
    KarySearch,
    FastLanes,
    NeonCount,
    ArrowCompare,
    Primer,
    Olfit,
    Olc,
    BfTree,
    Ebr,
    Verus,
    Shuttle,
    Ffs,
}

impl Note {
    const ALL: [Self; 25] = [
        Self::Gray,
        Self::FreeSpace,
        Self::ClickBench,
        Self::Ostp,
        Self::CacheConscious,
        Self::GraefeLarson,
        Self::Graefe,
        Self::Umbra,
        Self::StringView,
        Self::PrefixBTree,
        Self::Specialization,
        Self::Interleaving,
        Self::Layouts,
        Self::KarySearch,
        Self::FastLanes,
        Self::NeonCount,
        Self::ArrowCompare,
        Self::Primer,
        Self::Olfit,
        Self::Olc,
        Self::BfTree,
        Self::Ebr,
        Self::Verus,
        Self::Shuttle,
        Self::Ffs,
    ];

    fn number(self) -> usize {
        Self::ALL.iter().position(|note| *note == self).unwrap() + 1
    }

    fn id(self) -> String {
        format!("note-{}", self.number())
    }

    /// The note's text: who, what (linked), where, and why it's cited.
    fn text(self) -> Element {
        match self {
            Self::Gray => rsx! {
                "Jim Gray and Andreas Reuter, "
                em { "Transaction Processing: Concepts and Techniques" }
                " (1993), as quoted by Goetz Graefe in "
                NoteLink { href: "https://doi.org/10.1561/1900000028", title: "Modern B-Tree Techniques" }
                " (2011)."
            },
            Self::FreeSpace => rsx! {
                "Remzi H. Arpaci-Dusseau and Andrea C. Arpaci-Dusseau, "
                NoteLink {
                    href: "https://pages.cs.wisc.edu/~remzi/OSTEP/vm-freespace.pdf",
                    title: "Free-Space Management",
                }
                ", chapter 17 of "
                em { "Operating Systems: Three Easy Pieces" }
                ": external fragmentation is free space chopped into pieces of different sizes, so that a request can fail although the total would fit it."
            },
            Self::ClickBench => rsx! {
                NoteLink { href: "https://github.com/ClickHouse/ClickBench", title: "ClickBench" }
                ", ClickHouse's analytical benchmark; its "
                code { "hits" }
                " table as one Parquet file."
            },
            Self::Ostp => rsx! {
                "Not more complicated than an operating system! Remzi H. Arpaci-Dusseau and Andrea C. Arpaci-Dusseau, "
                NoteLink {
                    href: "https://pages.cs.wisc.edu/~remzi/OSTEP/",
                    title: "Operating Systems: Three Easy Pieces",
                }
                " (2016)."
            },
            Self::CacheConscious => rsx! {
                "Jun Rao and Kenneth A. Ross, "
                NoteLink { href: "https://doi.org/10.1145/342009.335449", title: "Making B+-Trees Cache Conscious in Main Memory" }
                " (SIGMOD 2000)."
            },
            Self::GraefeLarson => rsx! {
                "Goetz Graefe and Per-Åke Larson, "
                NoteLink { href: "https://doi.org/10.1109/ICDE.2001.914847", title: "B-Tree Indexes and CPU Caches" }
                " (ICDE 2001)."
            },
            Self::Graefe => rsx! {
                "Goetz Graefe, "
                NoteLink { href: "https://doi.org/10.1561/1900000070", title: "More Modern B-Tree Techniques" }
                " (2024): keep full keys out of the comparison stream with dense, fixed-width codes, and skip the prefix a page shares."
            },
            Self::Umbra => rsx! {
                "Thomas Neumann and Michael Freitag, "
                NoteLink { href: "https://db.in.tum.de/~freitag/papers/p29-neumann-cidr20.pdf", title: "Umbra: A Disk-Based System with In-Memory Performance" }
                " (CIDR 2020); see also my post on "
                NoteLink { href: "https://blog.xiangpeng.systems/posts/nature-of-string/", title: "the nature of strings" }
                "."
            },
            Self::StringView => rsx! {
                "Xiangpeng Hao and Andrew Lamb, Using StringView / German Style Strings to Make Queries Faster: "
                NoteLink { href: "https://datafusion.apache.org/blog/2024/09/13/string-view-german-style-strings-part-1/", title: "Part 1, Reading Parquet" }
                " and "
                NoteLink { href: "https://datafusion.apache.org/blog/2024/09/13/string-view-german-style-strings-part-2/", title: "Part 2, String Operations" }
                " (2024)."
            },
            Self::PrefixBTree => rsx! {
                "Rudolf Bayer and Karl Unterauer, "
                NoteLink { href: "https://doi.org/10.1145/320521.320530", title: "Prefix B-Trees" }
                " (TODS 1977)."
            },
            Self::Specialization => rsx! {
                NoteLink { href: "https://rust-lang.github.io/rfcs/1210-impl-specialization.html", title: "RFC 1210: impl specialization" }
                ", unstable since 2016 ("
                NoteLink { href: "https://github.com/rust-lang/rust/issues/31844", title: "tracking issue" }
                ")."
            },
            Self::Interleaving => rsx! {
                "Onur Kocberber, Babak Falsafi and Boris Grot, "
                NoteLink { href: "https://www.vldb.org/pvldb/vol9/p252-kocberber.pdf", title: "Asynchronous Memory Access Chaining" }
                " (VLDB 2015); Georgios Psaropoulos et al., "
                NoteLink { href: "https://www.vldb.org/pvldb/vol11/p230-psaropoulos.pdf", title: "Interleaving with Coroutines: A Practical Approach for Robust Index Joins" }
                " (VLDB 2017); Christopher Jonathan et al., "
                NoteLink { href: "https://www.vldb.org/pvldb/vol11/p1702-jonathan.pdf", title: "Exploiting Coroutines to Attack the \"Killer Nanoseconds\"" }
                " (VLDB 2018)."
            },
            Self::NeonCount => rsx! {
                "Lookups of "
                code { "u64" }
                " keys that hit, in a map of 10M "
                code { "u64" }
                " entries, one run each on an M1 Pro, with bbtree-eval's "
                NoteLink {
                    href: "https://github.com/XiangpengHao/bbtree-eval/tree/main/diagnostics/portable",
                    title: "portable probe",
                }
                ", before and after "
                NoteLink {
                    href: "https://github.com/XiangpengHao/bbtree/blob/main/design/03-decisions.md",
                    title: "decision D56",
                }
                ", which counts inline keys in NEON."
            },
            Self::Layouts => rsx! {
                "Paul-Virak Khuong and Pat Morin, "
                NoteLink { href: "https://arxiv.org/abs/1509.05053", title: "Array Layouts for Comparison-Based Searching" }
                " (2017)."
            },
            Self::KarySearch => rsx! {
                "Benjamin Schlegel, Rainer Gemulla and Wolfgang Lehner, "
                NoteLink { href: "https://doi.org/10.1145/1565694.1565705", title: "k-ary Search on Modern Processors" }
                " (DaMoN 2009)."
            },
            Self::FastLanes => rsx! {
                "Azim Afroozeh and Peter Boncz, "
                NoteLink { href: "https://www.vldb.org/pvldb/vol16/p2132-afroozeh.pdf", title: "The FastLanes Compression Layout: Decoding >100 Billion Integers per Second with Scalar Code" }
                " (VLDB 2023)."
            },
            Self::ArrowCompare => rsx! {
                "arrow-rs "
                NoteLink { href: "https://github.com/apache/arrow-rs/pull/7748", title: "#7748" }
                " compares inlined string views as one "
                code { "u128" }
                ", and their 4-byte prefixes as one "
                code { "u32" }
                ", instead of byte by byte."
            },
            Self::Primer => rsx! {
                "Vijay Nagarajan, Daniel J. Sorin, Mark D. Hill and David A. Wood, "
                NoteLink { href: "https://doi.org/10.1007/978-3-031-01764-3", title: "A Primer on Memory Consistency and Cache Coherence" }
                ", second edition (2020)."
            },
            Self::Olfit => rsx! {
                "Sang K. Cha, Sangyong Hwang, Kihong Kim and Keunjoo Kwon, "
                NoteLink { href: "https://www.vldb.org/conf/2001/P181.pdf", title: "Cache-Conscious Concurrency Control of Main-Memory Indexes on Shared-Memory Multiprocessor Systems" }
                " (VLDB 2001)."
            },
            Self::Olc => rsx! {
                "Viktor Leis et al., "
                NoteLink { href: "https://db.in.tum.de/~leis/papers/artsync.pdf", title: "The ART of Practical Synchronization" }
                " (DaMoN 2016)."
            },
            Self::BfTree => rsx! {
                "Xiangpeng Hao and Badrish Chandramouli, "
                NoteLink { href: "https://www.vldb.org/pvldb/vol17/p3442-hao.pdf", title: "Bf-Tree: A Modern Read-Write-Optimized Concurrent Larger-Than-Memory Range Index" }
                " (VLDB 2024); code at "
                NoteLink { href: "https://github.com/microsoft/bf-tree", title: "microsoft/bf-tree" }
                "."
            },
            Self::Ebr => rsx! {
                "Keir Fraser, "
                NoteLink { href: "https://www.cl.cam.ac.uk/techreports/UCAM-CL-TR-579.pdf", title: "Practical Lock-Freedom" }
                " (PhD thesis, 2004); in Rust, "
                NoteLink { href: "https://docs.rs/crossbeam-epoch", title: "crossbeam-epoch" }
                "."
            },
            Self::Verus => rsx! {
                "Andrea Lattuada et al., "
                NoteLink { href: "https://doi.org/10.1145/3586037", title: "Verus: Verifying Rust Programs using Linear Ghost Types" }
                " (OOPSLA 2023); code at "
                NoteLink { href: "https://github.com/verus-lang/verus", title: "verus-lang/verus" }
                "."
            },
            Self::Shuttle => rsx! {
                NoteLink { href: "https://github.com/awslabs/shuttle", title: "Shuttle" }
                ", a library for testing concurrent Rust code, from AWS."
            },
            Self::Ffs => rsx! {
                NoteLink { href: "https://github.com/aoli-al/ffs", title: "ffs" }
                ", fork-based deterministic concurrency testing."
            },
        }
    }
}

/// A citation's mark in the text, linking to its note.
#[component]
pub(crate) fn NoteRef(note: Note) -> Element {
    rsx! {
        NoteRefs { notes: vec![note] }
    }
}

/// The marks of works cited together, apart by commas, and each work's
/// note beside the line, in the margin (`.sidenote`), where the screen is
/// wide enough; elsewhere a tap on a mark unfolds its note under the line,
/// and another folds it.
#[component]
fn NoteRefs(notes: Vec<Note>) -> Element {
    let mut unfolded = use_signal(|| None::<Note>);
    rsx! {
        sup { class: "note-ref",
            for (index , note) in notes.iter().copied().enumerate() {
                if index > 0 {
                    ","
                }
                a {
                    href: "#{note.id()}",
                    "aria-label": "Note {note.number()}",
                    "aria-expanded": unfolded() == Some(note),
                    onclick: move |evt| {
                        evt.prevent_default();
                        unfolded.set(if unfolded() == Some(note) { None } else { Some(note) });
                    },
                    "{note.number()}"
                }
            }
        }
        for note in notes {
            span {
                class: if unfolded() == Some(note) { "sidenote unfolded" } else { "sidenote" },
                role: "note",
                span { class: "sidenote-number", "{note.number()}" }
                {note.text()}
            }
        }
    }
}

/// A link of a note, to the work it cites.
#[component]
fn NoteLink(href: &'static str, title: &'static str) -> Element {
    rsx! {
        a { href, target: "_blank", rel: "noopener", {title} }
    }
}

/// The notes the post cites, numbered as they are cited.
#[component]
pub(crate) fn Notes() -> Element {
    rsx! {
        section { class: "post-notes", role: "doc-endnotes", "aria-labelledby": "notes",
            h2 { id: "notes", class: "post-notes-title", "Notes" }
            ol {
                for note in Note::ALL {
                    li { key: "{note.id()}", id: note.id(), {note.text()} }
                }
            }
        }
    }
}
