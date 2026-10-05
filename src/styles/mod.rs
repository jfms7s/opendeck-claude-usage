//! SVG renderers that return a bare SVG string, plus the helpers they
//! share: the Usage Gauge's six keypad styles (`build_styled_icon` picks one
//! and wraps it as the data URI `setImage` expects), and the Combo, Heatmap
//! and Sparkline key and dial-strip renderers (wrapped by their views).
//! Single-layout keypad tiles - Burn Rate, Peak Clock, Metric Tile - live
//! in `<feature>_icon.rs` instead and return the data URI themselves.

pub mod bar;
pub mod combo;
pub mod donut;
pub mod heatmap;
pub mod ring;
pub mod sparkline;
pub mod speedometer;

use crate::format::UsageDisplay;
use crate::gauge_style::GaugeStyle;
use crate::tile::{self, TEXT_COLOR};

/// The keypad image for a Usage Gauge in `style`, as the data URI
/// OpenDeck's `setImage` expects.
pub fn build_styled_icon(display: &UsageDisplay, style: GaugeStyle) -> String {
    let svg = match style {
        GaugeStyle::Speedometer => speedometer::render(display),
        GaugeStyle::Bar => bar::render(display, false),
        GaugeStyle::SoftPill => bar::render(display, true),
        GaugeStyle::OpenDonut => donut::render(display, false),
        GaugeStyle::TrackedDonut => donut::render(display, true),
        GaugeStyle::ThinRing => ring::render(display),
    };
    tile::data_uri(&svg)
}

/// Sweeps at least this large are drawn as a whole circle.
const FULL_TURN: f64 = 359.9;

/// Unfilled part of every bar/arc - dark enough to read as "empty" on the
/// card, light enough to show the track's extent.
pub const TRACK_COLOR: &str = "#374151";

/// The 100x100 `<svg>` wrapper with the shared dark card underneath.
pub fn svg(body: &str) -> String {
    let card = tile::card();
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{card}{body}</svg>"#)
}

/// A point on a circle at `deg` degrees measured clockwise from +x in
/// screen space (y grows downward, so +sin is down): 0 = right, 90 =
/// bottom, 180 = left, 270/-90 = top.
pub fn polar(cx: f64, cy: f64, r: f64, deg: f64) -> (f64, f64) {
    let t = deg.to_radians();
    (cx + r * t.cos(), cy + r * t.sin())
}

/// A stroked arc sweeping clockwise from `start_deg` to `end_deg`. Nothing
/// for a zero/negative sweep; a `<circle>` for a (near-)full turn (an SVG
/// arc whose endpoints coincide draws nothing).
// Geometry plus stroke: a params struct would only rename these eight values.
#[allow(clippy::too_many_arguments)]
pub fn arc(
    cx: f64,
    cy: f64,
    r: f64,
    start_deg: f64,
    end_deg: f64,
    color: &str,
    width: f64,
    round_caps: bool,
) -> String {
    let sweep = end_deg - start_deg;
    if sweep <= 0.0 {
        return String::new();
    }
    // Within a tenth of a degree of a full turn, the path's endpoints round
    // to the same printed point and SVG draws nothing at all - so treat it
    // as full (0.1deg is invisible on a key).
    if sweep >= FULL_TURN {
        return format!(
            r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{color}" stroke-width="{width}" />"#
        );
    }
    let (sx, sy) = polar(cx, cy, r, start_deg);
    let (ex, ey) = polar(cx, cy, r, end_deg);
    let large = if sweep > 180.0 { 1 } else { 0 };
    let cap = if round_caps { "round" } else { "butt" };
    format!(
        r#"<path d="M {sx:.2} {sy:.2} A {r} {r} 0 {large} 1 {ex:.2} {ey:.2}" fill="none" stroke="{color}" stroke-width="{width}" stroke-linecap="{cap}" />"#
    )
}

/// A short radial line across a ring at `deg`, reaching `half` either side
/// of the stroke's center - how donuts and rings mark the Watch/Risk/
/// Critical thresholds.
pub fn radial_tick(cx: f64, cy: f64, r: f64, deg: f64, half: f64) -> String {
    let (x1, y1) = polar(cx, cy, r - half, deg);
    let (x2, y2) = polar(cx, cy, r + half, deg);
    format!(
        r#"<line x1="{x1:.2}" y1="{y1:.2}" x2="{x2:.2}" y2="{y2:.2}" stroke="{TEXT_COLOR}" stroke-width="1.5" />"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polar_is_clockwise_in_screen_space() {
        let (x, y) = polar(50.0, 50.0, 10.0, 90.0);
        assert!((x - 50.0).abs() < 1e-9 && (y - 60.0).abs() < 1e-9);
        let (x, y) = polar(50.0, 50.0, 10.0, -90.0);
        assert!((x - 50.0).abs() < 1e-9 && (y - 40.0).abs() < 1e-9);
    }

    #[test]
    fn large_arc_flag_only_above_180() {
        assert!(arc(50.0, 50.0, 10.0, 0.0, 180.0, "#fff", 2.0, true).contains(" 0 0 1 "));
        assert!(arc(50.0, 50.0, 10.0, 0.0, 181.0, "#fff", 2.0, true).contains(" 0 1 1 "));
    }

    #[test]
    fn full_turn_is_a_circle_and_zero_is_nothing() {
        assert!(arc(50.0, 50.0, 10.0, -90.0, 270.0, "#fff", 2.0, true).starts_with("<circle"));
        assert_eq!(arc(50.0, 50.0, 10.0, 10.0, 10.0, "#fff", 2.0, true), "");
    }

    #[test]
    fn caps_follow_the_flag() {
        assert!(arc(50.0, 50.0, 10.0, 0.0, 90.0, "#fff", 2.0, true).contains("round"));
        assert!(arc(50.0, 50.0, 10.0, 0.0, 90.0, "#fff", 2.0, false).contains("butt"));
    }

    #[test]
    fn radial_tick_spans_the_ring() {
        // At 0deg (right) a tick on r=28 around (50,47) runs x 71..85.
        let t = radial_tick(50.0, 47.0, 28.0, 0.0, 7.0);
        assert!(
            t.contains(r#"x1="71.00" y1="47.00" x2="85.00" y2="47.00""#),
            "got: {t}"
        );
    }

    #[test]
    fn every_style_builds_a_data_uri() {
        use crate::gauge_style::ALL_STYLES;
        let d = crate::styles::bar::tests::display(42.0);
        for style in ALL_STYLES {
            assert!(
                build_styled_icon(&d, style).starts_with("data:image/svg+xml;base64,"),
                "{style:?}"
            );
        }
    }

    #[test]
    fn styles_render_differently() {
        use crate::gauge_style::GaugeStyle;
        let d = crate::styles::bar::tests::display(42.0);
        assert_ne!(
            build_styled_icon(&d, GaugeStyle::Bar),
            build_styled_icon(&d, GaugeStyle::ThinRing)
        );
    }
}
