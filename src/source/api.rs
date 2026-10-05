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
        // The request carries the user's OAuth token, so it goes nowhere but
        // the fixed https URL: no redirects (a 3xx is just a failed
        // request), and never plain http.
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("opendeck-claude-usage/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
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
    // serde_json's message quotes the offending value - which here could be
    // the token itself (e.g. if `claudeAiOauth` were ever stored as a
    // string). Errors are logged, so only say where and what kind.
    let raw: RawCredentials = serde_json::from_str(json).map_err(|e| {
        UsageSourceError::Credentials(format!(
            "unreadable credentials file ({:?} error at line {}, column {})",
            e.classify(),
            e.line(),
            e.column()
        ))
    })?;
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
    /// Missing on some accounts (or in a future response shape): that
    /// only means extra usage isn't enabled, not that nothing parsed.
    #[serde(default)]
    extra_usage: Option<RawExtraUsage>,
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
        monthly: match raw.extra_usage {
            Some(extra) => MonthlyUsage {
                enabled: extra.is_enabled,
                percent: extra.utilization,
                used_dollars: extra.used_credits,
                limit_dollars: extra.monthly_limit,
            },
            None => MonthlyUsage {
                enabled: false,
                percent: None,
                used_dollars: None,
                limit_dollars: None,
            },
        },
    })
}

/// The error for a non-2xx response: only the status (never the body),
/// plus `Retry-After` (in seconds) on a 429.
fn status_error(status: reqwest::StatusCode, retry_after: Option<&str>) -> UsageSourceError {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        UsageSourceError::RateLimited {
            retry_after_secs: retry_after.and_then(|v| v.trim().parse().ok()),
        }
    } else {
        UsageSourceError::Request(format!("HTTP {status}"))
    }
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
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok());
            return Err(status_error(status, retry_after));
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

    // Synthetic: the session/weekly shape is copied from a real response,
    // but the account used to build this has never had extra usage
    // enabled, so the `extra_usage` values are made up (see
    // `parse_snapshot`).
    const ENABLED_MONTHLY: &str = r#"{
        "five_hour": {"utilization": 33.0, "resets_at": "2026-09-13T22:40:00.186282+00:00"},
        "seven_day": {"utilization": 29.0, "resets_at": "2026-09-17T06:00:00.186306+00:00"},
        "extra_usage": {"is_enabled": true, "monthly_limit": 50.0, "used_credits": 12.5, "utilization": 25.0, "currency": "USD"}
    }"#;

    // A trimmed real response, keeping only the fields this plugin reads.
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

    #[test]
    fn a_response_without_extra_usage_is_monthly_off() {
        let json = r#"{
            "five_hour": {"utilization": 1.0, "resets_at": null},
            "seven_day": {"utilization": 2.0, "resets_at": null}
        }"#;
        let snapshot = parse_snapshot(json).unwrap();
        assert_eq!(snapshot.session.percent, 1.0);
        assert!(!snapshot.monthly.enabled);
        assert_eq!(snapshot.monthly.percent, None);
    }

    /// The credentials file holds a refresh token that never expires;
    /// errors are logged, so a parse error must not quote the file.
    #[test]
    fn a_credentials_parse_error_does_not_echo_the_token() {
        for json in [
            r#"{"claudeAiOauth": "sk-ant-oat01-SECRET"}"#,
            r#"{"claudeAiOauth": {"accessToken": ["sk-ant-oat01-SECRET"]}}"#,
            r#"{"claudeAiOauth": {"accessToken": "sk-ant-oat01-SECRET", "expiresAt": "SECRET"}}"#,
            r#"sk-ant-oat01-SECRET"#,
        ] {
            let err = parse_token(json, noon()).unwrap_err();
            assert!(matches!(err, UsageSourceError::Credentials(_)));
            assert!(!err.to_string().contains("SECRET"), "{err}");
        }
    }

    #[test]
    fn a_429_is_rate_limited_with_its_retry_after() {
        use reqwest::StatusCode;
        assert!(matches!(
            status_error(StatusCode::TOO_MANY_REQUESTS, Some("120")),
            UsageSourceError::RateLimited {
                retry_after_secs: Some(120)
            }
        ));
        // An HTTP-date Retry-After isn't used; the backoff still applies.
        assert!(matches!(
            status_error(
                StatusCode::TOO_MANY_REQUESTS,
                Some("Wed, 21 Oct 2026 07:28:00 GMT")
            ),
            UsageSourceError::RateLimited {
                retry_after_secs: None
            }
        ));
        assert!(matches!(
            status_error(StatusCode::FOUND, None),
            UsageSourceError::Request(ref m) if m.contains("302")
        ));
    }

    /// Hits the real endpoint with this machine's real Claude login - run by
    /// hand (`cargo test -- --ignored live_`) before a release to check the
    /// API still matches `parse_snapshot`, never in CI. Record the date and
    /// version of the last run in the vault's known-issues note.
    #[tokio::test]
    #[ignore]
    async fn live_read_against_the_real_api() {
        let snapshot = ApiUsageSource::default().read().await.unwrap();
        println!("{snapshot:?}");
        for (name, window) in [("session", &snapshot.session), ("weekly", &snapshot.weekly)] {
            assert!(
                (0.0..=100.0).contains(&window.percent),
                "{name} percent out of range: {}",
                window.percent
            );
            // An idle window (0%) has no reset time; a used one must.
            assert!(
                window.resets_at.is_some() || window.percent == 0.0,
                "{name} is in use but has no reset time"
            );
        }
    }
}
