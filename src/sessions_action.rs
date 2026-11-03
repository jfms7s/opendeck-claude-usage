//! Sessions: how many Claude Code sessions are running and how long the
//! longest-waiting one has waited for you, from `~/.claude/sessions`.
//! Keypad only. A short press cycles summary → each session → summary (in
//! memory only, never saved); a long press re-reads now.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::press::{PressCycler, Release};
use crate::sessions::{Focus, Session, SystemLiveness, next_focus, read_session_file, scan};
use crate::sessions_icon::build_sessions_icon;
use crate::surface::{Frames, Output, Surface, for_each_tracked};
use crate::tasks::park_while_empty;

/// How often visible Sessions keys re-read the registry: a session going
/// busy → waiting should show within seconds, and the folder is a few
/// tiny files.
const REFRESH: Duration = Duration::from_secs(15);

/// No settings yet: the marks are fixed and the focus isn't saved. Any
/// stored value (unknown fields included) loads as this.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionsSettings {}

fn output(focus: &Focus, sessions: &[Session]) -> Output {
    Output::Image(build_sessions_icon(focus, sessions, chrono::Utc::now()))
}

#[derive(Clone)]
pub struct SessionsAction {
    dir: Arc<PathBuf>,
    /// Each visible key's focus, and its presses (KI-07, KI-08).
    presses: Arc<PressCycler<Focus>>,
    frames: Arc<Frames>,
    wake: Arc<Notify>,
    /// The last read, which a short press cycles through.
    latest: Arc<Mutex<Vec<Session>>>,
    /// Set once a folder error other than "missing" has been logged.
    warned: Arc<AtomicBool>,
}

impl SessionsAction {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir: Arc::new(dir),
            presses: Arc::new(PressCycler::default()),
            frames: Arc::new(Frames::default()),
            wake: Arc::new(Notify::new()),
            latest: Arc::new(Mutex::new(Vec::new())),
            warned: Arc::new(AtomicBool::new(false)),
        }
    }

    fn latest(&self) -> Vec<Session> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Re-reads the registry on the blocking pool. A missing folder is no
    /// sessions; any other folder error too, logged once.
    async fn refresh(&self) -> Vec<Session> {
        let dir = Arc::clone(&self.dir);
        let read =
            tokio::task::spawn_blocking(move || scan(&dir, read_session_file, &SystemLiveness))
                .await;
        let sessions = match read {
            Ok(Ok(sessions)) => sessions,
            Ok(Err(e)) => {
                if e.kind() != std::io::ErrorKind::NotFound
                    && !self.warned.swap(true, Ordering::SeqCst)
                {
                    log::warn!("could not list the Claude Code sessions folder: {e}");
                }
                Vec::new()
            }
            Err(e) => {
                log::warn!("reading the Claude Code sessions panicked: {e}");
                Vec::new()
            }
        };
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = sessions.clone();
        sessions
    }

    async fn render<S: Surface + ?Sized>(
        &self,
        surface: &S,
        focus: &Focus,
        sessions: &[Session],
    ) -> OpenActionResult<()> {
        self.frames.push(surface, output(focus, sessions)).await
    }

    /// Runs forever: every `REFRESH`, re-reads the registry once and
    /// redraws every visible Sessions key. Parks while none is visible.
    /// Spawned once from `main.rs`, supervised.
    pub async fn tick_loop(self) {
        loop {
            park_while_empty(|| self.presses.is_empty(), &self.wake).await;
            tokio::time::sleep(REFRESH).await;
            let sessions = self.refresh().await;
            self.render_tracked(&sessions, openaction::get_instance)
                .await;
        }
    }

    /// Redraws every tracked key, reading each one's focus only once its
    /// instance has been looked up (see `for_each_tracked`).
    async fn render_tracked<S, L, LF>(&self, sessions: &[Session], lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        for_each_tracked(
            self.presses.ids(),
            |id| self.presses.get(id),
            lookup,
            |surface, focus| async move {
                if let Err(e) = self.render(&surface, &focus, sessions).await {
                    log::warn!("sessions render failed: {e}");
                }
            },
        )
        .await;
    }

    /// A short press moves the focus on through the last read; a long one
    /// re-reads and redraws.
    async fn released<S: Surface + ?Sized>(&self, surface: &S) -> OpenActionResult<()> {
        let sessions = self.latest();
        match self.presses.up(surface.id(), &Focus::Summary, |f| {
            Some(next_focus(f, &sessions))
        }) {
            Release::Refresh => {
                let sessions = self.refresh().await;
                let focus = self.presses.current(surface.id(), &Focus::Summary);
                self.render(surface, &focus, &sessions).await
            }
            Release::Switch(focus) => self.render(surface, &focus, &sessions).await,
            Release::Stay => Ok(()),
        }
    }
}

