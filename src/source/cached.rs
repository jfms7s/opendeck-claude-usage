use super::{UsageSnapshot, UsageSource, UsageSourceError};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

type Outcome = Result<UsageSnapshot, UsageSourceError>;

/// How long to wait before retrying, and how long a last-good snapshot may
/// stand in for a failing source. See `CachedUsageSource`.
#[derive(Debug, Clone, Copy)]
pub struct CachePolicy {
    /// Minimum time between requests after a success - and the base delay
    /// that doubles with each consecutive failure.
    pub min_interval: Duration,
    /// Upper bound on the doubling retry delay.
    pub max_backoff: Duration,
    /// How old the last successful snapshot may get while it's still shown
    /// in place of a failure. Past this, callers see the error instead.
    pub stale_after: Duration,
}

/// Wraps another `UsageSource` and throttles it, all in memory - nothing is
/// persisted, so the cache lives and dies with the plugin process:
///
/// - After a success, the inner source isn't hit again for `min_interval`.
/// - After `n` consecutive failures (a 429 from a shared per-account rate
///   limit, a network blip, an expired token), the next attempt waits
///   `min_interval * 2^n`, capped at `max_backoff`.
/// - While failing, callers keep getting the last successful snapshot until
///   it's older than `stale_after`, so one rejected request doesn't flip
///   every dial to "no data".
///
/// Cloning shares the cache, which is how the Usage Gauge and the Metric
/// Tile end up behind a single request budget.
pub struct CachedUsageSource<S> {
    inner: Arc<Inner<S>>,
}

struct Inner<S> {
    source: S,
    policy: CachePolicy,
    /// Held across the inner read, so concurrent callers that all find the
    /// cache expired wait for one request instead of each firing their own.
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    last_good: Option<(Instant, UsageSnapshot)>,
    last_error: Option<UsageSourceError>,
    consecutive_failures: u32,
    /// `None` until the first read, which always goes to the source.
    next_attempt: Option<Instant>,
}

impl State {
    /// What callers see between attempts: the last good snapshot while it's
    /// fresh enough, otherwise the most recent error.
    fn served(&self, now: Instant, stale_after: Duration) -> Outcome {
        match (&self.last_good, &self.last_error) {
            (Some((at, snapshot)), _) if now.duration_since(*at) < stale_after => {
                Ok(snapshot.clone())
            }
            (_, Some(error)) => Err(error.clone()),
            // A stale snapshot with no error recorded can't happen (a
            // snapshot only goes stale while attempts keep failing), but
            // serving it beats inventing an error.
            (Some((_, snapshot)), None) => Ok(snapshot.clone()),
            (None, None) => unreachable!("served() is only called after a first read"),
        }
    }
}

impl<S> Clone for CachedUsageSource<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S: UsageSource> CachedUsageSource<S> {
    pub fn new(source: S, policy: CachePolicy) -> Self {
        Self {
            inner: Arc::new(Inner {
                source,
                policy,
                state: Mutex::new(State::default()),
            }),
        }
    }
}

fn backoff(policy: &CachePolicy, consecutive_failures: u32) -> Duration {
    // Saturating so a long outage can't overflow the shift or the multiply;
    // the cap kicks in long before either would matter.
    let factor = 1u32.checked_shl(consecutive_failures).unwrap_or(u32::MAX);
    policy
        .min_interval
        .saturating_mul(factor)
        .min(policy.max_backoff)
}

