//! Shared pieces for the keypad tiles' generated SVG icons: the dark card
//! background, the text colors, and text lines drawn *inside* the SVG.
//!
//! Text is baked into the image rather than sent as the tile's native
//! title because OpenDeck paints native titles with each key's own
//! font/size/stroke/alignment settings on top of the image - on a 96px key
//! that made the text hard to read and inconsistent between tiles. Drawing
//! it here keeps size, weight, placement, and contrast under our control.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

pub const CARD_COLOR: &str = "#111827";
pub const TEXT_COLOR: &str = "#f9fafb";
pub const MUTED_TEXT_COLOR: &str = "#d1d5db";

/// Rough average advance of a sans-serif glyph as a fraction of the font
/// size. Only used to decide when a line needs squeezing to fit, so a
/// slight overestimate is the safe side.
const REGULAR_CHAR_WIDTH: f64 = 0.58;
const BOLD_CHAR_WIDTH: f64 = 0.64;

/// Full-bleed dark card, so the tile never depends on the key's configured
/// background color for contrast.
pub fn card() -> String {
    format!(r#"<rect x="0" y="0" width="100" height="100" fill="{CARD_COLOR}" />"#)
}

/// Where `x` sits on a text line - SVG's `text-anchor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

impl Anchor {
    fn svg(self) -> &'static str {
        match self {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        }
    }

    /// Widest the line may render from `x` without touching the key's
    /// edge (3-unit margin). Centered at 50 this is 94, the old fixed limit.
    fn room(self, x: f64) -> f64 {
        match self {
            Anchor::Start => 97.0 - x,
            Anchor::End => x - 3.0,
            Anchor::Middle => 2.0 * x.min(100.0 - x) - 6.0,
        }
    }
}

/// One text line anchored at (`x`, `y`). Lines estimated to be wider than
/// the room available from `x` are squeezed to fit via `textLength` rather
/// than clipped.
pub fn text_at(
    x: f64,
    y: f64,
    anchor: Anchor,
    size: f64,
    bold: bool,
    color: &str,
    content: &str,
) -> String {
    let char_width = if bold {
        BOLD_CHAR_WIDTH
    } else {
        REGULAR_CHAR_WIDTH
    };
    let room = anchor.room(x);
    let estimated_width = content.chars().count() as f64 * size * char_width;
    let fit = if estimated_width > room {
        format!(r#" textLength="{room}" lengthAdjust="spacingAndGlyphs""#)
    } else {
        String::new()
    };
    let weight = if bold { "700" } else { "500" };
    let escaped = escape_xml(content);
    let text_anchor = anchor.svg();
    format!(
        r#"<text x="{x}" y="{y}" text-anchor="{text_anchor}" font-family="sans-serif" font-size="{size}" font-weight="{weight}" fill="{color}"{fit}>{escaped}</text>"#
    )
}

/// One horizontally-centered text line with its baseline at `y`.
pub fn text_line(y: f64, size: f64, bold: bool, color: &str, content: &str) -> String {
    text_at(50.0, y, Anchor::Middle, size, bold, color, content)
}

pub fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Builds the `image` string OpenDeck's `setImage` event expects: always a
/// base64 data URI, since `setImage` only treats `image` as inline data when
/// it starts with `"data:"` - anything else is treated as a relative
/// filename inside the plugin's bundle directory.
pub fn data_uri(svg: &str) -> String {
    let encoded = STANDARD.encode(svg.as_bytes());
    format!("data:image/svg+xml;base64,{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_are_not_squeezed() {
        let line = text_line(80.0, 14.0, false, TEXT_COLOR, "3h 54m");
        assert!(!line.contains("textLength"), "got: {line}");
        assert!(line.contains(">3h 54m</text>"), "got: {line}");
    }

    #[test]
    fn long_lines_are_squeezed_to_fit() {
        let line = text_line(80.0, 14.0, false, TEXT_COLOR, "spend unavailable");
        assert!(line.contains(r#"textLength="94""#), "got: {line}");
    }

    #[test]
    fn text_is_xml_escaped() {
        let line = text_line(80.0, 14.0, false, TEXT_COLOR, "<1m & up");
        assert!(line.contains(">&lt;1m &amp; up</text>"), "got: {line}");
    }

    #[test]
    fn text_at_start_and_end_set_anchor_and_x() {
        let start = text_at(8.0, 20.0, Anchor::Start, 11.0, true, TEXT_COLOR, "Session");
        assert!(
            start.contains(r#"x="8" y="20" text-anchor="start""#),
            "got: {start}"
        );
        let end = text_at(92.0, 20.0, Anchor::End, 13.0, true, TEXT_COLOR, "46%");
        assert!(
            end.contains(r#"x="92" y="20" text-anchor="end""#),
            "got: {end}"
        );
    }

    #[test]
    fn end_anchor_squeezes_to_the_room_left_of_x() {
        // Room at x 92 is 89; 11 bold chars at 13px ~ 91.5 wide.
        let wide = text_at(
            92.0,
            20.0,
            Anchor::End,
            13.0,
            true,
            TEXT_COLOR,
            "12345678901",
        );
        assert!(wide.contains(r#"textLength="89""#), "got: {wide}");
        let fits = text_at(
            92.0,
            20.0,
            Anchor::End,
            13.0,
            true,
            TEXT_COLOR,
            "1234567890",
        );
        assert!(!fits.contains("textLength"), "got: {fits}");
    }

    #[test]
    fn middle_anchor_room_depends_on_distance_to_the_nearer_edge() {
        // At x 32 the room is 2*32-6 = 58.
        let s = text_at(
            32.0,
            17.0,
            Anchor::Middle,
            13.0,
            true,
            TEXT_COLOR,
            "12345678",
        );
        assert!(s.contains(r#"textLength="58""#), "got: {s}");
    }

    #[test]
    fn text_line_is_text_at_the_center() {
        assert_eq!(
            text_line(80.0, 14.0, false, TEXT_COLOR, "spend unavailable"),
            text_at(
                50.0,
                80.0,
                Anchor::Middle,
                14.0,
                false,
                TEXT_COLOR,
                "spend unavailable"
            )
        );
    }
}
