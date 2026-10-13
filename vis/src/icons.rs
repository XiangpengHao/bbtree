//! The page's few icons, drawn in one stroke weight on a 16-pixel grid.

use dioxus::prelude::*;

/// An icon the controls use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Icon {
    Up,
    Down,
    Left,
    Right,
    /// Picks something at random.
    Shuffle,
    /// Plays the lookup's way down again.
    Replay,
    Search,
    /// Opens a menu.
    Disclose,
    Close,
}

impl Icon {
    /// The icon's strokes, as SVG path data.
    fn path(self) -> &'static str {
        match self {
            Icon::Up => "M4 10l4-4 4 4",
            Icon::Down => "M4 6l4 4 4-4",
            Icon::Left => "M10 4L6 8l4 4",
            Icon::Right => "M6 4l4 4-4 4",
            Icon::Shuffle => {
                "M2 4.5h2.5c3 0 4 7 7 7H14M12 9.5l2 2-2 2M2 11.5h2.5c1.2 0 2-1 2.6-2.2M9 6.7c.6-1.2 1.4-2.2 2.5-2.2H14M12 2.5l2 2-2 2"
            }
            Icon::Replay => "M3 8a5 5 0 1 0 1.5-3.6M3 2.5v3h3",
            Icon::Search => "M7 12A5 5 0 1 0 7 2a5 5 0 0 0 0 10zM10.6 10.6L14 14",
            Icon::Disclose => "M4.5 6.5L8 10l3.5-3.5",
            Icon::Close => "M4 4l8 8M12 4l-8 8",
        }
    }
}

/// `icon`, at the size of the text around it.
#[component]
pub(crate) fn IconGlyph(icon: Icon) -> Element {
    rsx! {
        svg { class: "icon", view_box: "0 0 16 16", "aria-hidden": "true",
            path { d: icon.path() }
        }
    }
}
