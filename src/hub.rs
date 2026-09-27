//! Shared state behind every usage-driven action (Usage Gauge, Burn Rate,
//! Session + Weekly, Usage Sparkline): one snapshot cache, one registry of
//! visible instances, one 20s poll loop - so adding an action never adds
//! another poller, and a single read serves every key and dial. It also
//! records each successful read into the `HistoryStore` the sparkline
//! draws from.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Local, Utc};
use dashmap::DashMap;
use openaction::{Instance, OpenActionResult};
use tokio::sync::RwLock;

use crate::burn::{BurnMetric, build_burn_display, burn_error_display, burn_feedback};
use crate::burn_icon::build_burn_icon;
use crate::combo::{ComboLayout, combo_feedback};
use crate::format::{build_display, error_display, feedback_for_display};
use crate::history::{HistoryStore, Reading};
use crate::level::ColorSettings;
use crate::source::{UsageSnapshot, UsageSource, UsageSourceError, WindowKind};
use crate::sparkline::{SparkSettings, build_sparkline};
use crate::style::GaugeStyle;
use crate::styles::build_styled_icon;
use crate::styles::combo::render as combo_key;
use crate::styles::sparkline::{render_key as sparkline_key, sparkline_feedback};
use crate::surface::{Surface, for_each_tracked};
use crate::tile;

/// The wire value OpenDeck sends as `Instance::controller` for a keypad
/// tile (vs. `"Encoder"` for a dial) - confirmed against openaction 2.7's
/// own `GenericInstancePayload`, which just forwards this string verbatim.
pub(crate) const KEYPAD_CONTROLLER: &str = "Keypad";

/// What one instance shows - built from its action's settings.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Gauge {
        window: WindowKind,
        colors: ColorSettings,
        style: GaugeStyle,
    },
    Burn {
        window: WindowKind,
        metric: BurnMetric,
        colors: ColorSettings,
    },
    Combo {
        colors: ColorSettings,
        layout: ComboLayout,
    },
    Sparkline {
        settings: SparkSettings,
        colors: ColorSettings,
    },
}

/// A rendered frame for one surface: a keypad tile's icon, or a dial's
/// touch-strip feedback.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Image(String),
    Feedback(serde_json::Value),
}

/// Renders one instance's frame. Pure, so every view × surface × data
/// state is unit-testable without an OpenDeck connection. `history` is the
/// recorded readings; only `View::Sparkline` uses it.
pub fn output_for(
    view: &View,
    snapshot: Option<&UsageSnapshot>,
    history: &[Reading],
    keypad: bool,
    now: DateTime<Utc>,
) -> Output {
    match view {
        View::Gauge {
            window,
            colors,
            style,
        } => {
            let display = match snapshot {
                Some(s) => build_display(s, *window, colors, now),
                None => error_display(),
            };
            if keypad {
                Output::Image(build_styled_icon(&display, *style))
            } else {
                Output::Feedback(feedback_for_display(&display))
            }
        }
        View::Burn {
            window,
            metric,
            colors,
        } => {
            let display = match snapshot {
                Some(s) => build_burn_display(s, *window, *metric, colors, now),
                None => burn_error_display(*metric),
            };
            if keypad {
                Output::Image(build_burn_icon(&display))
            } else {
                Output::Feedback(burn_feedback(&display))
            }
        }
        View::Combo { colors, layout } => {
            let (session, weekly) = match snapshot {
                Some(s) => (
                    build_display(s, WindowKind::Session, colors, now),
                    build_display(s, WindowKind::Weekly, colors, now),
                ),
                None => (error_display(), error_display()),
            };
            if keypad {
                Output::Image(tile::data_uri(&combo_key(&session, &weekly, *layout)))
            } else {
                Output::Feedback(combo_feedback(&session, &weekly))
            }
        }
        View::Sparkline { settings, colors } => {
            let display = build_sparkline(history, settings, colors, now.with_timezone(&Local));
            if keypad {
                Output::Image(tile::data_uri(&sparkline_key(&display)))
            } else {
                Output::Feedback(sparkline_feedback(&display))
            }
        }
    }
}

pub struct UsageHub {
    source: Box<dyn UsageSource>,
    latest: RwLock<Option<UsageSnapshot>>,
    registry: DashMap<String, View>,
    /// Tracks whether the poll loop's most recent read succeeded, so it
    /// logs a `warn!` only on the transition into failing (and an `info!`
    /// only on the recovery) instead of every ~20s tick forever. Starts
    /// `true` so the very first failure is logged. Not touched by
    /// `refresh_one` - a single manual press failing isn't part of that
    /// noise pattern.
    poll_last_read_ok: AtomicBool,
    /// Every successful read is recorded here for the sparkline.
    history: Arc<HistoryStore>,
}

