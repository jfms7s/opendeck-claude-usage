//! Per-key color model: user-set Watch/Risk/Critical marks (in % used), an
//! editable four-color palette, and the optional pace-based mode. Pure - no
//! I/O - so every rule here is unit-testable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Calm Claude copper - a key only turns a warning color once a mark is
/// crossed ("color when it counts").
pub const DEFAULT_NORMAL: &str = "#d97757";
pub const DEFAULT_WATCH: &str = "#eab308";
pub const DEFAULT_RISK: &str = "#f97316";
pub const DEFAULT_CRITICAL: &str = "#ef4444";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marks {
    pub watch: f64,
    pub risk: f64,
    pub critical: f64,
}

impl Default for Marks {
    fn default() -> Self {
        Self {
            watch: 50.0,
            risk: 75.0,
            critical: 90.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub normal: String,
    pub watch: String,
    pub risk: String,
    pub critical: String,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            normal: DEFAULT_NORMAL.to_string(),
            watch: DEFAULT_WATCH.to_string(),
            risk: DEFAULT_RISK.to_string(),
            critical: DEFAULT_CRITICAL.to_string(),
        }
    }
}

/// Declared in increasing severity so `Ord`/`max` pick the worse level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal,
    Watch,
    Risk,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    #[default]
    Fixed,
    Pace,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "ColorSettingsWire", into = "ColorSettingsWire")]
pub struct ColorSettings {
    pub marks: Marks,
    pub palette: Palette,
    pub mode: ColorMode,
}

/// The flat JSON the Property Inspector reads and writes. Every field is a
/// raw `Value` so a wrong type (a number sent as a string, `null`, garbage)
/// can't fail deserialization: openaction replaces the *whole* settings
/// struct with `Default::default()` on any failure, which would otherwise
/// also silently reset the key's `window`. Conversion to `ColorSettings`
/// falls back per field instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ColorSettingsWire {
    watch: Value,
    risk: Value,
    critical: Value,
    color_normal: Value,
    color_watch: Value,
    color_risk: Value,
    color_critical: Value,
    color_mode: Value,
}

impl Marks {
    /// Clamps each mark to 0..=100; marks that aren't strictly increasing
    /// can't draw sensible zones, so they're replaced wholesale by the
    /// defaults rather than guessed at.
    pub fn sanitized(self) -> Marks {
        let clamped = Marks {
            watch: self.watch.clamp(0.0, 100.0),
            risk: self.risk.clamp(0.0, 100.0),
            critical: self.critical.clamp(0.0, 100.0),
        };
        if clamped.watch < clamped.risk && clamped.risk < clamped.critical {
            clamped
        } else {
            log::warn!("ignoring non-increasing marks {self:?}; using defaults");
            Marks::default()
        }
    }
}

impl Palette {
    pub fn color(&self, level: Level) -> &str {
        match level {
            Level::Normal => &self.normal,
            Level::Watch => &self.watch,
            Level::Risk => &self.risk,
            Level::Critical => &self.critical,
        }
    }
}

impl Level {
    /// Each mark is inclusive: exactly 50% with Watch at 50 is Watch.
    pub fn for_percent(percent: f64, marks: &Marks) -> Level {
        if percent >= marks.critical {
            Level::Critical
        } else if percent >= marks.risk {
            Level::Risk
        } else if percent >= marks.watch {
            Level::Watch
        } else {
            Level::Normal
        }
    }
}

impl ColorSettings {
    /// The level to color a gauge with: actual % only in Fixed mode, the
    /// worse of actual and projected-at-reset in Pace mode. A `None`
    /// projection (too early in the window, or no window length) adds no
    /// extra warning.
    pub fn level(&self, actual: f64, projected: Option<f64>) -> Level {
        match self.mode {
            ColorMode::Fixed => Level::for_percent(actual, &self.marks),
            ColorMode::Pace => self.pace_level(actual, projected),
        }
    }

