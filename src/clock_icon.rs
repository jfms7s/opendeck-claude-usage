use crate::peak::{PeakStatus, PeakWindow};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const FACE_COLOR: &str = "#4b5563";
const PEAK_COLOR: &str = "#ef4444";
const POINTER_COLOR: &str = TEXT_COLOR;
const PEAK_TEXT_COLOR: &str = "#f87171";
const OFF_PEAK_TEXT_COLOR: &str = "#4ade80";

const CENTER_X: f64 = 50.0;
const CENTER_Y: f64 = 36.0;
const FACE_RADIUS: f64 = 27.0;
const RING_STROKE_WIDTH: f64 = 8.0;
const POINTER_LENGTH: f64 = 21.0;
const POINTER_WIDTH: f64 = 4.0;
const PIVOT_RADIUS: f64 = 4.0;

const STATUS_BASELINE: f64 = 82.0;
const STATUS_SIZE: f64 = 17.0;
const COUNTDOWN_BASELINE: f64 = 97.0;
const COUNTDOWN_SIZE: f64 = 13.0;

/// A point on the face ring at `minute_of_day`, with 00:00 at the top and the
/// day sweeping clockwise (matches a 24h analog clock face). Screen y grows
/// downward, so this rotates the top point `(CENTER_X, CENTER_Y - r)` by the
/// clockwise fraction of the day elapsed.
fn point_on_ring(minute_of_day: u32, radius: f64) -> (f64, f64) {
    let fraction = minute_of_day as f64 / 1440.0;
    let angle = fraction * std::f64::consts::TAU;
    (
        CENTER_X + radius * angle.sin(),
        CENTER_Y - radius * angle.cos(),
    )
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

/// Renders the tile as an SVG string: a dark card, the 24h clock face (a
/// full ring, red arc(s) over the peak window, and a light pointer at
/// `now_minutes`), and the status + countdown as two text lines underneath,
/// the status colored red/green so peak vs off-peak reads at a glance (see
/// `tile.rs` for why the text lives in the image).
fn render_svg(window: PeakWindow, now_minutes: u32, status: &PeakStatus) -> String {
    let card = tile::card();
    let arcs = peak_arcs(window);
    let (tx, ty) = point_on_ring(now_minutes, POINTER_LENGTH);
    let status_line = tile::text_line(
        STATUS_BASELINE,
        STATUS_SIZE,
        true,
        status_color(status.is_peak),
        status.status_text,
    );
    let countdown_line = tile::text_line(
        COUNTDOWN_BASELINE,
        COUNTDOWN_SIZE,
        false,
        MUTED_TEXT_COLOR,
        &status.countdown_text,
    );

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{card}<circle cx="{CENTER_X}" cy="{CENTER_Y}" r="{FACE_RADIUS}" fill="none" stroke="{FACE_COLOR}" stroke-width="{RING_STROKE_WIDTH}" />{arcs}<line x1="{CENTER_X}" y1="{CENTER_Y}" x2="{tx:.2}" y2="{ty:.2}" stroke="{POINTER_COLOR}" stroke-width="{POINTER_WIDTH}" stroke-linecap="round" /><circle cx="{CENTER_X}" cy="{CENTER_Y}" r="{PIVOT_RADIUS}" fill="{POINTER_COLOR}" />{status_line}{countdown_line}</svg>"#
    )
}

fn status_color(is_peak: bool) -> &'static str {
    if is_peak {
        PEAK_TEXT_COLOR
    } else {
        OFF_PEAK_TEXT_COLOR
    }
}

/// Builds the `image` string OpenDeck's `setImage` event expects.
pub fn build_clock_icon(window: PeakWindow, now_minutes: u32, status: &PeakStatus) -> String {
    tile::data_uri(&render_svg(window, now_minutes, status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

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

    fn status(is_peak: bool) -> PeakStatus {
        PeakStatus {
            is_peak,
            status_text: if is_peak { "Peak" } else { "Off-peak" },
            countdown_text: "peak in 01:00".to_string(),
        }
    }

    fn render(window: PeakWindow, now_minutes: u32) -> String {
        decode(&build_clock_icon(window, now_minutes, &status(false)))
    }

    fn pointer_tip(x: f64, y: f64) -> String {
        format!("x2=\"{x:.2}\" y2=\"{y:.2}\"")
    }

    #[test]
    fn builds_a_valid_svg_data_uri() {
        let svg = render(normal_window(), 600);
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(svg.contains(PEAK_COLOR));
    }

    #[test]
    fn normal_window_draws_exactly_one_peak_arc() {
        let svg = render(normal_window(), 600);
        assert_eq!(svg.matches(PEAK_COLOR).count(), 1);
    }

    #[test]
    fn overnight_window_draws_two_peak_arcs() {
        let svg = render(overnight_window(), 0);
        assert_eq!(svg.matches(PEAK_COLOR).count(), 2);
    }

    #[test]
    fn zero_length_window_draws_no_peak_arc() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 540,
        };
        let svg = render(window, 600);
        assert_eq!(svg.matches(PEAK_COLOR).count(), 0);
    }

    #[test]
    fn pointer_points_up_at_midnight() {
        let svg = render(normal_window(), 0);
        assert!(svg.contains(&pointer_tip(CENTER_X, CENTER_Y - POINTER_LENGTH)));
    }

    #[test]
    fn pointer_points_right_at_six_hours() {
        let svg = render(normal_window(), 360);
        assert!(svg.contains(&pointer_tip(CENTER_X + POINTER_LENGTH, CENTER_Y)));
    }

    #[test]
    fn pointer_points_down_at_noon() {
        let svg = render(normal_window(), 720);
        assert!(svg.contains(&pointer_tip(CENTER_X, CENTER_Y + POINTER_LENGTH)));
    }

    #[test]
    fn pointer_points_left_at_eighteen_hours() {
        let svg = render(normal_window(), 1080);
        assert!(svg.contains(&pointer_tip(CENTER_X - POINTER_LENGTH, CENTER_Y)));
    }

    #[test]
    fn draws_status_and_countdown_text_in_the_image() {
        let svg = render(normal_window(), 600);
        assert!(svg.contains(">Off-peak</text>"), "got: {svg}");
        assert!(svg.contains(">peak in 01:00</text>"), "got: {svg}");
    }

    #[test]
    fn status_text_is_red_when_peak_and_green_when_off_peak() {
        let peak = decode(&build_clock_icon(normal_window(), 600, &status(true)));
        assert!(peak.contains(&format!("fill=\"{PEAK_TEXT_COLOR}\">Peak</text>")));
        let off = render(normal_window(), 600);
        assert!(off.contains(&format!("fill=\"{OFF_PEAK_TEXT_COLOR}\">Off-peak</text>")));
    }
}
