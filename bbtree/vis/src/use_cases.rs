//! The post's "What can it do?" (design/14-blogpost.md): what a fast
//! B-tree is for, an overview of three cases, and the cases as tabs: the
//! memtable of LSM-trees, with their end-to-end results again
//! (`end-to-end.svg`); a database's index, with Turso's
//! (`database-index.svg`); and a map written to disk once and mapped back,
//! with its time to the first answer against std's map rebuilt
//! (`first-query.svg`), all of which `vis/figures/` writes.

use dioxus::prelude::*;

use crate::how_it_works::{Note, NoteRef};
use crate::outline::{PostHeading, SectionHeading};
use crate::part_tabs::{PartTabList, PartTabs, SectionPanel};
use crate::post::Bbtree;
use crate::post_figure::SvgFigure;
use crate::rust_code::RustCode;

const END_TO_END: Asset = asset!("/assets/figures/end-to-end.svg");
const DATABASE_INDEX: Asset = asset!("/assets/figures/database-index.svg");
const FIRST_QUERY: Asset = asset!("/assets/figures/first-query.svg");

/// The post's code, as doc 14 gives it.
const MMAP_CREATE: &str = r#"use bbtree::unsync::BBTreeMap;
use bbtree_mmap::BBTreeMapFile;

let names = BBTreeMap::from([(7_u64, String::from("ferris")), (42, String::from("corro"))]);
BBTreeMapFile::<u64, String>::create("names.bbtree", &names)?;"#;

const MMAP_OPEN: &str = r#"use bbtree_mmap::BBTreeMapFile;

let names = unsafe { BBTreeMapFile::<u64, String>::mmap("names.bbtree").open()? };
assert_eq!(names.get(&42), Some("corro"));"#;

#[component]
pub(crate) fn UseCases() -> Element {
    let tabs = PartTabs::use_new(PostHeading::UseCases);
    rsx! {
        SectionHeading { heading: PostHeading::UseCases }
        p {
            "B-trees are the most important data structure in big data systems, a fast b-tree like "
            Bbtree {}
            " benefits everything on top of it: agents memory, LLM training, RAG, KV store, state caching, vector search, and more."
        }
        p { "Here we discuss three representative use cases:" }
        ol {
            li {
                strong { "Replace skiplist in LSM-trees." }
                " In place of the skiplist of four LSM-trees, "
                Bbtree {}
                " is 1.37–6.83× faster."
            }
            li {
                strong { "In-memory database index." }
                " In place of Turso's skiplists, inserts are 2.22× and indexed "
                code { "SELECT" }
                "s 1.90× faster."
            }
            li {
                strong { "Mmap a bbtree from disk." }
                " No rebuild after a restart: the first answer comes 160–2,500× sooner than std's "
                code { "BTreeMap" }
                "."
            }
        }
        PartTabList { tabs }
        SectionPanel { tabs, section: PostHeading::CaseMemtable, Memtable {} }
        SectionPanel { tabs, section: PostHeading::CaseDatabaseIndex, DatabaseIndex {} }
        SectionPanel { tabs, section: PostHeading::CaseMmap, MmapFromDisk {} }
    }
}

