pub mod file;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct WindowUsage {
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

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
}

/// Which part of a `UsageSnapshot` a given dial is configured to show -
/// lives here (not in `action.rs`) so `format.rs` can render from it without
/// depending on the Action/settings module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    #[default]
    Session,
    Weekly,
    Monthly,
}

#[derive(Debug, Error)]
pub enum UsageSourceError {
    #[error("failed to read usage file: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse usage file: {0}")]
    Parse(#[from] serde_json::Error),
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
            Err(UsageSourceError::Read(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no file",
            )))
        }
    }

    #[tokio::test]
    async fn trait_object_is_usable_through_a_dyn_reference() {
        let source: Box<dyn UsageSource> = Box::new(AlwaysFails);
        let result = source.read().await;
        assert!(matches!(result, Err(UsageSourceError::Read(_))));
    }

    #[test]
    fn window_kind_defaults_to_session() {
        assert_eq!(WindowKind::default(), WindowKind::Session);
    }
}
