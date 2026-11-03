//! Shared state behind every usage-driven action (Usage Gauge, Burn Rate,
//! Session + Weekly, Usage Sparkline): one registry of visible instances
//! and one poll loop over the shared usage cache - so adding an action
//! never adds another poller, and a single read serves every key and dial.
//! It also records each new reading into the `HistoryStore` the sparkline
//! draws from.
//!
//! The hub knows nothing about how any feature renders: an instance
//! registers a `View`, and the view turns a snapshot into its frame.

use std::fmt::Debug;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, Utc};
use dashmap::DashMap;
use openaction::OpenActionResult;
use tokio::sync::Notify;

use crate::history::{HistoryStore, Reading};
use crate::source::cached::SharedUsage;
use crate::source::{UsageSnapshot, UsageSourceError};
use crate::surface::{Alert, Frames, Output, Surface, for_each_tracked};
use crate::tasks::{park_while_empty, sleep_to_next_minute};

/// What one instance shows, built from its action's settings: turns the
/// current snapshot (and, for views that plot it, the recorded history)
/// into a keypad image or dial feedback. Pure, so every view × surface ×
/// data state is unit-testable without an OpenDeck connection.
pub trait HubView: Debug + Send + Sync {
    fn output(
        &self,
        snapshot: Option<&UsageSnapshot>,
        history: &[Reading],
        keypad: bool,
        now: DateTime<Utc>,
    ) -> Output;

    /// Whether `output` reads `history` - only then is it copied out.
    fn needs_history(&self) -> bool {
        false
    }

    /// What to flash after a poll whose snapshot `cur` differs from the
    /// previous poll's `prev` (see `alert::window_alert`). `None` unless
    /// the key opted in.
    fn alert(&self, _prev: &UsageSnapshot, _cur: &UsageSnapshot) -> Option<Alert> {
        None
    }
}

pub type View = Arc<dyn HubView>;

/// How long a press waits for a usage read before drawing from the cache.
const REFRESH_DEADLINE: Duration = Duration::from_millis(500);

pub struct UsageHub {
    source: Arc<dyn SharedUsage>,
    registry: DashMap<String, View>,
    /// The last frame each instance was sent, so an unchanged one isn't
    /// sent again.
    frames: Frames,
    /// Pinged by `track`, so a poll loop parked on an empty registry
    /// resumes.
    wake: Notify,
    /// Tracks whether the poll loop's most recent read succeeded, so it
    /// logs a `warn!` only on the transition into failing (and an `info!`
    /// only on the recovery) instead of on every tick. Starts `true` so the
    /// very first failure is logged. Not touched by `refresh_one` - a
    /// single manual press failing isn't part of that noise pattern.
    poll_last_read_ok: AtomicBool,
    /// Every new reading is recorded here for the sparkline.
    history: Arc<HistoryStore>,
    /// The snapshot last handed to `history`, and the local day it was -
    /// so the cached snapshot served between fetches costs no trip to the
    /// blocking pool.
    last_recorded: Mutex<Option<(UsageSnapshot, NaiveDate)>>,
    /// The snapshot the poll loop last read, which the next poll compares
    /// against to raise alerts. Only `refresh_all` touches it, so a press's
    /// read never consumes a transition, and a failed read keeps it.
    last_polled: Mutex<Option<UsageSnapshot>>,
}

impl UsageHub {
    /// `source` is the plugin's one throttled usage cache (see
    /// `SharedUsage` - a raw source doesn't fit).
    pub fn new(source: Arc<dyn SharedUsage>, history: Arc<HistoryStore>) -> Arc<Self> {
        Arc::new(Self {
            source,
            registry: DashMap::new(),
            frames: Frames::default(),
            wake: Notify::new(),
            poll_last_read_ok: AtomicBool::new(true),
            history,
            last_recorded: Mutex::new(None),
            last_polled: Mutex::new(None),
        })
    }

    /// An instance appeared: its first frame is always sent.
    pub fn appear(&self, instance_id: &str, view: View) {
        self.frames.forget(instance_id);
        self.track(instance_id, view);
    }

