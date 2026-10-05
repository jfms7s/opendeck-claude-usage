//! Session + Weekly combo key: which keypad layout a key shows, and the
//! payload for its two-bar dial touch strip. Pure.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::format::UsageDisplay;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComboLayout {
    #[default]
    Horizontal,
    Vertical,
}

impl ComboLayout {
    pub fn flipped(self) -> Self {
        match self {
            ComboLayout::Horizontal => ComboLayout::Vertical,
            ComboLayout::Vertical => ComboLayout::Horizontal,
        }
    }
}

/// "46% · 6h 12m"; just "—" when there's no data at all, rather than a
/// dangling "— · no data".
fn strip_value(display: &UsageDisplay) -> String {
    if !display.has_data {
        "\u{2014}".to_string()
    } else {
        format!("{} \u{b7} {}", display.percent_text, display.tile_detail)
    }
}

/// Dial payload for `layouts/combo.json`: the "5h"/"7d" labels are static
/// in the layout, so only values and bars are sent.
pub fn combo_feedback(session: &UsageDisplay, weekly: &UsageDisplay) -> Value {
    json!({
        "s_value": strip_value(session),
        "s_bar": { "value": session.bar_value, "bar_fill_c": session.color },
        "w_value": strip_value(weekly),
        "w_bar": { "value": weekly.bar_value, "bar_fill_c": weekly.color },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{DISABLED_COLOR, error_display};
    use crate::styles::bar::tests::display;

    #[test]
    fn flipped_toggles_and_serializes_lowercase() {
        assert_eq!(ComboLayout::Horizontal.flipped(), ComboLayout::Vertical);
        assert_eq!(
            ComboLayout::Vertical.flipped().flipped(),
            ComboLayout::Vertical
        );
        assert_eq!(json!(ComboLayout::Vertical), json!("vertical"));
    }

    #[test]
    fn feedback_carries_both_windows() {
        let mut weekly = display(82.0);
        weekly.color = "#f97316".to_string();
        weekly.tile_detail = "3d 2h".to_string();
        let f = combo_feedback(&display(46.0), &weekly);
        assert_eq!(f["s_value"], "46% \u{b7} 3h 54m");
        assert_eq!(f["s_bar"]["value"], 46.0);
        assert_eq!(f["s_bar"]["bar_fill_c"], "#d97757");
        assert_eq!(f["w_value"], "82% \u{b7} 3d 2h");
        assert_eq!(f["w_bar"]["bar_fill_c"], "#f97316");
    }

    #[test]
    fn missing_reset_shows_dash_on_strip() {
        let mut s = display(46.0);
        s.tile_detail = "\u{2014}".to_string();
        assert_eq!(combo_feedback(&s, &s)["s_value"], "46% \u{b7} \u{2014}");
    }

    #[test]
    fn error_feedback_is_dashes() {
        let f = combo_feedback(&error_display(), &error_display());
        assert_eq!(f["s_value"], "\u{2014}");
        assert_eq!(f["w_value"], "\u{2014}");
        assert_eq!(f["s_bar"]["value"], 0.0);
        assert_eq!(f["w_bar"]["bar_fill_c"], DISABLED_COLOR);
    }
}
