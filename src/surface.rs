//! Where a rendered frame goes. `Surface` is what an OpenDeck `Instance`
//! offers the renderers, so tests can stand in for one without a
//! connection; `Frames` skips frames identical to the last one sent; and
//! `for_each_tracked` is the race-safe loop every periodic re-render uses.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use openaction::{Instance, OpenActionResult};

/// The wire value OpenDeck sends as `Instance::controller` for a keypad
/// tile (vs. `"Encoder"` for a dial) - confirmed against openaction 2.7's
/// own `GenericInstancePayload`, which just forwards this string verbatim.
pub const KEYPAD_CONTROLLER: &str = "Keypad";

/// A rendered frame for one surface: a keypad tile's icon, or a dial's
/// touch-strip feedback.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Image(String),
    Feedback(serde_json::Value),
}

#[async_trait]
pub trait Surface: Send + Sync {
    /// The OpenDeck instance id this surface draws.
    fn id(&self) -> &str;
    fn is_keypad(&self) -> bool;
    /// Sends one frame. `clear_title` is set on the first frame after the
    /// instance appeared: keypad text is drawn inside the icon (see
    /// tile.rs), so the native title is cleared once to stop OpenDeck
    /// painting a second copy.
    async fn send(&self, output: &Output, clear_title: bool) -> OpenActionResult<()>;
    /// Saves the instance's settings, which OpenDeck keeps across restarts.
    async fn persist(&self, settings: serde_json::Value) -> OpenActionResult<()>;
}

#[async_trait]
impl Surface for Instance {
    fn id(&self) -> &str {
        &self.instance_id
    }

    fn is_keypad(&self) -> bool {
        self.controller == KEYPAD_CONTROLLER
    }

    async fn send(&self, output: &Output, clear_title: bool) -> OpenActionResult<()> {
        match output {
            Output::Image(image) => {
                if clear_title {
                    self.set_title(Some(String::new()), None).await?;
                }
                self.set_image(Some(image.clone()), None).await
            }
            Output::Feedback(feedback) => self.set_feedback(feedback).await,
        }
    }

    async fn persist(&self, settings: serde_json::Value) -> OpenActionResult<()> {
        self.set_settings(&settings).await
    }
}

#[async_trait]
impl<S: Surface + ?Sized> Surface for Arc<S> {
    fn id(&self) -> &str {
        (**self).id()
    }

    fn is_keypad(&self) -> bool {
        (**self).is_keypad()
    }

    async fn send(&self, output: &Output, clear_title: bool) -> OpenActionResult<()> {
        (**self).send(output, clear_title).await
    }

    async fn persist(&self, settings: serde_json::Value) -> OpenActionResult<()> {
        (**self).persist(settings).await
    }
}

/// The last frame sent to each instance. Most periodic re-renders produce
/// exactly the frame already on the key - OpenDeck would still decode,
/// rasterize and write each one to the device - so an unchanged frame is
/// not sent again.
#[derive(Default)]
pub struct Frames {
    last: DashMap<String, Output>,
}

impl Frames {
    /// Forgets an instance's last frame - on `will_appear` (OpenDeck may
    /// have redrawn it from its own state) and `will_disappear`.
    pub fn forget(&self, instance_id: &str) {
        self.last.remove(instance_id);
    }

    /// Sends `output` unless it's the frame `surface` already shows.
    pub async fn push<S: Surface + ?Sized>(
        &self,
        surface: &S,
        output: Output,
    ) -> OpenActionResult<()> {
        // Recorded before the await, so of two pushes racing for one key
        // the one recorded last is also the one sent last.
        let first = match self.last.entry(surface.id().to_string()) {
            Entry::Occupied(mut seen) => {
                if *seen.get() == output {
                    return Ok(());
                }
                seen.insert(output.clone());
                false
            }
            Entry::Vacant(slot) => {
                slot.insert(output.clone());
                true
            }
        };
        let sent = surface.send(&output, first).await;
        if sent.is_err() {
            // Not on the key after all: let the next frame go out.
            self.last.remove(surface.id());
        }
        sent
    }
}

