//! Keypad tile for API Spend: label, the dollar value in its color, an
//! optional budget bar and the range caption - all drawn inside the SVG
//! (see tile.rs).

use crate::spend::SpendDisplay;
use crate::styles::{TRACK_COLOR, svg};
use crate::tile::{MUTED_TEXT_COLOR, text_line};

const LABEL_BASELINE: f64 = 22.0;
const LABEL_SIZE: f64 = 12.0;
const VALUE_BASELINE: f64 = 56.0;
const VALUE_SIZE: f64 = 26.0;
/// Error labels ("KEY PERMS") are longer than a value and aren't a number,
/// so they're drawn smaller.
const ERROR_SIZE: f64 = 16.0;
const BAR_X: f64 = 16.0;
const BAR_Y: f64 = 66.0;
const BAR_WIDTH: f64 = 68.0;
const BAR_HEIGHT: f64 = 6.0;
const CAPTION_BASELINE: f64 = 88.0;
const CAPTION_SIZE: f64 = 12.0;

pub fn render_key(display: &SpendDisplay) -> String {
    let label = text_line(
        LABEL_BASELINE,
        LABEL_SIZE,
        true,
        MUTED_TEXT_COLOR,
        "API SPEND",
    );
    let size = if display.available {
        VALUE_SIZE
    } else {
        ERROR_SIZE
    };
    let value = text_line(
        VALUE_BASELINE,
        size,
        true,
        &display.color,
        &display.value_text,
    );
    let bar = display
        .budget_percent
        .map(|percent| budget_bar(percent, &display.color))
        .unwrap_or_default();
    let caption = text_line(
        CAPTION_BASELINE,
        CAPTION_SIZE,
        false,
        MUTED_TEXT_COLOR,
        display.caption,
    );
    svg(&format!("{label}{value}{bar}{caption}"))
}

fn budget_bar(percent: f64, color: &str) -> String {
    let fill = BAR_WIDTH * percent.clamp(0.0, 100.0) / 100.0;
    format!(
        r#"<rect x="{BAR_X}" y="{BAR_Y}" width="{BAR_WIDTH}" height="{BAR_HEIGHT}" rx="3" fill="{TRACK_COLOR}" /><rect x="{BAR_X}" y="{BAR_Y}" width="{fill}" height="{BAR_HEIGHT}" rx="3" fill="{color}" />"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(budget_percent: Option<f64>, available: bool) -> SpendDisplay {
        SpendDisplay {
            value_text: if available { "$12.34" } else { "NO KEY" }.to_string(),
            available,
            caption: "THIS MONTH",
            detail: "API · MONTH".to_string(),
            color: "#fb923c".to_string(),
            budget_percent,
        }
    }

    #[test]
    fn draws_the_label_value_and_caption_without_a_bar() {
        let s = render_key(&display(None, true));
        assert!(s.starts_with("<svg"), "{s}");
        assert!(s.contains(">API SPEND</text>"), "{s}");
        assert!(s.contains(">$12.34</text>"), "{s}");
        assert!(s.contains(r##"fill="#fb923c""##), "{s}");
        assert!(s.contains(">THIS MONTH</text>"), "{s}");
        assert!(!s.contains(TRACK_COLOR), "no budget, no bar: {s}");
    }

    #[test]
    fn a_budget_draws_a_bar_filled_to_its_share() {
        let s = render_key(&display(Some(50.0), true));
        assert!(s.contains(TRACK_COLOR), "{s}");
        assert!(s.contains(r#"width="34" height="6""#), "{s}");
    }

    #[test]
    fn an_over_budget_bar_stops_at_full() {
        let s = render_key(&display(Some(150.0), true));
        assert!(
            s.contains(r##"width="68" height="6" rx="3" fill="#fb923c""##),
            "{s}"
        );
    }

    #[test]
    fn an_error_label_is_drawn_smaller_than_a_value() {
        let s = render_key(&display(None, false));
        assert!(s.contains(">NO KEY</text>"), "{s}");
        assert!(s.contains(r#"font-size="16""#), "{s}");
    }
}
