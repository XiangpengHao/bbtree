//! The post's "How do we make it correct?" (design/14-blogpost.md): why
//! correctness is the hard part, and bbtree's three guards as tabs.

use dioxus::prelude::*;

use crate::how_it_works::{Note, NoteRef};
use crate::outline::{PostHeading, SectionHeading};
use crate::part_tabs::{PartTabList, PartTabs, SectionPanel};
use crate::post::Bbtree;
use crate::site::{AUTHOR_PAGE, REPOSITORY};

#[component]
pub(crate) fn HowWeMakeItCorrect() -> Element {
    let tabs = PartTabs::use_new(PostHeading::Correctness);
    rsx! {
        SectionHeading { heading: PostHeading::Correctness }
        p {
            "Performance optimization is easy: you simply ask Claude or Codex to make the code 10× faster on a workload. Speed is measurable, and LLMs can iterate on it relentlessly."
        }
        p {
            "But making sure the code is correct is hard. Correctness is implicit: you can never list every DON'T in the prompt, you can never write enough tests to prove the code correct, and you can never enumerate every property you want to hold."
        }
        p {
            Bbtree {}
            " has three guards to approach correctness."
        }
        PartTabList { tabs }
        SectionPanel { tabs, section: PostHeading::GuardHuman, GuardHuman {} }
        SectionPanel { tabs, section: PostHeading::GuardLlms, GuardLlms {} }
        SectionPanel { tabs, section: PostHeading::GuardTesting, GuardTesting {} }
    }
}

#[component]
fn GuardHuman() -> Element {
    rsx! {
        p {
            a { href: AUTHOR_PAGE, target: "_blank", rel: "noopener", "Xiangpeng" }
            " is a real human behind "
            Bbtree {}
            ", and the front line guarding the correctness of its code. All of the code is written by LLMs, but Xiangpeng steers every design decision, and is accountable for the code."
        }
        p {
            "He spent his PhD writing many, many B-trees, including one that runs in production at Microsoft (Bf-Tree"
            NoteRef { note: Note::BfTree }
            "). Choosing "
            Bbtree {}
            " means offloading the attention to Xiangpeng."
        }
    }
}

#[component]
fn GuardLlms() -> Element {
    rsx! {
        p {
            "No human can write better code than LLMs. All of the implementation is written by LLMs, and cross-checked by LLMs from different providers."
        }
    }
}

#[component]
fn GuardTesting() -> Element {
    rsx! {
        p { "Conventional approaches still apply here:" }
        ol {
            li {
                strong { "Fuzzing" }
                " runs random operations on "
                Bbtree {}
                " and std's "
                code { "BTreeMap" }
                " side by side, checks that every answer matches, and checks the whole tree's invariants after every operation: "
                CodeLink { path: "tree/main/bbtree/fuzz/fuzz_targets", "8 targets" }
                ", fuzzed weekly and before every release ("
                CodeLink { path: "blob/main/.github/workflows/fuzz.yml", "CI" }
                ")."
            }
            li {
                strong { "Verification" }
                " with Verus"
                NoteRef { note: Note::Verus }
                " proves the critical properties of the code, e.g., that the inline keys order the same way as the keys themselves: "
                CodeLink { path: "tree/main/bbtree/src/proof", "the proofs" }
                ", checked on every pull request ("
                CodeLink { path: "blob/main/.github/workflows/verus.yml", "CI" }
                ")."
            }
            li {
                strong { "DSTs" }
                " (deterministic simulation testing) with Shuttle"
                NoteRef { note: Note::Shuttle }
                " and ffs"
                NoteRef { note: Note::Ffs }
                " test concurrent behavior in a deterministic and principled way: Shuttle runs "
                CodeLink { path: "blob/main/bbtree/src/map/shuttle_tests.rs", "each test" }
                " under thousands of thread schedules on every pull request ("
                CodeLink { path: "blob/main/.github/workflows/shuttle.yml", "CI" }
                "); ffs, run by its author, found a race Shuttle had missed ("
                CodeLink { path: "issues/9", "issue #9" }
                "), now fixed and tested."
            }
        }
    }
}

/// A link to `path` in bbtree's repository: code, CI, or an issue.
#[component]
fn CodeLink(path: &'static str, children: Element) -> Element {
    rsx! {
        a {
            href: "{REPOSITORY}/{path}",
            target: "_blank",
            rel: "noopener",
            {children}
        }
    }
}
