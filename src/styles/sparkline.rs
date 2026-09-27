//! Usage Sparkline drawings: caption, big headline, and a filled trend line
//! with an end dot - on a 100x100 key and a 200x100 dial strip.

use serde_json::{Value, json};

use crate::sparkline::SparkDisplay;
use crate::styles::svg;
use crate::tile::{self, CARD_COLOR, MUTED_TEXT_COLOR, TEXT_COLOR};

/// The rectangle the line is drawn in: x from `left` to `right`, y from
/// `top` (highest value) to `bottom` (lowest).
struct Area {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

const KEY_AREA: Area = Area {
    left: 8.0,
    right: 92.0,
    top: 58.0,
    bottom: 90.0,
};
const STRIP_AREA: Area = Area {
    left: 10.0,
    right: 190.0,
    top: 34.0,
    bottom: 92.0,
};

/// The filled area, the line and the end dot for `points` inside `area`.
/// The y range starts at 0 (or lower) and leaves 10% headroom, and never
/// spans less than 1 so a flat line can't divide by zero.
fn chart(points: &[(f64, f64)], area: &Area, color: &str) -> String {
    let lo = points.iter().map(|p| p.1).fold(0.0, f64::min);
    let max = points.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    let hi = (max * 1.1).max(lo + 1.0);
    let xy: Vec<(f64, f64)> = points
        .iter()
        .map(|(x, v)| {
            (
                area.left + x * (area.right - area.left),
                area.bottom - (v - lo) / (hi - lo) * (area.bottom - area.top),
            )
        })
        .collect();
    let line: Vec<String> = xy.iter().map(|(x, y)| format!("{x:.2},{y:.2}")).collect();
    let (first_x, _) = xy[0];
    let (last_x, last_y) = xy[xy.len() - 1];
    let bottom = area.bottom;
    let mut path = format!("M {first_x:.2} {bottom:.2}");
    for (x, y) in &xy {
        path.push_str(&format!(" L {x:.2} {y:.2}"));
    }
    path.push_str(&format!(" L {last_x:.2} {bottom:.2} Z"));
    format!(
        r#"<path d="{path}" fill="{color}" fill-opacity="0.2" /><polyline points="{}" fill="none" stroke="{color}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round" /><circle cx="{last_x:.2}" cy="{last_y:.2}" r="2.5" fill="{color}" />"#,
        line.join(" ")
    )
}

pub fn render_key(display: &SparkDisplay) -> String {
    let caption = tile::text_line(14.0, 11.0, true, MUTED_TEXT_COLOR, &display.caption);
    let headline = tile::text_line(46.0, 26.0, true, &display.color, &display.headline);
    let body = if display.points.is_empty() {
        tile::text_line(78.0, 11.0, false, MUTED_TEXT_COLOR, "collecting\u{2026}")
    } else {
        chart(&display.points, &KEY_AREA, &display.color)
    };
    svg(&format!("{caption}{headline}{body}"))
}

/// Own 200-wide wrapper and unsqueezed text, like the heatmap strip -
/// `styles::svg` and `tile::text_at` assume a 100-wide key.
pub fn render_strip(display: &SparkDisplay) -> String {
    let caption = tile::escape_xml(&display.caption);
    let headline = tile::escape_xml(&display.headline);
    let color = &display.color;
    let headline_size = strip_headline_size(&display.caption, &display.headline);
    let body = if display.points.is_empty() {
        format!(
            r#"<text x="100" y="70" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="500" fill="{MUTED_TEXT_COLOR}">collecting&#8230;</text>"#
        )
    } else {
        chart(&display.points, &STRIP_AREA, color)
    };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><rect x="0" y="0" width="200" height="100" fill="{CARD_COLOR}" /><text x="10" y="18" text-anchor="start" font-family="sans-serif" font-size="14" font-weight="700" fill="{TEXT_COLOR}">{caption}</text><text x="190" y="20" text-anchor="end" font-family="sans-serif" font-size="{headline_size}" font-weight="700" fill="{color}">{headline}</text>{body}</svg>"#
    )
}

/// The strip headline's font size: 20, shrunk (down to 10) so it ends
/// before the caption does, using the same bold-width estimate as
/// `tile::text_at`.
fn strip_headline_size(caption: &str, headline: &str) -> f64 {
    const CAPTION_SIZE: f64 = 14.0;
    const GAP: f64 = 6.0;
    let caption_width = caption.chars().count() as f64 * CAPTION_SIZE * tile::BOLD_CHAR_WIDTH;
    let room = 180.0 - caption_width - GAP;
    let chars = headline.chars().count().max(1) as f64;
    (room / (chars * tile::BOLD_CHAR_WIDTH))
        .clamp(10.0, 20.0)
        .floor()
}

