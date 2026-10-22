//! The post's outline: its headings below the cards, each with an id to
//! link to, and the outline that lists them beside the text, lighting the
//! section the reader is in. The headings and the outline read one list
//! (`PostHeading::ALL`), so they never disagree: the parts, each with its
//! sections, which the part shows as tabs (`part_tabs.rs`), and the
//! headings within them. On a narrow screen, the outline is a list of
//! contents above the sections instead.

use dioxus::prelude::*;

/// A heading of the post's sections, in the order they come.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PostHeading {
    UseCases,
    CaseMemtable,
    CaseMmap,
    CaseParquetIndex,
    HowItWorks,
    CacheMisses,
    ThoughtExperiment,
    ApplyToBTrees,
    CacheMissResults,
    BatchMode,
    Instructions,
    SearchAgain,
    SimdSearch,
    Compiler,
    InstructionResults,
    Coherence,
    ProblemReferences,
    ProblemReclamation,
    CoherenceResults,
    Correctness,
    GuardHuman,
    GuardLlms,
    GuardTesting,
    Reviewers,
}

impl PostHeading {
    const ALL: [Self; 24] = [
        Self::UseCases,
        Self::CaseMemtable,
        Self::CaseMmap,
        Self::CaseParquetIndex,
        Self::HowItWorks,
        Self::CacheMisses,
        Self::ThoughtExperiment,
        Self::ApplyToBTrees,
        Self::CacheMissResults,
        Self::BatchMode,
        Self::Instructions,
        Self::SearchAgain,
        Self::SimdSearch,
        Self::Compiler,
        Self::InstructionResults,
        Self::Coherence,
        Self::ProblemReferences,
        Self::ProblemReclamation,
        Self::CoherenceResults,
        Self::Correctness,
        Self::GuardHuman,
        Self::GuardLlms,
        Self::GuardTesting,
        Self::Reviewers,
    ];

    /// The heading's level, as doc 14 sets it: 2 for the post's parts, 3
    /// for their sections, 4 and 5 within a section.
    fn level(self) -> u8 {
        match self {
            Self::UseCases | Self::HowItWorks | Self::Correctness | Self::Reviewers => 2,
            Self::CaseMemtable
            | Self::CaseMmap
            | Self::CaseParquetIndex
            | Self::CacheMisses
            | Self::Instructions
            | Self::Coherence
            | Self::GuardHuman
            | Self::GuardLlms
            | Self::GuardTesting => 3,
            Self::ProblemReferences | Self::ProblemReclamation => 5,
            _ => 4,
        }
    }

