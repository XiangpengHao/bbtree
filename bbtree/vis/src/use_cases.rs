//! The post's "What it can do?" (design/14-blogpost.md): what a fast
//! B-tree is for, an overview of three cases, and the cases as tabs: the
//! memtable of LSM-trees, with the end-to-end results again
//! (`end-to-end.svg`); a map written to disk once and mapped back, with
//! its time to the first answer against std's map rebuilt
//! (`first-query.svg`), both of which `vis/figures/` writes; and an
//! external index of Parquet files, with its results on ClickBench's
//! `hits.parquet`.

use dioxus::prelude::*;

use crate::how_it_works::{Note, NoteRef};
use crate::outline::{PostHeading, SectionHeading};
use crate::part_tabs::{PartTabList, PartTabs, SectionPanel};
use crate::post::Bbtree;
use crate::post_figure::SvgFigure;
use crate::rust_code::RustCode;

const END_TO_END: Asset = asset!("/assets/figures/end-to-end.svg");
const FIRST_QUERY: Asset = asset!("/assets/figures/first-query.svg");

/// The post's code, as doc 14 gives it.
const MMAP_CREATE: &str = r#"use bbtree::unsync::BBTreeMap;
use bbtree_mmap::BBTreeMapFile;

let names = BBTreeMap::from([(7_u64, String::from("ferris")), (42, String::from("corro"))]);
BBTreeMapFile::<u64, String>::create("names.bbtree", &names)?;"#;

const MMAP_OPEN: &str = r#"use bbtree_mmap::BBTreeMapFile;

let names = unsafe { BBTreeMapFile::<u64, String>::mmap("names.bbtree").open()? };
assert_eq!(names.get(&42), Some("corro"));"#;

const PARQUET_INDEX: &str = r#"ParquetIndexBuilder::source("hits.parquet")
    .index(&["UserID"])
    .index(&["CounterID", "EventDate"])
    .build()?;

let state = SessionStateBuilder::new()
    .with_default_features()
    .with_parquet_index();"#;

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
                " In place of the skiplist of five systems, "
                Bbtree {}
                " is 1.16–6.83× faster."
            }
            li {
                strong { "Mmap a bbtree from disk." }
                " No rebuild after a restart: the first answer comes 160–2,500× sooner than std's "
                code { "BTreeMap" }
                "."
            }
            li {
                strong { "External index for Parquet files." }
                " Embedded at the end of an existing Parquet file, with "
                Bbtree {}
                " it runs 1.7–263× faster."
            }
        }
        PartTabList { tabs }
        SectionPanel { tabs, section: PostHeading::CaseMemtable, Memtable {} }
        SectionPanel { tabs, section: PostHeading::CaseMmap, MmapFromDisk {} }
        SectionPanel { tabs, section: PostHeading::CaseParquetIndex, ParquetIndex {} }
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
            " into five systems, on each system's own benchmarks:"
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
            li {
                SystemLink { name: "Turso", href: "https://github.com/tursodatabase/turso" }
                "'s MVCC store, not an LSM-tree but a skiplist all the same: inserts 2.22×, indexed "
                code { "SELECT" }
                " 1.90×."
            }
        }
        SvgFigure {
            src: END_TO_END,
            alt: "Nineteen workloads of five systems, each a dark bar as shipped with bbtree's shorter bright bar inside it: 1.16× to 6.83× faster.",
            caption: rsx! {
                "The End to End card again: each system's own benchmark, one thread, Ryzen 9 9900X. The dark bar is the time per operation as shipped, the bright one inside it with "
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

/// One query of the Parquet results: its filter, the rows it keeps, and
/// its time without the index and with it.
struct ParquetQuery {
    filter: &'static str,
    rows: &'static str,
    without_index: &'static str,
    with_index: &'static str,
    speedup: &'static str,
}

impl ParquetQuery {
    /// The queries doc 14's table gives, on ClickBench's `hits.parquet`.
    const ALL: [Self; 4] = [
        Self {
            filter: "UserID = -2398344306775305602",
            rows: "142",
            without_index: "5.97 s",
            with_index: "23 ms",
            speedup: "263×",
        },
        Self {
            filter: "CounterID = 1298 AND EventDate = 15889",
            rows: "2,072",
            without_index: "244 ms",
            with_index: "38 ms",
            speedup: "6.4×",
        },
        Self {
            filter: "CounterID = 1298",
            rows: "2,072",
            without_index: "200 ms",
            with_index: "29 ms",
            speedup: "6.9×",
        },
        Self {
            filter: "CounterID = 62 AND EventDate BETWEEN 15887 AND 15917",
            rows: "738,172",
            without_index: "353 ms",
            with_index: "204 ms",
            speedup: "1.7×",
        },
    ];
}

#[component]
fn ParquetIndex() -> Element {
    rsx! {
        p {
            "Parquet already has advanced stats and bloom filters to prune the data to scan, "
            Bbtree {}
            " pushes the pruning to its extreme: it directly pinpoints the rows to read."
        }
        p {
            "With "
            Bbtree {}
            ", you can directly embed the index to the end of an existing Parquet file without rewriting it."
        }
        p {
            "In DataFusion, it looks like this, more details in "
            a {
                href: "https://github.com/XiangpengHao/bbtree/tree/main/bbtree-datafusion-parquet",
                target: "_blank",
                rel: "noopener",
                "bbtree-datafusion-parquet"
            }
            ":"
        }
        RustCode { code: PARQUET_INDEX }
        p {
            "On ClickBench's "
            code { "hits.parquet" }
            NoteRef { note: Note::ClickBench }
            " (100M rows, 14.8 GB), with an index of "
            code { "UserID" }
            " (879 MB, 6% of the file) and one of "
            code { "(CounterID, EventDate)" }
            " (401 MB), "
            code { "SELECT * FROM hits WHERE ..." }
            " takes:"
        }
        div { class: "post-table",
            table {
                thead {
                    tr {
                        th { "Filter" }
                        th { class: "number", "Rows" }
                        th { class: "number", "Without index" }
                        th { class: "number", "With index" }
                        th { class: "number", "Speedup" }
                    }
                }
                tbody {
                    for query in ParquetQuery::ALL {
                        tr { key: "{query.filter}",
                            td {
                                code { "{query.filter}" }
                            }
                            td { class: "number", "{query.rows}" }
                            td { class: "number", "{query.without_index}" }
                            td { class: "number", "{query.with_index}" }
                            td { class: "number speedup", "{query.speedup}" }
                        }
                    }
                }
            }
        }
    }
}
