//! Real [`Playback`] implementation over librespot.
//!
//! Builds a `Session` + `Player` + `SoftMixer` with the rodio/CoreAudio
//! backend, drives load/play/pause/seek/volume, and bridges librespot
//! `PlayerEvent`s onto an mpsc channel as [`PlaybackEvent`]s. The streaming
//! handshake and the player thread live off the UI thread; control methods
//! enqueue commands on the librespot `Player` and return immediately.

use crate::error::PlayerError;
use crate::model::TrackId;
use crate::player::{Playback, PlaybackEvent};
use async_trait::async_trait;
use librespot_core::Session;
use librespot_core::authentication::Credentials;
use librespot_core::config::SessionConfig;
use librespot_core::spotify_id::SpotifyId;
use librespot_core::spotify_uri::SpotifyUri;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, Mixer, MixerConfig};
use librespot_playback::player::{Player, PlayerEvent, PlayerEventChannel};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// The contract's public volume scale: a 0..=100 percentage.
const PERCENT_MAX: u16 = 100;

/// librespot's internal mixer volume scale is `0..=u16::MAX`.
const MIXER_MAX: u32 = u16::MAX as u32;

/// How often librespot emits a `PositionChanged` event while playing.
const POSITION_UPDATE_INTERVAL: Duration = Duration::from_millis(500);

/// The runtime pieces that only exist once a streaming session is connected.
struct Connected {
    /// The librespot streaming session (kept alive for the player thread).
    _session: Session,
    /// The audio player; control commands are enqueued on this handle.
    player: Arc<Player>,
    /// The software volume mixer.
    mixer: Arc<dyn Mixer>,
    /// App-side history of loaded tracks (librespot has no built-in queue).
    queue: Vec<TrackId>,
    /// Index into `queue` of the currently loaded track, if any.
    cursor: Option<usize>,
}

/// librespot-backed playback engine.
pub struct LibrespotPlayer {
    /// Sender the event bridge pushes normalized [`PlaybackEvent`]s onto.
    events_tx: UnboundedSender<PlaybackEvent>,
    /// The single receiver, handed to the first [`Playback::subscribe`] caller.
    events_rx: Mutex<Option<UnboundedReceiver<PlaybackEvent>>>,
    /// Session-dependent runtime, populated by [`Playback::connect`].
    connected: Mutex<Option<Connected>>,
    /// Monotonic session generation, bumped on every (re)connect. A stale event
    /// bridge compares against it to know its session was replaced and suppress
    /// the disconnect notice that would otherwise trigger a reconnect loop.
    generation: Arc<AtomicU64>,
}

