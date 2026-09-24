use super::{MonthlyUsage, UsageSnapshot, UsageSource, UsageSourceError, WindowUsage};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

/// The endpoint Claude Code's own `/usage` reads. Undocumented, so its
/// shape could change without notice - `parse_snapshot` only depends on
/// the handful of fields it actually needs.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Fetches usage straight from Anthropic's API on every `read`, using the
/// OAuth access token Claude Code keeps in `~/.claude/.credentials.json`.
/// Unthrottled on its own - wrap it in `CachedUsageSource`.
///
/// The token is only ever read and sent in the `Authorization` header;
/// it is never refreshed here. Refreshing rotates the refresh token, which
/// would silently log Claude Code out - an expired token instead surfaces
/// as a `Credentials` error until Claude Code (CLI, desktop app or IDE
/// extension) next refreshes it itself.
pub struct ApiUsageSource {
    credentials_path: PathBuf,
    client: reqwest::Client,
}

impl ApiUsageSource {
    pub fn new(credentials_path: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("opendeck-claude-usage/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client with static config");
        Self {
            credentials_path,
            client,
        }
    }

    /// `~/.claude/.credentials.json` - where Claude Code stores its OAuth
    /// login on Linux.
    pub fn default_credentials_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        PathBuf::from(home).join(".claude/.credentials.json")
    }
}

impl Default for ApiUsageSource {
    fn default() -> Self {
        Self::new(Self::default_credentials_path())
    }
}

#[derive(Deserialize)]
struct RawCredentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: RawOauth,
}

#[derive(Deserialize)]
struct RawOauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// Unix epoch milliseconds.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
}

/// Pulls the access token out of the credentials file's contents, refusing
/// one that has already expired rather than sending a request that can
/// only come back 401.
fn parse_token(json: &str, now: DateTime<Utc>) -> Result<String, UsageSourceError> {
    let raw: RawCredentials = serde_json::from_str(json)
        .map_err(|e| UsageSourceError::Credentials(format!("unreadable credentials: {e}")))?;
    if let Some(ms) = raw.oauth.expires_at
        && ms <= now.timestamp_millis()
    {
        return Err(UsageSourceError::Credentials(
            "access token expired; it renews next time Claude Code runs".to_string(),
        ));
    }
    Ok(raw.oauth.access_token)
}

#[derive(Deserialize)]
struct RawUsage {
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
    let raw: RawUsage =
        serde_json::from_str(json).map_err(|e| UsageSourceError::Parse(e.to_string()))?;
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
impl UsageSource for ApiUsageSource {
    async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
        let credentials = tokio::fs::read_to_string(&self.credentials_path)
            .await
            .map_err(|e| UsageSourceError::Credentials(e.to_string()))?;
        let token = parse_token(&credentials, Utc::now())?;

        let response = self
            .client
            .get(USAGE_URL)
            .bearer_auth(token)
            .header("anthropic-beta", OAUTH_BETA)
            .send()
            .await
            .map_err(|e| UsageSourceError::Request(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(UsageSourceError::Request(format!("HTTP {status}")));
        }
        let body = response
            .text()
            .await
            .map_err(|e| UsageSourceError::Request(e.to_string()))?;
        parse_snapshot(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // A trimmed real response from the usage endpoint, keeping only the
    // fields this plugin reads.
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

    fn noon() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 23, 12, 0, 0).unwrap()
    }

    fn credentials_expiring_at(at: DateTime<Utc>) -> String {
        format!(
            r#"{{"claudeAiOauth": {{"accessToken": "tok-123", "refreshToken": "r", "expiresAt": {}}}}}"#,
            at.timestamp_millis()
        )
    }

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

    #[test]
    fn an_unexpired_token_is_returned() {
        let json = credentials_expiring_at(noon() + chrono::Duration::hours(1));
        assert_eq!(parse_token(&json, noon()).unwrap(), "tok-123");
    }

    #[test]
    fn an_expired_token_is_a_credentials_error() {
        let json = credentials_expiring_at(noon() - chrono::Duration::seconds(1));
        let result = parse_token(&json, noon());
        assert!(matches!(result, Err(UsageSourceError::Credentials(_))));
    }

    #[test]
    fn a_token_without_an_expiry_is_trusted() {
        let json = r#"{"claudeAiOauth": {"accessToken": "tok-123"}}"#;
        assert_eq!(parse_token(json, noon()).unwrap(), "tok-123");
    }

    #[test]
    fn credentials_without_an_oauth_login_are_a_credentials_error() {
        let result = parse_token(r#"{"somethingElse": {}}"#, noon());
        assert!(matches!(result, Err(UsageSourceError::Credentials(_))));
    }

    #[tokio::test]
    async fn a_missing_credentials_file_fails_before_any_request() {
        let source = ApiUsageSource::new(PathBuf::from("/nonexistent/.credentials.json"));
        let result = source.read().await;
        assert!(matches!(result, Err(UsageSourceError::Credentials(_))));
    }

    /// Hits the real endpoint with this machine's real Claude login - run by
    /// hand (`cargo test -- --ignored live_`) to check the API still matches
    /// `parse_snapshot`, never in CI.
    #[tokio::test]
    #[ignore]
    async fn live_read_against_the_real_api() {
        let snapshot = ApiUsageSource::default().read().await.unwrap();
        println!("{snapshot:?}");
        assert!(snapshot.session.resets_at.is_some());
    }
}
