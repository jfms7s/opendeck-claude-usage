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

/// Widest a text line may render, in viewBox units (of 100) - leaves a
/// small margin so glyphs never touch the key's edge.
const MAX_TEXT_WIDTH: f64 = 94.0;

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

/// One horizontally-centered text line with its baseline at `y`. Lines
/// estimated to be wider than the key are squeezed to fit via
/// `textLength` rather than clipped.
pub fn text_line(y: f64, size: f64, bold: bool, color: &str, content: &str) -> String {
    let char_width = if bold {
        BOLD_CHAR_WIDTH
    } else {
        REGULAR_CHAR_WIDTH
    };
    let estimated_width = content.chars().count() as f64 * size * char_width;
    let fit = if estimated_width > MAX_TEXT_WIDTH {
        format!(r#" textLength="{MAX_TEXT_WIDTH}" lengthAdjust="spacingAndGlyphs""#)
    } else {
        String::new()
    };
    let weight = if bold { "700" } else { "500" };
    let escaped = escape_xml(content);
    format!(
        r#"<text x="50" y="{y}" text-anchor="middle" font-family="sans-serif" font-size="{size}" font-weight="{weight}" fill="{color}"{fit}>{escaped}</text>"#
    )
}

fn escape_xml(s: &str) -> String {
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
}
