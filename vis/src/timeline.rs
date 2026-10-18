//! A timeline the browser plays: time runs over a sequence of steps (a
//! batch's searches, a figure's moments), and a loop in the browser
//! advances it each frame and writes it to an element as `--t`, so that
//! what the element draws moves in CSS without a redraw. Rust hears only
//! when another step starts, and whether it plays.
//!
//! The loop plays while its element is on screen and holds while it isn't;
//! paused or stepped, time rests just after a step, which every drawing
//! shows as done; with reduced motion, it jumps from step to step. Its
//! element starts at `--t: -1`, nothing shown, until the loop runs
//! (`.timeline` in `main.css`).

use dioxus::prelude::*;

use crate::chart_scale::FrameWidth;
use crate::icons::{Icon, IconGlyph};

/// What controls ask a timeline's loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlaybackCommand {
    /// Play, or pause; at the end, play from the start.
    Toggle,
    /// Pause after the step before.
    Back,
    /// Pause after the next step.
    Forward,
}

/// The steps of a timeline and its pace.
#[derive(Clone, PartialEq)]
pub(crate) struct Timeline {
    /// Where each step starts.
    starts: Vec<f64>,
    total: f64,
    /// How long a unit of time lasts on screen.
    step_ms: f64,
    /// Time before this runs at `slow_ms` a unit, slow enough to follow.
    slow_until: f64,
    slow_ms: f64,
}

impl Timeline {
    /// Steps starting at `starts`, ending at `total`, `step_ms` a unit.
    pub(crate) fn new(starts: Vec<f64>, total: f64, step_ms: f64) -> Self {
        Self {
            starts,
            total,
            step_ms,
            slow_until: 0.0,
            slow_ms: step_ms,
        }
    }

    /// The same, its time before `until` at `slow_ms` a unit.
    pub(crate) fn with_slow_start(self, until: f64, slow_ms: f64) -> Self {
        Self {
            slow_until: until,
            slow_ms,
            ..self
        }
    }

    /// `steps` steps, one a unit.
    pub(crate) fn of_steps(steps: usize, step_ms: f64) -> Self {
        Self::new(
            (0..steps).map(|step| step as f64).collect(),
            steps as f64,
            step_ms,
        )
    }

    pub(crate) fn total(&self) -> f64 {
        self.total
    }

    pub(crate) fn steps(&self) -> usize {
        self.starts.len()
    }

    /// Plays from the start on the element with the id `element`, telling
    /// `now` each step that starts and `playing` whether it plays, until
    /// the element goes.
    pub(crate) async fn run(
        self,
        element: &str,
        now: Signal<Option<usize>>,
        playing: Signal<bool>,
    ) {
        let (mut now, mut playing) = (now, playing);
        let mut heard = document::eval(
            r#"
            const [element_id, starts, slow_until, total, step_ms, slow_ms] = await dioxus.recv();
            const element = document.getElementById(element_id);
            if (!element) return;
            const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
            // Just before a step ends: done.
            const DONE = 0.999;
            let t = -1, playing = true, last = null, hold = 0, told = null;
            // The time last drawn, and whether a frame is asked for: the
            // loop runs only while playing, and draws only what changed.
            let drawn = null, asked = false;
            // Off screen it holds where it is, and plays on once back.
            let visible = true;
            const wake = () => {
                if (!asked) {
                    asked = true;
                    requestAnimationFrame(frame);
                }
            };
            const stepAt = (time) => {
                let low = 0, high = starts.length;
                while (low < high) {
                    const mid = (low + high) >> 1;
                    if (starts[mid] <= time) low = mid + 1; else high = mid;
                }
                return low - 1;
            };
            const after = (index) => index < 0 ? -1 : Math.min(starts[Math.min(index, starts.length - 1)] + DONE, total);
            element.addEventListener('timeline', (event) => {
                const command = event.detail;
                const index = stepAt(t);
                if (command === 'toggle') {
                    if (t >= total) t = -1;
                    else if (playing) t = after(index);
                    playing = !playing;
                } else if (command === 'back') {
                    playing = false;
                    t = after(index - 1);
                } else if (command === 'forward') {
                    playing = false;
                    t = after(index + 1);
                } else if (typeof command === 'number') {
                    playing = false;
                    t = after(stepAt(command));
                }
                last = null;
                wake();
            });
            const frame = (time) => {
                asked = false;
                if (!element.isConnected) return;
                if (!visible) {
                    last = null;
                    return;
                }
                if (playing && last !== null) {
                    if (reduced) {
                        if (time >= hold) {
                            t = after(stepAt(t) + 1);
                            hold = time + step_ms * 4;
                        }
                    } else {
                        t = Math.min(total, t + (time - last) / (t < slow_until ? slow_ms : step_ms));
                    }
                }
                if (t >= total || (reduced && stepAt(t) === starts.length - 1)) playing = false;
                last = time;
                if (t !== drawn) {
                    drawn = t;
                    element.style.setProperty('--t', t.toFixed(3));
                }
                const index = stepAt(t);
                const state = index + ':' + playing;
                if (state !== told) {
                    told = state;
                    dioxus.send([index, playing]);
                }
                if (playing) wake();
            };
            new IntersectionObserver((entries) => {
                visible = entries.some((entry) => entry.isIntersecting);
                if (visible && playing) wake();
            }).observe(element);
            wake();
            "#,
        );
        let _ = heard.send((
            element,
            &self.starts,
            self.slow_until,
            self.total,
            self.step_ms,
            self.slow_ms,
        ));
        while let Ok((index, is_playing)) = heard.recv::<(i64, bool)>().await {
            let index = usize::try_from(index).ok();
            if *now.peek() != index {
                now.set(index);
            }
            if *playing.peek() != is_playing {
                playing.set(is_playing);
            }
        }
    }