    /// Registers (or re-registers) what an instance shows.
    pub fn track(&self, instance_id: &str, view: View) {
        self.registry.insert(instance_id.to_string(), view);
        self.wake.notify_one();
    }

    #[cfg(test)]
    pub fn tracked_view(&self, instance_id: &str) -> Option<View> {
        self.registry.get(instance_id).map(|v| Arc::clone(&v))
    }

    pub fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
        self.frames.forget(instance_id);
    }

    /// Renders from the cached snapshot without fetching - used when an
    /// instance appears, its settings change or a press switches its view,
    /// so it shows *something* immediately. Served by the same cache and
    /// staleness rule as every poll, so it can't show numbers a poll would
    /// already call "no data".
    pub async fn render_cached<S: Surface + ?Sized>(
        &self,
        surface: &S,
        view: &View,
    ) -> OpenActionResult<()> {
        let snapshot = self.source.peek().and_then(Result::ok);
        self.push(surface, view, snapshot.as_ref()).await
    }

    /// Reads (through the throttle) and renders just this instance - a dial
    /// press or a long keypad press, without waiting for the next tick.
    ///
    /// openaction handles events one at a time, so the press waits at most
    /// `REFRESH_DEADLINE` for the read; past that it draws from the cache
    /// like `render_cached`. The read runs in its own task and is never
    /// cancelled (its rate-limit cost is already paid): when it lands,
    /// every tracked instance is redrawn from it.
    pub async fn refresh_one<S: Surface + ?Sized>(
        self: &Arc<Self>,
        surface: &S,
        view: &View,
    ) -> OpenActionResult<()> {
        self.refresh_one_with(surface, view, openaction::get_instance)
            .await
    }

    /// `refresh_one`, with the instance lookup used for the late redraw
    /// passed in (tests pass a fake).
    async fn refresh_one_with<S, T, L, LF>(
        self: &Arc<Self>,
        surface: &S,
        view: &View,
        lookup: L,
    ) -> OpenActionResult<()>
    where
        S: Surface + ?Sized,
        T: Surface + 'static,
        L: FnMut(String) -> LF + Send + 'static,
        LF: Future<Output = Option<T>> + Send,
    {
        let hub = Arc::clone(self);
        let mut read = tokio::spawn(async move { hub.read_and_record().await });
        let snapshot = match tokio::time::timeout(REFRESH_DEADLINE, &mut read).await {
            Ok(joined) => self.read_outcome(joined),
            Err(_) => {
                let hub = Arc::clone(self);
                tokio::spawn(async move {
                    let snapshot = hub.read_outcome(read.await);
                    hub.render_tracked(snapshot.as_ref(), None, lookup).await;
                });
                self.source.peek().and_then(Result::ok)
            }
        };
        self.push(surface, view, snapshot.as_ref()).await
    }

    /// The snapshot a spawned read produced, logging why there is none.
    /// A read that panicked falls back to the cache.
    fn read_outcome(
        &self,
        joined: Result<Result<UsageSnapshot, UsageSourceError>, tokio::task::JoinError>,
    ) -> Option<UsageSnapshot> {
        match joined {
            Ok(Ok(snapshot)) => Some(snapshot),
            Ok(Err(e)) => {
                log::warn!("usage source read failed: {e}");
                None
            }
            Err(e) => {
                log::error!("usage refresh task failed: {e}");
                self.source.peek().and_then(Result::ok)
            }
        }
    }

    async fn push<S: Surface + ?Sized>(
        &self,
        surface: &S,
        view: &View,
        snapshot: Option<&UsageSnapshot>,
    ) -> OpenActionResult<()> {
        let output = view.output(
            snapshot,
            &self.history_for(view),
            surface.is_keypad(),
            Utc::now(),
        );
        self.frames.push(surface, output).await
    }

    /// The recorded readings, copied only for views that plot them.
    fn history_for(&self, view: &View) -> Vec<Reading> {
        if view.needs_history() {
            self.history.readings()
        } else {
            Vec::new()
        }
    }

    /// Reads the shared source and records a new reading in the history -
    /// shared by `refresh_one` and `refresh_all`.
    async fn read_and_record(&self) -> Result<UsageSnapshot, UsageSourceError> {
        let result = self.source.read().await;
        if let Ok(snapshot) = &result {
            // Local time, so each local day's first poll is kept (see
            // `HistoryStore::record`).
            let now = Local::now();
            if self.is_new_reading(snapshot, now.date_naive()) {
                // Recording can touch the history file - off the async
                // threads, so a slow disk can't stall every key.
                let (history, recorded) = (self.history.clone(), snapshot.clone());
                if let Err(e) =
                    tokio::task::spawn_blocking(move || history.record(&recorded, now)).await
                {
                    log::error!("recording usage history failed: {e}");
                }
            }
        }
        result
    }

    /// False for the very snapshot already recorded today - the cache
    /// serves the same one between fetches.
    fn is_new_reading(&self, snapshot: &UsageSnapshot, today: NaiveDate) -> bool {
        let mut last = self
            .last_recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if last
            .as_ref()
            .is_some_and(|(seen, day)| seen == snapshot && *day == today)
        {
            return false;
        }
        *last = Some((snapshot.clone(), today));
        true
    }

    fn log_poll_read_transition(&self, read_ok: bool, error: Option<&UsageSourceError>) {
        let was_ok = self.poll_last_read_ok.swap(read_ok, Ordering::Relaxed);
        if was_ok && !read_ok {
            if let Some(e) = error {
                log::warn!("usage source read failed: {e}");
            }
        } else if !was_ok && read_ok {
            log::info!("usage source read recovered");
        }
    }

    /// Runs forever: just after every minute boundary (when countdowns
    /// change), reads the shared cache once and re-renders every tracked
    /// instance. Parks - no reads at all - while nothing is tracked.
    /// Spawned once from `main.rs`, supervised.
    pub async fn poll_loop(self: Arc<Self>) {
        loop {
            if self.registry.is_empty() {
                // No key watched what happens while parked: the first poll
                // after it must not flash for it.
                self.forget_last_poll();
            }
            park_while_empty(|| self.registry.is_empty(), &self.wake).await;
            self.refresh_all().await;
            sleep_to_next_minute().await;
        }
    }

    async fn refresh_all(&self) {
        self.refresh_all_with(openaction::get_instance).await;
    }

    /// `refresh_all`, with the instance lookup passed in (tests pass a
    /// fake).
    async fn refresh_all_with<S, L, LF>(&self, lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        let read_result = self.read_and_record().await;
        self.log_poll_read_transition(read_result.is_ok(), read_result.as_ref().err());

        let previous = read_result
            .as_ref()
            .ok()
            .and_then(|snapshot| self.previous_poll(snapshot));
        self.render_tracked(read_result.as_ref().ok(), previous.as_ref(), lookup)
            .await;
    }

    /// Drops the previous poll's snapshot, so the next poll can't alert.
    fn forget_last_poll(&self) {
        *self
            .last_polled
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The previous poll's snapshot when `snapshot` differs from it (so
    /// something may have happened), remembering `snapshot` for the next
    /// poll. `None` on the first poll and when the cache served the same
    /// snapshot again.
    fn previous_poll(&self, snapshot: &UsageSnapshot) -> Option<UsageSnapshot> {
        let mut last = self
            .last_polled
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let previous = last.replace(snapshot.clone());
        previous.filter(|prev| prev != snapshot)
    }

    /// Re-renders every tracked instance from `snapshot`, reading each
    /// one's view only once its instance has been looked up (see
    /// `for_each_tracked`). With `previous` (a poll that changed the
    /// snapshot), each view that asks for it then flashes once.
    async fn render_tracked<S, L, LF>(
        &self,
        snapshot: Option<&UsageSnapshot>,
        previous: Option<&UsageSnapshot>,
        lookup: L,
    ) where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        let ids = self.registry.iter().map(|e| e.key().clone()).collect();
        for_each_tracked(
            ids,
            |id| self.registry.get(id).map(|v| Arc::clone(&v)),
            lookup,
            |surface, view| async move {
                if let Err(e) = self.push(&surface, &view, snapshot).await {
                    log::warn!("render failed: {e}");
                }
                if let (Some(prev), Some(cur)) = (previous, snapshot)
                    && let Some(alert) = view.alert(prev, cur)
                    && let Err(e) = surface.flash(alert).await
                {
                    log::warn!("alert failed: {e}");
                }
            },
        )
        .await;
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::source::UsageSource;
    use crate::source::cached::test_support::shared;
    use async_trait::async_trait;

    /// A source for tests that never read usage.
    pub struct NeverCalled;

    #[async_trait]
    impl UsageSource for NeverCalled {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            unreachable!("this test never triggers a read")
        }
    }

    /// A hub over a source that is never read.
    pub fn idle_hub() -> Arc<UsageHub> {
        UsageHub::new(shared(NeverCalled), HistoryStore::in_memory())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::idle_hub;
    use super::*;
    use crate::burn::BurnMetric;
    use crate::burn_action::BurnView;
    use crate::gauge_action::GaugeView;
    use crate::gauge_style::GaugeStyle;
    use crate::level::ColorSettings;
    use crate::source::cached::test_support::shared;
    use crate::source::{MonthlyUsage, UsageSource, WindowKind, WindowUsage};
    use crate::sparkline::SparkSettings;
    use crate::sparkline_action::SparkView;
    use crate::surface::test_support::FakeSurface;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use std::sync::atomic::AtomicUsize;

    fn snapshot() -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage {
                percent: 33.0,
                resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 13, 22, 40, 0).unwrap()),
            },
            weekly: WindowUsage {
                percent: 29.0,
                resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 17, 6, 0, 0).unwrap()),
            },
            monthly: MonthlyUsage {
                enabled: true,
                percent: Some(25.0),
                used_dollars: Some(12.5),
                limit_dollars: Some(50.0),
            },
            details: Default::default(),
        }
    }

    /// Counts reads; succeeds with `snapshot()` until `fail` is set.
    #[derive(Clone, Default)]
    struct Counting {
        reads: Arc<AtomicUsize>,
        fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl UsageSource for Counting {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                Err(UsageSourceError::Request("down".to_string()))
            } else {
                Ok(snapshot())
            }
        }
    }

    fn hub_over(source: &Counting) -> Arc<UsageHub> {
        UsageHub::new(shared(source.clone()), HistoryStore::in_memory())
    }

    fn gauge() -> View {
        Arc::new(GaugeView {
            window: WindowKind::Session,
            colors: ColorSettings::default(),
            style: GaugeStyle::Speedometer,
            alerts: false,
        })
    }

    fn burn() -> View {
        Arc::new(BurnView {
            window: WindowKind::Session,
            metric: BurnMetric::EvenBurn,
            colors: ColorSettings::default(),
        })
    }

    fn sparkline_view() -> View {
        Arc::new(SparkView {
            settings: SparkSettings::default(),
            colors: ColorSettings::default(),
            alerts: false,
        })
    }

    fn tracked(hub: &UsageHub, id: &str) -> Option<View> {
        hub.tracked_view(id)
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let hub = idle_hub();
        let view = gauge();
        hub.track("ctx1", Arc::clone(&view));
        assert!(Arc::ptr_eq(&tracked(&hub, "ctx1").unwrap(), &view));
        hub.untrack("ctx1");
        assert!(tracked(&hub, "ctx1").is_none());
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_view() {
        let hub = idle_hub();
        hub.track("ctx1", gauge());
        let second = burn();
        hub.track("ctx1", Arc::clone(&second));
        assert!(Arc::ptr_eq(&tracked(&hub, "ctx1").unwrap(), &second));
        assert_eq!(hub.registry.len(), 1);
    }

    /// KI-22: one poll renders every tracked instance from the same read,
    /// so a Gauge and a Burn Rate side by side both show real data.
    #[tokio::test]
    async fn one_poll_renders_both_gauge_and_burn() {
        let source = Counting::default();
        let hub = hub_over(&source);
        hub.track("gauge", gauge());
        hub.track("burn", burn());
        let snapshot = hub.read_and_record().await.unwrap();

        let (g, b) = (FakeSurface::dial("gauge"), FakeSurface::dial("burn"));
        let surfaces = [g.clone(), b.clone()];
        hub.render_tracked(Some(&snapshot), None, |id| {
            std::future::ready(surfaces.iter().find(|s| s.id == id).cloned())
        })
        .await;

        let [Output::Feedback(g)] = &g.frames()[..] else {
            panic!("expected one gauge frame");
        };
        let [Output::Feedback(b)] = &b.frames()[..] else {
            panic!("expected one burn frame");
        };
        assert_eq!(g["percent"], "33%");
        assert!(b["detail"].as_str().unwrap().ends_with("session"));
        assert_ne!(b["detail"], "no data");
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_unchanged_frame_is_sent_once_across_polls() {
        let source = Counting::default();
        let hub = hub_over(&source);
        hub.track("gauge", gauge());
        let dial = FakeSurface::dial("gauge");
        for _ in 0..3 {
            let snapshot = hub.read_and_record().await.unwrap();
            hub.render_tracked(Some(&snapshot), None, |_| {
                std::future::ready(Some(dial.clone()))
            })
            .await;
        }
        assert_eq!(dial.frames().len(), 1);
    }

    #[tokio::test]
    async fn read_and_record_records_history() {
        let history = HistoryStore::in_memory();
        let hub = UsageHub::new(shared(Counting::default()), history.clone());
        hub.read_and_record().await.unwrap();
        assert_eq!(history.readings().len(), 1);
        assert_eq!(history.readings()[0].session, 33.0);
    }

    #[test]
    fn the_snapshot_already_recorded_today_is_not_recorded_again() {
        let hub = idle_hub();
        let today = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        assert!(hub.is_new_reading(&snapshot(), today));
        assert!(!hub.is_new_reading(&snapshot(), today));
        assert!(hub.is_new_reading(&snapshot(), today.succ_opt().unwrap()));
        let mut changed = snapshot();
        changed.session.percent = 34.0;
        assert!(hub.is_new_reading(&changed, today.succ_opt().unwrap()));
    }

    /// A slow disk must not stall the runtime every key renders on. The
    /// history file here is a FIFO, so writing to it blocks until a reader
    /// opens it - and the reader is a task on the same single-threaded
    /// runtime, which only gets to run if the write happens off-thread.
    #[test]
    fn recording_history_does_not_block_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let history = HistoryStore::load(path.clone(), Utc::now());
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success());
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let written = runtime.block_on(async {
                let reader = tokio::spawn(async move { tokio::fs::read(&path).await });
                UsageHub::new(shared(Counting::default()), history)
                    .read_and_record()
                    .await
                    .unwrap();
                reader.await.unwrap().unwrap()
            });
            done.send(written).unwrap();
        });
        let written = finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the runtime stalled on history file I/O");
        assert!(
            String::from_utf8(written)
                .unwrap()
                .contains("\"session\":33.0")
        );
    }

    #[tokio::test]
    async fn only_a_sparkline_gets_the_history() {
        let history = HistoryStore::in_memory();
        let hub = UsageHub::new(shared(Counting::default()), history.clone());
        hub.read_and_record().await.unwrap();
        assert_eq!(hub.history_for(&sparkline_view()).len(), 1);
        for view in [gauge(), burn()] {
            assert!(hub.history_for(&view).is_empty());
        }
    }

    /// Drawing on appear uses the same cache - and the same "no data after
    /// 15 minutes of failures" rule - as the polls (a second, never-expiring
    /// copy of the snapshot used to keep old numbers on screen).
    #[tokio::test(start_paused = true)]
    async fn drawing_from_the_cache_shows_no_data_once_the_snapshot_is_stale() {
        let source = Counting::default();
        let hub = hub_over(&source);
        hub.read_and_record().await.unwrap();
        let dial = FakeSurface::dial("ctx1");
        hub.render_cached(&dial, &gauge()).await.unwrap();

        source.fail.store(true, Ordering::SeqCst);
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        assert!(
            hub.read_and_record().await.is_ok(),
            "still within stale_after"
        );
        tokio::time::advance(std::time::Duration::from_secs(900)).await;
        hub.render_cached(&dial, &gauge()).await.unwrap();

        let frames = dial.frames();
        let [Output::Feedback(fresh), Output::Feedback(stale)] = &frames[..] else {
            panic!("expected two frames, got {frames:?}");
        };
        assert_eq!(fresh["percent"], "33%");
        assert_eq!(stale["detail"], "no data");
    }

    #[tokio::test]
    async fn drawing_from_the_cache_never_reads_the_source() {
        let source = Counting::default();
        let hub = hub_over(&source);
        hub.render_cached(&FakeSurface::dial("ctx1"), &gauge())
            .await
            .unwrap();
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    }

    /// Takes 5 s (virtual time) to answer; counts reads started and
    /// finished, and reports 10% per read started.
    #[derive(Clone, Default)]
    struct Slow {
        started: Arc<AtomicUsize>,
        finished: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl UsageSource for Slow {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            let n = self.started.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(Duration::from_secs(5)).await;
            self.finished.fetch_add(1, Ordering::SeqCst);
            let mut snapshot = snapshot();
            snapshot.session.percent = 10.0 * n as f64;
            Ok(snapshot)
        }
    }

    /// A press during a slow fetch draws the cached frame after the
    /// deadline - and the fetch runs to completion, once, instead of being
    /// cancelled with its rate-limit cost already paid.
    #[tokio::test(start_paused = true)]
    async fn a_press_during_a_slow_fetch_draws_the_cache_and_lets_the_fetch_finish() {
        let source = Slow::default();
        let hub = UsageHub::new(shared(source.clone()), HistoryStore::in_memory());
        hub.read_and_record().await.unwrap();
        tokio::time::advance(Duration::from_secs(120)).await; // a fetch is due

        hub.track("ctx1", gauge());
        let dial = FakeSurface::dial("ctx1");
        let lookup = {
            let dial = dial.clone();
            move |_: String| std::future::ready(Some(dial.clone()))
        };
        let pressed = tokio::time::Instant::now();
        hub.refresh_one_with(&dial, &gauge(), lookup.clone())
            .await
            .unwrap();
        assert_eq!(pressed.elapsed(), REFRESH_DEADLINE);
        let percent = |frame: &Output| match frame {
            Output::Feedback(f) => f["percent"].clone(),
            other => panic!("expected feedback, got {other:?}"),
        };
        assert_eq!(percent(&dial.frames()[0]), "10%", "not the cached frame");
        assert_eq!(source.started.load(Ordering::SeqCst), 2);

        // More presses while that fetch is in flight share it.
        for _ in 0..3 {
            hub.refresh_one_with(&dial, &gauge(), lookup.clone())
                .await
                .unwrap();
        }
        assert_eq!(source.started.load(Ordering::SeqCst), 2);

        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(source.finished.load(Ordering::SeqCst), 2, "fetch cancelled");
        assert_eq!(source.started.load(Ordering::SeqCst), 2);
        // The late read was recorded and drawn.
        assert_eq!(hub.history.readings().len(), 2);
        assert_eq!(
            percent(dial.frames().last().unwrap()),
            "20%",
            "the late read was not drawn"
        );
    }

    /// A read that answers within the deadline is drawn at once.
    #[tokio::test(start_paused = true)]
    async fn a_press_draws_a_read_that_answers_in_time() {
        let source = Counting::default();
        let hub = hub_over(&source);
        let dial = FakeSurface::dial("ctx1");
        hub.refresh_one(&dial, &gauge()).await.unwrap();
        let [Output::Feedback(frame)] = &dial.frames()[..] else {
            panic!("expected one frame, got {:?}", dial.frames());
        };
        assert_eq!(frame["percent"], "33%");
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    }

    /// The poll loop doesn't read the API at all while no key is visible,
    /// and starts as soon as one is tracked.
    #[tokio::test(start_paused = true)]
    async fn the_poll_loop_waits_for_a_tracked_instance() {
        let source = Counting::default();
        let hub = hub_over(&source);
        let poll = tokio::spawn(Arc::clone(&hub).poll_loop());
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);

        hub.track("ctx1", gauge());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(source.reads.load(Ordering::SeqCst), 1);
        poll.abort();
    }

    /// KI-06: a short press re-tracks the view while the poll is awaiting
    /// the instance lookup - the poll must draw the new view, not the one
    /// it saw before the await.
    #[tokio::test]
    async fn a_view_changed_during_the_lookup_is_the_one_drawn() {
        let hub = idle_hub();
        hub.track("ctx1", gauge());
        let surface = FakeSurface::dial("ctx1");
        hub.render_tracked(Some(&snapshot()), None, |id| {
            hub.track(&id, sparkline_view()); // the press lands here
            std::future::ready(Some(surface.clone()))
        })
        .await;
        let frames = surface.frames();
        let [Output::Feedback(f)] = frames.as_slice() else {
            panic!("expected one feedback, got {frames:?}");
        };
        assert!(f.get("chart").is_some(), "drew the old gauge: {f}");
    }

    #[tokio::test]
    async fn an_instance_untracked_during_the_lookup_is_not_drawn() {
        let hub = idle_hub();
        hub.track("ctx1", gauge());
        let surface = FakeSurface::dial("ctx1");
        hub.render_tracked(Some(&snapshot()), None, |id| {
            hub.untrack(&id);
            std::future::ready(Some(surface.clone()))
        })
        .await;
        assert!(surface.frames().is_empty());
    }
    /// Serves `script` one snapshot per (unthrottled) read, then repeats
    /// the last one.
    #[derive(Clone)]
    struct Scripted(Arc<Mutex<std::collections::VecDeque<UsageSnapshot>>>);

    #[async_trait]
    impl UsageSource for Scripted {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            let mut script = self.0.lock().unwrap();
            let next = if script.len() > 1 {
                script.pop_front().unwrap()
            } else {
                script.front().cloned().unwrap()
            };
            Ok(next)
        }
    }

    fn alerting_gauge(alerts: bool, critical: f64) -> View {
        let mut colors = ColorSettings::default();
        colors.marks.critical = critical;
        Arc::new(GaugeView {
            window: WindowKind::Session,
            colors,
            style: GaugeStyle::Speedometer,
            alerts,
        })
    }

    /// Polls `script` once per throttle interval (plus `extra` polls served
    /// from the cache in between) with an opted-in gauge, an opted-out one
    /// and an opted-in sparkline, and returns what each flashed.
    async fn alerts_over(
        script: Vec<UsageSnapshot>,
        extra_cached_polls: bool,
    ) -> (Vec<Alert>, Vec<Alert>, Vec<Alert>) {
        let polls = script.len();
        let source = Scripted(Arc::new(Mutex::new(script.into())));
        let hub = UsageHub::new(shared(source), HistoryStore::in_memory());
        hub.track("on", alerting_gauge(true, 90.0));
        hub.track("off", alerting_gauge(false, 90.0));
        hub.track(
            "spark",
            Arc::new(SparkView {
                settings: SparkSettings::default(),
                colors: ColorSettings::default(),
                alerts: true,
            }),
        );
        let surfaces: Vec<FakeSurface> = ["on", "off", "spark"]
            .into_iter()
            .map(|id| FakeSurface::new(id, true))
            .collect();
        let lookup = |id: String| std::future::ready(surfaces.iter().find(|s| s.id == id).cloned());
        for _ in 0..polls {
            hub.refresh_all_with(lookup).await;
            if extra_cached_polls {
                hub.refresh_all_with(lookup).await;
            }
            tokio::time::advance(Duration::from_secs(61)).await;
        }
        let flashed = |i: usize| surfaces[i].alerts.lock().unwrap().clone();
        (flashed(0), flashed(1), flashed(2))
    }

    fn session(percent: f64, resets_at: chrono::DateTime<Utc>) -> UsageSnapshot {
        let mut s = snapshot();
        s.session = WindowUsage {
            percent,
            resets_at: Some(resets_at),
        };
        s
    }

    fn reset_time() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 22, 40, 0).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_poll_never_alerts() {
        let (on, _, _) = alerts_over(vec![session(95.0, reset_time())], false).await;
        assert!(on.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_same_snapshot_served_twice_does_not_alert() {
        let s = session(95.0, reset_time());
        let (on, _, _) = alerts_over(vec![s.clone(), s], true).await;
        assert!(on.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn reset_jitter_within_tolerance_does_not_alert() {
        let r = reset_time();
        let (on, _, spark) = alerts_over(
            vec![
                session(40.0, r),
                session(41.0, r + chrono::Duration::minutes(4)),
            ],
            false,
        )
        .await;
        assert!(on.is_empty());
        assert!(spark.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_reset_flashes_fresh_once() {
        let r = reset_time();
        let after = session(3.0, r + chrono::Duration::hours(5));
        let (on, off, spark) =
            alerts_over(vec![session(70.0, r), after.clone(), after], true).await;
        assert_eq!(on, vec![Alert::Fresh]);
        assert_eq!(spark, vec![Alert::Fresh]);
        assert!(off.is_empty(), "a key that didn't opt in");
    }

    #[tokio::test(start_paused = true)]
    async fn crossing_the_keys_critical_mark_flashes_critical_once() {
        let r = reset_time();
        let (on, off, spark) = alerts_over(
            vec![session(85.0, r), session(92.0, r), session(93.0, r)],
            false,
        )
        .await;
        assert_eq!(on, vec![Alert::Critical], "Critical at 90 on this key");
        assert_eq!(spark, vec![Alert::Critical], "the default mark is 90 too");
        assert!(off.is_empty());
    }

    /// A press reads too, but never alerts and never uses up the
    /// transition the next poll reports.
    #[tokio::test(start_paused = true)]
    async fn a_press_neither_alerts_nor_consumes_the_transition() {
        let r = reset_time();
        let source = Scripted(Arc::new(Mutex::new(
            vec![
                session(70.0, r),
                session(3.0, r + chrono::Duration::hours(5)),
            ]
            .into(),
        )));
        let hub = UsageHub::new(shared(source), HistoryStore::in_memory());
        let view = alerting_gauge(true, 90.0);
        hub.track("on", Arc::clone(&view));
        let key = FakeSurface::new("on", true);
        let lookup = |_: String| std::future::ready(Some(key.clone()));
        hub.refresh_all_with(lookup).await;
        tokio::time::advance(Duration::from_secs(61)).await;
        hub.refresh_one_with(&key, &view, move |_: String| {
            std::future::ready(None::<FakeSurface>)
        })
        .await
        .unwrap();
        assert!(key.alerts.lock().unwrap().is_empty());
        hub.refresh_all_with(lookup).await;
        assert_eq!(*key.alerts.lock().unwrap(), vec![Alert::Fresh]);
    }
    /// While parked (no key visible) nothing watched the window: the first
    /// poll after it doesn't flash for a reset that happened meanwhile.
    #[tokio::test(start_paused = true)]
    async fn a_poll_after_parking_does_not_alert() {
        let r = reset_time();
        let source = Scripted(Arc::new(Mutex::new(
            vec![
                session(70.0, r),
                session(3.0, r + chrono::Duration::hours(5)),
            ]
            .into(),
        )));
        let hub = UsageHub::new(shared(source), HistoryStore::in_memory());
        hub.track("on", alerting_gauge(true, 90.0));
        let key = FakeSurface::new("on", true);
        let lookup = |_: String| std::future::ready(Some(key.clone()));
        hub.refresh_all_with(lookup).await;
        hub.forget_last_poll(); // what poll_loop does before parking
        tokio::time::advance(Duration::from_secs(61)).await;
        hub.refresh_all_with(lookup).await;
        assert!(key.alerts.lock().unwrap().is_empty());
    }
}
