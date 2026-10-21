//! The post's "How do we make it correct?" (design/14-blogpost.md): why
//! correctness is the hard part, and bbtree's three guards as tabs.

use dioxus::prelude::*;

use crate::how_it_works::{Note, NoteRef};
use crate::outline::{PostHeading, SectionHeading};
use crate::part_tabs::{PartTabList, PartTabs, SectionPanel};
use crate::post::Bbtree;
use crate::site::AUTHOR_PAGE;

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
                " side by side, checks that every answer matches, and checks the whole tree's invariants after every operation."
            }
            li {
                strong { "Verification" }
                " with Verus"
                NoteRef { note: Note::Verus }
                " proves the critical properties of the code, e.g., that the inline keys order the same way as the keys themselves."
            }
            li {
                strong { "DSTs" }
                " (deterministic simulation testing) with Shuttle"
                NoteRef { note: Note::Shuttle }
                " and ffs"
                NoteRef { note: Note::Ffs }
                " test concurrent behavior in a deterministic and principled way."
            }
        }
    }
}
