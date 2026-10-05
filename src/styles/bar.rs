//! Bar and Soft pill: caption, big percent, a horizontal bar with tick
//! marks at the key's thresholds, and the countdown underneath.

use crate::format::UsageDisplay;
use crate::styles::{TRACK_COLOR, svg};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const TRACK_X: f64 = 12.0;
const TRACK_W: f64 = 76.0;
/// The Soft pill's smallest non-zero fill: a visible dot for 1-5%.
const PILL_MIN_W: f64 = 4.0;

/// `pill` = Soft pill: a thicker, rounded bar that carries the level color
/// itself, so the percent above it stays white.
pub fn render(display: &UsageDisplay, pill: bool) -> String {
    let (track_y, height, rx) = if pill {
        (63.0, 12.0, 6.0)
    } else {
        (64.0, 8.0, 0.0)
    };
    let rounding = if pill {
        format!(r#" rx="{rx}""#)
    } else {
        String::new()
    };

    let label = tile::text_line(22.0, 12.0, true, MUTED_TEXT_COLOR, display.label);
    let percent_color = if pill {
        TEXT_COLOR
    } else {
        display.color.as_str()
    };
    let percent = tile::text_line(54.0, 28.0, true, percent_color, &display.percent_text);
    let detail = tile::text_line(91.0, 13.0, false, MUTED_TEXT_COLOR, &display.tile_detail);

    let track = format!(
        r#"<rect x="{TRACK_X}" y="{track_y}" width="{TRACK_W}" height="{height}"{rounding} fill="{TRACK_COLOR}" />"#
    );
    let fill = if display.bar_value > 0.0 {
        let mut width = TRACK_W * display.bar_value / 100.0;
        let fill_rounding = if pill {
            // A tiny value still shows as a dot; above that the width
            // tracks the value, so 5% and 15% don't look alike. The end
            // radius shrinks with a narrow fill so its caps never overlap.
            width = width.max(PILL_MIN_W);
            format!(r#" rx="{:.2}" ry="{rx}""#, (width / 2.0).min(rx))
        } else {
            String::new()
        };
        format!(
            r#"<rect x="{TRACK_X}" y="{track_y}" width="{width:.2}" height="{height}"{fill_rounding} fill="{}" />"#,
            display.color
        )
    } else {
        String::new()
    };
    let (y1, y2) = (track_y - 3.0, track_y + height + 3.0);
    let ticks: String = [display.marks.watch, display.marks.risk, display.marks.critical]
        .iter()
        .map(|m| {
            let x = TRACK_X + TRACK_W * m / 100.0;
            format!(
                r#"<line x1="{x:.2}" y1="{y1}" x2="{x:.2}" y2="{y2}" stroke="{TEXT_COLOR}" stroke-width="1.2" />"#
            )
        })
        .collect();

    svg(&format!("{label}{percent}{track}{fill}{ticks}{detail}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::level::{Marks, Palette};

    pub(crate) fn display(v: f64) -> UsageDisplay {
        UsageDisplay {
            has_data: true,
            percent_text: format!("{v}%"),
            color: "#d97757".to_string(),
            detail_text: "resets in 1h".to_string(),
            tile_detail: "3h 54m".to_string(),
            label: "SESSION",
            number_text: format!("{v}"),
            bar_value: v,
            marks: Marks::default(),
            palette: Palette::default(),
        }
    }

    #[test]
    fn bar_draws_label_percent_countdown_and_fill() {
        let s = render(&display(42.0), false);
        assert!(s.starts_with("<svg"), "got: {s}");
        assert!(s.contains(">SESSION</text>"));
        assert!(s.contains(r##"fill="#d97757">42%</text>"##), "got: {s}");
        assert!(s.contains(">3h 54m</text>"));
        // 76 * 0.42 = 31.92
        assert!(
            s.contains(r##"width="31.92" height="8" fill="#d97757""##),
            "got: {s}"
        );
    }

    #[test]
    fn bar_ticks_at_default_marks() {
        let s = render(&display(42.0), false);
        // watch 50 -> 50.00, risk 75 -> 69.00, critical 90 -> 80.40
        for x in ["50.00", "69.00", "80.40"] {
            assert!(
                s.contains(&format!(r#"x1="{x}" y1="61" x2="{x}" y2="75""#)),
                "tick {x} in {s}"
            );
        }
    }

    #[test]
    fn custom_marks_move_ticks() {
        let mut d = display(42.0);
        d.marks = Marks {
            watch: 25.0,
            risk: 60.0,
            critical: 95.0,
        };
        assert!(render(&d, false).contains(r#"x1="31.00""#));
    }

    #[test]
    fn ticks_at_track_ends() {
        let mut d = display(42.0);
        d.marks = Marks {
            watch: 0.0,
            risk: 50.0,
            critical: 100.0,
        };
        let s = render(&d, false);
        assert!(
            s.contains(r#"x1="12.00""#) && s.contains(r#"x1="88.00""#),
            "got: {s}"
        );
    }

    #[test]
    fn zero_draws_no_fill() {
        for pill in [false, true] {
            let s = render(&display(0.0), pill);
            assert!(
                !s.contains(r##"fill="#d97757" rx"##)
                    && !s.contains(r##"height="8" fill="#d97757""##),
                "got: {s}"
            );
        }
    }

    #[test]
    fn full_bar_spans_the_track() {
        assert!(
            render(&display(100.0), false).contains(r##"width="76.00" height="8" fill="#d97757""##)
        );
    }

    #[test]
    fn pill_is_rounded_with_a_minimum_width_and_white_percent() {
        // 76 * 0.03 = 2.28, raised to the 4px minimum dot.
        let s = render(&display(3.0), true);
        assert!(
            s.contains(r##"width="4.00" height="12" rx="2.00" ry="6" fill="#d97757""##),
            "got: {s}"
        );
        assert!(
            s.contains(&format!(r#"fill="{TEXT_COLOR}">3%</text>"#)),
            "got: {s}"
        );
        assert!(
            s.contains(r#"x1="50.00" y1="60" x2="50.00" y2="78""#),
            "got: {s}"
        );
    }

    fn pill_fill_width(v: f64) -> String {
        let s = render(&display(v), true);
        let fill = s
            .split("<rect")
            .find(|r| r.contains(r##"fill="#d97757""##))
            .unwrap_or_else(|| panic!("no fill in {s}"));
        let start = fill.find(r#"width=""#).unwrap() + 7;
        fill[start..].split('"').next().unwrap().to_string()
    }

    #[test]
    fn small_pill_values_are_told_apart() {
        // 76px track: 10% = 7.6px, 15% = 11.4px - both used to be 12px.
        assert_eq!(pill_fill_width(10.0), "7.60");
        assert_eq!(pill_fill_width(15.0), "11.40");
        assert_eq!(pill_fill_width(1.0), "4.00");
    }

    #[test]
    fn a_wide_pill_keeps_full_rounding() {
        assert!(
            render(&display(50.0), true)
                .contains(r##"width="38.00" height="12" rx="6.00" ry="6" fill="#d97757""##)
        );
    }
}
