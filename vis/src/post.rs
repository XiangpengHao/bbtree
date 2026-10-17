//! The post (doc/14-blogpost.md, whose text it shows): what bbtree is, how
//! fast, on one thread, on many, and in real systems
//! (`performance_cards.rs`), how it works (`how_it_works.rs`), how to try
//! it, and how to cite it. The figures that read a tree build it as the
//! reader comes to them, sharing the trees of one workload.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::figure::TreeStore;
use crate::how_it_works::{HowItWorks, Notes};
use crate::measurements::Measurements;
use crate::performance_cards::PerformanceCards;
use crate::site::{AUTHOR_PAGE, Credit, SiteHeader, SitePage};
use crate::spotlight::{HintLayer, PointerHint, Spotlight};

/// The post's BibTeX entry, as doc 14 gives it.
const CITATION: &str = r"@misc{hao2026bbtree,
  author       = {Xiangpeng Hao},
  title        = {{bbtree}: fastest ordered map in the world},
  year         = {2026},
  howpublished = {\url{https://github.com/XiangpengHao/bbtree}},
}";

#[component]
pub(crate) fn Post() -> Element {
    let trees = TreeStore::use_new();
    let hint = PointerHint::use_new();
    // The charts' own spotlight, for their hints: they light no node.
    let charts = Spotlight::use_new(hint);
    let measurements = use_hook(|| Rc::new(Measurements::of_ryzen_9900x()));
    rsx! {
        document::Title { "bbtree: fastest ordered map in the world" }
        div { class: "post-page",
            SiteHeader { current: SitePage::Post }
            article { class: "post",
                header { class: "post-head",
                    h1 { class: "post-title", "bbtree: fastest ordered map in the world" }
                }
                blockquote { class: "post-quote",
                    p {
                        "B-trees are by far the most important access path structure in database and file systems."
                    }
                    footer {
                        "— "
                        a {
                            href: "https://en.wikipedia.org/wiki/Jim_Gray_(computer_scientist)",
                            target: "_blank",
                            rel: "noopener",
                            "Jim Gray"
                        }
                    }
                }
                p {
                    strong { "bbtree" }
                    " is by far the fastest btree (ordered map):"
                }
                ol {
                    li {
                        "5x faster than std's "
                        code { "BTreeMap" }
                        " at single thread, 7x faster than "
                        code { "crossbeam-skiplist" }
                        " at 12 threads."
                    }
                    li { "30% lower memory usage, 5x lower p99 latency." }
                    li {
                        "Zero dependencies, "
                        code { "no_std" }
                        " compatible."
                    }
                    li {
                        code { "unsync::BBTreeMap" }
                        " is a "
                        strong { "drop-in replacement" }
                        " for std's "
                        code { "BTreeMap" }
                        "/"
                        code { "BTreeSet" }
                        "."
                    }
                    li {
                        "Production-ready by formal verification, model checking, fuzzing, and lots of testing."
                    }
                    li {
                        "Brought to you by decades of database research and careful curation by "
                        a { href: AUTHOR_PAGE, target: "_blank", rel: "noopener", "Xiangpeng Hao" }
                        "."
                    }
                }
                PerformanceCards { measurements, spotlight: charts }
                HowItWorks { trees }
                h2 { class: "post-section", "Try it" }
                pre {
                    code { "cargo add bbtree" }
                }
                ol {
                    li {
                        "Replacing std's "
                        code { "BTreeMap" }
                        "? Use "
                        code { "bbtree::unsync::BBTreeMap" }
                        "; change one "
                        code { "use" }
                        " line."
                    }
                    li {
                        "Sharing it across threads? Use "
                        code { "bbtree::BBTreeMap" }
                        "."
                    }
                    li {
                        "Many keys at once? Use "
                        code { "get_many_copied" }
                        " and "
                        code { "insert_many" }
                        "."
                    }
                    li {
                        "On x86, build with "
                        code { "-C target-cpu=native" }
                        " for AVX-512."
                    }
                }
                p { "If you like bbtree, don't forget to cite it in your paper:" }
                pre {
                    code { {CITATION} }
                }
                Notes {}
            }
            Credit {}
            HintLayer { hint }
        }
    }
}
