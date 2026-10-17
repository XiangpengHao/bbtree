//! Where a value goes on a chart's axis, and the clean numbers its ticks
//! show: a linear scale from zero, for times, throughput and bytes; a
//! duration as its axis reads it; and a smooth line through a chart's
//! points that never overshoots them; and the width a chart is drawn at.

use dioxus::prelude::*;

/// A chart's width in CSS pixels, measured from the frame it fills: a chart
/// drawn at it keeps its text's size on any screen, and lays itself out
/// for the room it has instead of shrinking or scrolling sideways.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct FrameWidth(Signal<f64>);

impl FrameWidth {
    /// Below this width, a chart takes its narrow layout.
    const NARROW: f64 = 560.0;

    pub(crate) fn use_new() -> Self {
        Self(use_signal(|| 0.0))
    }

    /// The width, zero before the frame is first measured.
    pub(crate) fn get(self) -> f64 {
        (self.0)()
    }

    pub(crate) fn is_narrow(width: f64) -> bool {
        width < Self::NARROW
    }

    /// The frame's `onresize`.
    pub(crate) fn measure(mut self, resized: Event<ResizeData>) {
        if let Ok(size) = resized.get_content_box_size()
            && size.width != *self.0.peek()
        {
            self.0.set(size.width);
        }
    }
}

/// A duration in nanoseconds, shown in nanoseconds, microseconds or
/// milliseconds, to three digits at most.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Nanos(pub(crate) f64);

impl std::fmt::Display for Nanos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (value, unit) = if self.0 < 1e3 {
            (self.0, "ns")
        } else if self.0 < 1e6 {
            (self.0 / 1e3, "µs")
        } else {
            (self.0 / 1e6, "ms")
        };
        if value < 10.0 && unit != "ns" {
            write!(f, "{value:.1} {unit}")
        } else {
            write!(f, "{value:.0} {unit}")
        }
    }
}

/// Values to positions, linearly, from zero to a clean number.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LinearScale {
    high: f64,
    step: f64,
    span: (f64, f64),
}

impl LinearScale {
    /// The scale from zero to the clean number at or above `greatest`,
    /// over `span`, with four or five ticks.
    pub(crate) fn fitting(greatest: f64, span: (f64, f64)) -> Self {
        let greatest = if greatest > 0.0 { greatest } else { 1.0 };
        let raw = greatest / 4.0;
        let magnitude = 10f64.powi(raw.log10().floor() as i32);
        let step = [1.0, 2.0, 2.5, 5.0, 10.0]
            .map(|unit| unit * magnitude)
            .into_iter()
            .find(|step| *step >= raw)
            .unwrap_or(10.0 * magnitude);
        Self {
            high: (greatest / step).ceil() * step,
            step,
            span,
        }
    }

    /// The scale from zero to `greatest` itself, over `span`, for bars
    /// whose longest ends at the span's end: ticks at the 1-2-5 numbers
    /// within, three to five of them.
    pub(crate) fn reaching(greatest: f64, span: (f64, f64)) -> Self {
        let greatest = if greatest > 0.0 { greatest } else { 1.0 };
        let raw = greatest / 4.0;
        let magnitude = 10f64.powi(raw.log10().floor() as i32);
        let step = [1.0, 2.0, 5.0, 10.0]
            .map(|unit| unit * magnitude)
            .into_iter()
            .find(|step| *step >= raw)
            .unwrap_or(10.0 * magnitude);
        Self {
            high: greatest,
            step,
            span,
        }
    }

    pub(crate) fn at(&self, value: f64) -> f64 {
        self.span.0 + value / self.high * (self.span.1 - self.span.0)
    }

    /// Every step from zero to the scale's top, or every second, third,
    /// …, whichever keeps their places at least `gap` apart.
    pub(crate) fn ticks_apart(&self, gap: f64) -> Vec<f64> {
        let ticks = self.ticks();
        let spacing = (self.at(self.step) - self.at(0.0)).abs();
        let every = (gap / spacing).ceil().max(1.0) as usize;
        ticks.into_iter().step_by(every).collect()
    }

    /// Every step from zero to the scale's top.
    pub(crate) fn ticks(&self) -> Vec<f64> {
        let count = (self.high / self.step + 1e-9).floor() as usize;
        (0..=count).map(|index| index as f64 * self.step).collect()
    }

    /// How long a bar to `value` takes to grow from zero, in milliseconds:
    /// every bar on the scale at one speed, so that the shorter bar, the
    /// faster time, lands first.
    pub(crate) fn growth_ms(&self, value: f64) -> f64 {
        const WHOLE_SCALE: f64 = 800.0;
        (value / self.high * WHOLE_SCALE).clamp(120.0, WHOLE_SCALE)
    }

    /// The SVG path of a column from zero up to `value`, `width` wide from
    /// `left`: square at the baseline, its top rounded by 4 units, or less
    /// for a column too short.
    pub(crate) fn column_path(&self, left: f64, width: f64, value: f64) -> String {
        self.column_between(left, width, self.at(0.0), self.at(value))
    }

    /// The SVG path of a column up to `value` inside the column from `left`
    /// `width` wide: `inset` in from its sides and its bottom, so that the
    /// outer one shows around it.
    pub(crate) fn inset_column_path(
        &self,
        left: f64,
        width: f64,
        value: f64,
        inset: f64,
    ) -> String {
        let top = self.at(value).min(self.at(0.0) - inset);
        self.column_between(left + inset, width - 2.0 * inset, self.at(0.0) - inset, top)
    }

