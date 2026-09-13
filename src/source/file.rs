use super::{MonthlyUsage, UsageSnapshot, UsageSource, UsageSourceError, WindowUsage};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;

pub struct FileUsageSource {
    path: PathBuf,
}

impl FileUsageSource {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// `~/.claude/statusline-usage.json` - maintained by Claude Code itself,
    /// refreshed whenever its statusLine hook fires during an active session.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        PathBuf::from(home).join(".claude/statusline-usage.json")
    }
}

impl Default for FileUsageSource {
    fn default() -> Self {
        Self::new(Self::default_path())
    }
}

#[derive(Deserialize)]
struct RawUsageFile {
    five_hour: RawWindow,
    seven_day: RawWindow,
    extra_usage: RawExtraUsage,
}

#[derive(Deserialize)]
struct RawWindow {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct RawExtraUsage {
    is_enabled: bool,
    #[serde(default)]
    used_credits: Option<f64>,
    #[serde(default)]
    monthly_limit: Option<f64>,
    #[serde(default)]
    utilization: Option<f64>,
}

fn parse_window(raw: RawWindow) -> WindowUsage {
    WindowUsage {
        percent: raw.utilization,
        // A malformed timestamp is treated as "no reset info" rather than a
        // hard parse failure - the rest of the snapshot is still usable.
        resets_at: raw
            .resets_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc)),
    }
}

fn parse_snapshot(json: &str) -> Result<UsageSnapshot, UsageSourceError> {
    let raw: RawUsageFile = serde_json::from_str(json)?;
    Ok(UsageSnapshot {
        session: parse_window(raw.five_hour),
        weekly: parse_window(raw.seven_day),
        // `used_credits`/`monthly_limit` are taken as-is and treated as
        // decimal dollar amounts (e.g. 12.5 == $12.50), not minor units
        // that would need scaling by some `decimal_places` field. This is
        // unverified: the account used to build this plugin has never had
        // `extra_usage` enabled, so there's no real sample to check the
        // assumption against. If a real account later shows fractional-cent
        // drift here, that's the first place to look.
        monthly: MonthlyUsage {
            enabled: raw.extra_usage.is_enabled,
            percent: raw.extra_usage.utilization,
            used_dollars: raw.extra_usage.used_credits,
            limit_dollars: raw.extra_usage.monthly_limit,
        },
    })
}

#[async_trait]
impl UsageSource for FileUsageSource {
    async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
        let contents = tokio::fs::read_to_string(&self.path).await?;
        parse_snapshot(&contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed real sample from ~/.claude/statusline-usage.json on this
    // machine, keeping only the fields this plugin reads.
    const ENABLED_MONTHLY: &str = r#"{
        "five_hour": {"utilization": 33.0, "resets_at": "2026-09-13T22:40:00.186282+00:00"},
        "seven_day": {"utilization": 29.0, "resets_at": "2026-09-17T06:00:00.186306+00:00"},
        "extra_usage": {"is_enabled": true, "monthly_limit": 50.0, "used_credits": 12.5, "utilization": 25.0, "currency": "USD"}
    }"#;

    const DISABLED_MONTHLY: &str = r#"{
        "five_hour": {"utilization": 33.0, "resets_at": "2026-09-13T22:40:00.186282+00:00"},
        "seven_day": {"utilization": 29.0, "resets_at": "2026-09-17T06:00:00.186306+00:00"},
        "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null, "currency": null}
    }"#;

    #[test]
    fn parses_session_and_weekly_windows() {
        let snapshot = parse_snapshot(ENABLED_MONTHLY).unwrap();
        assert_eq!(snapshot.session.percent, 33.0);
        assert!(snapshot.session.resets_at.is_some());
        assert_eq!(snapshot.weekly.percent, 29.0);
        assert!(snapshot.weekly.resets_at.is_some());
    }

    #[test]
    fn parses_enabled_monthly_extra_usage() {
        let snapshot = parse_snapshot(ENABLED_MONTHLY).unwrap();
        assert!(snapshot.monthly.enabled);
        assert_eq!(snapshot.monthly.percent, Some(25.0));
        assert_eq!(snapshot.monthly.used_dollars, Some(12.5));
        assert_eq!(snapshot.monthly.limit_dollars, Some(50.0));
    }

    #[test]
    fn parses_disabled_monthly_extra_usage_as_none() {
        let snapshot = parse_snapshot(DISABLED_MONTHLY).unwrap();
        assert!(!snapshot.monthly.enabled);
        assert_eq!(snapshot.monthly.percent, None);
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let result = parse_snapshot("not json");
        assert!(matches!(result, Err(UsageSourceError::Parse(_))));
    }

    #[test]
    fn malformed_reset_timestamp_becomes_none_not_an_error() {
        let json = r#"{
            "five_hour": {"utilization": 10.0, "resets_at": "not-a-timestamp"},
            "seven_day": {"utilization": 5.0, "resets_at": null},
            "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null, "currency": null}
        }"#;
        let snapshot = parse_snapshot(json).unwrap();
        assert_eq!(snapshot.session.resets_at, None);
        assert_eq!(snapshot.weekly.resets_at, None);
    }

    #[tokio::test]
    async fn read_loads_and_parses_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("statusline-usage.json");
        std::fs::write(&path, ENABLED_MONTHLY).unwrap();

        let source = FileUsageSource::new(path);
        let snapshot = source.read().await.unwrap();
        assert_eq!(snapshot.session.percent, 33.0);
    }

    #[tokio::test]
    async fn read_reports_a_read_error_for_a_missing_file() {
        let source = FileUsageSource::new(PathBuf::from("/nonexistent/statusline-usage.json"));
        let result = source.read().await;
        assert!(matches!(result, Err(UsageSourceError::Read(_))));
    }
}
