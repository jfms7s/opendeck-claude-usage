//! Usage Gauge keypad styles and the short-press cycle through them. Pure,
//! so the cycle rules and the lenient settings wire format are unit-testable.

use std::time::Duration;

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

/// Every style, in the order the Property Inspector lists them and the
/// default cycle visits them.
pub const ALL_STYLES: [GaugeStyle; 6] = [
    GaugeStyle::Speedometer,
    GaugeStyle::Bar,
    GaugeStyle::SoftPill,
    GaugeStyle::OpenDonut,
    GaugeStyle::TrackedDonut,
    GaugeStyle::ThinRing,
];

/// Held at least this long, a press refreshes instead of cycling.
pub const LONG_PRESS: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Short,
    Long,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "StyleSettingsWire", into = "StyleSettingsWire")]
pub struct StyleSettings {
    /// The style the key currently shows - changed only by a short press.
    pub style: GaugeStyle,
    /// The styles a short press visits, in order.
    pub cycle: Vec<GaugeStyle>,
}

impl Default for StyleSettings {
    fn default() -> Self {
        Self {
            style: GaugeStyle::default(),
            cycle: ALL_STYLES.to_vec(),
        }
    }
}

/// Raw `Value` fields for the same reason as `level::ColorSettingsWire`:
/// a malformed value must fall back on its own rather than make openaction
/// reset the key's whole settings struct.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StyleSettingsWire {
    style: Value,
    cycle_styles: Value,
}

/// The style a short press moves to: the one after `current` in `cycle`
/// (wrapping), or `cycle[0]` if `current` was unticked since. `None` when
/// fewer than two styles are ticked - there's nothing to cycle between.
pub fn next_style(current: GaugeStyle, cycle: &[GaugeStyle]) -> Option<GaugeStyle> {
    if cycle.len() < 2 {
        return None;
    }
    Some(match cycle.iter().position(|s| *s == current) {
        Some(i) => cycle[(i + 1) % cycle.len()],
        None => cycle[0],
    })
}

/// `None` means `key_up` arrived with no recorded `key_down` (e.g. the
/// plugin restarted mid-press) - treated as the cheaper, reversible short
/// press.
pub fn classify_press(held: Option<Duration>) -> Press {
    match held {
        Some(d) if d >= LONG_PRESS => Press::Long,
        _ => Press::Short,
    }
}

impl From<StyleSettingsWire> for StyleSettings {
    fn from(w: StyleSettingsWire) -> Self {
        // A missing or non-array value means "never configured": cycle
        // everything. An array - even one that filters down to nothing - is
        // the user's explicit choice, and unticking every box means "don't
        // cycle", not "cycle all six".
        let cycle = match w.cycle_styles.as_array() {
            None => ALL_STYLES.to_vec(),
            Some(entries) => {
                let mut cycle: Vec<GaugeStyle> = Vec::new();
                for entry in entries {
                    if let Ok(style) = serde_json::from_value::<GaugeStyle>(entry.clone())
                        && !cycle.contains(&style)
                    {
                        cycle.push(style);
                    }
                }
                cycle
            }
        };
        Self {
            style: serde_json::from_value(w.style).unwrap_or_default(),
            cycle,
        }
    }
}

impl From<StyleSettings> for StyleSettingsWire {
    fn from(s: StyleSettings) -> Self {
        Self {
            style: json!(s.style),
            cycle_styles: json!(s.cycle),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GaugeStyle::*;

    #[test]
    fn next_wraps_around() {
        assert_eq!(next_style(Bar, &[Speedometer, Bar]), Some(Speedometer));
        assert_eq!(next_style(Speedometer, &[Speedometer, Bar]), Some(Bar));
    }

    #[test]
    fn next_skips_unticked_styles() {
        assert_eq!(next_style(Bar, &[Bar, ThinRing]), Some(ThinRing));
    }

    #[test]
    fn current_not_in_cycle_goes_to_first() {
        assert_eq!(
            next_style(Speedometer, &[OpenDonut, ThinRing]),
            Some(OpenDonut)
        );
    }

    #[test]
    fn single_style_cycle_has_no_next() {
        assert_eq!(next_style(Bar, &[Bar]), None);
        assert_eq!(next_style(Bar, &[]), None);
    }

    #[test]
    fn press_threshold_is_500ms() {
        assert_eq!(
            classify_press(Some(Duration::from_millis(499))),
            Press::Short
        );
        assert_eq!(
            classify_press(Some(Duration::from_millis(500))),
            Press::Long
        );
    }

    #[test]
    fn no_key_down_is_short() {
        assert_eq!(classify_press(None), Press::Short);
    }

    #[test]
    fn empty_json_is_the_default() {
        let s: StyleSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, StyleSettings::default());
        assert_eq!(s.cycle, ALL_STYLES.to_vec());
    }

    #[test]
    fn unknown_style_falls_back_to_speedometer() {
        let s: StyleSettings = serde_json::from_str(r#"{"style":"hologram"}"#).unwrap();
        assert_eq!(s.style, Speedometer);
    }

    #[test]
    fn unknown_and_duplicate_cycle_entries_are_dropped() {
        let s: StyleSettings =
            serde_json::from_str(r#"{"cycleStyles":["thinRing","nope","bar","thinRing",3]}"#)
                .unwrap();
        assert_eq!(s.cycle, vec![ThinRing, Bar]);
    }

    #[test]
    fn missing_or_non_array_cycle_falls_back_to_all() {
        for json in [
            r#"{}"#,
            r#"{"cycleStyles":"bar"}"#,
            r#"{"cycleStyles":null}"#,
        ] {
            let s: StyleSettings = serde_json::from_str(json).unwrap();
            assert_eq!(s.cycle, ALL_STYLES.to_vec(), "for {json}");
        }
    }

    #[test]
    fn explicitly_empty_cycle_means_no_cycling() {
        // Unticking every box must stop cycling, not turn on all six.
        for json in [r#"{"cycleStyles":[]}"#, r#"{"cycleStyles":["x"]}"#] {
            let s: StyleSettings = serde_json::from_str(json).unwrap();
            assert!(s.cycle.is_empty(), "for {json}");
            assert_eq!(next_style(s.style, &s.cycle), None);
        }
    }

    #[test]
    fn wire_round_trips() {
        let s = StyleSettings {
            style: SoftPill,
            cycle: vec![SoftPill, OpenDonut],
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["style"], "softPill");
        assert_eq!(v["cycleStyles"], json!(["softPill", "openDonut"]));
        let back: StyleSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    /// The Property Inspector's checkboxes must offer exactly the wire
    /// names the plugin accepts.
    #[test]
    fn property_inspector_lists_every_style() {
        let html = include_str!("../assets/propertyInspector/index.html");
        for style in ALL_STYLES {
            let name = serde_json::to_value(style).unwrap();
            let needle = format!(r#"value="{}""#, name.as_str().unwrap());
            assert!(html.contains(&needle), "index.html is missing {needle}");
        }
    }
}
