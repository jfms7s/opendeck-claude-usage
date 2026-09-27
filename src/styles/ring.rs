//! Thin ring: a full-circle track filled clockwise from the top, with the
//! number inside and the window name underneath.

use crate::format::UsageDisplay;
use crate::styles::{TRACK_COLOR, arc, radial_tick, svg};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const CX: f64 = 50.0;
const CY: f64 = 47.0;
const R: f64 = 30.0;
const WIDTH: f64 = 4.0;
const TOP: f64 = -90.0;
/// Shorter than the donuts' ticks: the ring sits lower relative to its
/// label, and a longer tick at the bottom (50%) runs into it.
const TICK_HALF: f64 = 4.0;

fn angle(percent: f64) -> f64 {
    TOP + 360.0 * percent / 100.0
}

pub fn render(display: &UsageDisplay) -> String {
    let track = format!(
        r#"<circle cx="{CX}" cy="{CY}" r="{R}" fill="none" stroke="{TRACK_COLOR}" stroke-width="{WIDTH}" />"#
    );
    let progress = arc(
        CX,
        CY,
        R,
        TOP,
        angle(display.bar_value),
        &display.color,
        WIDTH,
        true,
    );
    let ticks: String = [
        display.marks.watch,
        display.marks.risk,
        display.marks.critical,
    ]
    .iter()
    .map(|m| radial_tick(CX, CY, R, angle(*m), TICK_HALF))
    .collect();
    let number = tile::text_line(56.0, 24.0, true, TEXT_COLOR, &display.number_text);
    let label = tile::text_line(92.0, 11.0, true, MUTED_TEXT_COLOR, display.label);
    svg(&format!("{track}{progress}{ticks}{number}{label}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styles::bar::tests::display;

    #[test]
    fn ring_draws_track_progress_number_and_label() {
        let s = render(&display(42.0));
        assert!(
            s.contains(&format!(
                r#"<circle cx="50" cy="47" r="30" fill="none" stroke="{TRACK_COLOR}""#
            )),
            "got: {s}"
        );
        assert!(s.contains(r##"stroke="#d97757""##));
        assert!(s.contains(">42</text>") && s.contains(">SESSION</text>"));
    }

    #[test]
    fn ring_ticks_at_default_marks() {
        let s = render(&display(42.0));
        // watch 50 -> 90deg (straight down): tick from (50,73) to (50,81),
        // short enough to clear the label underneath (cap top ~y 84).
        assert!(
            s.contains(r#"x1="50.00" y1="73.00" x2="50.00" y2="81.00""#),
            "got: {s}"
        );
    }

    #[test]
    fn zero_draws_no_fill_and_full_is_a_circle() {
        assert!(!render(&display(0.0)).contains(r##"stroke="#d97757""##));
        assert!(render(&display(100.0)).contains(r##"r="30" fill="none" stroke="#d97757""##));
    }
}
