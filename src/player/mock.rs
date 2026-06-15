//! In-memory mock [`Playback`] for tests.
//!
//! Records control calls and lets tests push synthetic [`PlaybackEvent`]s.

use crate::error::PlayerError;
use crate::model::TrackId;
use crate::player::{Playback, PlaybackEvent};
use async_trait::async_trait;
use std::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// A mock player that succeeds on every control call and records the last load.
pub struct MockPlayer {
    pub last_loaded: Mutex<Option<TrackId>>,
    events_tx: UnboundedSender<PlaybackEvent>,
    events_rx: Mutex<Option<UnboundedReceiver<PlaybackEvent>>>,
}

impl MockPlayer {
    /// Create a mock player with an attached event channel.
    #[must_use]
    pub fn new() -> Self {
        let (events_tx, events_rx) = unbounded_channel();
        Self {
            last_loaded: Mutex::new(None),
            events_tx,
            events_rx: Mutex::new(Some(events_rx)),
        }
    }

    /// Push a synthetic event onto the stream for assertions.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError::Control`] if the receiver has been dropped.
    pub fn emit(&self, event: PlaybackEvent) -> Result<(), PlayerError> {
        self.events_tx
            .send(event)
            .map_err(|e| PlayerError::Control(e.to_string()))
    }
}

impl Default for MockPlayer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Playback for MockPlayer {
    async fn connect(
        &mut self,
        _creds: librespot_core::authentication::Credentials,
    ) -> Result<(), PlayerError> {
        Ok(())
    }

    async fn reconnect(
        &self,
        _creds: librespot_core::authentication::Credentials,
    ) -> Result<(), PlayerError> {
        Ok(())
    }

    fn load_queue(
        &self,
        tracks: &[TrackId],
        start_index: usize,
        _start_playing: bool,
    ) -> Result<(), PlayerError> {
        if let Some(track) = tracks.get(start_index) {
            if let Ok(mut slot) = self.last_loaded.lock() {
                *slot = Some(track.clone());
            }
        }
        Ok(())
    }

    fn play(&self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn pause(&self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn next(&self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn previous(&self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn seek(&self, _position_ms: u32) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_volume(&self, _volume: u16) -> Result<(), PlayerError> {
        Ok(())
    }

    fn subscribe(&self) -> UnboundedReceiver<PlaybackEvent> {
        if let Ok(mut slot) = self.events_rx.lock() {
            if let Some(rx) = slot.take() {
                return rx;
            }
        }
        let (tx, rx) = unbounded_channel();
        drop(tx);
        rx
    }
}
