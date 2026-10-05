//! The plugin's background loops: how they're kept alive, and when they
//! wake up.
//!
//! - `spawn_supervised` runs a loop on its own task and restarts it if it
//!   panics, logging why - a dropped `JoinHandle` would otherwise swallow
//!   the panic and leave every key that loop serves frozen, silently.
//! - Loops sleep until their next deadline (the next minute boundary, or
//!   the next instance that's due) rather than polling every second, and
//!   park entirely while no instance is visible.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Timelike, Utc};
use tokio::sync::Notify;

/// Pause before restarting a loop that panicked, so a loop that panics
/// straight away doesn't spin.
const RESTART_DELAY: Duration = Duration::from_secs(5);

/// Spawns `make()`'s future and keeps it running: if it panics (or
/// returns, which these loops never should), the panic is logged and a
/// fresh one is started after `RESTART_DELAY`.
pub fn spawn_supervised<F, Fut>(name: &'static str, make: F) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(supervise(name, make, RESTART_DELAY))
}

async fn supervise<F, Fut>(name: &'static str, mut make: F, restart_delay: Duration)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    loop {
        match tokio::spawn(make()).await {
            Ok(()) => log::error!("{name} stopped unexpectedly; restarting it"),
            Err(e) if e.is_panic() => {
                log::error!("{name} panicked ({e}); restarting it");
            }
            Err(e) => {
                // Cancelled: the runtime is shutting down.
                log::info!("{name} cancelled: {e}");
                return;
            }
        }
        tokio::time::sleep(restart_delay).await;
    }
}

/// How long until just after the next whole minute - when a clock face or
/// a "resets in 2h 13m" countdown next changes. The small margin makes sure
/// the wake-up lands in the new minute.
pub fn until_next_minute(now: DateTime<Utc>) -> Duration {
    const MARGIN: Duration = Duration::from_millis(50);
    let into_minute = Duration::from_secs(u64::from(now.second()))
        + Duration::from_nanos(u64::from(now.nanosecond() % 1_000_000_000));
    Duration::from_secs(60).saturating_sub(into_minute) + MARGIN
}

/// Sleeps until just after the next whole minute.
pub async fn sleep_to_next_minute() {
    tokio::time::sleep(until_next_minute(Utc::now())).await;
}

/// Waits while `is_empty()` - no instance to draw - until `wake` is
/// notified. `Notify::notify_one` stores a permit when nobody is waiting,
/// so an instance tracked just before this checks is never missed.
pub async fn park_while_empty(is_empty: impl Fn() -> bool, wake: &Notify) {
    while is_empty() {
        wake.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn the_next_minute_is_measured_from_now() {
        let at = |s: u32, ms: u32| {
            Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, s).unwrap()
                + chrono::Duration::milliseconds(ms.into())
        };
        assert_eq!(until_next_minute(at(0, 0)), Duration::from_millis(60_050));
        assert_eq!(until_next_minute(at(59, 500)), Duration::from_millis(550));
        assert_eq!(until_next_minute(at(30, 0)), Duration::from_millis(30_050));
    }

    /// A loop that panics is restarted, and keeps serving.
    #[tokio::test(start_paused = true)]
    async fn a_panicking_loop_is_restarted() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let handle = tokio::spawn(supervise(
            "test loop",
            move || {
                let run = counted.fetch_add(1, Ordering::SeqCst);
                async move {
                    if run == 0 {
                        panic!("first run fails");
                    }
                    std::future::pending::<()>().await;
                }
            },
            Duration::from_secs(5),
        ));
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        handle.abort();
    }

    #[tokio::test]
    async fn parking_returns_once_something_is_tracked() {
        let tracked = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let (t, w) = (Arc::clone(&tracked), Arc::clone(&wake));
        let parked = tokio::spawn(async move {
            park_while_empty(|| !t.load(Ordering::SeqCst), &w).await;
        });
        tokio::task::yield_now().await;
        assert!(!parked.is_finished());
        tracked.store(true, Ordering::SeqCst);
        wake.notify_one();
        tokio::time::timeout(Duration::from_secs(1), parked)
            .await
            .expect("still parked")
            .unwrap();
    }

    /// The loop saw "empty", then an instance was tracked (and the wake
    /// sent) before it started waiting: the stored permit wakes it.
    #[tokio::test]
    async fn a_wake_sent_before_parking_is_not_lost() {
        let wake = Notify::new();
        let checks = AtomicUsize::new(0);
        let is_empty = || {
            let first = checks.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                wake.notify_one(); // tracked right after the check
            }
            first
        };
        tokio::time::timeout(Duration::from_secs(1), park_while_empty(is_empty, &wake))
            .await
            .expect("the wake was lost");
    }
}