    /// The anchor the heading takes, and that its link names.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::UseCases => "what-it-can-do",
            Self::CaseMemtable => "case-memtable",
            Self::CaseMmap => "case-bbtree-mmap",
            Self::CaseParquetIndex => "case-parquet-index",
            Self::HowItWorks => "how-it-works",
            Self::CacheMisses => "minimize-cache-misses",
            Self::ThoughtExperiment => "binary-vs-linear-search",
            Self::ApplyToBTrees => "apply-to-b-trees",
            Self::CacheMissResults => "cache-miss-results",
            Self::BatchMode => "batch-mode",
            Self::Instructions => "minimize-instructions",
            Self::SearchAgain => "binary-vs-linear-search-again",
            Self::SimdSearch => "how-bbtree-searches",
            Self::Compiler => "the-compiler",
            Self::InstructionResults => "instruction-results",
            Self::Coherence => "minimize-coherence-traffic",
            Self::ProblemReferences => "no-references",
            Self::ProblemReclamation => "memory-reclamation",
            Self::CoherenceResults => "coherence-results",
            Self::Correctness => "correctness",
            Self::GuardHuman => "guard-a-human",
            Self::GuardLlms => "guard-llms",
            Self::GuardTesting => "guard-testing",
            Self::Reviewers => "reviewers",
        }
    }

    /// The heading as plain text, as the outline and a section's tab show
    /// it.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::UseCases => "What it can do?",
            Self::CaseMemtable => "Case 1: faster memtable",
            Self::CaseMmap => "Case 2: bbtree-mmap",
            Self::CaseParquetIndex => "Case 3: Parquet index",
            Self::HowItWorks => "How it works?",
            Self::CacheMisses => "Minimize cache misses",
            Self::ThoughtExperiment => "Thought experiment: binary search vs linear search",
            Self::ApplyToBTrees => "How does that apply to B-trees?",
            Self::CacheMissResults | Self::InstructionResults | Self::CoherenceResults => "Results",
            Self::BatchMode => "Bonus: batch mode using Rust's async state machines",
            Self::Instructions => "Minimize instructions",
            Self::SearchAgain => "Binary search vs linear search, again",
            Self::SimdSearch => "How does bbtree search?",
            Self::Compiler => "The compiler is not your friend",
            Self::Coherence => "Minimize cache coherence traffic",
            Self::ProblemReferences => "Problem 1: we can no longer hand out a &V to the user",
            Self::ProblemReclamation => "Problem 2: memory reclamation",
            Self::Correctness => "How do we make it correct?",
            Self::GuardHuman => "Guard 1: Xiangpeng Hao",
            Self::GuardLlms => "Guard 2: LLMs",
            Self::GuardTesting => "Guard 3: fuzzing, verification, DSTs",
            Self::Reviewers => "Reviewers of this post",
        }
    }

    /// The principle's place in the order the post ranks them, which its
    /// tab shows.
    pub(crate) fn principle(self) -> Option<u8> {
        match self {
            Self::CacheMisses => Some(1),
            Self::Instructions => Some(2),
            Self::Coherence => Some(3),
            _ => None,
        }
    }

    /// The heading as the text shows it: with its code.
    fn rich_title(self) -> Element {
        match self {
            Self::SimdSearch => rsx! {
                "How does "
                code { "bbtree" }
                " search?"
            },
            Self::ProblemReferences => rsx! {
                "Problem 1: we can no longer hand out a "
                code { "&V" }
                " to the user"
            },
            _ => rsx! {
                {self.title()}
            },
        }
    }

    /// The section a heading of level 4 or 5 belongs to: a section's or a
    /// part's own heading for itself.
    pub(crate) fn section(self) -> Self {
        self.within(3)
    }

    /// The part a heading belongs to: a part's own heading for itself.
    pub(crate) fn part(self) -> Self {
        self.within(2)
    }

    /// The part's sections, in order: its tabs.
    pub(crate) fn sections(self) -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(move |heading| heading.level() == 3 && heading.part() == self)
    }

    /// The heading at `level` or above that this one comes under: the
    /// nearest such heading at or before it.
    fn within(self, level: u8) -> Self {
        let index = Self::ALL
            .iter()
            .position(|heading| *heading == self)
            .unwrap_or(0);
        Self::ALL[..=index]
            .iter()
            .rev()
            .find(|heading| heading.level() <= level)
            .copied()
            .unwrap_or(self)
    }

    pub(crate) fn of_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|heading| heading.id() == id)
    }
}

/// A part's heading, or one within a section, with its anchor. A
/// section's title is its tab (`part_tabs.rs`), so the headings within a
/// section are the page's h3 and h4.
#[component]
pub(crate) fn SectionHeading(heading: PostHeading) -> Element {
    let id = heading.id();
    let title = heading.rich_title();
    match heading.level() {
        2 => rsx! {
            h2 { id, class: "post-section", "data-heading": id, {title} }
        },
        4 => rsx! {
            h3 { id, class: "post-minor", "data-heading": id, {title} }
        },
        _ => rsx! {
            h4 { id, class: "post-detail", "data-heading": id, {title} }
        },
    }
}

