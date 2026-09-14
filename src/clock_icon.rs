use crate::peak::PeakWindow;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

const FACE_COLOR: &str = "#374151";
const PEAK_COLOR: &str = "#ef4444";
const POINTER_COLOR: &str = "#f9fafb";

const CENTER: f64 = 50.0;
const FACE_RADIUS: f64 = 40.0;
const RING_STROKE_WIDTH: f64 = 10.0;
const POINTER_LENGTH: f64 = 33.0;
const PIVOT_RADIUS: f64 = 4.0;

/// A point on the face ring at `minute_of_day`, with 00:00 at the top and the
/// day sweeping clockwise (matches a 24h analog clock face). Screen y grows
/// downward, so this rotates the top point `(CENTER, CENTER - r)` by the
/// clockwise fraction of the day elapsed.
fn point_on_ring(minute_of_day: u32, radius: f64) -> (f64, f64) {
    let fraction = minute_of_day as f64 / 1440.0;
    let angle = fraction * std::f64::consts::TAU;
    (CENTER + radius * angle.sin(), CENTER - radius * angle.cos())
}

/// One or two red arcs marking the peak window on the ring - two when the
/// window crosses midnight, since a single SVG arc command can't jump from
/// `end_minutes` back to `0`.
fn peak_arcs(window: PeakWindow) -> String {
    let spans: Vec<(u32, u32)> = if window.start_minutes <= window.end_minutes {
        vec![(window.start_minutes, window.end_minutes)]
    } else {
        vec![(window.start_minutes, 1440), (0, window.end_minutes)]
    };

    spans
        .into_iter()
        .filter(|(start, end)| start != end)
        .map(|(start, end)| {
            let (sx, sy) = point_on_ring(start, FACE_RADIUS);
            let (ex, ey) = point_on_ring(end, FACE_RADIUS);
            let large_arc = if end - start > 720 { 1 } else { 0 };
            format!(
                r#"<path d="M {sx:.2} {sy:.2} A {FACE_RADIUS} {FACE_RADIUS} 0 {large_arc} 1 {ex:.2} {ey:.2}" fill="none" stroke="{PEAK_COLOR}" stroke-width="{RING_STROKE_WIDTH}" stroke-linecap="round" />"#
            )
        })
        .collect()
}

/// Renders the 24h clock face as an SVG string: a full ring, red arc(s) over
/// the peak window, and a pointer at `now_minutes` - colored to match the
/// peak arcs when `is_peak` is true, reinforcing the label rather than just
/// duplicating it.
fn render_svg(window: PeakWindow, now_minutes: u32, is_peak: bool) -> String {
    let arcs = peak_arcs(window);
    let (tx, ty) = point_on_ring(now_minutes, POINTER_LENGTH);
    let pointer_color = pointer_color(is_peak);

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><circle cx="{CENTER}" cy="{CENTER}" r="{FACE_RADIUS}" fill="none" stroke="{FACE_COLOR}" stroke-width="{RING_STROKE_WIDTH}" />{arcs}<line x1="{CENTER}" y1="{CENTER}" x2="{tx:.2}" y2="{ty:.2}" stroke="{pointer_color}" stroke-width="3" stroke-linecap="round" /><circle cx="{CENTER}" cy="{CENTER}" r="{PIVOT_RADIUS}" fill="{pointer_color}" /></svg>"#
    )
}

fn pointer_color(is_peak: bool) -> &'static str {
    if is_peak { PEAK_COLOR } else { POINTER_COLOR }
}

/// Builds the `image` string OpenDeck's `setImage` event expects, same
/// base64 data-URI convention as `icon::build_icon`.
pub fn build_clock_icon(window: PeakWindow, now_minutes: u32, is_peak: bool) -> String {
    let svg = render_svg(window, now_minutes, is_peak);
    let encoded = STANDARD.encode(svg.as_bytes());
    format!("data:image/svg+xml;base64,{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(uri: &str) -> String {
        let prefix = "data:image/svg+xml;base64,";
        assert!(uri.starts_with(prefix), "got: {uri}");
        let bytes = STANDARD.decode(&uri[prefix.len()..]).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    fn normal_window() -> PeakWindow {
        PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        } // 9:00-17:00
    }

    fn overnight_window() -> PeakWindow {
        PeakWindow {
            start_minutes: 1320,
            end_minutes: 360,
        } // 22:00-6:00
    }

    #[test]
    fn builds_a_valid_svg_data_uri() {
        let svg = decode(&build_clock_icon(normal_window(), 600, false));
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(svg.contains(PEAK_COLOR));
    }

    #[test]
    fn normal_window_draws_exactly_one_peak_arc() {
        let svg = decode(&build_clock_icon(normal_window(), 600, false));
        assert_eq!(svg.matches("stroke-linecap=\"round\" />").count(), 2); // arc + pointer both round-capped
        assert_eq!(svg.matches(PEAK_COLOR).count(), 1);
    }

    #[test]
    fn overnight_window_draws_two_peak_arcs() {
        let svg = decode(&build_clock_icon(overnight_window(), 0, false));
        assert_eq!(svg.matches(PEAK_COLOR).count(), 2);
    }

    #[test]
    fn zero_length_window_draws_no_peak_arc() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 540,
        };
        let svg = decode(&build_clock_icon(window, 600, false));
        assert_eq!(svg.matches(PEAK_COLOR).count(), 0);
    }

    #[test]
    fn pointer_points_up_at_midnight() {
        let svg = decode(&build_clock_icon(normal_window(), 0, false));
        // point_on_ring(0, POINTER_LENGTH) = (CENTER, CENTER - POINTER_LENGTH)
        assert!(svg.contains(&format!(
            "x2=\"{CENTER:.2}\" y2=\"{:.2}\"",
            CENTER - POINTER_LENGTH
        )));
    }

    #[test]
    fn pointer_points_right_at_six_hours() {
        let svg = decode(&build_clock_icon(normal_window(), 360, false));
        assert!(svg.contains(&format!(
            "x2=\"{:.2}\" y2=\"{CENTER:.2}\"",
            CENTER + POINTER_LENGTH
        )));
    }

    #[test]
    fn pointer_points_down_at_noon() {
        let svg = decode(&build_clock_icon(normal_window(), 720, false));
        assert!(svg.contains(&format!(
            "x2=\"{CENTER:.2}\" y2=\"{:.2}\"",
            CENTER + POINTER_LENGTH
        )));
    }

    #[test]
    fn pointer_points_left_at_eighteen_hours() {
        let svg = decode(&build_clock_icon(normal_window(), 1080, false));
        assert!(svg.contains(&format!(
            "x2=\"{:.2}\" y2=\"{CENTER:.2}\"",
            CENTER - POINTER_LENGTH
        )));
    }

    #[test]
    fn pointer_is_peak_colored_when_currently_peak() {
        let svg = decode(&build_clock_icon(normal_window(), 600, true));
        assert!(svg.contains(&format!("stroke=\"{PEAK_COLOR}\" stroke-width=\"3\"")));
    }

    #[test]
    fn pointer_is_default_colored_when_currently_off_peak() {
        let svg = decode(&build_clock_icon(normal_window(), 600, false));
        assert!(svg.contains(&format!("stroke=\"{POINTER_COLOR}\" stroke-width=\"3\"")));
    }
}
