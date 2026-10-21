//! The post (design/14-blogpost.md, whose text it shows): what bbtree is, how
//! fast, on one thread, on many, and in real systems
//! (`performance_cards.rs`), what it can do (`use_cases.rs`), how it works
//! (`how_it_works.rs`), and how it is kept correct (`correctness.rs`), each
//! part an overview over its sections as tabs (`part_tabs.rs`), with the
//! works it cites.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::correctness::HowWeMakeItCorrect;
use crate::how_it_works::{HowItWorks, Note, NoteRef, Notes};
use crate::measurements::Measurements;
use crate::outline::{PostHeading, PostOutline, SectionHeading};
use crate::performance_cards::PerformanceCards;
use crate::site::{AUTHOR_PAGE, Credit, SiteHeader, SitePage};
use crate::spotlight::{HintLayer, PointerHint, Spotlight};
use crate::theme::{PostTheme, ThemeSwitch};
use crate::use_cases::UseCases;

/// bbtree, as the post's text names it: a code word, which the text
/// lights faintly in the site's highlighter (`.bbtree-name`).
#[component]
pub(crate) fn Bbtree() -> Element {
    rsx! {
        code { class: "bbtree-name", "bbtree" }
    }
}

#[component]
pub(crate) fn Post() -> Element {
    let hint = PointerHint::use_new();
    // The charts' own spotlight, for their hints: they light no node.
    let charts = Spotlight::use_new(hint);
    let measurements = use_hook(|| Rc::new(Measurements::of_ryzen_9900x()));
    let theme = PostTheme::use_new();
    use_hook(|| {
        // The heading the address names in view, as the page loads and as
        // the address changes: once drawn, as the tab that holds it may
        // open first (`part_tabs.rs`). The browser can't, as the page
        // draws after it loads.
        document::eval(
            r#"
            const follow = () => {
                const id = decodeURIComponent(location.hash.slice(1));
                if (!id) return;
                let frames = 10;
                const bring = () => {
                    const target = document.getElementById(id);
                    if (target) target.scrollIntoView();
                    else if (frames-- > 0) requestAnimationFrame(bring);
                };
                requestAnimationFrame(bring);
            };
            addEventListener('hashchange', follow);
            follow();
            "#,
        );
    });
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
                    strong { class: "bbtree-name", "bbtree" }
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
                    UseCases {}
                    HowItWorks {}
                    HowWeMakeItCorrect {}
                    Reviewers {}
                    Notes {}
                }
            }
            }
            Credit {}
            HintLayer { hint }
        }
    }
}

#[component]
fn Reviewers() -> Element {
    rsx! {
        SectionHeading { heading: PostHeading::Reviewers }
        p {
            "TODO: plan to ask friends and one person from each of the end-to-end benchmarks to review the post. Ask if they are willing to put their name here."
        }
    }
}
