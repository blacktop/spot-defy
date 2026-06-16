//! Streaming boundary: the [`Playback`] trait and its implementations.
//!
//! The trait decouples the TUI from librespot so playback can be driven by a
//! mock in tests. `next`/`previous` are an app-side queue cursor that calls
//! [`Playback::load`] — librespot's bare `Player` has no built-in queue
//! (librespot-connect/Spirc is intentionally not a dependency).

use crate::error::PlayerError;
use crate::model::TrackId;
use async_trait::async_trait;
use librespot_core::authentication::Credentials;
use tokio::sync::mpsc::UnboundedReceiver;

pub mod librespot_player;

#[cfg(test)]
pub mod mock;

/// A normalized subset of librespot's `PlayerEvent`, surfaced to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackEvent {
    /// A track started loading.
    Loading { track: TrackId },
    /// Playback started or resumed.
    Playing { track: TrackId, position_ms: u32 },
    /// Playback paused.
    Paused { track: TrackId, position_ms: u32 },
    /// The current track finished.
    EndOfTrack { track: TrackId },
    /// The current track is close enough to ending that the next queue item
    /// should be preloaded.
    PreloadNext { track: TrackId },
    /// Playback stopped without loading another track.
    Stopped { track: TrackId },
    /// A periodic position update while playing.
    PositionUpdate { position_ms: u32 },
    /// App (librespot) volume changed (0..=100 percentage).
    VolumeChanged { volume: u16 },
    /// The track is unavailable (region-restricted or not playable) — not a
    /// Premium gate.
    Unavailable { track: TrackId },
    /// The streaming session ended or errored.
    SessionDisconnected,
}

/// The streaming/playback surface used by spot-defy.
///
/// Control methods are synchronous and non-blocking (they enqueue commands on
/// the librespot player); [`Playback::connect`] is async because it performs a
/// network handshake. Every method returns [`PlayerError`] on failure and never
/// panics.
#[async_trait]
pub trait Playback: Send + Sync {
    /// Establish (or replace) the librespot streaming session.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if the handshake fails or the account is not
    /// Premium.
    async fn connect(&mut self, creds: Credentials) -> Result<(), PlayerError>;

    /// Rebuild the streaming session in place after it dropped, preserving the
    /// play queue and resuming the current track.
    ///
    /// Unlike [`Playback::connect`], this takes `&self` so it can be called on
    /// the shared `Arc<dyn Playback>` runtime handle when a broken pipe is
    /// detected mid-session.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if the new handshake fails.
    async fn reconnect(&self, creds: Credentials) -> Result<(), PlayerError>;

    /// Load `tracks` as the play queue and begin at `start_index`.
    ///
    /// The queue is what `next`/`previous` walk, so playing a track from a list
    /// passes the whole list here. `start_index` is clamped into range.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if no session is connected, `tracks` is empty, or
    /// the load fails.
    fn load_queue(
        &self,
        tracks: &[TrackId],
        start_index: usize,
        start_playing: bool,
    ) -> Result<(), PlayerError>;

    /// Resume playback of the loaded track.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if no track is loaded.
    fn play(&self) -> Result<(), PlayerError>;

    /// Pause playback.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if no track is loaded.
    fn pause(&self) -> Result<(), PlayerError>;

    /// Advance the app-side queue cursor and load the next track.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if the queue is empty or the load fails.
    fn next(&self) -> Result<(), PlayerError>;

    /// Preload the next app-side queue item after `current`, if one exists.
    ///
    /// This is a best-effort latency hint from the streaming engine. A stale
    /// hint, missing cursor, or end-of-queue condition is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if the player state lock is poisoned or the next
    /// track id cannot be converted into a playable URI.
    fn preload_next(&self, current: &TrackId) -> Result<(), PlayerError>;

    /// Rewind the app-side queue cursor and load the previous track.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if the queue is empty or the load fails.
    fn previous(&self) -> Result<(), PlayerError>;

    /// Seek the current track to `position_ms`.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if no track is loaded.
    fn seek(&self, position_ms: u32) -> Result<(), PlayerError>;

    /// Set the app (librespot) output volume (0..=100 percentage). This is
    /// independent of the macOS system volume.
    ///
    /// # Errors
    ///
    /// Returns [`PlayerError`] if the mixer rejects the value.
    fn set_volume(&self, volume: u16) -> Result<(), PlayerError>;

    /// Subscribe to the stream of [`PlaybackEvent`]s.
    fn subscribe(&self) -> UnboundedReceiver<PlaybackEvent>;
}
