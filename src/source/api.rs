use super::{
    Limit, MonthlyUsage, SurfaceShare, UsageDetails, UsageSnapshot, UsageSource, UsageSourceError,
    WindowUsage,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The endpoint Claude Code's own `/usage` reads. Undocumented, so its
/// shape could change without notice - `parse_snapshot` only depends on
/// the handful of fields it actually needs.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Fetches usage straight from Anthropic's API on every `read`, using the
/// OAuth access token Claude Code keeps in `~/.claude/.credentials.json` on
/// Linux and in the login Keychain on macOS (see `CredentialsLocation`).
/// Unthrottled on its own - wrap it in `CachedUsageSource`.
///
/// The token is only ever read and sent in the `Authorization` header;
/// the plugin never refreshes it itself or writes the credentials.
/// Refreshing rotates the refresh token, so a second refresher racing
/// Claude Code could silently log it out. When the token has expired, a
/// `Renewer` (Linux) asks Claude Code to renew it - `claude auth status`
/// refreshes through Claude Code's own code path - and the credentials
/// are read again; otherwise the expiry surfaces as a `Credentials` error
/// until Claude Code (CLI, desktop app or IDE extension) next runs.
pub struct ApiUsageSource {
    credentials: CredentialsLocation,
    renewer: Option<Renewer>,
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
            renewer: None,
            client,
        }
    }

    /// Asks `renewer` to renew an expired token before giving up on it.
    pub fn with_renewer(mut self, renewer: Renewer) -> Self {
        self.renewer = Some(renewer);
        self
    }

    /// The access token to send: read from the credentials, and - if it
    /// has expired - renewed by Claude Code and read again.
    async fn token(&self) -> Result<String, UsageSourceError> {
        let token = read_token(&self.credentials.read().await?, Utc::now())?;
        if !token.expired {
            return Ok(token.access);
        }
        // Without a refresh token there is nothing Claude Code could renew
        // with (e.g. it has just signed out): don't run it.
        let Some(renewer) = self.renewer.as_ref().filter(|_| token.renewable) else {
            return Err(expired(EXPIRED_OPEN_CLAUDE));
        };
        match renewer.renew().await {
            Renewal::CoolingDown => Err(expired(EXPIRED_RETRYING)),
            Renewal::Unavailable => Err(expired(EXPIRED_OPEN_CLAUDE)),
            Renewal::Failed(why) => Err(expired(&format!("{why}; {EXPIRED_OPEN_CLAUDE}"))),
            Renewal::Ran => {
                // Whatever `claude` did, only a fresh token in the
                // credentials counts.
                let token = read_token(&self.credentials.read().await?, Utc::now())?;
                if token.expired {
                    Err(expired(&format!(
                        "`claude auth status` did not renew it; {EXPIRED_OPEN_CLAUDE}"
                    )))
                } else {
                    log::info!("Claude Code renewed the expired OAuth access token");
                    Ok(token.access)
                }
            }
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
    /// The platform's credentials store; on Linux, an expired token is
    /// renewed through Claude Code. macOS keeps the plain expiry error:
    /// it is frozen until the Mac checklist has been run once (see
    /// `CLAUDE.md`'s Scope).
    fn default() -> Self {
        let source = Self::new(CredentialsLocation::platform_default());
        if cfg!(target_os = "macos") {
            source
        } else {
            source.with_renewer(Renewer::default())
        }
    }
}

const EXPIRED_OPEN_CLAUDE: &str = "open Claude Code to renew it";
const EXPIRED_RETRYING: &str =
    "Claude Code could not renew it less than 10 minutes ago; open Claude Code to renew it now";

fn expired(detail: &str) -> UsageSourceError {
    UsageSourceError::Credentials(format!("access token expired; {detail}"))
}

/// Gets an expired OAuth token renewed by Claude Code itself: runs
/// `claude auth status --json`, which (checked against Claude Code
/// 2.1.310 with fixture credentials) refreshes an expired access token
/// through Claude Code's own refresh path and writes the new tokens back
/// to its store, and sends no token request while the token is fresh.
/// The plugin never uses the refresh token (it only checks one is there),
/// so it can't race Claude Code over a rotated one; concurrent Claude Code
/// processes serialise their refreshes themselves (two simultaneous runs
/// against a rotating fixture endpoint made one token request). Its
/// output (account email, org) is discarded.
pub struct Renewer {
    /// Where to look for `claude` when `PATH` has none (OpenDeck started
    /// from a desktop session often lacks `~/.local/bin`).
    home: Option<PathBuf>,
    /// `PATH` to search, as for `std::env::split_paths`.
    path: Option<std::ffi::OsString>,
    /// A run (its whole process group) is killed after this. The usage
    /// cache waits for it, so no longer than the Keychain read's 10 s.
    timeout: Duration,
    /// At most one run per this long, whatever its outcome. (Finding no
    /// `claude` is not a run.)
    cooldown: Duration,
    last_run: Mutex<Option<Instant>>,
}

/// What a `Renewer::renew` call did.
#[derive(Debug, PartialEq)]
enum Renewal {
    /// `claude auth status` exited 0; read the credentials again.
    Ran,
    /// No `claude` executable was found.
    Unavailable,
    /// A run started within the cooldown; nothing was run.
    CoolingDown,
    /// It could not run, timed out or exited non-zero.
    Failed(String),
}

impl Default for Renewer {
    fn default() -> Self {
        Self::new(
            std::env::var_os("HOME").map(PathBuf::from),
            std::env::var_os("PATH"),
        )
    }
}

impl Renewer {
    pub fn new(home: Option<PathBuf>, path: Option<std::ffi::OsString>) -> Self {
        Self {
            home,
            path,
            timeout: Duration::from_secs(10),
            cooldown: Duration::from_secs(10 * 60),
            last_run: Mutex::new(None),
        }
    }

    /// The `claude` to run: the first executable `claude` in an absolute
    /// `PATH` entry, else the native installer's `~/.local/bin/claude`,
    /// else the older `~/.claude/local/claude`.
    fn claude(&self) -> Option<PathBuf> {
        let on_path = self
            .path
            .iter()
            .flat_map(std::env::split_paths)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("claude"));
        let in_home = self
            .home
            .iter()
            .filter(|home| home.is_absolute())
            .flat_map(|home| {
                [
                    home.join(".local/bin/claude"),
                    home.join(".claude/local/claude"),
                ]
            });
        on_path.chain(in_home).find(|p| is_executable(p))
    }

    async fn renew(&self) -> Renewal {
        let Some(claude) = self.claude() else {
            return Renewal::Unavailable;
        };
        {
            let mut last = self.last_run.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|at| at.elapsed() < self.cooldown) {
                return Renewal::CoolingDown;
            }
            *last = Some(Instant::now());
        }
        log::info!("OAuth access token expired; asking Claude Code to renew it");
        let mut command = tokio::process::Command::new(&claude);
        command
            .args(["auth", "status", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Its own process group, so a timeout kills anything it started.
            .process_group(0)
            // The credentials read here are `~/.claude`'s, so renew those.
            .env_remove("CLAUDE_CONFIG_DIR")
            .kill_on_drop(true);
        // Not the plugin's folder: run where a user's own `claude` would.
        if let Some(home) = self.home.as_deref().filter(|h| h.is_dir()) {
            command.current_dir(home);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => return Renewal::Failed(format!("could not run claude: {}", e.kind())),
        };
        let pid = child.id();
        match tokio::time::timeout(self.timeout, child.wait()).await {
            Err(_) => {
                if let Some(pgid) = pid.and_then(|p| libc::pid_t::try_from(p).ok()) {
                    // SAFETY: plain syscall; the child has not been reaped
                    // (`wait` did not finish), so its id still names its group.
                    unsafe { libc::kill(-pgid, libc::SIGKILL) };
                }
                Renewal::Failed("`claude auth status` timed out".to_string())
            }
            Ok(Err(e)) => Renewal::Failed(format!("`claude auth status` failed: {}", e.kind())),
            Ok(Ok(status)) if status.success() => Renewal::Ran,
            Ok(Ok(status)) => Renewal::Failed(match status.code() {
                Some(code) => format!("`claude auth status` exited {code}"),
                None => "`claude auth status` was killed".to_string(),
            }),
        }
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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
    /// 10 s, like Claude Code (see `keychain_default`).
    pub timeout: Duration,
}

/// `security`'s exit status for "The specified item could not be found".
const SECURITY_ITEM_NOT_FOUND: i32 = 44;
const NO_LOGIN: &str = "no Claude Code login in the Keychain or ~/.claude/.credentials.json";

/// The Keychain account Claude Code uses: `$USER`, or the login name when
/// that is empty or unset (`process.env.USER || os.userInfo().username`),
/// then a fixed name unless it is all `[A-Za-z0-9._-]`.
fn keychain_account(user: Option<&str>, login: Option<&str>) -> String {
    let name = user.filter(|u| !u.is_empty()).or(login).unwrap_or("");
    if !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        name.to_string()
    } else {
        "claude-code-user".to_string()
    }
}

/// This process's login name from the user database (what Node's
/// `os.userInfo().username` returns).
fn login_name() -> Option<String> {
    let mut buf = vec![0u8; 4096];
    // SAFETY: `pwd` is plain data getpwuid_r fills in; `buf` is valid for
    // `buf.len()` bytes and outlives every pointer read from `pwd`.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(pwd.pw_name)
            .to_str()
            .ok()
            .map(str::to_string)
    }
}

