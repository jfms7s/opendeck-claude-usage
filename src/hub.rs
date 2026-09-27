//! Shared state behind every usage-driven action (Usage Gauge, Burn Rate):
//! one snapshot cache, one registry of visible instances, one 20s poll
//! loop - so adding an action never adds another poller, and a single
//! read serves every key and dial.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use openaction::{Instance, OpenActionResult};
use tokio::sync::RwLock;

use crate::burn::{BurnMetric, build_burn_display, burn_error_display, burn_feedback};
use crate::burn_icon::build_burn_icon;
use crate::combo::{ComboLayout, combo_feedback};
use crate::format::{build_display, error_display, feedback_for_display};
use crate::level::ColorSettings;
use crate::source::{UsageSnapshot, UsageSource, UsageSourceError, WindowKind};
use crate::style::GaugeStyle;
use crate::styles::build_styled_icon;
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
}

/// A rendered frame for one surface: a keypad tile's icon, or a dial's
/// touch-strip feedback.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Image(String),
    Feedback(serde_json::Value),
}

/// Renders one instance's frame. Pure, so every view × surface × data
/// state is unit-testable without an OpenDeck connection.
pub fn output_for(
    view: &View,
    snapshot: Option<&UsageSnapshot>,
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
                Output::Image(tile::data_uri(&crate::styles::combo::render(
                    &session, &weekly, *layout,
                )))
            } else {
                Output::Feedback(combo_feedback(&session, &weekly))
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
}

impl UsageHub {
    pub fn new(source: impl UsageSource + 'static) -> Arc<Self> {
        Arc::new(Self {
            source: Box::new(source),
            latest: RwLock::new(None),
            registry: DashMap::new(),
            poll_last_read_ok: AtomicBool::new(true),
        })
    }

    pub fn track(&self, instance_id: &str, view: View) {
        self.registry.insert(instance_id.to_string(), view);
    }

