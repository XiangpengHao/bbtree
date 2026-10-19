//! The post (doc/14-blogpost.md, whose text it shows): what bbtree is, how
//! fast, on one thread, on many, and in real systems
//! (`performance_cards.rs`), and how it works (`how_it_works.rs`), with
//! the works it cites.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::how_it_works::{HowItWorks, Note, NoteRef, Notes};
use crate::measurements::Measurements;
use crate::outline::PostOutline;
use crate::performance_cards::PerformanceCards;
use crate::site::{AUTHOR_PAGE, Credit, SiteHeader, SitePage};
use crate::spotlight::{HintLayer, PointerHint, Spotlight};
use crate::theme::{PostTheme, ThemeSwitch};

#[component]
pub(crate) fn Post() -> Element {
    let hint = PointerHint::use_new();
    // The charts' own spotlight, for their hints: they light no node.
    let charts = Spotlight::use_new(hint);
    let measurements = use_hook(|| Rc::new(Measurements::of_ryzen_9900x()));
    let theme = PostTheme::use_new();
    rsx! {
        document::Title { "bbtree: fastest ordered map" }
        div { class: "post-page",
            SiteHeader { current: SitePage::Post,
                ThemeSwitch { theme }
            }
            main { class: "post-main",
            article { class: "post",
                // The opening flows as plain blocks, as the sections do, so
                // that the quote's note floats past it.
                div { class: "post-intro",
                header { class: "post-head",
                    h1 { class: "post-title", "bbtree: fastest ordered map" }
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
                        " and Andreas Reuter"
                        NoteRef { note: Note::Gray }
                    }
                }
                p {
                    strong { "bbtree" }
                    " is by far the fastest B-tree (that I'm aware of):"
                }
                ol {
                    li {
                        "5× faster than std's "
                        code { "BTreeMap" }
                        " on a single thread, 12× faster than "
                        code { "crossbeam-skiplist" }
                        " at 96 threads."
                    }
                    li { "30% lower memory usage, 5× lower p99 latency." }
                    li {
                        "Zero dependencies, "
                        code { "no_std" }
                        " compatible."
                    }
                    li {
                        code { "unsync::BBTreeMap" }
                        " is a drop-in replacement for std's "
                        code { "BTreeMap" }
                        "/"
                        code { "BTreeSet" }
                        "."
                    }
                    li {
                        "Production-ready by formal verification, model checking, deterministic simulation, fuzzing, and lots of testing."
                    }
                    li {
                        "Brought to you by decades of database research and careful curation by "
                        a { href: AUTHOR_PAGE, target: "_blank", rel: "noopener", "Xiangpeng Hao" }
                        "."
                    }
                }
                }
                PerformanceCards { measurements, spotlight: charts }
                // The sections, with the outline beside them and the
                // notes after them.
                div { class: "post-body",
                    PostOutline {}
                    HowItWorks {}
                    Notes {}
                }
            }
            }
            Credit {}
            HintLayer { hint }
        }
    }
}