/// Renders each of `ids` with the settings `current` holds for it. The
/// ids are collected up front (holding a DashMap iterator across an
/// await would keep its shard locked), and `lookup` awaits once per
/// instance - so the settings are only read after that await, or a press
/// landing meanwhile would be drawn over with the old ones (KI-06, KI-07).
/// An instance whose lookup or settings are gone by then is skipped.
pub async fn for_each_tracked<V, T, L, LF, R, RF>(
    ids: Vec<String>,
    current: impl Fn(&str) -> Option<V>,
    mut lookup: L,
    mut render: R,
) where
    L: FnMut(String) -> LF,
    LF: Future<Output = Option<T>>,
    R: FnMut(T, V) -> RF,
    RF: Future<Output = ()>,
{
    for id in ids {
        let Some(target) = lookup(id.clone()).await else {
            continue; // disappeared between collecting the ids and now
        };
        let Some(settings) = current(&id) else {
            continue; // untracked while we awaited
        };
        render(target, settings).await;
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Mutex;

    /// Stands in for an OpenDeck instance: records every frame and every
    /// settings save it's sent, and can run a hook during `persist` (to
    /// see what the plugin's state is while that await is pending).
    #[derive(Clone)]
    pub struct FakeSurface {
        pub id: String,
        pub keypad: bool,
        pub sent: Arc<Mutex<Vec<(Output, bool)>>>,
        pub persisted: Arc<Mutex<Vec<serde_json::Value>>>,
        #[allow(clippy::type_complexity)]
        pub on_persist: Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>,
    }

    impl FakeSurface {
        pub fn new(id: &str, keypad: bool) -> Self {
            Self {
                id: id.to_string(),
                keypad,
                sent: Arc::default(),
                persisted: Arc::default(),
                on_persist: Arc::new(Mutex::new(None)),
            }
        }

        pub fn dial(id: &str) -> Self {
            Self::new(id, false)
        }

        pub fn frames(&self) -> Vec<Output> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|(o, _)| o.clone())
                .collect()
        }
    }

    #[async_trait]
    impl Surface for FakeSurface {
        fn id(&self) -> &str {
            &self.id
        }

        fn is_keypad(&self) -> bool {
            self.keypad
        }

        async fn send(&self, output: &Output, clear_title: bool) -> OpenActionResult<()> {
            self.sent
                .lock()
                .unwrap()
                .push((output.clone(), clear_title));
            Ok(())
        }

        async fn persist(&self, settings: serde_json::Value) -> OpenActionResult<()> {
            if let Some(hook) = self.on_persist.lock().unwrap().as_mut() {
                hook();
            }
            self.persisted.lock().unwrap().push(settings);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeSurface;
    use super::*;

    fn image(s: &str) -> Output {
        Output::Image(s.to_string())
    }

    #[tokio::test]
    async fn an_unchanged_frame_is_not_sent_again() {
        let frames = Frames::default();
        let key = FakeSurface::new("a", true);
        frames.push(&key, image("one")).await.unwrap();
        frames.push(&key, image("one")).await.unwrap();
        frames.push(&key, image("two")).await.unwrap();
        assert_eq!(key.frames(), vec![image("one"), image("two")]);
    }

    #[tokio::test]
    async fn only_the_first_frame_after_appearing_clears_the_title() {
        let frames = Frames::default();
        let key = FakeSurface::new("a", true);
        frames.push(&key, image("one")).await.unwrap();
        frames.push(&key, image("two")).await.unwrap();
        frames.forget("a");
        frames.push(&key, image("two")).await.unwrap();
        let clears: Vec<bool> = key.sent.lock().unwrap().iter().map(|(_, c)| *c).collect();
        assert_eq!(clears, vec![true, false, true]);
    }

    #[tokio::test]
    async fn frames_are_tracked_per_instance() {
        let frames = Frames::default();
        let (a, b) = (FakeSurface::new("a", true), FakeSurface::new("b", true));
        frames.push(&a, image("same")).await.unwrap();
        frames.push(&b, image("same")).await.unwrap();
        assert_eq!(a.frames().len(), 1);
        assert_eq!(b.frames().len(), 1);
    }
}