    /// Pace-based level regardless of `mode` - Burn Rate always colors this
    /// way, since a burn readout colored by actual % alone would contradict
    /// the number it shows.
    pub fn pace_level(&self, actual: f64, projected: Option<f64>) -> Level {
        let by_actual = Level::for_percent(actual, &self.marks);
        match projected {
            Some(p) => by_actual.max(Level::for_percent(p, &self.marks)),
            None => by_actual,
        }
    }
}

/// `#rrggbb`, either case - the only color form settings accept.
pub fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn number_or(value: &Value, default: f64) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite())
        .unwrap_or(default)
}

fn color_or(value: &Value, default: &str) -> String {
    value
        .as_str()
        .filter(|s| is_hex_color(s))
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| default.to_string())
}

impl From<ColorSettingsWire> for ColorSettings {
    fn from(w: ColorSettingsWire) -> Self {
        let d = Marks::default();
        Self {
            marks: Marks {
                watch: number_or(&w.watch, d.watch),
                risk: number_or(&w.risk, d.risk),
                critical: number_or(&w.critical, d.critical),
            }
            .sanitized(),
            palette: Palette {
                normal: color_or(&w.color_normal, DEFAULT_NORMAL),
                watch: color_or(&w.color_watch, DEFAULT_WATCH),
                risk: color_or(&w.color_risk, DEFAULT_RISK),
                critical: color_or(&w.color_critical, DEFAULT_CRITICAL),
            },
            mode: serde_json::from_value(w.color_mode).unwrap_or_default(),
        }
    }
}

