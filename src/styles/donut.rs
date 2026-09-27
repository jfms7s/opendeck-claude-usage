//! Open donut (a 270deg arc, gap at the bottom) and Tracked donut (the same
//! arc as ten segments), with the number in the middle and the window name
//! underneath.

use crate::format::UsageDisplay;
use crate::styles::{TRACK_COLOR, arc, radial_tick, svg};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const CX: f64 = 50.0;
const CY: f64 = 47.0;
const R: f64 = 28.0;
const WIDTH: f64 = 9.0;
/// Bottom-left, sweeping clockwise over the top to bottom-right.
const START: f64 = 135.0;
const SWEEP: f64 = 270.0;
const SEGMENTS: usize = 10;
const SEGMENT_GAP: f64 = 6.0;
/// Ticks reach past the 9-wide stroke on both sides.
const TICK_HALF: f64 = 7.0;

fn angle(percent: f64) -> f64 {
    START + SWEEP * percent / 100.0
}

pub fn render(display: &UsageDisplay, tracked: bool) -> String {
    let v = display.bar_value;
    let rings = if tracked {
        let span = (SWEEP - SEGMENT_GAP * (SEGMENTS - 1) as f64) / SEGMENTS as f64;
        (0..SEGMENTS)
            .map(|i| {
                let start = START + i as f64 * (span + SEGMENT_GAP);
                // Segment i lights once usage passes its start, so 42%
                // lights five and 0% lights none.
                let color = if v > i as f64 * 10.0 {
                    display.color.as_str()
                } else {
                    TRACK_COLOR
                };
                arc(CX, CY, R, start, start + span, color, WIDTH, false)
            })
            .collect::<String>()
    } else {
        let track = arc(CX, CY, R, START, START + SWEEP, TRACK_COLOR, WIDTH, true);
        let progress = arc(CX, CY, R, START, angle(v), &display.color, WIDTH, true);
        format!("{track}{progress}")
    };
    let ticks: String = [
        display.marks.watch,
        display.marks.risk,
        display.marks.critical,
    ]
    .iter()
    .map(|m| radial_tick(CX, CY, R, angle(*m), TICK_HALF))
    .collect();
    let number = tile::text_line(55.0, 22.0, true, TEXT_COLOR, &display.number_text);
    let label = tile::text_line(92.0, 11.0, true, MUTED_TEXT_COLOR, display.label);
    svg(&format!("{rings}{ticks}{number}{label}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::Marks;
    use crate::styles::bar::tests::display;

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    #[test]
    fn open_donut_draws_track_progress_number_and_label() {
        let s = render(&display(42.0), false);
        assert!(s.starts_with("<svg"));
        assert!(s.contains(&format!(r#"stroke="{TRACK_COLOR}""#)));
        assert_eq!(count(&s, r##"stroke="#d97757""##), 1, "got: {s}");
        assert!(
            s.contains(">42</text>") && s.contains(">SESSION</text>"),
            "got: {s}"
        );
    }

    #[test]
    fn open_donut_ticks_at_default_marks() {
        let s = render(&display(42.0), false);
        // watch 50 -> 270deg (straight up): tick from (50,26) to (50,12)
        assert!(
            s.contains(r#"x1="50.00" y1="26.00" x2="50.00" y2="12.00""#),
            "got: {s}"
        );
        assert_eq!(count(&s, "<line"), 3);
    }

    #[test]
    fn tick_at_zero_and_hundred() {
        let mut d = display(42.0);
        d.marks = Marks {
            watch: 0.0,
            risk: 50.0,
            critical: 100.0,
        };
        let s = render(&d, false);
        assert_eq!(count(&s, "<line"), 3);
        assert!(!s.contains("NaN"));
    }

    #[test]
    fn open_donut_zero_draws_no_fill() {
        assert_eq!(
            count(&render(&display(0.0), false), r##"stroke="#d97757""##),
            0
        );
    }

    #[test]
    fn tracked_donut_lights_ceil_tenths() {
        assert_eq!(
            count(&render(&display(42.0), true), r##"stroke="#d97757""##),
            5
        );
        assert_eq!(
            count(&render(&display(40.0), true), r##"stroke="#d97757""##),
            4
        );
        assert_eq!(
            count(&render(&display(0.0), true), r##"stroke="#d97757""##),
            0
        );
        assert_eq!(
            count(&render(&display(100.0), true), r##"stroke="#d97757""##),
            10
        );
    }

    #[test]
    fn tracked_segments_have_butt_caps() {
        let s = render(&display(42.0), true);
        assert_eq!(count(&s, "stroke-linecap=\"butt\""), 10);
    }
}
