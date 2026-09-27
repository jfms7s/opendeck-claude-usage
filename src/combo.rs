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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "LayoutSettingsWire", into = "LayoutSettingsWire")]
pub struct LayoutSettings {
    /// Changed only by a short press on the key.
    pub layout: ComboLayout,
}

/// Raw `Value` for the same reason as `level::ColorSettingsWire`: a bad
/// value must fall back on its own rather than make openaction reset the
/// key's whole settings struct.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutSettingsWire {
    layout: Value,
}

impl ComboLayout {
    pub fn flipped(self) -> Self {
        match self {
            ComboLayout::Horizontal => ComboLayout::Vertical,
            ComboLayout::Vertical => ComboLayout::Horizontal,
        }
    }
}

impl From<LayoutSettingsWire> for LayoutSettings {
    fn from(w: LayoutSettingsWire) -> Self {
        Self {
            layout: serde_json::from_value(w.layout).unwrap_or_default(),
        }
    }
}

impl From<LayoutSettings> for LayoutSettingsWire {
    fn from(s: LayoutSettings) -> Self {
        Self {
            layout: json!(s.layout),
        }
    }
}

/// "46% · 6h 12m"; just "—" when there's no data at all, rather than a
/// dangling "— · no data".
fn strip_value(display: &UsageDisplay) -> String {
    if display.percent_text == "\u{2014}" {
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
    fn empty_json_is_horizontal() {
        let s: LayoutSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.layout, ComboLayout::Horizontal);
    }

    #[test]
    fn vertical_parses() {
        let s: LayoutSettings = serde_json::from_str(r#"{"layout":"vertical"}"#).unwrap();
        assert_eq!(s.layout, ComboLayout::Vertical);
    }

    #[test]
    fn garbage_layout_falls_back() {
        for json in [
            r#"{"layout":7}"#,
            r#"{"layout":"diagonal"}"#,
            r#"{"layout":null}"#,
        ] {
            let s: LayoutSettings = serde_json::from_str(json).unwrap();
            assert_eq!(s.layout, ComboLayout::Horizontal, "for {json}");
        }
    }

    #[test]
    fn flipped_toggles_and_round_trips() {
        assert_eq!(ComboLayout::Horizontal.flipped(), ComboLayout::Vertical);
        assert_eq!(
            ComboLayout::Vertical.flipped().flipped(),
            ComboLayout::Vertical
        );
        let v = serde_json::to_value(LayoutSettings {
            layout: ComboLayout::Vertical,
        })
        .unwrap();
        assert_eq!(v, json!({"layout": "vertical"}));
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