    /// The SVG path of a column between two positions, `bottom` below
    /// `top`: square at the bottom, its top rounded by 4 units, or less
    /// for a column too short.
    fn column_between(&self, left: f64, width: f64, bottom: f64, top: f64) -> String {
        let radius = (bottom - top).min(width / 2.0).clamp(0.0, 4.0);
        let right = left + width;
        format!(
            "M{left:.1},{bottom:.1}V{:.1}A{radius},{radius} 0 0 1 {:.1},{top:.1}H{:.1}A{radius},{radius} 0 0 1 {right:.1},{:.1}V{bottom:.1}Z",
            top + radius,
            left + radius,
            right - radius,
            top + radius,
        )
    }

    /// The SVG path of a horizontal bar from zero to `value`, `thickness`
    /// tall from `top`: square at the baseline, its data end rounded by
    /// 4 units, or less for a bar too short.
    pub(crate) fn bar_path(&self, value: f64, top: f64, thickness: f64) -> String {
        let (start, end) = (self.at(0.0), self.at(value));
        let radius = (end - start).min(thickness / 2.0).clamp(0.0, 4.0);
        let bottom = top + thickness;
        format!(
            "M{start:.1},{top:.1}H{:.1}A{radius},{radius} 0 0 1 {end:.1},{:.1}V{:.1}A{radius},{radius} 0 0 1 {:.1},{bottom:.1}H{start:.1}Z",
            end - radius,
            top + radius,
            bottom - radius,
            end - radius,
        )
    }
}

/// A smooth line through points in increasing x: a monotone cubic
/// (Fritsch–Carlson) as Bézier segments, which never rises above or dips
/// below its points between them, so a curve says nothing the data doesn't.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct SmoothLine {
    points: Vec<(f64, f64)>,
}

impl SmoothLine {
    pub(crate) fn through(points: Vec<(f64, f64)>) -> Self {
        Self { points }
    }

    /// Each point's slope: the mean of the segments either side, zero at a
    /// peak or a trough, then scaled down where it would overshoot.
    fn slopes(&self) -> Vec<f64> {
        let points = &self.points;
        let secants: Vec<f64> = (points.windows(2))
            .map(|pair| (pair[1].1 - pair[0].1) / (pair[1].0 - pair[0].0))
            .collect();
        let mut slopes: Vec<f64> = (0..points.len())
            .map(|index| match (index.checked_sub(1), secants.get(index)) {
                (None, Some(&next)) => next,
                (Some(before), None) => secants[before],
                (Some(before), Some(&next)) if secants[before] * next > 0.0 => {
                    (secants[before] + next) / 2.0
                }
                _ => 0.0,
            })
            .collect();
        for (index, &secant) in secants.iter().enumerate() {
            if secant == 0.0 {
                slopes[index] = 0.0;
                slopes[index + 1] = 0.0;
                continue;
            }
            let (a, b) = (slopes[index] / secant, slopes[index + 1] / secant);
            let length = a.hypot(b);
            if length > 3.0 {
                slopes[index] = 3.0 / length * a * secant;
                slopes[index + 1] = 3.0 / length * b * secant;
            }
        }
        slopes
    }

    /// The SVG path of the line.
    pub(crate) fn path(&self) -> String {
        let Some(&(x, y)) = self.points.first() else {
            return String::new();
        };
        let slopes = self.slopes();
        let mut path = format!("M{x:.1},{y:.1}");
        for (index, pair) in self.points.windows(2).enumerate() {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            let third = (x1 - x0) / 3.0;
            path.push_str(&format!(
                "C{:.1},{:.1} {:.1},{:.1} {x1:.1},{y1:.1}",
                x0 + third,
                y0 + slopes[index] * third,
                x1 - third,
                y1 - slopes[index + 1] * third,
            ));
        }
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_scales_tick_in_clean_steps() {
        let scale = LinearScale::fitting(93.0, (0.0, 200.0));
        assert_eq!(scale.ticks(), vec![0.0, 25.0, 50.0, 75.0, 100.0]);
        assert_eq!(scale.at(50.0), 100.0);
        // Narrow: every other tick, so the labels don't run together.
        let narrow = LinearScale::fitting(400.0, (0.0, 150.0));
        assert_eq!(narrow.ticks_apart(64.0), vec![0.0, 200.0, 400.0]);
        // Up to the longest bar: 1009 ns, ticks every 500 within it.
        let bars = LinearScale::reaching(1009.0, (0.0, 1009.0));
        assert_eq!(bars.ticks(), vec![0.0, 500.0, 1000.0]);
        assert_eq!(bars.at(1009.0), 1009.0);
    }

    #[test]
    fn smooth_lines_stay_level_where_the_data_is_level() {
        // A rise, then a plateau: the plateau's slope is zero, so the curve
        // can't bulge above it.
        let line = SmoothLine::through(vec![(0.0, 0.0), (1.0, 10.0), (2.0, 10.0), (3.0, 10.0)]);
        assert_eq!(line.slopes()[1..], [0.0, 0.0, 0.0]);
        assert!(line.path().starts_with("M0.0,0.0C"));
    }

    #[test]
    fn durations_read_in_their_unit() {
        assert_eq!(Nanos(240.4).to_string(), "240 ns");
        assert_eq!(Nanos(2834.0).to_string(), "2.8 µs");
        assert_eq!(Nanos(6_123_830.0).to_string(), "6.1 ms");
    }
}
