use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

const CARD_COLOR: &str = "#111827";
const UNDERLINE_WIDTH: f64 = 24.0;
const UNDERLINE_X: f64 = (100.0 - UNDERLINE_WIDTH) / 2.0;
const UNDERLINE_Y: f64 = 24.0;

fn render_svg(accent_color: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect x="0" y="0" width="100" height="100" rx="12" fill="{CARD_COLOR}" /><rect x="{UNDERLINE_X}" y="{UNDERLINE_Y}" width="{UNDERLINE_WIDTH}" height="3" rx="1.5" fill="{accent_color}" /></svg>"#
    )
}

/// Builds the `image` string OpenDeck's `setImage` event expects, same
/// base64 data-URI convention as `icon::build_icon` /
/// `clock_icon::build_clock_icon`. The label/value/subtitle text itself
/// renders as the tile's native title (crisper, consistent with the
/// other two tiles - see `icon.rs`'s rationale) - this SVG only draws
/// the card background and a colored underline accent beneath where the
/// label line sits.
pub fn build_metric_icon(accent_color: &str) -> String {
    let svg = render_svg(accent_color);
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

    #[test]
    fn builds_a_valid_svg_data_uri() {
        let svg = decode(&build_metric_icon("#38bdf8"));
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(svg.contains(CARD_COLOR));
    }

    #[test]
    fn draws_the_passed_accent_color() {
        let svg = decode(&build_metric_icon("#fb923c"));
        assert!(svg.contains("#fb923c"));
    }

    #[test]
    fn different_accent_colors_produce_different_icons() {
        let a = build_metric_icon("#38bdf8");
        let b = build_metric_icon("#fb923c");
        assert_ne!(a, b);
    }
}