#[async_trait]
impl Action for SessionsAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.sessions";
    type Settings = SessionsSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.frames.forget(&instance.instance_id);
        self.presses.set(&instance.instance_id, &Focus::Summary);
        self.wake.notify_one();
        let sessions = self.refresh().await;
        self.render(instance, &Focus::Summary, &sessions).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.forget(&instance.instance_id);
        self.frames.forget(&instance.instance_id);
        Ok(())
    }

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.down(&instance.instance_id);
        Ok(())
    }

    async fn key_up(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.released(instance).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::manifest_entry;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/sessions");

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<SessionsAction as Action>::UUID);
        assert_eq!(entry["Controllers"], serde_json::json!(["Keypad"]));
        assert!(entry.get("Encoder").is_none(), "the dial strip is frozen");
    }

    #[test]
    fn any_stored_settings_load() {
        for v in [
            serde_json::json!({}),
            serde_json::json!({"focus": 3, "watch": "x"}),
        ] {
            serde_json::from_value::<SessionsSettings>(v).unwrap();
        }
    }

    /// With no `~/.claude/sessions` the key shows `0 sessions`.
    #[tokio::test]
    async fn a_missing_folder_shows_zero_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let action = SessionsAction::new(dir.path().join("sessions"));
        let sessions = action.refresh().await;
        assert!(sessions.is_empty());
        let key = FakeSurface::new("a", true);
        action
            .render(&key, &Focus::Summary, &sessions)
            .await
            .unwrap();
        let Output::Image(uri) = &key.frames()[0] else {
            panic!("not an image")
        };
        let svg = String::from_utf8(
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                uri.trim_start_matches("data:image/svg+xml;base64,"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(svg.contains(">0 sessions</text>"), "{svg}");
        assert!(!action.warned.load(Ordering::SeqCst));
    }

    /// Short presses walk the last read: summary → each session → summary.
    /// The fixtures' pids are not running here, so `latest` is set by hand.
    #[tokio::test]
    async fn short_presses_cycle_the_focus() {
        let action = SessionsAction::new(FIXTURES.into());
        let sessions =
            crate::sessions::scan(std::path::Path::new(FIXTURES), read_session_file, &AllAlive)
                .unwrap();
        *action.latest.lock().unwrap() = sessions.clone();
        action.presses.set("a", &Focus::Summary);
        let key = FakeSurface::new("a", true);
        let mut seen = vec![];
        for _ in 0..4 {
            action.released(&key).await.unwrap();
            seen.push(action.presses.get("a").unwrap());
        }
        assert_eq!(seen[3], Focus::Summary);
        assert_eq!(seen[0], Focus::Session(sessions[0].id.clone()));
        assert_eq!(key.frames().len(), 4);
        assert!(
            key.persisted.lock().unwrap().is_empty(),
            "focus is not saved"
        );
    }

    /// KI-07: a focus changed while the tick awaits the lookup is drawn.
    #[tokio::test]
    async fn a_focus_changed_during_the_lookup_is_the_one_drawn() {
        let action = SessionsAction::new(FIXTURES.into());
        let sessions =
            crate::sessions::scan(std::path::Path::new(FIXTURES), read_session_file, &AllAlive)
                .unwrap();
        let focused = Focus::Session(sessions[1].id.clone());
        action.presses.set("a", &Focus::Summary);
        let key = FakeSurface::new("a", true);
        action
            .render_tracked(&sessions, |id| {
                action.presses.set(&id, &focused);
                std::future::ready(Some(key.clone()))
            })
            .await;
        assert_eq!(key.frames(), vec![output(&focused, &sessions)]);
    }

    struct AllAlive;

    impl crate::sessions::Liveness for AllAlive {
        fn is_alive(&self, _: &Session) -> bool {
            true
        }
    }
}