/// Dial payload for the shared `layouts/chart.json`.
pub fn sparkline_feedback(display: &SparkDisplay) -> Value {
    json!({ "chart": tile::data_uri(&render_strip(display)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(points: Vec<(f64, f64)>) -> SparkDisplay {
        SparkDisplay {
            caption: "TREND \u{b7} 5H".to_string(),
            headline: "40%".to_string(),
            points,
            color: "#d97757".to_string(),
        }
    }

    fn rising() -> Vec<(f64, f64)> {
        vec![(0.0, 10.0), (0.5, 20.0), (1.0, 40.0)]
    }

    #[test]
    fn key_draws_caption_headline_line_fill_and_dot() {
        let s = render_key(&display(rising()));
        assert!(s.contains(">TREND \u{b7} 5H</text>"), "got: {s}");
        assert!(s.contains(r##"fill="#d97757">40%</text>"##), "got: {s}");
        // y spans 0..44 (40 * 1.1) over 90..58.
        assert!(
            s.contains(r#"points="8.00,82.73 50.00,75.45 92.00,60.91""#),
            "got: {s}"
        );
        assert!(s.contains(r##"d="M 8.00 90.00 L 8.00 82.73 L 50.00 75.45 L 92.00 60.91 L 92.00 90.00 Z" fill="#d97757" fill-opacity="0.2""##), "got: {s}");
        assert!(
            s.contains(r##"<circle cx="92.00" cy="60.91" r="2.5" fill="#d97757""##),
            "got: {s}"
        );
    }

    #[test]
    fn flat_line_does_not_divide_by_zero() {
        let s = render_key(&display(vec![(0.0, 0.0), (1.0, 0.0)]));
        assert!(s.contains(r#"points="8.00,90.00 92.00,90.00""#), "got: {s}");
        assert!(!s.contains("NaN"));
    }

    #[test]
    fn collecting_has_no_line() {
        let mut d = display(Vec::new());
        d.headline = "\u{2014}".to_string();
        let s = render_key(&d);
        assert!(s.contains(">collecting\u{2026}</text>"), "got: {s}");
        assert!(!s.contains("<polyline"));
    }

    #[test]
    fn strip_is_200_wide_with_right_aligned_headline() {
        let s = render_strip(&display(rising()));
        assert!(s.contains(r#"viewBox="0 0 200 100""#));
        assert!(s.contains(&format!(r#"width="200" height="100" fill="{CARD_COLOR}""#)));
        assert!(s.contains(r##"x="190" y="20" text-anchor="end" font-family="sans-serif" font-size="20" font-weight="700" fill="#d97757">40%</text>"##), "got: {s}");
        assert!(
            s.contains(r#"points="10.00,"#) && s.contains(" 190.00,"),
            "got: {s}"
        );
        assert!(!s.contains("textLength"));
    }

    /// KI-01: a long caption and headline must not overlap on the strip.
    #[test]
    fn strip_headline_shrinks_to_clear_a_long_caption() {
        let mut d = display(rising());
        d.caption = "PER POLL \u{b7} 5H".to_string();
        d.headline = "+12.3pp".to_string();
        let s = render_strip(&d);
        let size: f64 = s
            .split(r#"x="190" y="20" text-anchor="end" font-family="sans-serif" font-size=""#)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no headline in {s}"));
        let caption_right = 10.0 + 13.0 * 14.0 * 0.64;
        let headline_left = 190.0 - 7.0 * size * 0.64;
        assert!(caption_right < headline_left, "size {size} overlaps: {s}");
        assert!(size >= 10.0, "size {size} too small");
    }

    #[test]
    fn strip_collecting_is_centered_text() {
        let s = render_strip(&display(Vec::new()));
        assert!(
            s.contains(r#"x="100" y="70" text-anchor="middle""#),
            "got: {s}"
        );
        assert!(!s.contains("<polyline"));
    }

    #[test]
    fn feedback_is_a_chart_data_uri() {
        let f = sparkline_feedback(&display(rising()));
        assert!(
            f["chart"]
                .as_str()
                .unwrap()
                .starts_with("data:image/svg+xml;base64,")
        );
    }
}