    /// Asks the loop of the element with the id `element` to play, pause or
    /// step.
    pub(crate) fn send(element: &str, command: PlaybackCommand) {
        let detail = match command {
            PlaybackCommand::Toggle => "'toggle'",
            PlaybackCommand::Back => "'back'",
            PlaybackCommand::Forward => "'forward'",
        };
        Self::dispatch(element, detail);
    }

    /// Pauses the loop of the element with the id `element` just after the
    /// step running at `time`.
    pub(crate) fn seek(element: &str, time: f64) {
        Self::dispatch(element, &format!("{time:.3}"));
    }

    fn dispatch(element: &str, detail: &str) {
        document::eval(&format!(
            "document.getElementById('{element}')?.dispatchEvent(new CustomEvent('timeline', {{ detail: {detail} }}));"
        ));
    }
}

/// A timeline's controls: play, pause or play again, and a bar of its
/// time, filled to the moment (`--t` from the element around it), which a
/// click or the arrow keys move.
#[component]
pub(crate) fn TimelineControls(
    element: String,
    timeline: Timeline,
    now: Signal<Option<usize>>,
    playing: Signal<bool>,
    /// What the timeline plays, for the bar's name.
    label: String,
) -> Element {
    let steps = timeline.steps();
    let total = timeline.total();
    let at_end = now().is_some_and(|now| now + 1 == steps) && !playing();
    let bar = FrameWidth::use_new();
    let (toggle_at, bar_at, keys_at) = (element.clone(), element.clone(), element);
    rsx! {
        div { class: "timeline-controls",
            button {
                class: "icon-button",
                "aria-label": if playing() { "Pause" } else if at_end { "Play again" } else { "Play" },
                title: if playing() { "Pause" } else if at_end { "Play again" } else { "Play" },
                onclick: move |_| Timeline::send(&toggle_at, PlaybackCommand::Toggle),
                IconGlyph { icon: if playing() { Icon::Pause } else if at_end { Icon::Replay } else { Icon::Play } }
            }
            div {
                class: "timeline-bar",
                style: "--total:{total}",
                role: "slider",
                tabindex: "0",
                "aria-label": label,
                "aria-valuemin": "0",
                "aria-valuemax": "{steps}",
                "aria-valuenow": "{now().map_or(0, |now| now + 1)}",
                onresize: move |resized| bar.measure(resized),
                onclick: move |evt| {
                    let width = bar.get();
                    if width > 0.0 {
                        let share = (evt.element_coordinates().x / width).clamp(0.0, 1.0);
                        Timeline::seek(&bar_at, share * total);
                    }
                },
                onkeydown: move |evt| match evt.key() {
                    Key::ArrowLeft => Timeline::send(&keys_at, PlaybackCommand::Back),
                    Key::ArrowRight => Timeline::send(&keys_at, PlaybackCommand::Forward),
                    _ => {}
                },
                span { class: "timeline-fill" }
            }
        }
    }
}
