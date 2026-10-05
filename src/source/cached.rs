use super::{UsageSnapshot, UsageSource, UsageSourceError};
use async_trait::async_trait;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::{Arc, PoisonError};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

type Outcome = Result<UsageSnapshot, UsageSourceError>;

/// How often to ask, how long to wait before retrying, and how long a
/// last-good snapshot may stand in for a failing source. See
/// `CachedUsageSource`.
#[derive(Debug, Clone, Copy)]
pub struct CachePolicy {
    /// Time between requests after a success - and the base delay that
    /// doubles with each consecutive failure.
    pub min_interval: Duration,
    /// Each interval after a success is stretched or shrunk by up to this
    /// fraction at random, so this plugin's requests don't stay in lockstep
    /// with anything else polling the same per-account limit.
    pub jitter: f64,
    /// Upper bound on the doubling retry delay.
    pub max_backoff: Duration,
    /// How old the last successful snapshot may get while it's still shown
    /// in place of a failure. Past this, callers see the error instead.
    pub stale_after: Duration,
}

/// A `Retry-After` longer than this is treated as this - an hour without a
/// retry is already "no data" on every key.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

/// The usage data every action shares: reads go through one throttle, so
/// however many keys, dials and taps there are, the account's rate limit
/// (shared with Claude Code's own `/usage`) sees one client. Only
/// `CachedUsageSource` implements this, so an action can't be handed a raw,
/// unthrottled `ApiUsageSource` - that doesn't compile.
#[async_trait]
pub trait SharedUsage: Send + Sync {
    /// The current usage, fetching only when the throttle allows.
    async fn read(&self) -> Outcome;
    /// What `read` would hand out right now, without ever fetching or
    /// waiting for a fetch in progress; `None` before the first read
    /// finished. Applies the same staleness rule as `read`.
    fn peek(&self) -> Option<Outcome>;
}

/// Wraps a `UsageSource` and throttles it, all in memory - nothing is
/// persisted, so the cache lives and dies with the plugin process:
///
/// - After a success, the inner source isn't hit again for `min_interval`
///   (± `jitter`).
/// - After `n` consecutive failures (a 429 from the shared per-account
///   rate limit, a network blip, an expired token), the next attempt waits
///   `min_interval * 2^n`, capped at `max_backoff` - or longer, if a 429
///   said `Retry-After`.
/// - While failing, callers keep getting the last successful snapshot until
///   it's older than `stale_after`, so one rejected request doesn't flip
///   every key to "no data".
///
/// Cloning shares the cache: every action in the plugin reads one clone.
pub struct CachedUsageSource<S> {
    inner: Arc<Inner<S>>,
}

struct Inner<S> {
    source: S,
    policy: CachePolicy,
    /// Held across the inner read, so concurrent callers that all find the
    /// cache expired wait for one request instead of each firing their own.
    fetch: Mutex<Schedule>,
    /// What callers are served. Only briefly locked, never across an
    /// await, so `peek` never waits on a slow request.
    served: std::sync::Mutex<Served>,
}

#[derive(Default)]
struct Schedule {
    consecutive_failures: u32,
    /// `None` until the first read, which always goes to the source.
    next_attempt: Option<Instant>,
}

#[derive(Default)]
struct Served {
    last_good: Option<(Instant, UsageSnapshot)>,
    last_error: Option<UsageSourceError>,
}

impl Served {
    /// The last good snapshot while it's fresh enough, otherwise the most
    /// recent error; `None` before anything was read.
    fn outcome(&self, now: Instant, stale_after: Duration) -> Option<Outcome> {
        match (&self.last_good, &self.last_error) {
            (Some((at, snapshot)), _) if now.duration_since(*at) < stale_after => {
                Some(Ok(snapshot.clone()))
            }
            (_, Some(error)) => Some(Err(error.clone())),
            // A stale snapshot with no error recorded can't happen (a
            // snapshot only goes stale while attempts keep failing), but
            // serving it beats inventing an error.
            (Some((_, snapshot)), None) => Some(Ok(snapshot.clone())),
            (None, None) => None,
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
                fetch: Mutex::new(Schedule::default()),
                served: std::sync::Mutex::new(Served::default()),
            }),
        }
    }

    fn served(&self) -> std::sync::MutexGuard<'_, Served> {
        // Plain data, valid after any panic: keep serving it.
        self.inner
            .served
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

/// `base` stretched by `jitter * unit`, for `unit` in -1..=1.
fn jittered(base: Duration, jitter: f64, unit: f64) -> Duration {
    base.mul_f64((1.0 + jitter.clamp(0.0, 1.0) * unit.clamp(-1.0, 1.0)).max(0.0))
}

/// A random number in -1..=1. `RandomState` is randomly keyed per
/// instance, which is plenty for spreading out a poll.
fn random_unit() -> f64 {
    let bits = RandomState::new().hash_one(0u8);
    (bits as f64 / u64::MAX as f64) * 2.0 - 1.0
}

