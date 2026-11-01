//! Usage Gauge keypad styles and the short-press cycle through all six.
//! Pure, so the cycle and the lenient settings wire format are unit-testable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GaugeStyle {
    #[default]
    Speedometer,
    Bar,
    SoftPill,
    OpenDonut,
    TrackedDonut,
    ThinRing,
}

/// Every style, in the order a short press visits them.
pub const ALL_STYLES: [GaugeStyle; 6] = [
    GaugeStyle::Speedometer,
    GaugeStyle::Bar,
    GaugeStyle::SoftPill,
    GaugeStyle::OpenDonut,
    GaugeStyle::TrackedDonut,
    GaugeStyle::ThinRing,
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "StyleSettingsWire", into = "StyleSettingsWire")]
pub struct StyleSettings {
    /// The style the key currently shows - changed only by a short press.
    pub style: GaugeStyle,
}

/// A raw `Value` for the same reason as `level::ColorSettingsWire`: a
/// malformed style must fall back on its own rather than make openaction
/// reset the key's whole settings struct. A `cycleStyles` list saved
/// before 0.19 (the removed Cycle styles checklist) is ignored like any
/// other unknown key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StyleSettingsWire {
    style: Value,
}

/// The style a short press moves to: the one after `current` in
/// `ALL_STYLES`, wrapping.
pub fn next_style(current: GaugeStyle) -> GaugeStyle {
    let i = ALL_STYLES
        .iter()
        .position(|s| *s == current)
        .unwrap_or_default();
    ALL_STYLES[(i + 1) % ALL_STYLES.len()]
}

impl From<StyleSettingsWire> for StyleSettings {
    fn from(w: StyleSettingsWire) -> Self {
        Self {
            style: serde_json::from_value(w.style).unwrap_or_default(),
        }
    }
}

impl From<StyleSettings> for StyleSettingsWire {
    fn from(s: StyleSettings) -> Self {
        Self {
            style: json!(s.style),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GaugeStyle::*;

    #[test]
    fn six_presses_visit_every_style_once() {
        let mut style = Speedometer;
        let mut seen = Vec::new();
        for _ in 0..6 {
            style = next_style(style);
            seen.push(style);
        }
        assert_eq!(style, Speedometer);
        assert_eq!(
            seen,
            [
                Bar,
                SoftPill,
                OpenDonut,
                TrackedDonut,
                ThinRing,
                Speedometer
            ]
        );
    }

    #[test]
    fn empty_json_is_the_default() {
        let s: StyleSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, StyleSettings::default());
        assert_eq!(s.style, Speedometer);
    }

    #[test]
    fn unknown_style_falls_back_to_speedometer() {
        for json in [
            r#"{"style":"hologram"}"#,
            r#"{"style":3}"#,
            r#"{"style":null}"#,
        ] {
            let s: StyleSettings = serde_json::from_str(json).unwrap();
            assert_eq!(s.style, Speedometer, "for {json}");
        }
    }

    /// Keys saved before 0.19 may hold a `cycleStyles` list (even a subset
    /// or an empty one): it is ignored and every press moves through all six.
    #[test]
    fn stored_cycle_styles_is_ignored() {
        let s: StyleSettings =
            serde_json::from_str(r#"{"style":"bar","cycleStyles":["bar"]}"#).unwrap();
        assert_eq!(s.style, Bar);
        assert_eq!(next_style(s.style), SoftPill);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v, json!({"style": "bar"}));
    }

    #[test]
    fn wire_round_trips() {
        let s = StyleSettings { style: SoftPill };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["style"], "softPill");
        let back: StyleSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }
}
