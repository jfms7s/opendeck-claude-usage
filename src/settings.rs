//! Lenient settings fields. openaction replaces an action's *whole*
//! settings struct with `Default::default()` when the stored JSON fails to
//! deserialize, so one unknown or mistyped value (a downgrade, a hand-edited
//! profile, a PI that sent `""`) would wipe every other setting of that
//! key, colors, marks and styles included (KI-10). Every settings field
//! therefore falls back on its own instead:
//!
//! - plain fields use `#[serde(default, deserialize_with = "lenient")]`;
//! - fields that need their own fallback rules (marks that must increase,
//!   the style cycle list) go through a `*Wire` struct of raw `Value`s
//!   (`level::ColorSettingsWire`, `gauge_style::StyleSettingsWire`).

use serde::{Deserialize, Deserializer};

/// Deserializes the field as `T`, or `T::default()` if the stored value
/// isn't a valid `T` - never an error.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    /// Values a stored setting can turn out to be: wrong type, empty,
    /// unknown variant, out of range, nested garbage.
    fn garbage() -> Vec<Value> {
        vec![
            json!(null),
            json!(""),
            json!("bogus"),
            json!(7),
            json!(-1),
            json!(1.5e300),
            json!(true),
            json!([]),
            json!(["bogus", 3]),
            json!({}),
            json!({"nested": "object"}),
        ]
    }

    /// Every field of `T`, set one at a time to each garbage value (with a
    /// valid `probe` field alongside), must still deserialize - and keep
    /// the probe.
    fn assert_lenient<T: DeserializeOwned>(
        fields: &[&str],
        probe: (&str, Value),
        keeps_probe: impl Fn(&T) -> bool,
    ) {
        for field in fields {
            for bad in garbage() {
                let mut settings = serde_json::Map::new();
                settings.insert(probe.0.to_string(), probe.1.clone());
                if *field != probe.0 {
                    settings.insert((*field).to_string(), bad.clone());
                }
                let json = Value::Object(settings);
                let parsed: T = serde_json::from_value(json.clone()).unwrap_or_else(|e| {
                    panic!("{} rejected {json}: {e}", std::any::type_name::<T>())
                });
                assert!(
                    keeps_probe(&parsed),
                    "{} lost {} when given {json}",
                    std::any::type_name::<T>(),
                    probe.0
                );
            }
        }
    }

    const COLOR_FIELDS: [&str; 8] = [
        "watch",
        "risk",
        "critical",
        "colorNormal",
        "colorWatch",
        "colorRisk",
        "colorCritical",
        "colorMode",
    ];

    fn with_colors(fields: &[&'static str]) -> Vec<&'static str> {
        fields.iter().copied().chain(COLOR_FIELDS).collect()
    }

    /// One table for every action: no stored value can reset a key's
    /// other settings (KI-10).
    #[test]
    fn no_settings_field_can_reset_the_others() {
        use crate::burn_action::BurnRateSettings;
        use crate::clock_action::PeakClockSettings;
        use crate::combo_action::ComboSettings;
        use crate::gauge_action::UsageGaugeSettings;
        use crate::heatmap::HeatmapSettings;
        use crate::metric_action::MetricTileSettings;
        use crate::sparkline_action::SparklineSettings;

        let probe_color = ("colorNormal", json!("#123456"));
        assert_lenient::<UsageGaugeSettings>(
            &with_colors(&["window", "style", "cycleStyles"]),
            probe_color.clone(),
            |s| s.colors.palette.normal == "#123456",
        );
        assert_lenient::<BurnRateSettings>(
            &with_colors(&["window", "metric"]),
            probe_color.clone(),
            |s| s.colors.palette.normal == "#123456",
        );
        assert_lenient::<ComboSettings>(&with_colors(&["layout"]), probe_color.clone(), |s| {
            s.colors.palette.normal == "#123456"
        });
        assert_lenient::<SparklineSettings>(
            &with_colors(&["window", "series"]),
            probe_color,
            |s| s.colors.palette.normal == "#123456",
        );
        assert_lenient::<HeatmapSettings>(
            &["metric", "view", "color"],
            ("color", json!("#123456")),
            |s| s.color == "#123456",
        );
        assert_lenient::<MetricTileSettings>(
            &["metric", "range", "refresh_seconds"],
            ("refresh_seconds", json!(30)),
            |s| s.refresh_seconds == 30,
        );
        assert_lenient::<PeakClockSettings>(
            &["peak_start", "peak_end", "peak_days"],
            ("peak_start", json!("09:30")),
            |s| s.peak_start == "09:30",
        );
    }
}
