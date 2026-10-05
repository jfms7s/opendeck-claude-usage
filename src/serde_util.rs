//! Lenient settings fields. openaction replaces the *whole* settings struct
//! with its default when any field fails to deserialize, so a field that
//! can hold junk falls back on its own instead (KI-10).

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// An unknown value falls back to that field's default.
pub fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// A positive amount sent as a number or a numeric string. Anything else
/// (blank, zero, negative, junk) is `None`.
pub fn positive_or_none<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite() && *n > 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Budget {
        #[serde(default, deserialize_with = "positive_or_none")]
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
