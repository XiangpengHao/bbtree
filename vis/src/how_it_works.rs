//! The post's "How it works" (doc/14-blogpost.md): a lookup searches and
//! fetches at every node, and each section makes one of the two cheaper,
//! with one figure that shows that one idea. The figures of a mechanism
//! are drawn plain (`prefix_keys_figure.rs`, `key_count_figure.rs`,
//! `versions_figure.rs`); those of what a lookup did read a real tree
//! (`cache_lines_figure.rs`, `batch_lanes_figure.rs`). The footnotes the
//! section cites close the post.

use dioxus::prelude::*;

use crate::batch_lanes_figure::BatchLanesFigure;
use crate::cache_lines_figure::CacheLinesFigure;
use crate::figure::TreeStore;
use crate::key_count_figure::KeyCountFigure;
use crate::prefix_keys_figure::PrefixKeysFigure;
use crate::versions_figure::VersionsFigure;

#[component]
pub(crate) fn HowItWorks(trees: TreeStore) -> Element {
    rsx! {
        h2 { class: "post-section", "How it works" }
        p { "A lookup walks from the root to a leaf: 5 nodes at 10M entries. At each node, it does two things:" }
        ol {
            li {
                strong { "Search" }
                ": pick the child to go to."
            }
            li {
                strong { "Fetch" }
                ": bring that child into cache, usually a miss to DRAM."
            }
        }
        p {
            "Every B-tree pays both, at every level. bbtree makes the search cheaper (prefix keys, SIMD), makes the fetch smaller (layout), and overlaps the fetches of different lookups (batching)."
        }

        h3 { class: "post-subsection", "Prefix keys to improve comparisons" }
        p {
            "Comparing two "
            code { "String" }
            "s means following two pointers into the heap. A binary search over a node of "
            code { "String" }
            "s is a chain of heap misses: each probe waits for the one before."
        }
        p {
            "The fix is old database wisdom"
            NoteRef { note: Note::Graefe }
            ", and the same trick as German strings"
            NoteRef { note: Note::Umbra }
            ": keep a short, fixed-size prefix of each key in the node, and compare that instead. bbtree keeps up to 8 bytes per key, in a dense array next to the keys. Two keys whose prefixes differ are ordered by their prefixes alone; only a tie reads the heap."
        }
        p {
            "But real keys share prefixes. In 10M Wikipedia titles, 65% of neighbors share their first 8 bytes, and then the prefix decides nothing."
        }
        p {
            "So bbtree skips what a node's keys share. Every key in a node lies between the node's two fences (the separators beside it in its parents), so every key starts with whatever the fences share. The node stores only that length, a "
            code { "u16" }
            ", and keeps the 8 bytes "
            em { "after" }
            " it. No fence key is stored."
        }
        PrefixKeysFigure {}
        p {
            "When we added prefix keys, Wikipedia lookups went from 1,073 to 704 ns and loaded half as many cache lines from DRAM. Skipping shared prefixes made lookups of Linux paths another 1.5× faster (693 → 457 ns at 1M)."
        }

        h3 { class: "post-subsection", "AVX-512/NEON SIMD to improve search speed" }
        p {
            "With prefix keys, a node search is a search over a small array of integers: 2 bytes a key for integers and UUIDs, 4 to 8 for strings, which is enough once the shared prefix is gone."
        }
        p {
            "Binary search is the textbook answer, and the wrong one here. Each step waits for the one before, and its branch is a coin flip for the CPU."
        }
        p {
            "bbtree doesn't search; it counts. The child to follow is the number of keys smaller than the target. One AVX-512 instruction compares the target with 32 two-byte keys at once, and a popcount of the result is the answer. A whole internal node is two 64-byte vectors: no loop, no branch, every load issued at once. Empty slots hold the largest value, so they never count."
        }
        KeyCountFigure {}
        p {
            "On ARM, the same count runs in NEON. On an M1 Pro, it took a 10M "
            code { "u64" }
            " lookup from 390 to 174 ns, and from 1,678 instructions to 407."
        }
        p {
            "AVX-512 needs "
            code { "-C target-cpu=native" }
            " (Rust 1.89 or later); without it, bbtree counts with SSE2."
        }

        h3 { class: "post-subsection", "Careful layouts to improve cache locality" }
        p {
            "At 10M entries, the tree is many times larger than the cache, so most fetches go to DRAM. What a fetch costs is the number of 64-byte cache lines it touches. bbtree lays out each node so that a lookup touches as few as possible:"
        }
        ol {
            li {
                strong { "1 KiB nodes." }
                " Disk B-trees use big pages because a disk read is expensive. In memory, the unit is a cache line, and every insert shifts half a node. In our sweep, 1 KiB beat everything from 512 B to 8 KiB on random gets and inserts; 4 KiB nodes made random gets 1.4× slower."
            }
            li {
                strong { "Internal nodes keep no keys" }
                ", only prefix keys and child pointers. A search reads the prefix keys, then one pointer; full keys in between took cache lines for nothing. 49 children fit in 512 bytes. On a tie, the search reads the full key from the leaf, where it lives anyway."
            }
            li {
                strong { "Leaves keep each key next to its value" }
                ", aligned so that a pair never straddles two lines: the line that confirms the key brings the value with it."
            }
        }
        CacheLinesFigure { trees }
        p {
            "And no software prefetch. Prefetch hides a bad layout instead of fixing it: when we tried it, misses got 12–33% slower, and DRAM traffic went up 4–6×."
        }

        h3 { class: "post-subsection", "Batching to improve throughput" }
        p {
            "A single lookup can't beat its chain of misses: the CPU can't fetch a child before the parent's search picks it."
        }
        p {
            "But a caller often has many keys at hand: a join probing an index, a bulk load. Their lookups are independent, so their misses can overlap."
        }
        p {
            code { "get_many_copied" }
            " looks up an array of keys at once:"
        }
        pre {
            code { "let values = map.get_many_copied([&a, &b, &c, &d]);" }
        }
        p {
            "Each key walks the tree in its own "
            em { "lane" }
            ": it searches a node, prefetches the child it picked, and steps aside. While it waits for memory, the other lanes search."
        }
        BatchLanesFigure { trees }
        p {
            "On one thread, batching takes a "
            code { "u64" }
            " get from 109 to 74 ns, and a JSON-key get from 162 to 102 ns. At 96 threads, UUID gets go from 591 to 795 M/s with 64 keys a call."
        }
        p {
            "Each lane is an "
            code { "async" }
            " block that reads like the plain lookup, and a tiny executor polls the lanes in turn: no allocation, no runtime. It is also the only place bbtree prefetches: the one line each lane reads next."
        }

        h3 { class: "post-subsection", "Optimistic latch coupling to improve concurrency" }
        p {
            "The usual way to share a B-tree is a reader-writer latch on every node. But taking a read latch writes to the latch, and every lookup takes the root's. At 96 cores, the root's cache line bounces between cores on every lookup: readers never block each other, yet they still queue for one line."
        }
        p { "bbtree's readers write nothing. Each node carries a version number:" }
        ol {
            li {
                "A reader reads the version, reads the node, and checks the version again. Unchanged: what it read is consistent. Changed: start over."
            }
            li {
                "A writer latches the node with a compare-and-swap, changes it, and bumps the version as it lets go."
            }
        }
        p {
            "Lookups read their leaf the same way: "
            code { "get_copied" }
            " copies the value out and checks the version after, like a seqlock. A lookup, hit or miss, writes nothing to shared memory."
        }
        VersionsFigure {}
        p {
            "A reader may still be reading a node that a writer just freed. So a freed node never goes back to the allocator while the map lives: it waits on a free list for the next node of its kind. A late reader reads a stale node, fails its version check, and retries. No epochs, no guards to hold."
        }
        p {
            "At 96 threads, bbtree's lookups run at 86–94% of 96 times its one-thread speed. On UUID gets, "
            code { "RwLock<BTreeMap>" }
            " does 19 M/s; bbtree 591 M/s, 795 batched. Bf-tree"
            NoteRef { note: Note::BfTree }
            " latches its inner nodes the same way"
            NoteRef { note: Note::Olc }
            "."
        }

        h3 { class: "post-subsection", "Model checking and fuzzing to ensure correctness" }
        p { "A faster B-tree is worthless if it loses your data. bbtree is checked four ways:" }
        ol {
            li {
                strong { "Differential fuzzing against std." }
                " Random operations run on bbtree and std's "
                code { "BTreeMap" }
                " side by side; every answer must match, and the tree's invariants are checked after every operation, under AddressSanitizer. Seven targets, every week and before every release."
            }
            li {
                strong { "Model checking with Shuttle." }
                " Shuttle runs the threads one at a time and switches between them at every atomic they share: latch words, the root pointer, free lists. It reaches interleavings that stress tests rarely hit, and found two bugs that our threaded tests and a 12-thread stress run had missed. One: after a merge, a lookup of a separator key could run its closure on a stale copy of an entry, which another thread could then remove and free."
            }
            li {
                strong { "Miri" }
                ", with tree borrows and strict provenance, on a selected set of tests."
            }
            li {
                strong { "Hostile user code." }
                " An "
                code { "Ord" }
                ", "
                code { "Clone" }
                " or "
                code { "Drop" }
                " that panics or lies can give wrong answers, but never undefined behavior. A fuzz target checks exactly that."
            }
        }
    }
}