/// How Claude Code reads its login on macOS.
fn keychain_default(user: Option<&str>, fallback: Option<PathBuf>) -> Keychain {
    let login = login_name();
    Keychain {
        service: "Claude Code-credentials".to_string(),
        account: keychain_account(user, login.as_deref()),
        security: PathBuf::from("/usr/bin/security"),
        fallback: fallback.unwrap_or_else(ApiUsageSource::default_credentials_path),
        // Claude Code's own limit. A hung read holds up any event handler
        // waiting on the cache, so don't wait longer.
        timeout: Duration::from_secs(10),
    }
}

impl CredentialsLocation {
    /// The Keychain on macOS, the credentials file elsewhere.
    pub fn platform_default() -> Self {
        let file = ApiUsageSource::default_credentials_path();
        if cfg!(target_os = "macos") {
            let user = std::env::var("USER").ok();
            Self::Keychain(keychain_default(user.as_deref(), Some(file)))
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
    /// Only whether there is one: the value is never kept or used.
    #[serde(rename = "refreshToken", default, deserialize_with = "is_present")]
    has_refresh_token: bool,
}

/// True for a non-empty string; any other value (or type) is false, never
/// a parse error.
fn is_present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(matches!(value, serde_json::Value::String(s) if !s.is_empty()))
}

/// The access token from the credentials JSON, and whether it has expired
/// (an expired one is never sent: the request could only come back 401).
struct Token {
    access: String,
    expired: bool,
    /// Whether a refresh token is there for Claude Code to renew with.
    renewable: bool,
}

/// Pulls the access token out of the credentials file's contents, refusing
/// one that has already expired rather than sending a request that can
/// only come back 401.
#[cfg(test)]
fn parse_token(json: &str, now: DateTime<Utc>) -> Result<String, UsageSourceError> {
    let token = read_token(json, now)?;
    if token.expired {
        return Err(expired(EXPIRED_OPEN_CLAUDE));
    }
    Ok(token.access)
}

fn read_token(json: &str, now: DateTime<Utc>) -> Result<Token, UsageSourceError> {
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
    let expired = raw
        .oauth
        .expires_at
        .is_some_and(|ms| ms <= now.timestamp_millis());
    Ok(Token {
        access: raw.oauth.access_token,
        expired,
        renewable: raw.oauth.has_refresh_token,
    })
}

#[derive(Deserialize)]
struct RawUsage {
    five_hour: RawWindow,
    seven_day: RawWindow,
    /// Missing on some accounts (or in a future response shape): that
    /// only means extra usage isn't enabled, not that nothing parsed.
    #[serde(default)]
    extra_usage: Option<RawExtraUsage>,
    /// Taken as raw JSON and read leniently (`parse_details`): a change
    /// in their shape must never fail the windows above.
    #[serde(default)]
    seven_day_breakdown: serde_json::Value,
    #[serde(default)]
    limits: serde_json::Value,
}

#[derive(Deserialize)]
struct RawWindow {
    utilization: f64,
    resets_at: Option<String>,
    #[serde(default)]
    locked_reason: serde_json::Value,
}

/// A non-empty string, or nothing (whatever else the value is).
fn text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn timestamp(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// `seven_day_breakdown.rows` and `limits[]`. A row or entry missing
/// what identifies it (or its share) is skipped, not an error.
fn parse_details(raw: &RawUsage) -> UsageDetails {
    let rows = raw.seven_day_breakdown["rows"].as_array();
    let weekly_shares = rows
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let key = text(&row["key"])?;
            Some(SurfaceShare {
                name: text(&row["display_name"]).unwrap_or_else(|| key.clone()),
                percent: row["percent"].as_f64()?,
                key,
            })
        })
        .collect();
    let limits = raw
        .limits
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(Limit {
                kind: text(&entry["kind"])?,
                group: text(&entry["group"]),
                percent: entry["percent"].as_f64(),
                severity: text(&entry["severity"]),
                resets_at: timestamp(&entry["resets_at"]),
                is_active: entry["is_active"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    UsageDetails {
        weekly_shares,
        limits,
        session_locked: text(&raw.five_hour.locked_reason),
        weekly_locked: text(&raw.seven_day.locked_reason),
    }
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

fn parse_window(raw: &RawWindow) -> WindowUsage {
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
    let details = parse_details(&raw);
    Ok(UsageSnapshot {
        session: parse_window(&raw.five_hour),
        weekly: parse_window(&raw.seven_day),
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
        details,
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
        let token = self.token().await?;

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

    // Synthetic, in the shape of a 2026-09-23 capture of the real response
    // (key names and types copied, values made up), including the fields
    // the plugin ignores: codename windows, `spend`, more `extra_usage`.
    const WITH_DETAILS: &str = r#"{
        "five_hour": {"utilization": 100.0, "resets_at": "2026-09-13T22:40:00+00:00",
            "limit_dollars": null, "used_dollars": null, "remaining_dollars": null,
            "locked_reason": "session_limit"},
        "seven_day": {"utilization": 61.0, "resets_at": "2026-09-17T06:00:00+00:00",
            "limit_dollars": null, "used_dollars": null, "remaining_dollars": null,
            "locked_reason": null},
        "seven_day_opus": null,
        "nimbus_quill": {"utilization": 3.0, "resets_at": null, "locked_reason": null},
        "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null,
            "utilization": null, "currency": null, "decimal_places": null,
            "user_disabled": false, "daily": null},
        "limits": [
            {"kind": "session", "group": "session", "percent": 100, "severity": "critical",
             "resets_at": "2026-09-13T22:40:00+00:00", "scope": null, "is_active": true},
            {"kind": "weekly_all", "group": "weekly", "percent": 61, "severity": "warning",
             "resets_at": "2026-09-17T06:00:00+00:00", "scope": null, "is_active": false}
        ],
        "spend": {"used": {"amount_minor": 0, "currency": "USD", "exponent": 2}, "percent": 0},
        "member_dashboard_available": false,
        "seven_day_breakdown": {
            "as_of": "2026-09-13T20:00:00+00:00",
            "window_started_at": "2026-09-10T06:00:00+00:00",
            "rows": [
                {"key": "claude_code", "display_name": "Claude Code", "percent": 48},
                {"key": "chat", "display_name": "Chats", "percent": 9},
                {"key": "cowork", "display_name": "Cowork", "percent": 0},
                {"key": "other", "display_name": "Other", "percent": 4}
            ]
        }
    }"#;

    #[test]
    fn a_response_without_the_details_parses_as_before() {
        let snapshot = parse_snapshot(DISABLED_MONTHLY).unwrap();
        assert_eq!(snapshot.session.percent, 33.0);
        assert_eq!(snapshot.weekly.percent, 29.0);
        assert_eq!(snapshot.details, UsageDetails::default());
    }

    #[test]
    fn parses_the_weekly_breakdown_limits_and_lock_reason() {
        let snapshot = parse_snapshot(WITH_DETAILS).unwrap();
        assert_eq!(snapshot.session.percent, 100.0);
        assert_eq!(snapshot.weekly.percent, 61.0);
        let d = &snapshot.details;
        let shares: Vec<_> = d
            .weekly_shares
            .iter()
            .map(|s| (s.key.as_str(), s.name.as_str(), s.percent))
            .collect();
        assert_eq!(
            shares,
            [
                ("claude_code", "Claude Code", 48.0),
                ("chat", "Chats", 9.0),
                ("cowork", "Cowork", 0.0),
                ("other", "Other", 4.0),
            ]
        );
        assert_eq!(
            d.limits,
            [
                Limit {
                    kind: "session".into(),
                    group: Some("session".into()),
                    percent: Some(100.0),
                    severity: Some("critical".into()),
                    resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 13, 22, 40, 0).unwrap()),
                    is_active: true,
                },
                Limit {
                    kind: "weekly_all".into(),
                    group: Some("weekly".into()),
                    percent: Some(61.0),
                    severity: Some("warning".into()),
                    resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 17, 6, 0, 0).unwrap()),
                    is_active: false,
                },
            ]
        );
        assert_eq!(d.session_locked.as_deref(), Some("session_limit"));
        assert_eq!(d.weekly_locked, None);
    }

