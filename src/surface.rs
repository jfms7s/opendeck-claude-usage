//! Where a rendered frame goes, and the loop that re-renders every tracked
//! instance. `Surface` is what an OpenDeck `Instance` offers the renderers,
//! so tests can stand in for one without a connection.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use openaction::{Instance, OpenActionResult};

use crate::hub::{KEYPAD_CONTROLLER, Output};

#[async_trait]
pub trait Surface: Send + Sync {
    fn is_keypad(&self) -> bool;
    async fn push(&self, output: Output) -> OpenActionResult<()>;
}

#[async_trait]
impl Surface for Instance {
    fn is_keypad(&self) -> bool {
        self.controller == KEYPAD_CONTROLLER
    }

    /// Pushes a frame via whichever surface the instance's controller
    /// has. Keypad text is drawn inside the icon (see tile.rs), so the
    /// native title is cleared to stop OpenDeck painting a second copy.
    async fn push(&self, output: Output) -> OpenActionResult<()> {
        match output {
            Output::Image(image) => {
                self.set_title(Some(String::new()), None).await?;
                self.set_image(Some(image), None).await
            }
            Output::Feedback(feedback) => self.set_feedback(&feedback).await,
        }
    }
}

#[async_trait]
impl<S: Surface + ?Sized> Surface for Arc<S> {
    fn is_keypad(&self) -> bool {
        (**self).is_keypad()
    }

    async fn push(&self, output: Output) -> OpenActionResult<()> {
        (**self).push(output).await
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