/// A figure drawn from nothing but what it shows: a mechanism, not a
/// lookup the reader can open in the explorer. Framed as the explorer's
/// figures are (`FigureFrame`).
#[component]
pub(crate) fn Diagram(caption: Element, children: Element) -> Element {
    rsx! {
        figure { class: "figure",
            div { class: "figure-view diagram", {children} }
            figcaption { class: "figure-caption",
                div { class: "figure-text", {caption} }
            }
        }
    }
}

/// A work the post cites, in the order it first cites them.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Note {
    Graefe,
    Umbra,
    BfTree,
    Olc,
}

impl Note {
    const ALL: [Self; 4] = [Self::Graefe, Self::Umbra, Self::BfTree, Self::Olc];

    fn number(self) -> usize {
        Self::ALL.iter().position(|note| *note == self).unwrap() + 1
    }

    fn id(self) -> &'static str {
        match self {
            Self::Graefe => "note-graefe",
            Self::Umbra => "note-umbra",
            Self::BfTree => "note-bftree",
            Self::Olc => "note-olc",
        }
    }
}

/// A citation's mark in the text, linking to its note.
#[component]
fn NoteRef(note: Note) -> Element {
    rsx! {
        sup { class: "note-ref",
            a { href: "#{note.id()}", "{note.number()}" }
        }
    }
}

