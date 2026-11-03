pub mod api;
pub mod cached;
pub mod logs;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct WindowUsage {
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

/// `extra_usage` (pay-as-you-go spend against a monthly cap). Parsed and
/// recorded in the history file, but no key shows it: the Monthly window
/// option was cut (its dollar scaling could never be verified).
#[derive(Debug, Clone, PartialEq)]
pub struct MonthlyUsage {
    pub enabled: bool,
    pub percent: Option<f64>,
    pub used_dollars: Option<f64>,
    pub limit_dollars: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    pub session: WindowUsage,
    pub weekly: WindowUsage,
    pub monthly: MonthlyUsage,
    pub details: UsageDetails,
}

/// The rest of what the usage response carries, beyond the windows the
/// keys draw. All of it is optional in the (undocumented) response: when
/// a part is missing or has an unexpected shape it is simply empty, and
/// the windows still parse. Parsed ahead of any display (see the vault
/// spec `2026-11-01-usage-details-design`), so nothing reads it yet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageDetails {
    /// `seven_day_breakdown.rows`: each surface's share of the weekly
    /// limit, in the response's order. Shares, not parts of the weekly
    /// figure: they need not add up to it.
    pub weekly_shares: Vec<SurfaceShare>,
    /// `limits[]`: Anthropic's own reading of each limit.
    pub limits: Vec<Limit>,
    /// `five_hour.locked_reason` / `seven_day.locked_reason`, when locked.
    pub session_locked: Option<String>,
    pub weekly_locked: Option<String>,
}

/// One row of the weekly breakdown, e.g. `claude_code` / "Claude Code".
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceShare {
    pub key: String,
    pub name: String,
    pub percent: f64,
}

/// One `limits[]` entry, e.g. kind `weekly_all`, group `weekly`.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub kind: String,
    pub group: Option<String>,
    pub percent: Option<f64>,
    /// As sent (seen: `critical`); not mapped to a `level::Level`.
    pub severity: Option<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub is_active: bool,
}

/// Which part of a `UsageSnapshot` a given key or dial is configured to
/// show - lives here (not in an action module) so the pure display modules
/// can render from it without depending on any action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    #[default]
    Session,
    Weekly,
}

/// Errors carry only their message (not the underlying `io::Error` /
/// `reqwest::Error`), so they're `Clone` - `CachedUsageSource` hands the
/// same failed outcome to every caller within its throttle window instead
/// of re-requesting on each one.
#[derive(Debug, Clone, Error)]
pub enum UsageSourceError {
    #[error("no usable Claude credentials: {0}")]
    Credentials(String),
    #[error("usage request failed: {0}")]
    Request(String),
    /// HTTP 429 - the per-account limit shared with Claude Code's own
    /// `/usage`. `retry_after_secs` is the response's `Retry-After`, if any.
    #[error("usage request rate-limited (HTTP 429){}", retry_after_secs.map(|s| format!(", retry after {s}s")).unwrap_or_default())]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("failed to parse usage response: {0}")]
    Parse(String),
}

#[async_trait]
pub trait UsageSource: Send + Sync {
    async fn read(&self) -> Result<UsageSnapshot, UsageSourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysFails;

    #[async_trait]
    impl UsageSource for AlwaysFails {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            Err(UsageSourceError::Request("offline".to_string()))
        }
    }

    #[tokio::test]
    async fn trait_object_is_usable_through_a_dyn_reference() {
        let source: Box<dyn UsageSource> = Box::new(AlwaysFails);
        let result = source.read().await;
        assert!(matches!(result, Err(UsageSourceError::Request(_))));
    }

    #[test]
    fn window_kind_defaults_to_session() {
        assert_eq!(WindowKind::default(), WindowKind::Session);
    }
}
