//! A figure as an SVG document: elements, the site's palette and type,
//! and the motion of a figure that plays, all in the one file, so that the
//! same file shows on the site and in any other page (an `<img>` runs no
//! script and loads no stylesheet, but plays the CSS animations inside).

use std::fmt::{Display, Write};
use std::fs;
use std::io;
use std::path::Path;

/// One element and what it holds.
pub(crate) struct Element {
    name: &'static str,
    attributes: Vec<(&'static str, String)>,
    children: String,
}

impl Element {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            attributes: Vec::new(),
            children: String::new(),
        }
    }

    pub(crate) fn rect(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self::new("rect")
            .attr("x", x)
            .attr("y", y)
            .attr("width", width.max(0.0))
            .attr("height", height.max(0.0))
    }

    pub(crate) fn line(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self::new("line")
            .attr("x1", x1)
            .attr("y1", y1)
            .attr("x2", x2)
            .attr("y2", y2)
    }

    pub(crate) fn path(d: &str) -> Self {
        Self::new("path").attr("d", d)
    }

    /// Text whose baseline starts at `(x, y)`.
    pub(crate) fn text(x: f64, y: f64, text: &str) -> Self {
        Self::new("text").attr("x", x).attr("y", y).content(text)
    }

    /// Monospace text stretched to exactly `chars` columns of `column`
    /// pixels, so that it lines up with marks drawn per byte whatever
    /// monospace font the reader's machine has.
    pub(crate) fn columns(x: f64, y: f64, text: &str, column: f64) -> Self {
        let chars = text.chars().count() as f64;
        Self::text(x, y, text)
            .class("data")
            .attr("textLength", chars * column)
            .attr("lengthAdjust", "spacingAndGlyphs")
    }

    pub(crate) fn group() -> Self {
        Self::new("g")
    }

    pub(crate) fn attr(mut self, name: &'static str, value: impl Display) -> Self {
        self.attributes.push((name, value.to_string()));
        self
    }

    /// Adds `class` to the element's classes.
    pub(crate) fn class(mut self, class: &str) -> Self {
        match self
            .attributes
            .iter_mut()
            .find(|(name, _)| *name == "class")
        {
            Some((_, classes)) => {
                classes.push(' ');
                classes.push_str(class);
            }
            None => self.attributes.push(("class", class.to_string())),
        }
        self
    }

    pub(crate) fn content(mut self, text: &str) -> Self {
        self.children.push_str(&Self::escaped(text));
        self
    }

    pub(crate) fn child(mut self, child: Element) -> Self {
        self.children.push_str(&child.render());
        self
    }

    fn render(&self) -> String {
        let mut attributes = String::new();
        for (name, value) in &self.attributes {
            let _ = write!(attributes, " {name}=\"{}\"", Self::escaped(value));
        }
        if self.children.is_empty() {
            format!("<{}{attributes}/>", self.name)
        } else {
            format!(
                "<{}{attributes}>{}</{}>",
                self.name, self.children, self.name
            )
        }
    }

    fn escaped(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
}

/// A figure's SVG document: its size, what it says for screen readers,
/// its own styles over the shared ones, and its elements.
pub(crate) struct Svg {
    width: f64,
    height: f64,
    title: String,
    style: String,
    body: String,
}

impl Svg {
    /// The site's palette, as `main.css` names it, light and dark.
    const LIGHT: &'static str = "--paper: #f6f7f9; --panel: #ffffff; --ink: #15171c; \
        --graphite: #5a606b; --muted: #636974; --rule: #e2e5ea; --rule-strong: #c6ccd5; \
        --well: #eef0f3; --ok: #11795a; --error: #b42318; --error-soft: #fdf0ee; \
        --link: #2266c8; --marker: #ffe24a; --marker-strong: #a67c00; --marker-soft: #fff4b8; \
        --marker-ink: #7a5a00; --compare: #f7a47f; --compare-soft: #fde3d6; \
        --slot-filled: #7f8999; --slot-smaller: #4c5462; --slot-larger: #bcc3cd; \
        --theirs-1: #353b47; --theirs-2: #59616e; --theirs-3: #6e7682; --theirs-4: #878e99;";
    const DARK: &'static str = "--paper: #1b1e24; --panel: #131519; --ink: #e6e8ec; \
        --graphite: #b4bac3; --muted: #9aa1ab; --rule: #2a2e35; --rule-strong: #3d434c; \
        --well: #23262c; --ok: #4cc38a; --error: #ff8a80; --error-soft: #3b1f1d; \
        --link: #8cb4ff; --marker: #ffe24a; --marker-strong: #e0b628; --marker-soft: #4a3f14; \
        --marker-ink: #f3d55b; --compare: #f7a47f; --compare-soft: #4a2a1e; \
        --slot-filled: #6b7482; --slot-smaller: #c9cfd8; --slot-larger: #3d434c; \
        --theirs-1: #d3d7de; --theirs-2: #aeb4be; --theirs-3: #8e959f; --theirs-4: #717884;";