    pub fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
    }

    /// Pushes a frame via whichever surface the instance's controller
    /// has. Keypad text is drawn inside the icon (see tile.rs), so the
    /// native title is cleared to stop OpenDeck painting a second copy.
    async fn push(instance: &Instance, output: Output) -> OpenActionResult<()> {
        match output {
            Output::Image(image) => {
                instance.set_title(Some(String::new()), None).await?;
                instance.set_image(Some(image), None).await
            }
            Output::Feedback(feedback) => instance.set_feedback(&feedback).await,
        }
    }

    fn is_keypad(instance: &Instance) -> bool {
        instance.controller == KEYPAD_CONTROLLER
    }

    /// Renders from the last cached snapshot (no fresh read) - used when an
    /// instance appears or its settings change, so it shows *something*
    /// immediately rather than waiting for the next poll tick.
    pub async fn render_cached(&self, instance: &Instance, view: &View) -> OpenActionResult<()> {
        let snapshot = self.latest.read().await.clone();
        let output = output_for(
            view,
            snapshot.as_ref(),
            Self::is_keypad(instance),
            Utc::now(),
        );
        Self::push(instance, output).await
    }

    /// Reads the source and caches it on success - shared by `refresh_one`
    /// and `refresh_all` so "read, then cache" exists in one place.
    async fn read_and_cache(&self) -> Result<UsageSnapshot, UsageSourceError> {
        let result = self.source.read().await;
        if let Ok(snapshot) = &result {
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
            Self::is_keypad(instance),
            Utc::now(),
        );
        Self::push(instance, output).await
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

        // Collect first, releasing the DashMap shard lock before awaiting
        // per instance - holding an iterator guard across an await would
        // keep that shard locked for the whole loop.
        let entries: Vec<(String, View)> = self
            .registry
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();

        for (instance_id, view) in entries {
            let Some(instance) = openaction::get_instance(instance_id).await else {
                continue; // disappeared between the snapshot and now
            };
            let output = output_for(
                &view,
                read_result.as_ref().ok(),
                Self::is_keypad(&instance),
                Utc::now(),
            );
            if let Err(e) = Self::push(&instance, output).await {
                log::warn!("render failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use crate::style::GaugeStyle;
    use async_trait::async_trait;
    use chrono::TimeZone;

    struct NeverCalled;

    #[async_trait]
    impl UsageSource for NeverCalled {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            unreachable!("this test never triggers a read")
        }
    }

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
        let hub = UsageHub::new(AlwaysOk);
        hub.read_and_cache().await.unwrap();
        assert!(hub.latest.read().await.is_some());
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let hub = UsageHub::new(NeverCalled);
        hub.track("ctx1", gauge());
        assert_eq!(*hub.registry.get("ctx1").unwrap(), gauge());
        hub.untrack("ctx1");
        assert!(hub.registry.get("ctx1").is_none());
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_view() {
        let hub = UsageHub::new(NeverCalled);
        hub.track("ctx1", gauge());
        hub.track("ctx1", burn());
        assert_eq!(*hub.registry.get("ctx1").unwrap(), burn());
    }

    #[test]
    fn gauge_and_burn_instances_share_one_registry() {
        let hub = UsageHub::new(NeverCalled);
        hub.track("gauge", gauge());
        hub.track("burn", burn());
        assert_eq!(hub.registry.len(), 2);
    }

    #[test]
    fn gauge_on_a_keypad_is_an_image() {
        let out = output_for(&gauge(), Some(&snapshot()), true, now());
        assert!(matches!(out, Output::Image(ref s) if s.starts_with("data:image/svg+xml;base64,")));
    }

    #[test]
    fn gauge_on_a_dial_is_feedback() {
        let Output::Feedback(f) = output_for(&gauge(), Some(&snapshot()), false, now()) else {
            panic!("expected feedback");
        };
        assert_eq!(f["percent"], "33%");
    }

    #[test]
    fn burn_on_a_dial_is_feedback() {
        let Output::Feedback(f) = output_for(&burn(), Some(&snapshot()), false, now()) else {
            panic!("expected feedback");
        };
        assert!(f["detail"].as_str().unwrap().ends_with("session"));
    }

    #[test]
    fn burn_on_a_keypad_is_an_image() {
        assert!(matches!(
            output_for(&burn(), Some(&snapshot()), true, now()),
            Output::Image(_)
        ));
    }

    #[test]
    fn no_snapshot_renders_no_data_for_both_views() {
        for view in [gauge(), burn()] {
            let Output::Feedback(f) = output_for(&view, None, false, now()) else {
                panic!("expected feedback");
            };
            assert_eq!(f["detail"], "no data");
        }
    }

    #[test]
    fn every_gauge_style_is_an_image_on_a_keypad_and_unchanged_on_a_dial() {
        let dial = output_for(&gauge(), Some(&snapshot()), false, now());
        for style in crate::style::ALL_STYLES {
            let view = View::Gauge {
                window: WindowKind::Session,
                colors: ColorSettings::default(),
                style,
            };
            assert!(matches!(
                output_for(&view, Some(&snapshot()), true, now()),
                Output::Image(_)
            ));
            assert_eq!(output_for(&view, Some(&snapshot()), false, now()), dial);
        }
    }

    fn combo(layout: crate::combo::ComboLayout) -> View {
        View::Combo {
            colors: ColorSettings::default(),
            layout,
        }
    }

    #[test]
    fn combo_on_a_keypad_is_an_image_per_layout() {
        use crate::combo::ComboLayout;
        let h = output_for(
            &combo(ComboLayout::Horizontal),
            Some(&snapshot()),
            true,
            now(),
        );
        let v = output_for(
            &combo(ComboLayout::Vertical),
            Some(&snapshot()),
            true,
            now(),
        );
        assert!(matches!(h, Output::Image(_)));
        assert_ne!(h, v);
    }

    #[test]
    fn combo_on_a_dial_is_two_bar_feedback() {
        let Output::Feedback(f) = output_for(
            &combo(crate::combo::ComboLayout::Horizontal),
            Some(&snapshot()),
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
        let Output::Feedback(f) = output_for(
            &combo(crate::combo::ComboLayout::Vertical),
            None,
            false,
            now(),
        ) else {
            panic!("expected feedback");
        };
        assert_eq!(f["s_value"], "\u{2014}");
    }
}