impl UsageHub {
    pub fn new(source: impl UsageSource + 'static, history: Arc<HistoryStore>) -> Arc<Self> {
        Arc::new(Self {
            source: Box::new(source),
            latest: RwLock::new(None),
            registry: DashMap::new(),
            poll_last_read_ok: AtomicBool::new(true),
            history,
        })
    }

    pub fn track(&self, instance_id: &str, view: View) {
        self.registry.insert(instance_id.to_string(), view);
    }

    pub fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
    }

    /// Renders from the last cached snapshot (no fresh read) - used when an
    /// instance appears or its settings change, so it shows *something*
    /// immediately rather than waiting for the next poll tick.
    pub async fn render_cached(&self, instance: &Instance, view: &View) -> OpenActionResult<()> {
        let snapshot = self.latest.read().await.clone();
        let output = output_for(
            view,
            snapshot.as_ref(),
            &self.history_for(view),
            instance.is_keypad(),
            Utc::now(),
        );
        instance.push(output).await
    }

    /// The recorded readings, copied only for the one view that plots them.
    fn history_for(&self, view: &View) -> Vec<Reading> {
        match view {
            View::Sparkline { .. } => self.history.readings(),
            _ => Vec::new(),
        }
    }

    /// Reads the source and caches it on success - shared by `refresh_one`
    /// and `refresh_all` so "read, then cache" exists in one place.
    async fn read_and_cache(&self) -> Result<UsageSnapshot, UsageSourceError> {
        let result = self.source.read().await;
        if let Ok(snapshot) = &result {
            self.history.record(snapshot, Utc::now());
            *self.latest.write().await = Some(snapshot.clone());
        }
        result
    }

    /// Reads directly and renders just this instance - a dial press or a
    /// keypad tap, without waiting for the next tick.
    pub async fn refresh_one(&self, instance: &Instance, view: &View) -> OpenActionResult<()> {
        let result = self.read_and_cache().await;
        if let Err(e) = &result {
            log::warn!("usage source read failed: {e}");
        }
        let output = output_for(
            view,
            result.as_ref().ok(),
            &self.history_for(view),
            instance.is_keypad(),
            Utc::now(),
        );
        instance.push(output).await
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

    /// Runs forever: every ~20s, reads once and re-renders every tracked
    /// instance of every usage-driven action. Spawned once from `main.rs`.
    pub async fn poll_loop(self: Arc<Self>) {
        loop {
            self.refresh_all().await;
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        }
    }

    async fn refresh_all(&self) {
        let read_result = self.read_and_cache().await;
        self.log_poll_read_transition(read_result.is_ok(), read_result.as_ref().err());

        self.render_tracked(read_result.as_ref().ok(), openaction::get_instance)
            .await;
    }

    /// Re-renders every tracked instance from `snapshot`, reading each
    /// one's view only once its instance has been looked up (see
    /// `for_each_tracked`).
    async fn render_tracked<S, L, LF>(&self, snapshot: Option<&UsageSnapshot>, lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        let ids = self.registry.iter().map(|e| e.key().clone()).collect();
        for_each_tracked(
            ids,
            |id| self.registry.get(id).map(|v| v.clone()),
            lookup,
            |instance, view| async move {
                let output = output_for(
                    &view,
                    snapshot,
                    &self.history_for(&view),
                    instance.is_keypad(),
                    Utc::now(),
                );
                if let Err(e) = instance.push(output).await {
                    log::warn!("render failed: {e}");
                }
            },
        )
        .await;
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use async_trait::async_trait;

    /// A source for tests that never read usage.
    pub struct NeverCalled;

    #[async_trait]
    impl UsageSource for NeverCalled {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            unreachable!("this test never triggers a read")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use crate::style::ALL_STYLES;
    use async_trait::async_trait;
    use chrono::TimeZone;

    use test_support::NeverCalled;

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
        }
    }

    /// Always succeeds with `snapshot()` - lets `read_and_cache` populate
    /// the cache, unlike `NeverCalled`.
    struct AlwaysOk;

    #[async_trait]
    impl UsageSource for AlwaysOk {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            Ok(snapshot())
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 20, 30, 0).unwrap()
    }

    fn gauge() -> View {
        View::Gauge {
            window: WindowKind::Session,
            colors: ColorSettings::default(),
            style: GaugeStyle::Speedometer,
        }
    }

    fn burn() -> View {
        View::Burn {
            window: WindowKind::Session,
            metric: BurnMetric::EvenBurn,
            colors: ColorSettings::default(),
        }
    }

    #[tokio::test]
    async fn read_and_cache_populates_the_cached_snapshot() {
        let hub = UsageHub::new(AlwaysOk, HistoryStore::in_memory());
        hub.read_and_cache().await.unwrap();
        assert!(hub.latest.read().await.is_some());
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let hub = UsageHub::new(NeverCalled, HistoryStore::in_memory());
        hub.track("ctx1", gauge());
        assert_eq!(*hub.registry.get("ctx1").unwrap(), gauge());
        hub.untrack("ctx1");
        assert!(hub.registry.get("ctx1").is_none());
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_view() {
        let hub = UsageHub::new(NeverCalled, HistoryStore::in_memory());
        hub.track("ctx1", gauge());
        hub.track("ctx1", burn());
        assert_eq!(*hub.registry.get("ctx1").unwrap(), burn());
    }

    #[test]
    fn gauge_and_burn_instances_share_one_registry() {
        let hub = UsageHub::new(NeverCalled, HistoryStore::in_memory());
        hub.track("gauge", gauge());
        hub.track("burn", burn());
        assert_eq!(hub.registry.len(), 2);
    }

    /// KI-22: one poll renders every tracked instance from the same read,
    /// so a Gauge and a Burn Rate side by side both show real data.
    #[tokio::test]
    async fn one_poll_renders_both_gauge_and_burn() {
        let hub = UsageHub::new(AlwaysOk, HistoryStore::in_memory());
        hub.track("gauge", gauge());
        hub.track("burn", burn());
        let snapshot = hub.read_and_cache().await.unwrap();

        let pushed = Arc::default();
        hub.render_tracked(Some(&snapshot), |id| {
            std::future::ready(Some(FakeSurface {
                id,
                pushed: Arc::clone(&pushed),
            }))
        })
        .await;
        let mut frames = pushed.lock().unwrap().clone();
        frames.sort_by(|a, b| a.0.cmp(&b.0));

        let [
            (burn_id, Output::Feedback(b)),
            (gauge_id, Output::Feedback(g)),
        ] = &frames[..]
        else {
            panic!("expected two feedback frames, got {frames:?}");
        };
        assert_eq!((burn_id.as_str(), gauge_id.as_str()), ("burn", "gauge"));
        assert_eq!(g["percent"], "33%");
        assert!(b["detail"].as_str().unwrap().ends_with("session"));
        assert_ne!(b["detail"], "no data");
    }

    #[test]
    fn gauge_on_a_keypad_is_an_image() {
        let out = output_for(&gauge(), Some(&snapshot()), &[], true, now());
        assert!(matches!(out, Output::Image(ref s) if s.starts_with("data:image/svg+xml;base64,")));
    }

    #[test]
    fn gauge_on_a_dial_is_feedback() {
        let Output::Feedback(f) = output_for(&gauge(), Some(&snapshot()), &[], false, now()) else {
            panic!("expected feedback");
        };
        assert_eq!(f["percent"], "33%");
    }

    #[test]
    fn burn_on_a_dial_is_feedback() {
        let Output::Feedback(f) = output_for(&burn(), Some(&snapshot()), &[], false, now()) else {
            panic!("expected feedback");
        };
        assert!(f["detail"].as_str().unwrap().ends_with("session"));
    }

    #[test]
    fn burn_on_a_keypad_is_an_image() {
        assert!(matches!(
            output_for(&burn(), Some(&snapshot()), &[], true, now()),
            Output::Image(_)
        ));
    }

    #[test]
    fn no_snapshot_renders_no_data_for_both_views() {
        for view in [gauge(), burn()] {
            let Output::Feedback(f) = output_for(&view, None, &[], false, now()) else {
                panic!("expected feedback");
            };
            assert_eq!(f["detail"], "no data");
        }
    }

    #[test]
    fn every_gauge_style_is_an_image_on_a_keypad_and_unchanged_on_a_dial() {
        let dial = output_for(&gauge(), Some(&snapshot()), &[], false, now());
        for style in ALL_STYLES {
            let view = View::Gauge {
                window: WindowKind::Session,
                colors: ColorSettings::default(),
                style,
            };
            assert!(matches!(
                output_for(&view, Some(&snapshot()), &[], true, now()),
                Output::Image(_)
            ));
            assert_eq!(
                output_for(&view, Some(&snapshot()), &[], false, now()),
                dial
            );
        }
    }

    fn combo(layout: ComboLayout) -> View {
        View::Combo {
            colors: ColorSettings::default(),
            layout,
        }
    }

    #[test]
    fn combo_on_a_keypad_is_an_image_per_layout() {
        let h = output_for(
            &combo(ComboLayout::Horizontal),
            Some(&snapshot()),
            &[],
            true,
            now(),
        );
        let v = output_for(
            &combo(ComboLayout::Vertical),
            Some(&snapshot()),
            &[],
            true,
            now(),
        );
        assert!(matches!(h, Output::Image(_)));
        assert_ne!(h, v);
    }

    #[test]
    fn combo_on_a_dial_is_two_bar_feedback() {
        let Output::Feedback(f) = output_for(
            &combo(ComboLayout::Horizontal),
            Some(&snapshot()),
            &[],
            false,
            now(),
        ) else {
            panic!("expected feedback");
        };
        assert_eq!(f["s_bar"]["value"], 33.0);
        assert_eq!(f["w_bar"]["value"], 29.0);
    }

    #[test]
    fn combo_without_data_is_dashes() {
        let Output::Feedback(f) =
            output_for(&combo(ComboLayout::Vertical), None, &[], false, now())
        else {
            panic!("expected feedback");
        };
        assert_eq!(f["s_value"], "\u{2014}");
    }

    #[tokio::test]
    async fn read_and_cache_records_history() {
        let history = HistoryStore::in_memory();
        let hub = UsageHub::new(AlwaysOk, history.clone());
        hub.read_and_cache().await.unwrap();
        assert_eq!(history.readings().len(), 1);
        assert_eq!(history.readings()[0].session, 33.0);
    }

    #[tokio::test]
    async fn only_a_sparkline_gets_the_history() {
        let history = HistoryStore::in_memory();
        let hub = UsageHub::new(AlwaysOk, history.clone());
        hub.read_and_cache().await.unwrap();
        assert_eq!(hub.history_for(&sparkline_view()).len(), 1);
        for view in [gauge(), burn()] {
            assert!(hub.history_for(&view).is_empty());
        }
    }

    fn sparkline_view() -> View {
        View::Sparkline {
            settings: SparkSettings::default(),
            colors: ColorSettings::default(),
        }
    }

    #[test]
    fn sparkline_on_a_keypad_is_an_image() {
        assert!(matches!(
            output_for(&sparkline_view(), Some(&snapshot()), &[], true, now()),
            Output::Image(_)
        ));
    }

    #[test]
    fn sparkline_on_a_dial_is_chart_feedback() {
        let Output::Feedback(f) = output_for(&sparkline_view(), None, &[], false, now()) else {
            panic!("expected feedback");
        };
        assert!(
            f["chart"]
                .as_str()
                .unwrap()
                .starts_with("data:image/svg+xml;base64,")
        );
    }

    /// Stands in for an OpenDeck instance (a dial): records what it's sent.
    #[derive(Clone, Default)]
    struct FakeSurface {
        id: String,
        pushed: Arc<std::sync::Mutex<Vec<(String, Output)>>>,
    }

    #[async_trait]
    impl Surface for FakeSurface {
        fn is_keypad(&self) -> bool {
            false
        }

        async fn push(&self, output: Output) -> OpenActionResult<()> {
            self.pushed.lock().unwrap().push((self.id.clone(), output));
            Ok(())
        }
    }

    /// KI-06: a short press re-tracks the view while the poll is awaiting
    /// the instance lookup - the poll must draw the new view, not the one
    /// it saw before the await.
    #[tokio::test]
    async fn a_view_changed_during_the_lookup_is_the_one_drawn() {
        let hub = UsageHub::new(NeverCalled, HistoryStore::in_memory());
        hub.track("ctx1", gauge());
        let surface = FakeSurface::default();
        hub.render_tracked(Some(&snapshot()), |id| {
            hub.track(&id, sparkline_view()); // the press lands here
            std::future::ready(Some(surface.clone()))
        })
        .await;
        let pushed = surface.pushed.lock().unwrap();
        let [(_, Output::Feedback(f))] = pushed.as_slice() else {
            panic!("expected one feedback, got {pushed:?}");
        };
        assert!(f.get("chart").is_some(), "drew the old gauge: {f}");
    }

    #[tokio::test]
    async fn an_instance_untracked_during_the_lookup_is_not_drawn() {
        let hub = UsageHub::new(NeverCalled, HistoryStore::in_memory());
        hub.track("ctx1", gauge());
        let surface = FakeSurface::default();
        hub.render_tracked(Some(&snapshot()), |id| {
            hub.untrack(&id);
            std::future::ready(Some(surface.clone()))
        })
        .await;
        assert!(surface.pushed.lock().unwrap().is_empty());
    }
}
