//! A panel that opens from a toolbar button over the page: a click outside
//! it or Escape closes it, and Escape hands focus back to its button.

use dioxus::prelude::*;

/// The panel, while `open`, below its button: wrap both in a
/// `.menu` so it hangs from the button.
#[component]
pub(crate) fn Popover(
    open: Signal<bool>,
    /// What the panel is, for screen readers.
    label: &'static str,
    /// The id of the button that opens it.
    trigger: &'static str,
    /// Which edge of the button the panel lines up with.
    align_end: bool,
    children: Element,
) -> Element {
    let mut open = open;
    if !open() {
        return rsx! {};
    }
    rsx! {
        div { class: "popover-backdrop", onclick: move |_| open.set(false) }
        div {
            class: if align_end { "popover end" } else { "popover" },
            role: "dialog",
            "aria-label": label,
            onkeydown: move |evt| {
                if evt.key() == Key::Escape {
                    evt.stop_propagation();
                    open.set(false);
                    document::eval(&format!("document.getElementById('{trigger}')?.focus()"));
                }
            },
            {children}
        }
    }
}