/// The outline beside the text on a wide screen, and the list of contents
/// above the sections on a narrow one: every part and section, and the
/// headings within the section the reader is in.
#[component]
pub(crate) fn PostOutline() -> Element {
    let mut current = use_signal(|| None::<PostHeading>);
    use_hook(move || {
        spawn(async move {
            // The heading last scrolled past a third of the way down the
            // screen, told when it changes.
            let mut heard = document::eval(
                r#"
                let told = null, asked = false;
                const look = () => {
                    asked = false;
                    const line = innerHeight / 3;
                    let here = null;
                    for (const heading of document.querySelectorAll('[data-heading]')) {
                        if (heading.getBoundingClientRect().top <= line) here = heading.dataset.heading;
                        else break;
                    }
                    if (here !== told) {
                        told = here;
                        dioxus.send(here ?? '');
                    }
                };
                addEventListener('scroll', () => {
                    if (!asked) {
                        asked = true;
                        requestAnimationFrame(look);
                    }
                }, { passive: true });
                look();
                "#,
            );
            while let Ok(id) = heard.recv::<String>().await {
                current.set(PostHeading::of_id(&id));
            }
        });
    });
    let section = current().map(PostHeading::section);
    let entries = PostHeading::ALL
        .into_iter()
        .filter(|heading| heading.level() <= 3 || Some(heading.section()) == section);
    rsx! {
        aside { class: "post-outline",
            nav { class: "post-outline-nav", "aria-label": "On this page",
                p { class: "post-outline-title", "On this page" }
                ol {
                    for heading in entries {
                        li { key: "{heading.id()}", class: "outline-level-{heading.level()}",
                            a {
                                href: "#{heading.id()}",
                                "aria-current": if current() == Some(heading) { "location" } else { "false" },
                                if let Some(number) = heading.principle() {
                                    span { class: "outline-number", "{number}" }
                                }
                                {heading.title()}
                            }
                        }
                    }
                }
            }
        }
        details { class: "post-contents",
            summary { "Contents" }
            ol {
                for heading in PostHeading::ALL.into_iter().filter(|heading| heading.level() <= 3) {
                    li { key: "{heading.id()}", class: "outline-level-{heading.level()}",
                        a { href: "#{heading.id()}", {heading.title()} }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_have_unique_ids() {
        for (index, heading) in PostHeading::ALL.iter().enumerate() {
            assert!(
                PostHeading::ALL[index + 1..]
                    .iter()
                    .all(|other| other.id() != heading.id()),
                "{heading:?}'s id twice"
            );
            assert_eq!(PostHeading::of_id(heading.id()), Some(*heading));
        }
    }

    #[test]
    fn a_part_of_a_section_lights_its_section() {
        assert_eq!(
            PostHeading::ProblemReclamation.section(),
            PostHeading::Coherence
        );
        assert_eq!(
            PostHeading::ThoughtExperiment.section(),
            PostHeading::CacheMisses
        );
        assert_eq!(PostHeading::Coherence.section(), PostHeading::Coherence);
        assert_eq!(
            PostHeading::GuardTesting.section(),
            PostHeading::GuardTesting
        );
        assert_eq!(PostHeading::HowItWorks.section(), PostHeading::HowItWorks);
    }

    #[test]
    fn a_part_has_its_sections() {
        assert_eq!(
            PostHeading::UseCases.sections().collect::<Vec<_>>(),
            [
                PostHeading::CaseMemtable,
                PostHeading::CaseMmap,
                PostHeading::CaseParquetIndex
            ]
        );
        assert_eq!(
            PostHeading::HowItWorks.sections().collect::<Vec<_>>(),
            [
                PostHeading::CacheMisses,
                PostHeading::Instructions,
                PostHeading::Coherence
            ]
        );
        assert_eq!(
            PostHeading::Correctness.sections().collect::<Vec<_>>(),
            [
                PostHeading::GuardHuman,
                PostHeading::GuardLlms,
                PostHeading::GuardTesting
            ]
        );
        assert_eq!(PostHeading::Reviewers.sections().count(), 0);
        assert_eq!(
            PostHeading::ProblemReclamation.part(),
            PostHeading::HowItWorks
        );
    }
}
