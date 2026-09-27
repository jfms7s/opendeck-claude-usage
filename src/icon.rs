use crate::format::UsageDisplay;
use crate::level::{Level, Marks};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

/// Light, so the needle stands out against the dark card.
const NEEDLE_COLOR: &str = TEXT_COLOR;

const CENTER_X: f64 = 50.0;
const CENTER_Y: f64 = 48.0;
const RADIUS: f64 = 32.0;
const STROKE_WIDTH: f64 = 10.0;
const NEEDLE_LENGTH: f64 = 25.0;
const NEEDLE_HALF_WIDTH: f64 = 3.0;
const PIVOT_RADIUS: f64 = 5.0;

const PERCENT_BASELINE: f64 = 79.0;
const PERCENT_SIZE: f64 = 26.0;
const DETAIL_BASELINE: f64 = 96.0;
const DETAIL_SIZE: f64 = 15.0;

/// A point on the gauge's dome at `theta_deg` degrees, measured
/// counter-clockwise from the positive x-axis. Screen y grows downward, so
/// this subtracts the sin term to sweep the arc upward (180deg = left,
/// 90deg = top, 0deg = right) - matches the reference speedometer shape.
fn polar_point(theta_deg: f64) -> (f64, f64) {
    let theta = theta_deg.to_radians();
    (
        CENTER_X + RADIUS * theta.cos(),
        CENTER_Y - RADIUS * theta.sin(),
    )
}

/// One rounded-cap arc segment from `theta_start` down to `theta_end`
/// (degrees, `theta_start` > `theta_end`). Every span is <= 90deg (see
/// `zone_segments`), so `large-arc-flag=0` is always correct.
fn arc_path(theta_start: f64, theta_end: f64, color: &str) -> String {
    let (sx, sy) = polar_point(theta_start);
    let (ex, ey) = polar_point(theta_end);
    format!(
        r#"<path d="M {sx:.2} {sy:.2} A {RADIUS} {RADIUS} 0 0 1 {ex:.2} {ey:.2}" fill="none" stroke="{color}" stroke-width="{STROKE_WIDTH}" stroke-linecap="round" />"#
    )
}

/// 0% -> 180deg (left), 100% -> 0deg (right) - the same mapping as
/// `needle_rotation_deg`, so zones and needle can never disagree.
fn percent_to_theta(percent: f64) -> f64 {
    180.0 - percent.clamp(0.0, 100.0) * 1.8
}

/// The colored zones as `(from %, to %, level)`: normal up to Watch, then
/// Watch, Risk, Critical up to 100. Zero-length zones are dropped, and any
/// zone crossing 50% is split there so no arc spans more than 90deg
/// (`arc_path` always uses `large-arc-flag=0`).
fn zone_segments(marks: &Marks) -> Vec<(f64, f64, Level)> {
    let bounds = [0.0, marks.watch, marks.risk, marks.critical, 100.0];
    let levels = [Level::Normal, Level::Watch, Level::Risk, Level::Critical];
    let mut zones = Vec::new();
    for (i, level) in levels.into_iter().enumerate() {
        let (from, to) = (bounds[i], bounds[i + 1]);
        if to <= from {
            continue;
        }
        if from < 50.0 && to > 50.0 {
            zones.push((from, 50.0, level));
            zones.push((50.0, to, level));
        } else {
            zones.push((from, to, level));
        }
    }
    zones
}

/// 0% -> needle full left (180deg), 100% -> full right (0deg), sweeping
/// clockwise up through the dome at 50% (90deg, straight up) - the fixed
/// zone boundaries below use these exact same angles, so the needle and the
/// background it points at can never disagree.
fn needle_rotation_deg(bar_value: f64) -> f64 {
    bar_value.clamp(0.0, 100.0) * 1.8 - 90.0
}

/// Renders the tile as an SVG string: a dark card, a four-zone
/// semicircular speedometer drawn from the key's own marks and palette,
/// a needle rotated to `display.bar_value`, and the
/// percent + compact countdown as two text lines underneath (see `tile.rs`
/// for why the text lives in the image rather than the native title).
fn render_svg(display: &UsageDisplay) -> String {
    let rotation = needle_rotation_deg(display.bar_value);

    let arcs: String = zone_segments(&display.marks)
        .into_iter()
        .map(|(from, to, level)| {
            arc_path(
                percent_to_theta(from),
                percent_to_theta(to),
                display.palette.color(level),
            )
        })
        .collect();

    let nx1 = CENTER_X - NEEDLE_HALF_WIDTH;
    let nx2 = CENTER_X + NEEDLE_HALF_WIDTH;
    let tip_y = CENTER_Y - NEEDLE_LENGTH;

    let card = tile::card();
    // The percent carries the level color - on a keypad it's the only
    // place a pace-based warning shows, since the zones follow actual %.
    let percent = tile::text_line(
        PERCENT_BASELINE,
        PERCENT_SIZE,
        true,
        &display.color,
        &display.percent_text,
    );
    let detail = tile::text_line(
        DETAIL_BASELINE,
        DETAIL_SIZE,
        false,
        MUTED_TEXT_COLOR,
        &display.tile_detail,
    );

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{card}{arcs}<polygon points="{nx1},{CENTER_Y} {nx2},{CENTER_Y} {CENTER_X},{tip_y}" fill="{NEEDLE_COLOR}" stroke-linejoin="round" transform="rotate({rotation:.2} {CENTER_X} {CENTER_Y})" /><circle cx="{CENTER_X}" cy="{CENTER_Y}" r="{PIVOT_RADIUS}" fill="{NEEDLE_COLOR}" />{percent}{detail}</svg>"#
    )
}