#[async_trait]
impl<S: UsageSource> UsageSource for CachedUsageSource<S> {
    async fn read(&self) -> Outcome {
        let policy = &self.inner.policy;
        let mut state = self.inner.state.lock().await;
        let now = Instant::now();
        if state.next_attempt.is_some_and(|at| now < at) {
            return state.served(now, policy.stale_after);
        }

        match self.inner.source.read().await {
            Ok(snapshot) => {
                state.last_good = Some((now, snapshot.clone()));
                state.last_error = None;
                state.consecutive_failures = 0;
                state.next_attempt = Some(now + policy.min_interval);
                Ok(snapshot)
            }
            Err(error) => {
                state.consecutive_failures += 1;
                let delay = backoff(policy, state.consecutive_failures);
                log::warn!(
                    "usage fetch failed ({} in a row), retrying in {}s: {error}",
                    state.consecutive_failures,
                    delay.as_secs()
                );
                state.last_error = Some(error);
                state.next_attempt = Some(now + delay);
                state.served(now, policy.stale_after)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const POLICY: CachePolicy = CachePolicy {
        min_interval: Duration::from_secs(60),
        max_backoff: Duration::from_secs(600),
        stale_after: Duration::from_secs(900),
    };

    /// Counts reads (reporting the count as the session percent, so tests
    /// can tell which read a snapshot came from), and fails every read
    /// while `fail` is set.
    #[derive(Clone, Default)]
    struct Flaky {
        reads: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
    }

    impl Flaky {
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        fn set_failing(&self, fail: bool) {
            self.fail.store(fail, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl UsageSource for Flaky {
        async fn read(&self) -> Outcome {
            let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail.load(Ordering::SeqCst) {
                return Err(UsageSourceError::Request("HTTP 429".to_string()));
            }
            Ok(UsageSnapshot {
                session: WindowUsage {
                    percent: n as f64,
                    resets_at: None,
                },
                weekly: WindowUsage {
                    percent: 0.0,
                    resets_at: None,
                },
                monthly: MonthlyUsage {
                    enabled: false,
                    percent: None,
                    used_dollars: None,
                    limit_dollars: None,
                },
            })
        }
    }

    fn cached() -> (CachedUsageSource<Flaky>, Flaky) {
        let flaky = Flaky::default();
        (CachedUsageSource::new(flaky.clone(), POLICY), flaky)
    }

    async fn advance_secs(secs: u64) {
        tokio::time::advance(Duration::from_secs(secs)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn reads_within_the_interval_reuse_the_cached_snapshot() {
        let (cached, flaky) = cached();
        let first = cached.read().await.unwrap();
        advance_secs(59).await;
        let second = cached.read().await.unwrap();

        assert_eq!(flaky.reads(), 1);
        assert_eq!(first, second);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_after_the_interval_hits_the_source_again() {
        let (cached, flaky) = cached();
        cached.read().await.unwrap();
        advance_secs(60).await;
        let second = cached.read().await.unwrap();

        assert_eq!(flaky.reads(), 2);
        assert_eq!(second.session.percent, 2.0);
    }

    #[tokio::test(start_paused = true)]
    async fn clones_share_one_cache() {
        let (cached, flaky) = cached();
        let other = cached.clone();
        cached.read().await.unwrap();
        other.read().await.unwrap();

        assert_eq!(flaky.reads(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_with_nothing_cached_is_an_error_and_is_not_retried_immediately() {
        let (cached, flaky) = cached();
        flaky.set_failing(true);
        assert!(cached.read().await.is_err());
        assert!(cached.read().await.is_err());

        assert_eq!(flaky.reads(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_keeps_serving_the_last_good_snapshot() {
        let (cached, flaky) = cached();
        cached.read().await.unwrap();
        flaky.set_failing(true);
        advance_secs(60).await;

        let served = cached.read().await.unwrap();
        assert_eq!(flaky.reads(), 2);
        assert_eq!(served.session.percent, 1.0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_retry_delay_doubles_with_each_consecutive_failure() {
        let (cached, flaky) = cached();
        flaky.set_failing(true);
        cached.read().await.unwrap_err(); // failure 1 -> next attempt in 120s

        advance_secs(119).await;
        let _ = cached.read().await;
        assert_eq!(flaky.reads(), 1);
        advance_secs(1).await;
        let _ = cached.read().await; // failure 2 -> next attempt in 240s
        assert_eq!(flaky.reads(), 2);

        advance_secs(239).await;
        let _ = cached.read().await;
        assert_eq!(flaky.reads(), 2);
        advance_secs(1).await;
        let _ = cached.read().await;
        assert_eq!(flaky.reads(), 3);
    }

    #[test]
    fn the_retry_delay_is_capped() {
        assert_eq!(backoff(&POLICY, 1), Duration::from_secs(120));
        assert_eq!(backoff(&POLICY, 4), Duration::from_secs(600));
        assert_eq!(backoff(&POLICY, 64), Duration::from_secs(600));
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_resets_the_backoff() {
        let (cached, flaky) = cached();
        flaky.set_failing(true);
        cached.read().await.unwrap_err();
        advance_secs(120).await;
        flaky.set_failing(false);
        cached.read().await.unwrap();

        // Back to the plain interval, not the 240s a third failure would get.
        advance_secs(60).await;
        cached.read().await.unwrap();
        assert_eq!(flaky.reads(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_older_than_stale_after_gives_way_to_the_error() {
        let (cached, flaky) = cached();
        cached.read().await.unwrap();
        flaky.set_failing(true);
        advance_secs(60).await;
        assert!(cached.read().await.is_ok()); // failure 1, snapshot 60s old

        advance_secs(900 - 60).await;
        assert!(matches!(
            cached.read().await,
            Err(UsageSourceError::Request(_))
        ));
    }
}
