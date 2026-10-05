//! Keypad tile for Metric Tile: the label on top with an accent underline,
//! the value large, the range underneath - all drawn inside the SVG like
//! every other tile (see tile.rs for why the native title isn't used).

use crate::metric::MetricDisplay;
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const LABEL_BASELINE: f64 = 24.0;
const LABEL_SIZE: f64 = 13.0;
const UNDERLINE_WIDTH: f64 = 24.0;
const UNDERLINE_X: f64 = (100.0 - UNDERLINE_WIDTH) / 2.0;
const UNDERLINE_Y: f64 = 30.0;
const VALUE_BASELINE: f64 = 64.0;
const VALUE_SIZE: f64 = 28.0;
const SUBTITLE_BASELINE: f64 = 86.0;
const SUBTITLE_SIZE: f64 = 14.0;

fn render_svg(display: &MetricDisplay) -> String {
    let card = tile::card();
    let label = tile::text_line(
        LABEL_BASELINE,
        LABEL_SIZE,
        true,
        MUTED_TEXT_COLOR,
        &display.label.to_uppercase(),
    );
    let accent = display.accent_color;
    let underline = format!(
        r#"<rect x="{UNDERLINE_X}" y="{UNDERLINE_Y}" width="{UNDERLINE_WIDTH}" height="3" rx="1.5" fill="{accent}" />"#
    );
    let value = tile::text_line(
        VALUE_BASELINE,
        VALUE_SIZE,
        true,
        TEXT_COLOR,
        &display.value_text,
    );
    let subtitle = tile::text_line(
        SUBTITLE_BASELINE,
        SUBTITLE_SIZE,
        false,
        MUTED_TEXT_COLOR,
        display.subtitle,
    );
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{card}{label}{underline}{value}{subtitle}</svg>"#
    )
}

/// The `image` string OpenDeck's `setImage` expects.
pub fn build_metric_icon(display: &MetricDisplay) -> String {
    tile::data_uri(&render_svg(display))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::{COST_ACCENT, TOKENS_ACCENT, error_display};

    fn tokens() -> MetricDisplay {
        MetricDisplay {
            label: "Tokens",
            value_text: "318.5K".to_string(),
            subtitle: "today",
            accent_color: TOKENS_ACCENT,
        }
    }

    #[test]
    fn draws_label_value_and_subtitle_on_the_shared_card() {
        let svg = render_svg(&tokens());
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(svg.contains(&tile::card()), "got: {svg}");
        assert!(svg.contains(">TOKENS</text>"), "got: {svg}");
        assert!(svg.contains(">318.5K</text>"), "got: {svg}");
        assert!(svg.contains(">today</text>"), "got: {svg}");
    }

    #[test]
    fn the_underline_takes_the_accent_color() {
        assert!(render_svg(&tokens()).contains(TOKENS_ACCENT));
        let cost = MetricDisplay {
            label: "Cost",
            value_text: "$8.40".to_string(),
            subtitle: "7 days",
            accent_color: COST_ACCENT,
        };
        assert!(render_svg(&cost).contains(COST_ACCENT));
    }

    #[test]
    fn no_data_is_drawn_too() {
        let svg = render_svg(&error_display());
        assert!(svg.contains(">no data</text>"), "got: {svg}");
    }

    #[test]
    fn builds_an_svg_data_uri() {
        assert!(build_metric_icon(&tokens()).starts_with("data:image/svg+xml;base64,"));
    }
}
