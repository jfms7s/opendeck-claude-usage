//! Keypad tile for Burn Rate: label on top, the value large in the level
//! color, the subtitle underneath - all drawn inside the SVG (see tile.rs).

use crate::burn::BurnDisplay;
use crate::tile::{self, MUTED_TEXT_COLOR};

const LABEL_BASELINE: f64 = 24.0;
const LABEL_SIZE: f64 = 13.0;
const VALUE_BASELINE: f64 = 62.0;
const VALUE_SIZE: f64 = 30.0;
const SUBTITLE_BASELINE: f64 = 86.0;
const SUBTITLE_SIZE: f64 = 14.0;

fn render_svg(display: &BurnDisplay) -> String {
    let card = tile::card();
    let label = tile::text_line(
        LABEL_BASELINE,
        LABEL_SIZE,
        true,
        MUTED_TEXT_COLOR,
        display.label,
    );
    let value = tile::text_line(
        VALUE_BASELINE,
        VALUE_SIZE,
        true,
        &display.color,
        &display.value_text,
    );
    let subtitle = tile::text_line(
        SUBTITLE_BASELINE,
        SUBTITLE_SIZE,
        false,
        MUTED_TEXT_COLOR,
        &display.subtitle,
    );
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{card}{label}{value}{subtitle}</svg>"#
    )
}

/// Builds the `image` string OpenDeck's `setImage` event expects.
pub fn build_burn_icon(display: &BurnDisplay) -> String {
    tile::data_uri(&render_svg(display))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    fn decode(uri: &str) -> String {
        let prefix = "data:image/svg+xml;base64,";
        assert!(uri.starts_with(prefix), "got: {uri}");
        String::from_utf8(STANDARD.decode(&uri[prefix.len()..]).unwrap()).unwrap()
    }

    fn display() -> BurnDisplay {
        BurnDisplay {
            label: "EVEN BURN",
            value_text: "1.4x".to_string(),
            subtitle: "even burn".to_string(),
            detail_text: "even burn \u{b7} session".to_string(),
            color: "#ef4444".to_string(),
            bar_value: 100.0,
        }
    }

    #[test]
    fn draws_label_value_and_subtitle() {
        let svg = decode(&build_burn_icon(&display()));
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(svg.contains(">EVEN BURN</text>"), "got: {svg}");
        assert!(svg.contains(">1.4x</text>"), "got: {svg}");
        assert!(svg.contains(">even burn</text>"), "got: {svg}");
    }

    #[test]
    fn value_is_drawn_in_the_level_color() {
        let svg = decode(&build_burn_icon(&display()));
        assert!(
            svg.contains(r##"fill="#ef4444">1.4x</text>"##),
            "got: {svg}"
        );
    }
}
