use crate::format::UsageDisplay;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

const ZONE_GREEN: &str = "#22c55e";
const ZONE_YELLOW: &str = "#eab308";
const ZONE_RED: &str = "#ef4444";
const NEEDLE_COLOR: &str = "#1f2937";

const CENTER_X: f64 = 50.0;
const CENTER_Y: f64 = 65.0;
const RADIUS: f64 = 35.0;
const STROKE_WIDTH: f64 = 12.0;
const NEEDLE_LENGTH: f64 = 28.0;
const NEEDLE_HALF_WIDTH: f64 = 2.5;
const PIVOT_RADIUS: f64 = 4.0;

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
/// (degrees, `theta_start` > `theta_end`, both spans here are <= 90deg so a
/// single-arc SVG path with `large-arc-flag=0` is always correct).
fn arc_path(theta_start: f64, theta_end: f64, color: &str) -> String {
    let (sx, sy) = polar_point(theta_start);
    let (ex, ey) = polar_point(theta_end);
    format!(
        r#"<path d="M {sx:.2} {sy:.2} A {RADIUS} {RADIUS} 0 0 1 {ex:.2} {ey:.2}" fill="none" stroke="{color}" stroke-width="{STROKE_WIDTH}" stroke-linecap="round" />"#
    )
}

/// 0% -> needle full left (180deg), 100% -> full right (0deg), sweeping
/// clockwise up through the dome at 50% (90deg, straight up) - the fixed
/// zone boundaries below use these exact same angles, so the needle and the
/// background it points at can never disagree.
fn needle_rotation_deg(bar_value: f64) -> f64 {
    bar_value.clamp(0.0, 100.0) * 1.8 - 90.0
}

/// Renders the gauge as an SVG string: a fixed three-zone semicircular
/// speedometer background (zone boundaries match `bar_color`'s own
/// 50%/80% thresholds) plus a needle rotated to `display.bar_value`. The
/// percent/detail text is deliberately not drawn here - it renders as the
/// tile's native title text instead (crisper font rendering, one less thing
/// to keep in sync with this SVG).
fn render_svg(display: &UsageDisplay) -> String {
    let rotation = needle_rotation_deg(display.bar_value);

    let arcs: String = [
        (180.0, 90.0, ZONE_GREEN),
        (90.0, 36.0, ZONE_YELLOW),
        (36.0, 0.0, ZONE_RED),
    ]
    .iter()
    .map(|(start, end, color)| arc_path(*start, *end, color))
    .collect();

    let nx1 = CENTER_X - NEEDLE_HALF_WIDTH;
    let nx2 = CENTER_X + NEEDLE_HALF_WIDTH;
    let tip_y = CENTER_Y - NEEDLE_LENGTH;

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{arcs}<polygon points="{nx1},{CENTER_Y} {nx2},{CENTER_Y} {CENTER_X},{tip_y}" fill="{NEEDLE_COLOR}" transform="rotate({rotation:.2} {CENTER_X} {CENTER_Y})" /><circle cx="{CENTER_X}" cy="{CENTER_Y}" r="{PIVOT_RADIUS}" fill="{NEEDLE_COLOR}" /></svg>"#
    )
}

/// Builds the `image` string OpenDeck's `setImage` event expects: always a
/// base64 data URI, since `setImage` only treats `image` as inline data when
/// it starts with `"data:"` - anything else is treated as a relative
/// filename inside the plugin's bundle directory (same rule
/// opendeck-focus-launcher's icon.rs documents).
pub fn build_icon(display: &UsageDisplay) -> String {
    let svg = render_svg(display);
    let encoded = STANDARD.encode(svg.as_bytes());
    format!("data:image/svg+xml;base64,{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(bar_value: f64) -> UsageDisplay {
        UsageDisplay {
            percent_text: format!("{bar_value}%"),
            color: ZONE_GREEN,
            detail_text: "resets in 1h".to_string(),
            bar_value,
        }
    }

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
        assert!(svg.contains(ZONE_GREEN));
        assert!(svg.contains(ZONE_YELLOW));
        assert!(svg.contains(ZONE_RED));
    }

    #[test]
    fn needle_points_left_at_zero_percent() {
        let svg = decode(&build_icon(&display(0.0)));
        assert!(svg.contains("rotate(-90.00 50 65)"), "got: {svg}");
    }

    #[test]
    fn needle_points_up_at_fifty_percent() {
        let svg = decode(&build_icon(&display(50.0)));
        assert!(svg.contains("rotate(0.00 50 65)"), "got: {svg}");
    }

    #[test]
    fn needle_points_right_at_hundred_percent() {
        let svg = decode(&build_icon(&display(100.0)));
        assert!(svg.contains("rotate(90.00 50 65)"), "got: {svg}");
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
            color: "#6b7280",
            detail_text: "not enabled".to_string(),
            bar_value: 0.0,
        };
        let svg = decode(&build_icon(&d));
        assert!(svg.contains("rotate(-90.00 50 65)"), "got: {svg}");
    }
}