impl From<ColorSettings> for ColorSettingsWire {
    fn from(c: ColorSettings) -> Self {
        Self {
            watch: json!(c.marks.watch),
            risk: json!(c.marks.risk),
            critical: json!(c.marks.critical),
            color_normal: json!(c.palette.normal),
            color_watch: json!(c.palette.watch),
            color_risk: json!(c.palette.risk),
            color_critical: json!(c.palette.critical),
            color_mode: json!(c.mode),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(watch: f64, risk: f64, critical: f64) -> Marks {
        Marks {
            watch,
            risk,
            critical,
        }
    }

    #[test]
    fn levels_are_inclusive_at_each_mark() {
        let m = Marks::default();
        assert_eq!(Level::for_percent(0.0, &m), Level::Normal);
        assert_eq!(Level::for_percent(49.9, &m), Level::Normal);
        assert_eq!(Level::for_percent(50.0, &m), Level::Watch);
        assert_eq!(Level::for_percent(74.9, &m), Level::Watch);
        assert_eq!(Level::for_percent(75.0, &m), Level::Risk);
        assert_eq!(Level::for_percent(89.9, &m), Level::Risk);
        assert_eq!(Level::for_percent(90.0, &m), Level::Critical);
        assert_eq!(Level::for_percent(140.0, &m), Level::Critical);
    }

    #[test]
    fn increasing_marks_survive_sanitizing() {
        assert_eq!(marks(10.0, 20.0, 30.0).sanitized(), marks(10.0, 20.0, 30.0));
    }

    #[test]
    fn out_of_range_marks_are_clamped() {
        assert_eq!(
            marks(-5.0, 60.0, 150.0).sanitized(),
            marks(0.0, 60.0, 100.0)
        );
    }

    #[test]
    fn non_increasing_marks_fall_back_to_defaults() {
        assert_eq!(marks(80.0, 60.0, 90.0).sanitized(), Marks::default());
        assert_eq!(marks(50.0, 50.0, 90.0).sanitized(), Marks::default());
        assert_eq!(marks(f64::NAN, 60.0, 90.0).sanitized(), Marks::default());
    }

    #[test]
    fn palette_maps_each_level() {
        let p = Palette::default();
        assert_eq!(p.color(Level::Normal), DEFAULT_NORMAL);
        assert_eq!(p.color(Level::Watch), DEFAULT_WATCH);
        assert_eq!(p.color(Level::Risk), DEFAULT_RISK);
        assert_eq!(p.color(Level::Critical), DEFAULT_CRITICAL);
    }

    #[test]
    fn fixed_mode_ignores_projection() {
        let c = ColorSettings::default();
        assert_eq!(c.level(30.0, Some(140.0)), Level::Normal);
    }

    #[test]
    fn pace_mode_takes_the_worse_level() {
        let c = ColorSettings {
            mode: ColorMode::Pace,
            ..ColorSettings::default()
        };
        assert_eq!(c.level(30.0, Some(80.0)), Level::Risk);
        assert_eq!(c.level(95.0, Some(60.0)), Level::Critical);
        assert_eq!(c.level(30.0, None), Level::Normal);
    }

    #[test]
    fn pace_level_ignores_mode() {
        let c = ColorSettings::default(); // Fixed
        assert_eq!(c.pace_level(30.0, Some(140.0)), Level::Critical);
    }

    #[test]
    fn empty_json_is_the_default() {
        let c: ColorSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(c, ColorSettings::default());
    }

    #[test]
    fn full_wire_format_round_trips() {
        let json = r##"{"watch":40,"risk":60,"critical":80,
            "colorNormal":"#112233","colorWatch":"#445566",
            "colorRisk":"#778899","colorCritical":"#AABBCC","colorMode":"pace"}"##;
        let c: ColorSettings = serde_json::from_str(json).unwrap();
        assert_eq!(c.marks, marks(40.0, 60.0, 80.0));
        assert_eq!(c.palette.critical, "#aabbcc");
        assert_eq!(c.mode, ColorMode::Pace);
        let back: ColorSettings =
            serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn string_numbers_are_accepted() {
        let c: ColorSettings =
            serde_json::from_str(r#"{"watch":"40","risk":" 60 ","critical":80}"#).unwrap();
        assert_eq!(c.marks, marks(40.0, 60.0, 80.0));
    }

    #[test]
    fn garbage_field_falls_back_alone() {
        let c: ColorSettings = serde_json::from_str(
            r##"{"watch":null,"colorWatch":"yellow","colorRisk":"#123456","colorMode":7}"##,
        )
        .unwrap();
        assert_eq!(c.marks, Marks::default());
        assert_eq!(c.palette.watch, DEFAULT_WATCH);
        assert_eq!(c.palette.risk, "#123456");
        assert_eq!(c.mode, ColorMode::Fixed);
    }

    #[test]
    fn empty_number_input_falls_back() {
        let c: ColorSettings = serde_json::from_str(r#"{"watch":""}"#).unwrap();
        assert_eq!(c.marks.watch, 50.0);
    }

    /// colors.js duplicates the defaults for the Property Inspector - fail
    /// if the two ever disagree.
    #[test]
    fn property_inspector_defaults_match() {
        let js = include_str!("../assets/propertyInspector/colors.js");
        let m = Marks::default();
        for needle in [
            format!("watch: {}", m.watch),
            format!("risk: {}", m.risk),
            format!("critical: {}", m.critical),
            format!("colorNormal: \"{DEFAULT_NORMAL}\""),
            format!("colorWatch: \"{DEFAULT_WATCH}\""),
            format!("colorRisk: \"{DEFAULT_RISK}\""),
            format!("colorCritical: \"{DEFAULT_CRITICAL}\""),
            "colorMode: \"fixed\"".to_string(),
        ] {
            assert!(js.contains(&needle), "colors.js is missing `{needle}`");
        }
    }

    /// colors.js's marks warning and `Marks::sanitized` must agree on
    /// which marks the key actually uses: both are checked against the
    /// same cases (the JS side in tests/pi/colors.test.mjs). The PI's
    /// KI-11/12/13 behaviour itself is exercised there, under node.
    #[test]
    fn sanitized_marks_match_the_shared_pi_cases() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/pi/marks-cases.json")).unwrap();
        for case in cases["cases"].as_array().unwrap() {
            let m: Vec<f64> = serde_json::from_value(case["marks"].clone()).unwrap();
            let given = marks(m[0], m[1], m[2]);
            let clamped = marks(
                m[0].clamp(0.0, 100.0),
                m[1].clamp(0.0, 100.0),
                m[2].clamp(0.0, 100.0),
            );
            let expected = if case["used"].as_bool().unwrap() {
                clamped
            } else {
                Marks::default()
            };
            assert_eq!(given.sanitized(), expected, "marks {m:?}");
        }
    }
}
