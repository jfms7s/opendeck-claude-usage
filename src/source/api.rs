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
/// OAuth access token Claude Code keeps in `~/.claude/.credentials.json` on
/// Linux and in the login Keychain on macOS (see `CredentialsLocation`).
/// Unthrottled on its own - wrap it in `CachedSource`.
///
/// The token is only ever read and sent in the `Authorization` header;
/// it is never refreshed here. Refreshing rotates the refresh token, which
/// would silently log Claude Code out - an expired token instead surfaces
/// as a `Credentials` error until Claude Code (CLI, desktop app or IDE
/// extension) next refreshes it itself.
pub struct ApiUsageSource {
    credentials: CredentialsLocation,
    client: reqwest::Client,
}

impl ApiUsageSource {
    pub fn new(credentials: CredentialsLocation) -> Self {
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
            credentials,
            client,
        }
    }

    /// `~/.claude/.credentials.json` - where Claude Code stores its OAuth
    /// login on Linux (and on macOS when it can't use the Keychain).
    pub fn default_credentials_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        PathBuf::from(home).join(".claude/.credentials.json")
    }
}

impl Default for ApiUsageSource {
    fn default() -> Self {
        Self::new(CredentialsLocation::platform_default())
    }
}

/// Where Claude Code's OAuth login (the credentials JSON) is read from.
pub enum CredentialsLocation {
    /// A credentials file - Claude Code's store on Linux.
    File(PathBuf),
    /// The macOS login Keychain, read the way Claude Code itself reads it.
    Keychain(Keychain),
}

/// Claude Code writes its login with `/usr/bin/security add-generic-password`,
/// so the item's access list trusts that tool: reading it through the same
/// tool needs no Keychain prompt, even after this plugin is updated.
pub struct Keychain {
    pub service: String,
    pub account: String,
    /// `/usr/bin/security`; a field so tests can stand in a script.
    pub security: PathBuf,
    /// Read when the Keychain has no item (`security` exit 44): Claude Code
    /// falls back to the credentials file when it can't use the Keychain.
    pub fallback: PathBuf,
    /// Longer than Claude Code's own 10 s, so that if a prompt ever does
    /// appear there is time to answer it.
    pub timeout: Duration,
}

/// `security`'s exit status for "The specified item could not be found".
const SECURITY_ITEM_NOT_FOUND: i32 = 44;
const NO_LOGIN: &str = "no Claude Code login in the Keychain or ~/.claude/.credentials.json";

/// The Keychain account Claude Code uses: `$USER`, unless it holds anything
/// outside `[A-Za-z0-9._-]` (or is empty/unset), then a fixed name.
fn keychain_account(user: Option<&str>) -> String {
    match user {
        Some(u)
            if !u.is_empty()
                && u.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) =>
        {
            u.to_string()
        }
        _ => "claude-code-user".to_string(),
    }
}

impl CredentialsLocation {
    /// The Keychain on macOS, the credentials file elsewhere.
    pub fn platform_default() -> Self {
        let file = ApiUsageSource::default_credentials_path();
        if cfg!(target_os = "macos") {
            let user = std::env::var("USER").ok();
            Self::Keychain(Keychain {
                service: "Claude Code-credentials".to_string(),
                account: keychain_account(user.as_deref()),
                security: PathBuf::from("/usr/bin/security"),
                fallback: file,
                timeout: Duration::from_secs(30),
            })
        } else {
            Self::File(file)
        }
    }

    /// The credentials JSON. Errors never carry what `security` printed:
    /// on success its output is the token itself.
    pub async fn read(&self) -> Result<String, UsageSourceError> {
        match self {
            Self::File(path) => tokio::fs::read_to_string(path)
                .await
                .map_err(|e| UsageSourceError::Credentials(e.to_string())),
            Self::Keychain(k) => k.read().await,
        }
    }
}

impl Keychain {
    async fn read(&self) -> Result<String, UsageSourceError> {
        let child = tokio::process::Command::new(&self.security)
            .args([
                "find-generic-password",
                "-a",
                &self.account,
                "-s",
                &self.service,
                "-w",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                UsageSourceError::Credentials(format!("could not run security: {}", e.kind()))
            })?;
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| UsageSourceError::Credentials("Keychain read timed out".to_string()))?
            .map_err(|e| {
                UsageSourceError::Credentials(format!("Keychain read failed: {}", e.kind()))
            })?;
        match output.status.code() {
            Some(0) => String::from_utf8(output.stdout)
                .map(|s| s.trim().to_string())
                .map_err(|_| {
                    UsageSourceError::Credentials("Keychain item is not text".to_string())
                }),
            Some(SECURITY_ITEM_NOT_FOUND) => tokio::fs::read_to_string(&self.fallback)
                .await
                .map_err(|_| UsageSourceError::Credentials(NO_LOGIN.to_string())),
            Some(code) => Err(UsageSourceError::Credentials(format!(
                "Keychain read failed (security exited {code})"
            ))),
            None => Err(UsageSourceError::Credentials(
                "Keychain read failed (security was killed)".to_string(),
            )),
        }
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
        let credentials = self.credentials.read().await?;
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
        let source = ApiUsageSource::new(CredentialsLocation::File(PathBuf::from(
            "/nonexistent/.credentials.json",
        )));
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

#[cfg(test)]
mod keychain_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const JSON: &str = r#"{"claudeAiOauth": {"accessToken": "sk-ant-oat01-secret"}}"#;