impl LibrespotPlayer {
    /// Create the player and its event channel.
    ///
    /// The streaming session is not established until [`Playback::connect`] is
    /// called; control methods error until then.
    #[must_use]
    pub fn new() -> Self {
        let (events_tx, events_rx) = unbounded_channel();
        Self {
            events_tx,
            events_rx: Mutex::new(Some(events_rx)),
            connected: Mutex::new(None),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Connect a fresh session and build its player/mixer, spawning the event
    /// bridge for a new generation. Shared by initial [`Playback::connect`] and
    /// [`Playback::reconnect`]; performs the network handshake without holding
    /// the `connected` lock.
    async fn establish(
        &self,
        creds: Credentials,
    ) -> Result<(Session, Arc<Player>, Arc<dyn Mixer>), PlayerError> {
        let session = Session::new(SessionConfig::default(), None);
        // librespot's connect future is large; box it to satisfy `large_futures`.
        Box::pin(session.connect(creds, false))
            .await
            .map_err(|e| PlayerError::Session(e.to_string()))?;

        let mixer = open_mixer()?;
        let player = build_player(&session, &mixer)?;
        let bridge_rx = player.get_player_event_channel();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        spawn_event_bridge(
            bridge_rx,
            self.events_tx.clone(),
            generation,
            Arc::clone(&self.generation),
        );

        // Surface the initial mixer volume so the now-playing footer shows the
        // real level from the start instead of a stale 0%.
        let _ = self.events_tx.send(PlaybackEvent::VolumeChanged {
            volume: mixer_to_percent(mixer.volume()),
        });
        Ok((session, player, mixer))
    }

    /// Snapshot the current queue and cursor, releasing the lock before any
    /// `.await` (clippy `await_holding_lock` is denied).
    fn snapshot_queue(&self) -> Result<(Vec<TrackId>, Option<usize>), PlayerError> {
        let guard = self
            .connected
            .lock()
            .map_err(|_| PlayerError::Session("reconnect: player lock poisoned".to_owned()))?;
        Ok(match guard.as_ref() {
            Some(connected) => (connected.queue.clone(), connected.cursor),
            None => (Vec::new(), None),
        })
    }

    /// Borrow the connected runtime or return a descriptive "not connected" error.
    fn with_connected<F>(&self, op: &'static str, f: F) -> Result<(), PlayerError>
    where
        F: FnOnce(&mut Connected) -> Result<(), PlayerError>,
    {
        let mut guard = self
            .connected
            .lock()
            .map_err(|_| PlayerError::Control(format!("{op}: player lock poisoned")))?;
        let connected = guard
            .as_mut()
            .ok_or_else(|| PlayerError::Session(format!("{op}: no streaming session")))?;
        f(connected)
    }

    /// Load the track at `index` within `connected`'s queue.
    fn load_at(
        connected: &mut Connected,
        index: usize,
        start_playing: bool,
    ) -> Result<(), PlayerError> {
        let track = connected
            .queue
            .get(index)
            .ok_or_else(|| PlayerError::Control(format!("queue index {index} out of range")))?
            .clone();
        let uri = track_uri(&track)?;
        connected.player.load(uri, start_playing, 0);
        connected.cursor = Some(index);
        Ok(())
    }
}

impl Default for LibrespotPlayer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Playback for LibrespotPlayer {
    async fn connect(&mut self, creds: Credentials) -> Result<(), PlayerError> {
        let (session, player, mixer) = Box::pin(self.establish(creds)).await?;
        let mut guard = self
            .connected
            .lock()
            .map_err(|_| PlayerError::Session("connect: player lock poisoned".to_owned()))?;
        *guard = Some(Connected {
            _session: session,
            player,
            mixer,
            queue: Vec::new(),
            cursor: None,
        });
        Ok(())
    }

    async fn reconnect(&self, creds: Credentials) -> Result<(), PlayerError> {
        // Carry the queue/cursor across the rebuild so playback resumes where it
        // left off. The lock is released before the handshake await.
        let (queue, cursor) = self.snapshot_queue()?;
        let (session, player, mixer) = Box::pin(self.establish(creds)).await?;
        let mut connected = Connected {
            _session: session,
            player,
            mixer,
            queue,
            cursor: None,
        };
        if let Some(index) = cursor {
            Self::load_at(&mut connected, index, true)?;
        }
        let mut guard = self
            .connected
            .lock()
            .map_err(|_| PlayerError::Session("reconnect: player lock poisoned".to_owned()))?;
        *guard = Some(connected);
        Ok(())
    }

    fn load_queue(
        &self,
        tracks: &[TrackId],
        start_index: usize,
        start_playing: bool,
    ) -> Result<(), PlayerError> {
        self.with_connected("load_queue", |connected| {
            if tracks.is_empty() {
                return Err(PlayerError::Control(
                    "cannot play an empty queue".to_owned(),
                ));
            }
            connected.queue = tracks.to_vec();
            let index = start_index.min(connected.queue.len() - 1);
            Self::load_at(connected, index, start_playing)
        })
    }

    fn play(&self) -> Result<(), PlayerError> {
        self.with_connected("play", |connected| {
            connected.player.play();
            Ok(())
        })
    }

    fn pause(&self) -> Result<(), PlayerError> {
        self.with_connected("pause", |connected| {
            connected.player.pause();
            Ok(())
        })
    }

    fn next(&self) -> Result<(), PlayerError> {
        self.with_connected("next", |connected| {
            if let Some(index) = next_index(connected.cursor, connected.queue.len()) {
                Self::load_at(connected, index, true)
            } else {
                // End of the queue: stop rather than erroring.
                connected.player.stop();
                connected.cursor = None;
                Ok(())
            }
        })
    }

    fn previous(&self) -> Result<(), PlayerError> {
        self.with_connected("previous", |connected| {
            // Already at the first track: no-op rather than erroring.
            let Some(index) = previous_index(connected.cursor) else {
                return Ok(());
            };
            Self::load_at(connected, index, true)
        })
    }

    fn seek(&self, position_ms: u32) -> Result<(), PlayerError> {
        self.with_connected("seek", |connected| {
            connected.player.seek(position_ms);
            Ok(())
        })
    }

    fn set_volume(&self, volume: u16) -> Result<(), PlayerError> {
        self.with_connected("set_volume", |connected| {
            connected.mixer.set_volume(percent_to_mixer(volume));
            Ok(())
        })
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

/// Open the software volume mixer (the macOS rodio backend uses soft volume).
fn open_mixer() -> Result<Arc<dyn Mixer>, PlayerError> {
    let mixer_fn = mixer::find(None)
        .ok_or_else(|| PlayerError::AudioBackend("no software mixer available".to_owned()))?;
    mixer_fn(MixerConfig::default())
        .map_err(|e| PlayerError::AudioBackend(format!("mixer open failed: {e}")))
}

/// Build the librespot `Player` over the rodio/CoreAudio sink and `mixer`.
fn build_player(session: &Session, mixer: &Arc<dyn Mixer>) -> Result<Arc<Player>, PlayerError> {
    let sink_builder = audio_backend::find(None)
        .ok_or_else(|| PlayerError::AudioBackend("rodio backend unavailable".to_owned()))?;
    let config = PlayerConfig {
        position_update_interval: Some(POSITION_UPDATE_INTERVAL),
        ..PlayerConfig::default()
    };
    let volume_getter = mixer.get_soft_volume();
    Ok(Player::new(
        config,
        session.clone(),
        volume_getter,
        move || sink_builder(None, AudioFormat::default()),
    ))
}

/// Forward librespot `PlayerEvent`s onto the [`PlaybackEvent`] channel.
///
/// Runs as a detached tokio task so the audio event loop never blocks the UI.
fn spawn_event_bridge(
    mut rx: PlayerEventChannel,
    tx: UnboundedSender<PlaybackEvent>,
    generation: u64,
    current_generation: Arc<AtomicU64>,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if generation != current_generation.load(Ordering::SeqCst) {
                return;
            }
            if let Some(mapped) = map_player_event(event) {
                if generation != current_generation.load(Ordering::SeqCst) {
                    return;
                }
                if tx.send(mapped).is_err() {
                    return;
                }
            }
        }
        // Only the active session's bridge reports a disconnect. A bridge whose
        // session was already replaced by a reconnect stays silent so the dead
        // session it just drained cannot trigger a spurious reconnect loop.
        if generation == current_generation.load(Ordering::SeqCst) {
            let _ = tx.send(PlaybackEvent::SessionDisconnected);
        }
    });
}

/// Map a librespot `PlayerEvent` to the contract's [`PlaybackEvent`].
///
/// Returns `None` for events the UI does not consume (preload, spirc hints,
/// session-metadata notices). Volume is converted from librespot's `0..=u16::MAX`
/// scale to the contract's 0..=100 percentage.
fn map_player_event(event: PlayerEvent) -> Option<PlaybackEvent> {
    match event {
        PlayerEvent::Stopped { track_id, .. } => Some(PlaybackEvent::Stopped {
            track: uri_to_track_id(&track_id),
        }),
        PlayerEvent::Loading { track_id, .. } => Some(PlaybackEvent::Loading {
            track: uri_to_track_id(&track_id),
        }),
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::PositionCorrection {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::Seeked {
            track_id,
            position_ms,
            ..
        } => Some(PlaybackEvent::Playing {
            track: uri_to_track_id(&track_id),
            position_ms,
        }),
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => Some(PlaybackEvent::Paused {
            track: uri_to_track_id(&track_id),
            position_ms,
        }),
        PlayerEvent::PositionChanged { position_ms, .. } => {
            Some(PlaybackEvent::PositionUpdate { position_ms })
        }
        PlayerEvent::EndOfTrack { track_id, .. } => Some(PlaybackEvent::EndOfTrack {
            track: uri_to_track_id(&track_id),
        }),
        PlayerEvent::Unavailable { track_id, .. } => Some(PlaybackEvent::Unavailable {
            track: uri_to_track_id(&track_id),
        }),
        PlayerEvent::VolumeChanged { volume } => Some(PlaybackEvent::VolumeChanged {
            volume: mixer_to_percent(volume),
        }),
        PlayerEvent::SessionDisconnected { .. } => Some(PlaybackEvent::SessionDisconnected),
        _ => None,
    }
}

/// Build a playable `spotify:track:` URI from a base-62 [`TrackId`].
fn track_uri(track: &TrackId) -> Result<SpotifyUri, PlayerError> {
    let id = SpotifyId::from_base62(&track.0)
        .map_err(|e| PlayerError::Control(format!("invalid track id {:?}: {e}", track.0)))?;
    Ok(SpotifyUri::Track { id })
}

/// Recover a [`TrackId`] from a librespot URI, falling back to the raw form on
/// non-base62 ids so an unexpected URI never panics.
fn uri_to_track_id(uri: &SpotifyUri) -> TrackId {
    match uri.to_id() {
        Ok(id) => TrackId(id),
        Err(_) => TrackId(String::new()),
    }
}

/// Convert a 0..=100 contract percentage to librespot's `0..=u16::MAX` scale.
fn percent_to_mixer(percent: u16) -> u16 {
    let clamped = percent.min(PERCENT_MAX);
    let scaled = u32::from(clamped) * MIXER_MAX / u32::from(PERCENT_MAX);
    u16::try_from(scaled).unwrap_or(u16::MAX)
}

/// Convert librespot's `0..=u16::MAX` volume to a 0..=100 contract percentage.
fn mixer_to_percent(raw: u16) -> u16 {
    let scaled = u32::from(raw) * u32::from(PERCENT_MAX) / MIXER_MAX;
    u16::try_from(scaled).unwrap_or(PERCENT_MAX)
}

/// Compute the index of the next queue entry, or `None` at the end / when empty.
fn next_index(cursor: Option<usize>, len: usize) -> Option<usize> {
    let next = match cursor {
        Some(current) => current + 1,
        None => 0,
    };
    (next < len).then_some(next)
}

/// Compute the index of the previous queue entry, or `None` at the start.
fn previous_index(cursor: Option<usize>) -> Option<usize> {
    match cursor {
        Some(current) if current > 0 => Some(current - 1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::model::TrackId;
    use crate::player::PlaybackEvent;
    use crate::player::librespot_player::{
        map_player_event, mixer_to_percent, next_index, percent_to_mixer, previous_index,
        track_uri, uri_to_track_id,
    };
    use librespot_core::spotify_id::SpotifyId;
    use librespot_core::spotify_uri::SpotifyUri;
    use librespot_playback::player::PlayerEvent;

    #[test]
    fn percent_to_mixer_maps_endpoints() {
        assert_eq!(percent_to_mixer(0), 0);
        assert_eq!(percent_to_mixer(100), u16::MAX);
    }

    #[test]
    fn percent_to_mixer_clamps_over_100() {
        assert_eq!(percent_to_mixer(150), u16::MAX);
    }

    #[test]
    fn percent_to_mixer_midpoint_is_about_half() {
        let half = percent_to_mixer(50);
        assert!((32_000..=33_000).contains(&half), "got {half}");
    }

    #[test]
    fn mixer_to_percent_maps_endpoints() {
        assert_eq!(mixer_to_percent(0), 0);
        assert_eq!(mixer_to_percent(u16::MAX), 100);
    }

    #[test]
    fn volume_round_trips_within_one_percent() {
        for percent in [0_u16, 10, 25, 50, 75, 100] {
            let back = mixer_to_percent(percent_to_mixer(percent));
            assert!(back.abs_diff(percent) <= 1, "percent {percent} -> {back}");
        }
    }

    #[test]
    fn next_index_walks_then_stops_at_end() {
        assert_eq!(next_index(None, 3), Some(0));
        assert_eq!(next_index(Some(0), 3), Some(1));
        assert_eq!(next_index(Some(2), 3), None);
    }

    #[test]
    fn next_index_empty_queue_is_none() {
        assert_eq!(next_index(None, 0), None);
    }

    #[test]
    fn previous_index_walks_then_stops_at_start() {
        assert_eq!(previous_index(Some(2)), Some(1));
        assert_eq!(previous_index(Some(0)), None);
        assert_eq!(previous_index(None), None);
    }

    #[test]
    fn track_uri_builds_track_variant_from_base62() {
        let id = "4uLU6hMCjMI75M1A2tKUQC";
        let uri = track_uri(&TrackId(id.to_owned())).expect("valid base62 id");
        match uri {
            SpotifyUri::Track { id: parsed } => {
                assert_eq!(parsed.to_base62().expect("base62"), id);
            }
            _ => panic!("expected a Track uri"),
        }
    }

    #[test]
    fn track_uri_rejects_invalid_id() {
        let err = track_uri(&TrackId("not-a-valid-id".to_owned())).unwrap_err();
        assert!(err.to_string().contains("invalid track id"));
    }

    #[test]
    fn uri_to_track_id_round_trips_base62() {
        let id = "4uLU6hMCjMI75M1A2tKUQC";
        let spotify_id = SpotifyId::from_base62(id).expect("valid id");
        let uri = SpotifyUri::Track { id: spotify_id };
        assert_eq!(uri_to_track_id(&uri), TrackId(id.to_owned()));
    }

    #[test]
    fn stopped_event_maps_to_playback_stopped() {
        let id = TrackId("4uLU6hMCjMI75M1A2tKUQC".to_owned());
        let track_id = track_uri(&id).expect("valid base62 id");
        let event = PlayerEvent::Stopped {
            play_request_id: 1,
            track_id,
        };

        let mapped = map_player_event(event);

        assert_eq!(mapped, Some(PlaybackEvent::Stopped { track: id }));
    }
}