#[component]
fn Memtable() -> Element {
    rsx! {
        p {
            "All LSM-trees use an ordered map to hold recent writes in memory, the memtable. In Rust, this ordered map is usually a skiplist, like "
            code { "crossbeam-skiplist" }
            "."
        }
        p {
            Bbtree {}
            " can easily replace it and is much faster. A skiplist takes a cache miss for almost every node it visits on its way down; a "
            Bbtree {}
            " lookup touches a handful of cache lines (see "
            a { href: "#how-it-works", "How it works" }
            ", below)."
        }
        p {
            Bbtree {}
            " has a similar interface as "
            code { "crossbeam-skiplist" }
            " but simpler: there is no epoch, no need to worry about when to pin or re-enter an epoch."
        }
        p {
            Bbtree {}
            " allows more advanced memory management:"
        }
        ol {
            li { "Custom allocator is supported on day one." }
            li {
                "It never allocates on irregular sizes (i.e., not a power of two): memory management stays easy, with no external fragmentation"
                NoteRef { note: Note::FreeSpace }
                "."
            }
            li { "Allocation failure can be gracefully handled." }
        }
        p {
            "We integrated "
            Bbtree {}
            " into four LSM-trees, on each system's own benchmarks:"
        }
        ol {
            li {
                SystemLink { name: "fjall", href: "https://github.com/fjall-rs/fjall" }
                ": gets 2.47×, inserts 1.61×, write batches 3.00×."
            }
            li {
                SystemLink { name: "SlateDB", href: "https://github.com/slatedb/slatedb" }
                ": gets 1.71×, puts 1.65×, write batches of 10 rows 2.36×."
            }
            li {
                SystemLink { name: "Tonbo", href: "https://github.com/tonbo-io/tonbo" }
                ": 10M-row writes 6.83×, conflict-checked transactions 4.10×."
            }
            li {
                SystemLink { name: "Lance", href: "https://github.com/lance-format/lance" }
                "'s MemWAL index (its own skiplist): point lookups 2.19×, batched lookups 3.94×, batched puts 4.21×."
            }
        }
        SvgFigure {
            src: END_TO_END,
            alt: "Fifteen workloads of four LSM-trees, each a dark bar as shipped with bbtree's shorter bright bar inside it: 1.37× to 6.83× faster.",
            caption: rsx! {
                "The LSM-trees of the End to End card: each system's own benchmark, one thread, Ryzen 9 9900X. The dark bar is the time per operation as shipped, the bright one inside it with "
                Bbtree {}
                "; only the map changed."
            },
        }
    }
}

#[component]
fn DatabaseIndex() -> Element {
    rsx! {
        p {
            "A database's tables change while they are read: rows are inserted, updated and deleted between queries, and every index of a table changes with it. An in-memory database keeps each in an ordered map, which has to stay sorted under these writes."
        }
        p {
            SystemLink { name: "Turso", href: "https://github.com/tursodatabase/turso" }
            ", SQLite rewritten in Rust, keeps its MVCC store in memory: its tables' rows and its indexes' entries, each in a "
            code { "crossbeam-skiplist" }
            ". With "
            Bbtree {}
            " in their place, on Turso's own benchmarks:"
        }
        ol {
            li {
                code { "INSERT" }
                " of random rowids: 2.22×."
            }
            li {
                "Indexed point "
                code { "SELECT" }
                ": 1.90×."
            }
            li {
                code { "UPDATE" }
                " in "
                code { "BEGIN CONCURRENT" }
                ": 1.22×."
            }
            li {
                "Point "
                code { "SELECT" }
                " by rowid: 1.16×."
            }
        }
        SvgFigure {
            src: DATABASE_INDEX,
            alt: "Four workloads of Turso, each a dark bar as shipped with bbtree's shorter bright bar inside it: 1.16× to 2.22× faster.",
            caption: rsx! {
                "Turso on the End to End card: its own benchmark, one thread, Ryzen 9 9900X. The dark bar is the time per operation as shipped, the bright one inside it with "
                Bbtree {}
                "; only the map changed."
            },
        }
    }
}

#[component]
fn MmapFromDisk() -> Element {
    rsx! {
        p {
            a {
                href: "https://crates.io/crates/bbtree-mmap",
                target: "_blank",
                rel: "noopener",
                code { "bbtree-mmap" }
            }
            " allows you to build a bbtree and write it to disk once; when you need it, you can simply mmap it and use it instantly, without rebuilding anything."
        }
        pre {
            code { "cargo add bbtree-mmap" }
        }
        p {
            strong { "Build a bbtree and save to disk" }
        }
        RustCode { code: MMAP_CREATE }
        p {
            strong { "Mmap bbtree and query it immediately" }
        }
        RustCode { code: MMAP_OPEN }
        SvgFigure {
            src: FIRST_QUERY,
            alt: "Time from a restart to the first answer: std's BTreeMap reads and rebuilds 10M entries in 45.1 ms with its file in the page cache and 101.5 ms from disk; bbtree-mmap answers in 18 µs and 618 µs.",
            caption: rsx! {
                "Time to the first answer after a restart, 10M "
                code { "u64" }
                " entries, Ryzen 9 9900X. std's "
                code { "BTreeMap" }
                " reads its file back and rebuilds; "
                code { "bbtree-mmap" }
                " maps its file and reads only the pages the lookup touches."
            },
        }
    }
}

#[component]
fn SystemLink(name: &'static str, href: &'static str) -> Element {
    rsx! {
        a { href, target: "_blank", rel: "noopener", {name} }
    }
}