#[async_trait]
impl<S: UsageSource> SharedUsage for CachedUsageSource<S> {
    async fn read(&self) -> Outcome {
        let policy = &self.inner.policy;
        let mut schedule = self.inner.fetch.lock().await;
        let now = Instant::now();
        if schedule.next_attempt.is_some_and(|at| now < at)
            && let Some(outcome) = self.served().outcome(now, policy.stale_after)
        {
            return outcome;
        }

        let result = self.inner.source.read().await;
        let mut served = self.served();
        match result {
            Ok(snapshot) => {
                served.last_good = Some((now, snapshot.clone()));
                served.last_error = None;
                schedule.consecutive_failures = 0;
                schedule.next_attempt =
                    Some(now + jittered(policy.min_interval, policy.jitter, random_unit()));
                Ok(snapshot)
            }
            Err(error) => {
                schedule.consecutive_failures += 1;
                let mut delay = backoff(policy, schedule.consecutive_failures);
                if let UsageSourceError::RateLimited {
                    retry_after_secs: Some(secs),
                } = &error
                {
                    delay = delay.max(Duration::from_secs(*secs).min(MAX_RETRY_AFTER));
                }
                log::warn!(
                    "usage fetch failed ({} in a row), retrying in {}s: {error}",
                    schedule.consecutive_failures,
                    delay.as_secs()
                );
                served.last_error = Some(error.clone());
                schedule.next_attempt = Some(now + delay);
                served
                    .outcome(now, policy.stale_after)
                    .unwrap_or(Err(error))
            }
        }
    }

    fn peek(&self) -> Option<Outcome> {
        self.served()
            .outcome(Instant::now(), self.inner.policy.stale_after)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A policy for tests: no jitter, so request times are exact.
    pub const POLICY: CachePolicy = CachePolicy {
        min_interval: Duration::from_secs(60),
        jitter: 0.0,
        max_backoff: Duration::from_secs(600),
        stale_after: Duration::from_secs(900),
    };

    /// `source` behind its own cache, for tests of anything that reads
    /// usage.
    pub fn shared(source: impl UsageSource + 'static) -> Arc<dyn SharedUsage> {
        Arc::new(CachedUsageSource::new(source, POLICY))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::POLICY;
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    /// Counts reads (reporting the count as the session percent, so tests
    /// can tell which read a snapshot came from), fails every read while
    /// `fail` is set, and takes `delay` (virtual time) to answer.
    #[derive(Clone, Default)]
    struct Flaky {
        reads: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
        retry_after: Arc<AtomicU64>,
        delay_secs: Arc<AtomicU64>,
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
            let delay = self.delay_secs.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            if self.fail.load(Ordering::SeqCst) {
                let retry_after = self.retry_after.load(Ordering::SeqCst);
                return Err(UsageSourceError::RateLimited {
                    retry_after_secs: (retry_after > 0).then_some(retry_after),
                });
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

    /// The single-flight promise: callers that all find the cache expired
    /// while one request is in flight wait for it instead of each sending
    /// their own (removing the lock around the inner read fails this).
    #[tokio::test(start_paused = true)]
    async fn concurrent_reads_share_one_request() {
        let (cached, flaky) = cached();
        flaky.delay_secs.store(5, Ordering::SeqCst);
        let reads: Vec<_> = (0..5)
            .map(|_| {
                let cached = cached.clone();
                tokio::spawn(async move { cached.read().await })
            })
            .collect();
        for read in reads {
            assert_eq!(read.await.unwrap().unwrap().session.percent, 1.0);
        }
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

    /// A 429's `Retry-After` beyond the backoff is honoured.
    #[tokio::test(start_paused = true)]
    async fn a_retry_after_longer_than_the_backoff_is_waited_out() {
        let (cached, flaky) = cached();
        flaky.set_failing(true);
        flaky.retry_after.store(1800, Ordering::SeqCst);
        cached.read().await.unwrap_err();
        advance_secs(1799).await;
        let _ = cached.read().await;
        assert_eq!(flaky.reads(), 1);
        advance_secs(1).await;
        let _ = cached.read().await;
        assert_eq!(flaky.reads(), 2);
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
            Err(UsageSourceError::RateLimited { .. })
        ));
    }

    /// `peek` follows the same staleness rule as `read`, never fetches -
    /// and so a key drawn on appear can't show numbers `read` would no
    /// longer serve.
    #[tokio::test(start_paused = true)]
    async fn peek_never_fetches_and_expires_like_read() {
        let (cached, flaky) = cached();
        assert!(cached.peek().is_none());
        cached.read().await.unwrap();
        flaky.set_failing(true);
        advance_secs(60).await;
        cached.read().await.unwrap(); // failure 1; snapshot still fresh
        assert!(cached.peek().unwrap().is_ok());
        advance_secs(900).await;
        assert!(cached.peek().unwrap().is_err());
        assert_eq!(flaky.reads(), 2);
    }

    #[test]
    fn jitter_stretches_the_interval_by_at_most_its_fraction() {
        let base = Duration::from_secs(200);
        assert_eq!(jittered(base, 0.1, -1.0), Duration::from_secs(180));
        assert_eq!(jittered(base, 0.1, 0.0), base);
        assert_eq!(jittered(base, 0.1, 1.0), Duration::from_secs(220));
        assert_eq!(jittered(base, 0.0, 1.0), base);
        for _ in 0..100 {
            let unit = random_unit();
            assert!((-1.0..=1.0).contains(&unit), "{unit}");
        }
    }
}
