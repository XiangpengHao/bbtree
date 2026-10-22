//! The post's figures: SVG files (`vis/figures/` writes them into
//! `assets/figures/`), each embedded as an image, in the post's theme,
//! each playing its own CSS animation, with its caption. A screen too
//! narrow to show a figure legibly shows it whole, shrunk, and a tap shows
//! it at its own size over the page, to pan or read with the phone turned.

use dioxus::prelude::*;

use crate::chart_scale::FrameWidth;
use crate::icons::{Icon, IconGlyph};
use crate::theme::PostTheme;

/// A figure: an SVG file, played by its own CSS as its image loads, which
/// it does as the reader nears it, in the post's theme, and its caption.
#[component]
pub(crate) fn SvgFigure(
    src: Asset,
    alt: &'static str,
    caption: Element,
    /// Whether the figure plays in a loop, which the reader can pause.
    #[props(default)]
    loops: bool,
) -> Element {
    // The figure's palette is the page's, and it rests when the reader
    // pauses it: an image can't see the page's CSS or hear its buttons, so
    // its address says both.
    let theme = PostTheme::of_context();
    let mut still = use_signal(|| false);
    let frame = FrameWidth::use_new();
    let mut enlarged = use_signal(|| false);
    let address = format!("{src}{}", theme.figure_fragment(still()));
    let overlay_address = address.clone();
    // Shrunk to a phone's width, the figure's text is too small to read.
    let shrunk = frame.get() > 0.0 && FrameWidth::is_narrow(frame.get());
    let (label, icon) = if still() {
        ("Play", Icon::Play)
    } else {
        ("Pause", Icon::Pause)
    };
    rsx! {
        figure { class: "figure",
            div {
                class: if shrunk { "figure-view shrunk" } else { "figure-view" },
                onresize: move |resized| frame.measure(resized),
                img {
                    class: "figure-svg",
                    src: address,
                    alt,
                    loading: "lazy",
                    decoding: "async",
                    // The caption's button says so; the image is the
                    // bigger target.
                    onclick: move |_| {
                        if shrunk {
                            enlarged.set(true);
                        }
                    },
                }
            }
            figcaption { class: "figure-caption",
                div { class: "figure-text", {caption} }
                if shrunk || loops {
                    div { class: "figure-actions",
                        if shrunk {
                            button {
                                class: "figure-action",
                                r#type: "button",
                                "aria-label": "Enlarge the figure",
                                onclick: move |_| enlarged.set(true),
                                IconGlyph { icon: Icon::Enlarge }
                                "Enlarge"
                            }
                        }
                        if loops {
                            button {
                                class: "figure-action",
                                r#type: "button",
                                "aria-label": "{label} the figure's animation",
                                onclick: move |_| still.set(!still()),
                                IconGlyph { icon }
                                {label}
                            }
                        }
                    }
                }
            }
        }
        if enlarged() {
            FigureOverlay { address: overlay_address, alt, onclose: move |_| enlarged.set(false) }
        }
    }
}

/// A figure at its own size over the page, which holds still beneath it:
/// the reader pans or zooms it, or turns the phone, and closes it.
#[component]
fn FigureOverlay(address: String, alt: &'static str, onclose: EventHandler) -> Element {
    rsx! {
        div {
            class: "figure-overlay",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": alt,
            onkeydown: move |evt| {
                if evt.key() == Key::Escape {
                    onclose.call(());
                }
            },
            div { class: "figure-overlay-view",
                img { class: "figure-overlay-svg", src: address, alt }
            }
            button {
                class: "figure-overlay-close",
                r#type: "button",
                "aria-label": "Close the figure",
                onmounted: move |mounted| async move {
                    let _ = mounted.set_focus(true).await;
                },
                onclick: move |_| onclose.call(()),
                IconGlyph { icon: Icon::Close }
            }
        }
    }
}