/// The notes the post cites, numbered as they are cited.
#[component]
pub(crate) fn Notes() -> Element {
    rsx! {
        ol { class: "post-notes",
            for note in Note::ALL {
                li { key: "{note.id()}", id: note.id(),
                    match note {
                        Note::Graefe => rsx! {
                            "Goetz Graefe, "
                            a {
                                href: "https://doi.org/10.1561/1900000070",
                                target: "_blank",
                                rel: "noopener",
                                "More Modern B-Tree Techniques"
                            }
                            " (2024): keep full keys out of the comparison stream with dense, fixed-width codes, and skip the prefix a page shares."
                        },
                        Note::Umbra => rsx! {
                            a {
                                href: "https://db.in.tum.de/~freitag/papers/p29-neumann-cidr20.pdf",
                                target: "_blank",
                                rel: "noopener",
                                "Umbra: A Disk-Based System with In-Memory Performance"
                            }
                            " (CIDR 2020); see also my post on "
                            a {
                                href: "https://blog.xiangpeng.systems/posts/nature-of-string/",
                                target: "_blank",
                                rel: "noopener",
                                "the nature of strings"
                            }
                            "."
                        },
                        Note::BfTree => rsx! {
                            a {
                                href: "https://github.com/microsoft/bf-tree",
                                target: "_blank",
                                rel: "noopener",
                                "Bf-Tree: A Modern Read-Write-Optimized Concurrent Larger-Than-Memory Range Index"
                            }
                            " (VLDB 2024)."
                        },
                        Note::Olc => rsx! {
                            "Viktor Leis et al., "
                            a {
                                href: "https://db.in.tum.de/~leis/papers/artsync.pdf",
                                target: "_blank",
                                rel: "noopener",
                                "The ART of Practical Synchronization"
                            }
                            " (DaMoN 2016)."
                        },
                    }
                }
            }
        }
    }
}
