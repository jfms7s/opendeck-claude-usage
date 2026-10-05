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

/// A positive amount sent as a number or a numeric string. Anything else
/// (blank, zero, negative, junk) is `None` - never an error.
pub fn positive_or_none<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite() && *n > 0.0))
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
        use crate::spend::ApiSpendSettings;

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
            &["metric", "range", "refresh_seconds", "source"],
            ("refresh_seconds", json!(30)),
            |s| s.refresh_seconds == 30,
        );
        assert_lenient::<ApiSpendSettings>(
            &with_colors(&["range", "budgetDollars"]),
            ("colorNormal", json!("#123456")),
            |s| s.colors.palette.normal == "#123456",
        );
        assert_lenient::<PeakClockSettings>(
            &["peak_start", "peak_end", "peak_days"],
            ("peak_start", json!("09:30")),
            |s| s.peak_start == "09:30",
        );
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Budget {
        #[serde(default, deserialize_with = "super::positive_or_none")]
        budget: Option<f64>,
    }

    fn budget(json: &str) -> Option<f64> {
        serde_json::from_str::<Budget>(json).unwrap().budget
    }

    #[test]
    fn positive_numbers_and_numeric_strings_are_kept() {
        assert_eq!(budget(r#"{"budget": 50}"#), Some(50.0));
        assert_eq!(budget(r#"{"budget": " 12.5 "}"#), Some(12.5));
    }

    #[test]
    fn blank_zero_negative_or_junk_budgets_are_none() {
        for v in [r#""""#, "0", "-5", r#""abc""#, "null", "true", "[]"] {
            assert_eq!(budget(&format!(r#"{{"budget": {v}}}"#)), None, "{v}");
        }
        assert_eq!(budget("{}"), None);
    }
}