    #[test]
    fn unexpected_detail_shapes_never_fail_the_windows() {
        for details in [
            // Wrong types for every new field.
            r#""seven_day_breakdown": "soon", "limits": {"kind": "session"}"#,
            r#""seven_day_breakdown": {"rows": {"key": "x"}}, "limits": 7"#,
            r#""seven_day_breakdown": null, "limits": null"#,
            // Rows and entries missing what identifies them, or mistyped.
            r#""seven_day_breakdown": {"rows": [{"display_name": "Chats", "percent": 9},
                {"key": "chat", "percent": "9"}, 3, null]},
               "limits": [{"percent": 5}, {"kind": 1}, "weekly", null]"#,
        ] {
            let json = format!(
                r#"{{
                    "five_hour": {{"utilization": 33.0, "resets_at": null, "locked_reason": 5}},
                    "seven_day": {{"utilization": 29.0, "resets_at": null, "locked_reason": ["x"]}},
                    {details}
                }}"#
            );
            let snapshot = parse_snapshot(&json).unwrap_or_else(|e| panic!("{details}: {e}"));
            assert_eq!(snapshot.session.percent, 33.0, "{details}");
            assert_eq!(snapshot.weekly.percent, 29.0, "{details}");
            assert_eq!(snapshot.details, UsageDetails::default(), "{details}");
        }
    }

    #[test]
    fn partial_details_keep_what_parses() {
        let json = r#"{
            "five_hour": {"utilization": 1.0, "resets_at": null, "locked_reason": "  "},
            "seven_day": {"utilization": 2.0, "resets_at": null, "locked_reason": "weekly_limit"},
            "seven_day_breakdown": {"rows": [{"key": "chat", "percent": 9.5}, {"key": "x"}]},
            "limits": [{"kind": "weekly_all", "resets_at": "not a date"}]
        }"#;
        let d = parse_snapshot(json).unwrap().details;
        // No display name: the key stands in for it.
        assert_eq!(
            d.weekly_shares,
            [SurfaceShare {
                key: "chat".into(),
                name: "chat".into(),
                percent: 9.5
            }]
        );
        assert_eq!(
            d.limits,
            [Limit {
                kind: "weekly_all".into(),
                group: None,
                percent: None,
                severity: None,
                resets_at: None,
                is_active: false,
            }]
        );
        assert_eq!(d.session_locked, None);
        assert_eq!(d.weekly_locked.as_deref(), Some("weekly_limit"));
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
        // The details the plugin parses but doesn't draw yet: the
        // response shape is undocumented, so check it still matches.
        let d = &snapshot.details;
        assert!(
            !d.weekly_shares.is_empty(),
            "no seven_day_breakdown rows parsed"
        );
        for share in &d.weekly_shares {
            assert!(
                (0.0..=100.0).contains(&share.percent),
                "{} share out of range: {}",
                share.key,
                share.percent
            );
        }
        assert!(!d.limits.is_empty(), "no limits[] entries parsed");
        for limit in &d.limits {
            assert!(limit.severity.is_some(), "{} has no severity", limit.kind);
            assert!(
                limit.percent.is_some_and(|p| (0.0..=100.0).contains(&p)),
                "{} percent missing or out of range: {:?}",
                limit.kind,
                limit.percent
            );
        }
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

    /// Reads `location`, retrying while the fake script is "busy": another
    /// test spawning a process at the moment a script is written briefly
    /// holds its write handle (ETXTBSY). Only the test scripts are written;
    /// the real `/usr/bin/security` never is.
    async fn read(location: &CredentialsLocation) -> Result<String, UsageSourceError> {
        for _ in 0..50 {
            match location.read().await {
                Err(UsageSourceError::Credentials(msg)) if msg.contains("busy") => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                other => return other,
            }
        }
        location.read().await
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
        assert_eq!(read(&location).await.unwrap(), JSON);
    }

    #[tokio::test]
    async fn keychain_not_found_falls_back_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".credentials.json");
        std::fs::write(&file, JSON).unwrap();
        let location = keychain(fake_security(dir.path(), "exit 44"), file);
        assert_eq!(read(&location).await.unwrap(), JSON);
    }

    #[tokio::test]
    async fn keychain_not_found_without_a_file_is_no_login() {
        let dir = tempfile::tempdir().unwrap();
        let location = keychain(
            fake_security(dir.path(), "exit 44"),
            dir.path().join("none"),
        );
        let Err(UsageSourceError::Credentials(msg)) = read(&location).await else {
            panic!("expected a Credentials error");
        };
        assert!(msg.contains("no Claude Code login"), "{msg}");
    }

    #[tokio::test]
    async fn keychain_failure_does_not_leak_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = format!("printf '%s' '{JSON}'; echo 'sk-ant-oat01-secret' >&2; exit 51");
        let location = keychain(fake_security(dir.path(), &script), dir.path().join("none"));
        let Err(UsageSourceError::Credentials(msg)) = read(&location).await else {
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
        let Err(UsageSourceError::Credentials(msg)) = read(&location).await else {
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
            read(&location).await,
            Err(UsageSourceError::Credentials(_))
        ));
    }

    #[test]
    fn account_follows_claude_codes_rule() {
        // Claude Code: `process.env.USER || os.userInfo().username`, then
        // `claude-code-user` unless it is `^[a-zA-Z0-9._-]+$`.
        assert_eq!(
            keychain_account(Some("jf.ms-7_s"), Some("other")),
            "jf.ms-7_s"
        );
        assert_eq!(
            keychain_account(Some("Jane Doe"), Some("jane")),
            "claude-code-user"
        );
        assert_eq!(keychain_account(Some("joão"), None), "claude-code-user");
        // An empty or unset USER falls back to the login name, like Claude Code.
        assert_eq!(keychain_account(Some(""), Some("jf")), "jf");
        assert_eq!(keychain_account(None, Some("jf")), "jf");
        assert_eq!(keychain_account(None, Some("Jane Doe")), "claude-code-user");
        assert_eq!(keychain_account(None, None), "claude-code-user");
    }

    #[test]
    fn the_keychain_read_times_out_like_claude_code() {
        // A hung read holds up event handlers waiting on the cache, so it
        // must not wait longer than Claude Code's own 10 s.
        let k = keychain_default(Some("jf"), None);
        assert_eq!(k.timeout, Duration::from_secs(10));
        assert_eq!(k.account, "jf");
        assert_eq!(k.service, "Claude Code-credentials");
    }

    #[test]
    fn the_login_name_is_found() {
        assert!(login_name().is_some_and(|n| !n.is_empty()));
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

#[cfg(test)]
mod renewer_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const SECRET: &str = "sk-ant-oat01-FIXTURE-SECRET";

    fn credentials(expires_at: DateTime<Utc>, token: &str) -> String {
        format!(
            r#"{{"claudeAiOauth": {{"accessToken": "{token}", "refreshToken": "sk-ant-ort01-FIXTURE", "expiresAt": {}}}, "other": {{"kept": true}}}}"#,
            expires_at.timestamp_millis()
        )
    }

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        /// A temp home whose credentials file holds an expired token.
        fn expired() -> Self {
            let f = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            let past = Utc::now() - chrono::Duration::hours(1);
            std::fs::write(f.credentials_path(), credentials(past, SECRET)).unwrap();
            f
        }

        fn home(&self) -> PathBuf {
            self.dir.path().to_path_buf()
        }

        fn credentials_path(&self) -> PathBuf {
            self.dir.path().join(".credentials.json")
        }

        fn runs_path(&self) -> PathBuf {
            self.dir.path().join("runs")
        }

        /// How many times the fake `claude` ran.
        fn runs(&self) -> usize {
            std::fs::read_to_string(self.runs_path())
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }

        /// A fake `claude` in `<home>/<rel>` that logs its arguments and
        /// then runs `body`.
        fn claude_at(&self, rel: &str, body: &str) -> PathBuf {
            let path = self.dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let script = format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n",
                self.runs_path().display()
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// A fake `claude` on `PATH` that renews the token like Claude
        /// Code does: a new access token with a later expiry.
        fn renewing_claude(&self) -> PathBuf {
            let future = Utc::now() + chrono::Duration::hours(8);
            let body = format!(
                "printf '%s' '{}' > '{}'",
                credentials(future, "sk-ant-oat01-RENEWED"),
                self.credentials_path().display()
            );
            self.claude_at("bin/claude", &body)
        }

        fn renewer(&self, path_dirs: &[PathBuf]) -> Renewer {
            let mut r = Renewer::new(
                Some(self.home()),
                Some(std::env::join_paths(path_dirs).unwrap()),
            );
            r.timeout = Duration::from_millis(1500);
            r
        }

        fn source(&self, renewer: Option<Renewer>) -> ApiUsageSource {
            let source = ApiUsageSource::new(CredentialsLocation::File(self.credentials_path()));
            match renewer {
                Some(r) => source.with_renewer(r),
                None => source,
            }
        }
    }

    /// `source.token()`, retrying while a fake script is "busy": another
    /// test spawning a process just as a script is written briefly holds
    /// its write handle (ETXTBSY).
    async fn token(source: &ApiUsageSource) -> Result<String, UsageSourceError> {
        for _ in 0..50 {
            match source.token().await {
                Err(UsageSourceError::Credentials(msg)) if msg.contains("busy") => {
                    if let Some(r) = &source.renewer {
                        *r.last_run.lock().unwrap() = None;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                other => return other,
            }
        }
        source.token().await
    }

    fn assert_no_secret(err: &UsageSourceError) {
        let text = format!("{err} {err:?}");
        assert!(!text.contains("FIXTURE"), "error leaks a token: {text}");
    }

    #[tokio::test]
    async fn an_expired_token_is_renewed_by_claude_and_read_again() {
        let f = Fixture::expired();
        let claude = f.renewing_claude();
        let source = f.source(Some(f.renewer(&[claude.parent().unwrap().into()])));

        assert_eq!(token(&source).await.unwrap(), "sk-ant-oat01-RENEWED");
        assert_eq!(f.runs(), 1);
        // The exact command: nothing that could start a session or a model call.
        let args = std::fs::read_to_string(f.runs_path()).unwrap();
        assert_eq!(args.trim(), "auth status --json");
    }

    #[tokio::test]
    async fn the_plugin_never_writes_the_credentials_itself() {
        let f = Fixture::expired();
        // `claude` succeeds but changes nothing.
        let claude = f.claude_at("bin/claude", "exit 0");
        let before = std::fs::read(f.credentials_path()).unwrap();
        let source = f.source(Some(f.renewer(&[claude.parent().unwrap().into()])));

        let err = token(&source).await.unwrap_err();
        assert!(err.to_string().contains("did not renew"), "{err}");
        assert!(err.to_string().contains("open Claude Code"), "{err}");
        assert_no_secret(&err);
        assert_eq!(std::fs::read(f.credentials_path()).unwrap(), before);
    }

    #[tokio::test]
    async fn an_unexpired_token_never_runs_claude() {
        let f = Fixture::expired();
        let future = Utc::now() + chrono::Duration::hours(1);
        std::fs::write(f.credentials_path(), credentials(future, SECRET)).unwrap();
        let claude = f.renewing_claude();
        let source = f.source(Some(f.renewer(&[claude.parent().unwrap().into()])));

        assert_eq!(token(&source).await.unwrap(), SECRET);
        assert_eq!(f.runs(), 0);
    }

    #[tokio::test]
    async fn a_failed_renewal_is_a_credentials_error_without_the_token() {
        let f = Fixture::expired();
        let claude = f.claude_at("bin/claude", "echo sk-ant-oat01-FIXTURE-OUTPUT; exit 3");
        let source = f.source(Some(f.renewer(&[claude.parent().unwrap().into()])));

        let err = token(&source).await.unwrap_err();
        assert!(matches!(err, UsageSourceError::Credentials(_)));
        assert!(err.to_string().contains("exited 3"), "{err}");
        assert!(err.to_string().contains("open Claude Code"), "{err}");
        assert_no_secret(&err);
    }

    #[tokio::test]
    async fn renewal_runs_at_most_once_per_cooldown() {
        let f = Fixture::expired();
        let claude = f.claude_at("bin/claude", "exit 1");
        let source = f.source(Some(f.renewer(&[claude.parent().unwrap().into()])));

        let first = token(&source).await.unwrap_err();
        let second = source.token().await.unwrap_err();
        assert!(first.to_string().contains("exited 1"), "{first}");
        assert!(
            second.to_string().contains("less than 10 minutes ago"),
            "{second}"
        );
        assert_eq!(f.runs(), 1);

        // Once the cooldown has passed, it tries again.
        *source.renewer.as_ref().unwrap().last_run.lock().unwrap() =
            Some(Instant::now() - Duration::from_secs(11 * 60));
        token(&source).await.unwrap_err();
        assert_eq!(f.runs(), 2);
    }

    #[tokio::test]
    async fn a_hung_claude_is_killed_at_the_timeout() {
        let f = Fixture::expired();
        let claude = f.claude_at("bin/claude", "sleep 30");
        let mut renewer = f.renewer(&[claude.parent().unwrap().into()]);
        renewer.timeout = Duration::from_millis(200);
        let source = f.source(Some(renewer));

        let started = Instant::now();
        let err = token(&source).await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn without_claude_the_error_says_to_open_claude_code() {
        let f = Fixture::expired();
        let source = f.source(Some(f.renewer(&[f.home().join("empty")])));

        let err = token(&source).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "no usable Claude credentials: access token expired; open Claude Code to renew it"
        );
        assert_eq!(f.runs(), 0);
    }

    #[tokio::test]
    async fn without_a_renewer_an_expired_token_is_an_error() {
        let f = Fixture::expired();
        f.renewing_claude();
        let err = token(&f.source(None)).await.unwrap_err();
        assert!(err.to_string().contains("open Claude Code"), "{err}");
        assert_no_secret(&err);
        assert_eq!(f.runs(), 0);
    }

    #[tokio::test]
    async fn without_a_refresh_token_claude_is_not_run() {
        let f = Fixture::expired();
        let past = (Utc::now() - chrono::Duration::hours(1)).timestamp_millis();
        for oauth in [
            format!(r#"{{"accessToken": "{SECRET}", "expiresAt": {past}}}"#),
            r#"{"accessToken": "", "refreshToken": "", "expiresAt": 0}"#.to_string(),
            format!(r#"{{"accessToken": "{SECRET}", "refreshToken": 7, "expiresAt": {past}}}"#),
        ] {
            let json = format!(r#"{{"claudeAiOauth": {oauth}}}"#);
            std::fs::write(f.credentials_path(), json).unwrap();
            let claude = f.renewing_claude();
            let source = f.source(Some(f.renewer(&[claude.parent().unwrap().into()])));
            let err = token(&source).await.unwrap_err();
            assert!(err.to_string().contains("open Claude Code"), "{err}");
        }
        assert_eq!(f.runs(), 0);
    }

    #[tokio::test]
    async fn not_finding_claude_does_not_start_the_cooldown() {
        let f = Fixture::expired();
        let bin = f.home().join("bin");
        let source = f.source(Some(f.renewer(std::slice::from_ref(&bin))));
        token(&source).await.unwrap_err();
        // `claude` is installed afterwards: the next read uses it at once.
        f.renewing_claude();
        assert_eq!(token(&source).await.unwrap(), "sk-ant-oat01-RENEWED");
        assert_eq!(f.runs(), 1);
    }

    #[tokio::test]
    async fn a_timeout_kills_what_claude_started() {
        let f = Fixture::expired();
        let marker = f.home().join("grandchild-ran");
        // A grandchild that would write the marker if it outlived the kill.
        let body = format!("(sleep 1; touch '{}') &\nwait", marker.display());
        let claude = f.claude_at("bin/claude", &body);
        let mut renewer = f.renewer(&[claude.parent().unwrap().into()]);
        renewer.timeout = Duration::from_millis(200);
        let err = token(&f.source(Some(renewer))).await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !marker.exists(),
            "a process claude started outlived the timeout"
        );
    }

    #[test]
    fn claude_is_found_on_path_first_then_in_home() {
        let f = Fixture::expired();
        let local = f.claude_at(".local/bin/claude", "exit 0");
        let old = f.claude_at(".claude/local/claude", "exit 0");
        let on_path = f.claude_at("bin/claude", "exit 0");
        let bin = on_path.parent().unwrap().to_path_buf();

        assert_eq!(
            f.renewer(std::slice::from_ref(&bin)).claude(),
            Some(on_path)
        );
        // Relative PATH entries are skipped.
        assert_eq!(f.renewer(&["bin".into()]).claude(), Some(local.clone()));
        std::fs::remove_file(&local).unwrap();
        assert_eq!(f.renewer(&[]).claude(), Some(old.clone()));
        // A file that isn't executable doesn't count.
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(f.renewer(&[]).claude(), None);
        // Nor does a directory called claude, a relative home, or no home.
        std::fs::create_dir_all(f.home().join("dir/claude")).unwrap();
        assert_eq!(f.renewer(&[f.home().join("dir")]).claude(), None);
        let r = Renewer::new(Some(PathBuf::from("home")), None);
        assert_eq!(r.claude(), None);
        assert_eq!(Renewer::new(None, None).claude(), None);
    }
}
