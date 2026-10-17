//! The post's figure of optimistic latch coupling (doc 14, Figure 5): one
//! node's version, a writer that changes the node, and two readers, a
//! column each, time running down. A reader whose check finds the version
//! it read keeps what it read; one that overlapped the change finds a new
//! version and starts over. Drawn plain: the protocol, not a run.

use std::ops::Range;

use dioxus::prelude::*;

use crate::how_it_works::Diagram;

/// What one column does to the node, from moment to moment.
struct Act {
    column: usize,
    moments: Range<usize>,
    text: &'static str,
    kind: ActKind,
}

#[derive(Clone, Copy)]
enum ActKind {
    Version,
    NewVersion,
    Write,
    Read,
    Kept,
    Retried,
}

impl ActKind {
    fn class(self) -> &'static str {
        match self {
            Self::Version => "act version",
            Self::NewVersion => "act version new",
            Self::Write => "act write",
            Self::Read => "act",
            Self::Kept => "act kept",
            Self::Retried => "act retried",
        }
    }
}

/// The figure's columns and what each does when.
struct VersionTimeline;

impl VersionTimeline {
    const COLUMNS: [&'static str; 4] = ["version", "writer", "reader 1", "reader 2"];
    const ACTS: [Act; 11] = [
        Act {
            column: 0,
            moments: 0..6,
            text: "7",
            kind: ActKind::Version,
        },
        Act {
            column: 0,
            moments: 6..7,
            text: "8",
            kind: ActKind::NewVersion,
        },
        Act {
            column: 1,
            moments: 3..4,
            text: "latch",
            kind: ActKind::Write,
        },
        Act {
            column: 1,
            moments: 4..5,
            text: "change",
            kind: ActKind::Write,
        },
        Act {
            column: 1,
            moments: 5..6,
            text: "release",
            kind: ActKind::Write,
        },
        Act {
            column: 2,
            moments: 0..1,
            text: "read v7",
            kind: ActKind::Read,
        },
        Act {
            column: 2,
            moments: 1..2,
            text: "read node",
            kind: ActKind::Read,
        },
        Act {
            column: 2,
            moments: 2..3,
            text: "check v7 ✓",
            kind: ActKind::Kept,
        },
        Act {
            column: 3,
            moments: 1..2,
            text: "read v7",
            kind: ActKind::Read,
        },
        Act {
            column: 3,
            moments: 2..6,
            text: "read node",
            kind: ActKind::Read,
        },
        Act {
            column: 3,
            moments: 6..7,
            text: "check v8 ✗",
            kind: ActKind::Retried,
        },
    ];
}

#[component]
pub(crate) fn VersionsFigure() -> Element {
    rsx! {
        Diagram {
            caption: rsx! {
                "Time runs down. Readers only read. Reader 1 finished before the writer changed anything, so its read stands. Reader 2 overlapped the change, finds version 8, and starts over."
            },
            div { class: "diagram",
                div { class: "versions",
                    for (column , name) in VersionTimeline::COLUMNS.iter().enumerate() {
                        span {
                            key: "head-{column}",
                            class: "versions-head",
                            style: "grid-column:{column + 1}",
                            {*name}
                        }
                    }
                    for (index , act) in VersionTimeline::ACTS.iter().enumerate() {
                        span {
                            key: "act-{index}",
                            class: act.kind.class(),
                            style: "grid-column:{act.column + 1};grid-row:{act.moments.start + 2} / {act.moments.end + 2}",
                            {act.text}
                        }
                    }
                }
            }
        }
    }
}
