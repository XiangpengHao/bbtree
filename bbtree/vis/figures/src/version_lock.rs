//! The figure of the version lock: one node's version, a writer that
//! changes the node, and two readers, a column each, time running down
//! under a playhead. A reader whose check finds the version it read keeps
//! what it read; one that overlapped the change finds a new version and
//! starts over. The protocol, not a run.

use std::ops::Range;

use crate::svg::{Element, Motion, Svg};

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
            Self::Read => "act read",
            Self::Kept => "act kept",
            Self::Retried => "act retried",
        }
    }

    /// The check's outcome, drawn after its text.
    fn mark(self) -> &'static str {
        match self {
            Self::Kept => " ✓",
            Self::Retried => " ✗",
            _ => "",
        }
    }
}

/// The figure's columns and what each does when.
pub(crate) struct VersionLock;

impl VersionLock {
    const COLUMNS: [&'static str; 4] = ["version", "writer", "reader 1", "reader 2"];
    const MOMENTS: usize = 7;
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
            text: "check v7",
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
            text: "check v8",
            kind: ActKind::Retried,
        },
    ];
    const LEFT: f64 = 60.0;
    const COLUMN: f64 = 150.0;
    const TOP: f64 = 36.0;
    const MOMENT: f64 = 34.0;

    fn column_x(column: usize) -> f64 {
        Self::LEFT + column as f64 * Self::COLUMN
    }

    pub(crate) fn svg() -> Svg {
        let bottom = Self::TOP + Self::MOMENTS as f64 * Self::MOMENT;
        let mut svg = Svg::new(
            720.0,
            bottom + 16.0,
            "A node's version, a writer and two readers, time running down",
        );
        let mut motion = Motion::looping(Self::MOMENTS as f64, 0.8, 3.5);
        svg.add(Element::text(14.0, Self::TOP + 14.0, "time").class("label"));
        svg.add(
            Element::path(&format!("M30 {} V{}", Self::TOP + 24.0, bottom - 6.0)).class("arrow"),
        );
        svg.add(Element::path(&format!("M26 {} l4 8 l4 -8", bottom - 12.0)).class("arrow"));
        for (column, name) in Self::COLUMNS.iter().enumerate() {
            let middle = Self::column_x(column) + (Self::COLUMN - 12.0) / 2.0;
            svg.add(Element::text(middle, 22.0, name).class("label middle"));
            svg.add(Element::line(middle, Self::TOP, middle, bottom).class("lifeline"));
        }
        for (index, act) in Self::ACTS.iter().enumerate() {
            let class = format!("act-{index}");
            motion.appear(&class, act.moments.start as f64);
            let x = Self::column_x(act.column);
            let y = Self::TOP + act.moments.start as f64 * Self::MOMENT + 2.0;
            let height = act.moments.len() as f64 * Self::MOMENT - 4.0;
            svg.add(
                Element::group()
                    .class(&format!("{} {class}", act.kind.class()))
                    .child(Element::rect(x, y, Self::COLUMN - 12.0, height).attr("rx", 4))
                    .child(
                        Element::text(
                            x + (Self::COLUMN - 12.0) / 2.0,
                            y + height / 2.0 + 4.5,
                            &format!("{}{}", act.text, act.kind.mark()),
                        )
                        .class("data middle"),
                    ),
            );
        }
        let distance = Self::MOMENTS as f64 * Self::MOMENT;
        motion.animate(
            "playhead",
            &[
                (0.0, "transform: translateY(0px)".into()),
                (
                    Self::MOMENTS as f64,
                    format!("transform: translateY({distance}px)"),
                ),
            ],
        );
        svg.add(
            Element::line(
                Self::LEFT - 8.0,
                Self::TOP,
                Self::column_x(4) - 4.0,
                Self::TOP,
            )
            .class("playhead-line playhead"),
        );
        svg.style(
            ".lifeline { stroke: var(--rule-strong); stroke-dasharray: 3 3; } \
             .arrow { stroke: var(--muted); fill: none; stroke-width: 1.2; } \
             .act rect { fill: var(--panel); stroke: var(--rule-strong); } \
             .version rect { fill: var(--well); stroke: none; } \
             .version.new rect { fill: var(--marker-soft); } \
             .write rect { fill: var(--marker-soft); stroke: var(--marker-strong); } \
             .kept text { fill: var(--ok); font-weight: 600; } \
             .retried rect { fill: var(--error-soft); stroke: var(--error); } \
             .retried text { fill: var(--error); font-weight: 600; } \
             .playhead-line { stroke: var(--link); stroke-width: 1.5; }",
        );
        svg.style(motion.css());
        svg
    }
}
