//! The page's few icons, drawn in one stroke weight on a 16-pixel grid, and
//! GitHub's mark.

use dioxus::prelude::*;

/// An icon the controls use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Icon {
    Left,
    Right,
    /// Picks something at random.
    Shuffle,
    /// Plays the lookup's way down again.
    Replay,
    Search,
    /// Opens a menu.
    Disclose,
    /// Leads to the code on GitHub: its mark, filled.
    GitHub,
}

impl Icon {
    /// The icon's strokes, as SVG path data.
    fn path(self) -> &'static str {
        match self {
            Icon::Left => "M10 4L6 8l4 4",
            Icon::Right => "M6 4l4 4-4 4",
            Icon::Shuffle => {
                "M2 4.5h2.5c3 0 4 7 7 7H14M12 9.5l2 2-2 2M2 11.5h2.5c1.2 0 2-1 2.6-2.2M9 6.7c.6-1.2 1.4-2.2 2.5-2.2H14M12 2.5l2 2-2 2"
            }
            Icon::Replay => "M3 8a5 5 0 1 0 1.5-3.6M3 2.5v3h3",
            Icon::Search => "M7 12A5 5 0 1 0 7 2a5 5 0 0 0 0 10zM10.6 10.6L14 14",
            Icon::Disclose => "M4.5 6.5L8 10l3.5-3.5",
            Icon::GitHub => {
                "M8 0c4.42 0 8 3.58 8 8a8.013 8.013 0 0 1-5.45 7.59c-.4.08-.55-.17-.55-.38 0-.27.01-1.13.01-2.2 0-.75-.25-1.23-.54-1.48 1.78-.2 3.65-.88 3.65-3.95 0-.88-.31-1.59-.82-2.15.08-.2.36-1.02-.08-2.12 0 0-.67-.22-2.2.82-.64-.18-1.32-.27-2-.27-.68 0-1.36.09-2 .27-1.53-1.03-2.2-.82-2.2-.82-.44 1.1-.16 1.92-.08 2.12-.51.56-.82 1.28-.82 2.15 0 3.06 1.86 3.75 3.64 3.95-.23.2-.44.55-.51 1.07-.46.21-1.61.55-2.33-.66-.15-.24-.6-.83-1.23-.82-.67.01-.27.38.01.53.34.19.73.9.82 1.13.16.45.68 1.31 2.69.94 0 .67.01 1.3.01 1.49 0 .21-.15.45-.55.38A7.995 7.995 0 0 1 0 8c0-4.42 3.58-8 8-8Z"
            }
        }
    }

    /// Whether the icon is a filled shape rather than strokes.
    fn is_filled(self) -> bool {
        self == Icon::GitHub
    }
}

/// `icon`, at the size of the text around it.
#[component]
pub(crate) fn IconGlyph(icon: Icon) -> Element {
    rsx! {
        svg {
            class: if icon.is_filled() { "icon filled" } else { "icon" },
            view_box: "0 0 16 16",
            "aria-hidden": "true",
            path { d: icon.path() }
        }
    }
}