    /// A stand-in for `/usr/bin/security`: a shell script with `body`.
    fn fake_security(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("security");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn keychain(security: PathBuf, fallback: PathBuf) -> CredentialsLocation {
        CredentialsLocation::Keychain(Keychain {
            service: "Claude Code-credentials".into(),
            account: "jf".into(),
            security,
            fallback,
            timeout: Duration::from_millis(500),
        })
    }

    #[tokio::test]
    async fn keychain_success_returns_the_json() {
        let dir = tempfile::tempdir().unwrap();
        // Echo the arguments too, so a wrong call shape fails the match.
        let script = format!(
            r#"[ "$*" = "find-generic-password -a jf -s Claude Code-credentials -w" ] || exit 9
printf '%s\n' '{JSON}'"#
        );
        let location = keychain(fake_security(dir.path(), &script), dir.path().join("none"));
        assert_eq!(location.read().await.unwrap(), JSON);
    }

    #[tokio::test]
    async fn keychain_not_found_falls_back_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".credentials.json");
        std::fs::write(&file, JSON).unwrap();
        let location = keychain(fake_security(dir.path(), "exit 44"), file);
        assert_eq!(location.read().await.unwrap(), JSON);
    }

    #[tokio::test]
    async fn keychain_not_found_without_a_file_is_no_login() {
        let dir = tempfile::tempdir().unwrap();
        let location = keychain(
            fake_security(dir.path(), "exit 44"),
            dir.path().join("none"),
        );
        let Err(UsageSourceError::Credentials(msg)) = location.read().await else {
            panic!("expected a Credentials error");
        };
        assert!(msg.contains("no Claude Code login"), "{msg}");
    }

    #[tokio::test]
    async fn keychain_failure_does_not_leak_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = format!("printf '%s' '{JSON}'; echo 'sk-ant-oat01-secret' >&2; exit 51");
        let location = keychain(fake_security(dir.path(), &script), dir.path().join("none"));
        let Err(UsageSourceError::Credentials(msg)) = location.read().await else {
            panic!("expected a Credentials error");
        };
        assert!(msg.contains("51"), "{msg}");
        assert!(!msg.contains("secret") && !msg.contains("sk-ant"), "{msg}");
    }

    #[tokio::test]
    async fn keychain_timeout_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let location = keychain(
            fake_security(dir.path(), "sleep 5"),
            dir.path().join("none"),
        );
        let started = std::time::Instant::now();
        let Err(UsageSourceError::Credentials(msg)) = location.read().await else {
            panic!("expected a Credentials error");
        };
        assert!(msg.contains("timed out"), "{msg}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_missing_security_tool_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let location = keychain(dir.path().join("no-such-tool"), dir.path().join("none"));
        assert!(matches!(
            location.read().await,
            Err(UsageSourceError::Credentials(_))
        ));
    }

    #[test]
    fn account_follows_claude_codes_rule() {
        assert_eq!(keychain_account(Some("jf.ms-7_s")), "jf.ms-7_s");
        assert_eq!(keychain_account(Some("Jane Doe")), "claude-code-user");
        assert_eq!(keychain_account(Some("joão")), "claude-code-user");
        assert_eq!(keychain_account(Some("")), "claude-code-user");
        assert_eq!(keychain_account(None), "claude-code-user");
    }

    #[test]
    fn platform_default_matches_the_os() {
        let location = CredentialsLocation::platform_default();
        if cfg!(target_os = "macos") {
            let CredentialsLocation::Keychain(k) = location else {
                panic!("macOS reads the Keychain");
            };
            assert_eq!(k.service, "Claude Code-credentials");
            assert_eq!(k.security, PathBuf::from("/usr/bin/security"));
            assert!(k.fallback.ends_with(".claude/.credentials.json"));
        } else {
            assert!(
                matches!(location, CredentialsLocation::File(p) if p.ends_with(".claude/.credentials.json"))
            );
        }
    }
}