    /// The ids of the groups that wrap a figure, which its address can
    /// name to ask for a theme, motion, or both (`figure.svg#dark-still`):
    /// the site's own switches, which an image can't otherwise hear.
    const STATES: [&'static str; 5] = ["still", "light-still", "dark-still", "light", "dark"];

    /// What every figure's text and marks share, over the palette: light,
    /// dark where the reader's system is, and either where the address
    /// asks for it. A still figure, as one with reduced motion, rests on
    /// its last frame, which is what its elements draw without their
    /// animations.
    fn shared_style() -> String {
        let (light, dark) = (Self::LIGHT, Self::DARK);
        format!(
            r#"
svg {{ {light} --on-marker: #15171c;
  font-family: "IBM Plex Sans", system-ui, -apple-system, "Segoe UI", sans-serif;
  font-size: 13px;
}}
@media (prefers-color-scheme: dark) {{ svg {{ {dark} }} }}
#light:target, #light-still:target {{ {light} }}
#dark:target, #dark-still:target {{ {dark} }}
:is(#still, #light-still, #dark-still):target * {{ animation: none !important; }}
.background {{ fill: var(--panel); }}
text {{ fill: var(--ink); }}
.label {{ fill: var(--muted); font-size: 12px; }}
.note {{ fill: var(--graphite); }}
.strong {{ font-weight: 600; }}
.data {{
  font-family: "IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-variant-numeric: tabular-nums;
  font-size: 13.5px;
}}
text.on-marker {{ fill: var(--on-marker); }}
.end {{ text-anchor: end; }}
.middle {{ text-anchor: middle; }}
@media (prefers-reduced-motion: reduce) {{
  * {{ animation: none !important; }}
}}
"#
        )
    }

    pub(crate) fn new(width: f64, height: f64, title: &str) -> Self {
        Self {
            width,
            height,
            title: title.to_string(),
            style: String::new(),
            body: String::new(),
        }
    }

    pub(crate) fn height(&self) -> f64 {
        self.height
    }

    pub(crate) fn style(&mut self, css: &str) {
        self.style.push_str(css);
    }

    pub(crate) fn add(&mut self, element: Element) {
        self.body.push_str(&element.render());
    }

    pub(crate) fn write(&self, path: &Path) -> io::Result<()> {
        let (width, height) = (self.width, self.height);
        let title = Element::new("title").content(&self.title).render();
        let document = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\" \
             width=\"{width}\" height=\"{height}\" role=\"img\">{title}<style>{}{}</style>\
             {}<rect class=\"background\" width=\"{width}\" height=\"{height}\"/>{}{}</svg>\n",
            Self::shared_style(),
            self.style,
            Self::STATES.map(|id| format!("<g id=\"{id}\">")).concat(),
            self.body,
            "</g>".repeat(Self::STATES.len()),
        );
        fs::write(path, document)
    }
}

/// How a figure plays: steps of `step` seconds each, then a hold on the
/// last frame, then again from the start. Every animated mark gets
/// keyframes over the whole loop, so that all marks stay in step loop after
/// loop; a mark's own style, without its animation, is its look on the
/// last frame.
pub(crate) struct Motion {
    steps: f64,
    step: f64,
    hold: f64,
    /// Played once, the figure keeps its last frame instead of looping.
    once: bool,
    css: String,
}

impl Motion {
    /// The moments before the first step, the figure at its start.
    const LEAD: f64 = 0.6;
    /// How long a mark takes to fade in or out, in steps.
    const FADE: f64 = 0.15;

    pub(crate) fn looping(steps: f64, step: f64, hold: f64) -> Self {
        Self {
            steps,
            step,
            hold,
            once: false,
            css: String::new(),
        }
    }

    pub(crate) fn once(steps: f64, step: f64) -> Self {
        Self {
            steps,
            step,
            hold: 0.0,
            once: true,
            css: String::new(),
        }
    }

    fn seconds(&self) -> f64 {
        Self::LEAD + self.steps * self.step + self.hold
    }

    /// Where time `at`, counted in steps, falls in the loop, in percent.
    fn percent(&self, at: f64) -> f64 {
        ((Self::LEAD + at * self.step) / self.seconds() * 100.0).clamp(0.0, 100.0)
    }

    /// Adds the animation `name` to the marks of that class: each frame a
    /// time in steps and the declarations that hold then. At the loop's
    /// end, the first frame comes back over the last fade.
    pub(crate) fn animate(&mut self, name: &str, frames: &[(f64, String)]) {
        let mut keyframes = String::new();
        for (at, declarations) in frames {
            let _ = write!(keyframes, "{:.3}% {{ {declarations} }} ", self.percent(*at));
        }
        if let (false, Some((_, first)), Some((_, last))) =
            (self.once, frames.first(), frames.last())
        {
            let fade_out = 100.0 - Self::FADE * self.step / self.seconds() * 100.0;
            let _ = write!(keyframes, "{fade_out:.3}% {{ {last} }} 100% {{ {first} }}");
        }
        let seconds = self.seconds();
        let count = if self.once { "1 both" } else { "infinite" };
        let last = frames.last().map_or("", |(_, last)| last.as_str());
        let _ = writeln!(
            self.css,
            "@keyframes {name} {{ {keyframes}}} .{name} {{ {last}; animation: {name} {seconds:.2}s linear {count}; }}"
        );
    }

    /// A mark that appears at step `at`, and stays.
    pub(crate) fn appear(&mut self, name: &str, at: f64) {
        self.animate(
            name,
            &[
                (0.0, "opacity: 0".into()),
                (at, "opacity: 0".into()),
                (at + Self::FADE, "opacity: 1".into()),
            ],
        );
    }

    /// A mark shown only from step `from` until step `until`.
    pub(crate) fn between(&mut self, name: &str, from: f64, until: f64) {
        self.animate(
            name,
            &[
                (0.0, "opacity: 0".into()),
                (from, "opacity: 0".into()),
                (from + Self::FADE, "opacity: 1".into()),
                (until, "opacity: 1".into()),
                (until + Self::FADE, "opacity: 0".into()),
            ],
        );
    }

    /// A mark that is there until step `at`, then fades.
    pub(crate) fn vanish(&mut self, name: &str, at: f64) {
        self.animate(
            name,
            &[
                (0.0, "opacity: 1".into()),
                (at, "opacity: 1".into()),
                (at + Self::FADE, "opacity: 0".into()),
            ],
        );
    }

    /// The animations, for the figure's style.
    pub(crate) fn css(&self) -> &str {
        &self.css
    }
}
