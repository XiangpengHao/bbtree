//! The post's figures: SVG files (`vis/figures/` writes them into
//! `assets/figures/`), each drawn into the page with its caption, in the
//! post's theme, playing its own CSS animation. A screen too narrow to
//! show a figure legibly shows it whole, shrunk, and a tap shows it at its
//! own size over the page, to pan or read with the phone turned.

use dioxus::prelude::*;

use crate::chart_scale::FrameWidth;
use crate::element_ids::ElementIds;
use crate::icons::{Icon, IconGlyph};
use crate::theme::PostTheme;

/// A figure and its caption.
#[component]
pub(crate) fn SvgFigure(
    src: Asset,
    alt: &'static str,
    caption: Element,
    /// Whether the figure plays in a loop, which the reader can pause.
    #[props(default)]
    loops: bool,
) -> Element {
    let mut paused = use_signal(|| false);
    let frame = FrameWidth::use_new();
    let mut enlarged = use_signal(|| false);
    // Shrunk to a phone's width, the figure's text is too small to read.
    let shrunk = frame.get() > 0.0 && FrameWidth::is_narrow(frame.get());
    let (label, icon) = if paused() {
        ("Play", Icon::Play)
    } else {
        ("Pause", Icon::Pause)
    };
    rsx! {
        figure { class: "figure",
            div {
                class: if shrunk { "figure-view shrunk" } else { "figure-view" },
                onresize: move |resized| frame.measure(resized),
                // The caption's button says so; the figure is the bigger
                // target.
                onclick: move |_| {
                    if shrunk {
                        enlarged.set(true);
                    }
                },
                SvgDrawing {
                    src,
                    alt,
                    class: "figure-svg",
                    paused: paused(),
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
                                onclick: move |_| paused.set(!paused()),
                                IconGlyph { icon }
                                {label}
                            }
                        }
                    }
                }
            }
        }
        if enlarged() {
            FigureOverlay { src, alt, onclose: move |_| enlarged.set(false) }
        }
    }
}

/// A figure at its own size over the page, which holds still beneath it:
/// the reader pans or zooms it, or turns the phone, and closes it.
#[component]
fn FigureOverlay(src: Asset, alt: &'static str, onclose: EventHandler) -> Element {
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
                SvgDrawing {
                    src,
                    alt,
                    class: "figure-overlay-svg",
                    paused: false,
                }
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

/// An SVG file, drawn into the page as the reader nears it, in the post's
/// theme: it plays its CSS animation from the start once the reader sees
/// it, and while `paused`, holds still where it is. An image couldn't be
/// paused but on its last frame, nor take the page's fonts; drawn in a
/// shadow root, the file's style stays its own.
#[component]
fn SvgDrawing(src: Asset, alt: &'static str, class: &'static str, paused: bool) -> Element {
    let theme = PostTheme::of_context();
    let id = ElementIds::of_current().id("drawing");
    let drawn = id.clone();
    rsx! {
        div {
            id,
            class,
            role: "img",
            "aria-label": alt,
            // The script follows these, as the drawing's palette and
            // whether it plays.
            "data-theme": theme.picked_slug(),
            "data-paused": paused.then_some("true"),
            onmounted: move |_| {
                let drawing = document::eval(
                    r#"
                    const [id, src] = await dioxus.recv();
                    const host = document.getElementById(id);
                    let drawing = null;
                    let seen = false;
                    const follow = () => {
                        const svg = drawing?.querySelector('svg');
                        if (!svg) return;
                        if (host.dataset.theme) svg.dataset.theme = host.dataset.theme;
                        else delete svg.dataset.theme;
                        const plays = seen && host.dataset.paused === undefined;
                        // A finished animation stays finished: play() would
                        // start it over.
                        for (const animation of drawing.getAnimations()) {
                            if (plays && animation.playState === 'paused') animation.play();
                            if (!plays && animation.playState === 'running') animation.pause();
                        }
                    };
                    new MutationObserver(follow).observe(host, { attributeFilter: ['data-theme', 'data-paused'] });
                    new IntersectionObserver(async ([entry], observer) => {
                        if (!entry.isIntersecting) return;
                        observer.disconnect();
                        const file = await (await fetch(src)).text();
                        drawing = host.attachShadow({ mode: 'open' });
                        drawing.innerHTML = `<style>svg { display: block; width: 100%; height: auto; }</style>${file}`;
                        drawing.querySelector('svg')?.setAttribute('aria-hidden', 'true');
                        follow();
                    }, { rootMargin: '100% 0px' }).observe(host);
                    new IntersectionObserver(([entry], observer) => {
                        if (!entry.isIntersecting) return;
                        observer.disconnect();
                        seen = true;
                        follow();
                    }, { threshold: 0.3 }).observe(host);
                    "#,
                );
                let _ = drawing.send((drawn.clone(), src.to_string()));
            },
        }
    }
}