/// Builds the `image` string OpenDeck's `setImage` event expects.
pub fn build_icon(display: &UsageDisplay) -> String {
    tile::data_uri(&render_svg(display))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::level::{
        DEFAULT_CRITICAL, DEFAULT_NORMAL, DEFAULT_RISK, DEFAULT_WATCH, Level, Marks, Palette,
    };

    fn display(bar_value: f64) -> UsageDisplay {
        UsageDisplay {
            percent_text: format!("{bar_value}%"),
            color: DEFAULT_NORMAL.to_string(),
            detail_text: "resets in 1h".to_string(),
            tile_detail: "1h".to_string(),
            bar_value,
            marks: Marks::default(),
            palette: Palette::default(),
        }
    }

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    fn decode(uri: &str) -> String {
        let prefix = "data:image/svg+xml;base64,";
        assert!(uri.starts_with(prefix), "got: {uri}");
        let bytes = STANDARD.decode(&uri[prefix.len()..]).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn builds_a_valid_svg_data_uri() {
        let svg = decode(&build_icon(&display(50.0)));
        assert!(svg.starts_with("<svg"), "got: {svg}");
        for color in [
            DEFAULT_NORMAL,
            DEFAULT_WATCH,
            DEFAULT_RISK,
            DEFAULT_CRITICAL,
        ] {
            assert!(svg.contains(color), "missing {color} in {svg}");
        }
    }

    #[test]
    fn default_marks_make_four_zones() {
        assert_eq!(
            zone_segments(&Marks::default()),
            vec![
                (0.0, 50.0, Level::Normal),
                (50.0, 75.0, Level::Watch),
                (75.0, 90.0, Level::Risk),
                (90.0, 100.0, Level::Critical),
            ]
        );
    }

    #[test]
    fn zero_length_zone_is_skipped() {
        let marks = Marks {
            watch: 0.0,
            risk: 60.0,
            critical: 100.0,
        };
        let levels: Vec<Level> = zone_segments(&marks).iter().map(|z| z.2).collect();
        assert!(!levels.contains(&Level::Normal));
        assert!(!levels.contains(&Level::Critical));
    }

    #[test]
    fn zones_crossing_half_are_split_so_no_arc_exceeds_90_degrees() {
        let marks = Marks {
            watch: 20.0,
            risk: 80.0,
            critical: 95.0,
        };
        let zones = zone_segments(&marks);
        assert!(zones.contains(&(20.0, 50.0, Level::Watch)));
        assert!(zones.contains(&(50.0, 80.0, Level::Watch)));
        for (from, to, _) in zones {
            assert!(to > from && to - from <= 50.0, "zone {from}..{to}");
        }
    }

    #[test]
    fn custom_palette_colors_the_zones() {
        let mut d = display(10.0);
        d.palette.critical = "#abcdef".to_string();
        assert!(decode(&build_icon(&d)).contains("#abcdef"));
    }

    #[test]
    fn draws_percent_and_detail_text_in_the_image() {
        let svg = decode(&build_icon(&display(42.0)));
        assert!(svg.contains(">42%</text>"), "got: {svg}");
        assert!(svg.contains(">1h</text>"), "got: {svg}");
    }

    #[test]
    fn needle_points_left_at_zero_percent() {
        let svg = decode(&build_icon(&display(0.0)));
        assert!(svg.contains("rotate(-90.00 50 48)"), "got: {svg}");
    }

    #[test]
    fn needle_points_up_at_fifty_percent() {
        let svg = decode(&build_icon(&display(50.0)));
        assert!(svg.contains("rotate(0.00 50 48)"), "got: {svg}");
    }

    #[test]
    fn needle_points_right_at_hundred_percent() {
        let svg = decode(&build_icon(&display(100.0)));
        assert!(svg.contains("rotate(90.00 50 48)"), "got: {svg}");
    }

    #[test]
    fn needle_rotation_clamps_out_of_range_bar_values() {
        // UsageDisplay::bar_value is always pre-clamped by format.rs, but
        // this defends the icon renderer itself against ever pointing the
        // needle past the drawn gauge if that invariant is ever broken.
        assert_eq!(needle_rotation_deg(142.0), 90.0);
        assert_eq!(needle_rotation_deg(-10.0), -90.0);
    }

    #[test]
    fn disabled_color_still_renders_a_valid_icon() {
        let d = UsageDisplay {
            percent_text: "\u{2014}".to_string(),
            color: "#6b7280".to_string(),
            detail_text: "not enabled".to_string(),
            tile_detail: "not enabled".to_string(),
            bar_value: 0.0,
            marks: Marks::default(),
            palette: Palette::default(),
        };
        let svg = decode(&build_icon(&d));
        assert!(svg.contains("rotate(-90.00 50 48)"), "got: {svg}");
    }

    #[test]
    fn percent_text_is_drawn_in_the_level_color() {
        // A Pace-mode Risk level must be visible on a keypad tile, not
        // only on a dial's bar.
        let mut d = display(20.0);
        d.color = DEFAULT_RISK.to_string();
        let svg = decode(&build_icon(&d));
        assert!(
            svg.contains(&format!(r#"fill="{DEFAULT_RISK}">20%</text>"#)),
            "got: {svg}"
        );
    }
}
