//! The TEA `Message`/`Action` split.
//!
//! A [`Message`] is an input event fed to `crate::update::update`; it mutates
//! the `Model` and returns [`Action`]s. An [`Action`] is a side-effecting
//! request the `crate::app` event loop turns into a spawned task that sends
//! further [`Message`]s back. `update` itself performs no I/O.

use crate::api::SearchResultset;
use crate::error::ApiError;
use crate::ipc::NowPlayingPayload;
use crate::model::{
    AlbumArtImage, AlbumId, AlbumItem, ArtistItem, PlaybackSnapshot, PlaylistId, PlaylistItem,
    TimeRange, TrackId, TrackItem, TrackListSource,
};
use crate::player::PlaybackEvent;
use crate::state::Screen;
use crossterm::event::KeyEvent;
use tokio::sync::oneshot;

/// An event delivered to the pure `update` reducer.
#[derive(Debug)]
pub enum Message {
    /// User requested to quit.
    Quit,
    /// Periodic tick (drives debounce + position interpolation).
    Tick,
    /// A raw key event from crossterm.
    KeyPress(KeyEvent),
    /// The terminal was resized.
    Resize(u16, u16),
    /// Switch to a different screen.
    EnterScreen(Screen),
    /// Enter text-insertion mode.
    EnterInsertMode,
    /// Leave text-insertion mode.
    ExitInsertMode,
    /// A character was typed into the search box.
    SearchInputChar(char),
    /// Backspace in the search box.
    SearchBackspace,
    /// Submit the current search query.
    SearchSubmit,
    /// Search results returned from the API.
    SearchResults(Result<SearchResultset, ApiError>),
    /// Move selection to the next item.
    SelectNext,
    /// Move selection to the previous item.
    SelectPrevious,
    /// Move selection to the first item.
    SelectFirst,
    /// Move selection to the last item.
    SelectLast,
    /// Activate the selected item (open playlist, play track, ...).
    ActivateSelection,
    /// The user's playlists finished loading.
    PlaylistsLoaded(Result<Vec<PlaylistItem>, ApiError>),
    /// A requested track list finished loading.
    TrackListLoaded {
        source: TrackListSource,
        result: Result<Vec<TrackItem>, ApiError>,
    },
    /// Top artists finished loading.
    TopArtistsLoaded(Result<Vec<ArtistItem>, ApiError>),
    /// Saved albums finished loading.
    SavedAlbumsLoaded(Result<Vec<AlbumItem>, ApiError>),
    /// A normalized playback event from the streaming engine.
    PlaybackEvent(PlaybackEvent),
    /// Toggle between play and pause.
    TogglePlayPause,
    /// Skip to the next queued track.
    NextTrack,
    /// Skip to the previous queued track.
    PrevTrack,
    /// Seek by a relative number of milliseconds (may be negative).
    SeekRelative(i32),
    /// Change the app (librespot) volume by a relative percentage.
    VolumeDelta(i16),
    /// An IPC client asked for the current now-playing line.
    NowPlayingRequested(oneshot::Sender<NowPlayingPayload>),
    /// A non-fatal error to surface in the UI.
    Error(String),
}

/// A side-effecting request produced by `update`, executed by the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run a search query.
    Search { query: String, limit: u32 },
    /// Load the user's playlists.
    LoadPlaylists,
    /// Load a playlist's tracks.
    LoadPlaylistTracks(PlaylistId),
    /// Load top tracks for a time window.
    LoadTopTracks(TimeRange),
    /// Load top artists for a time window.
    LoadTopArtists(TimeRange),
    /// Load recently played tracks.
    LoadRecentlyPlayed,
    /// Load saved tracks.
    LoadSavedTracks,
    /// Load the user's saved albums.
    LoadSavedAlbums,
    /// Load an album's tracks (then drill into the Tracks screen).
    LoadAlbumTracks(AlbumId),
    /// Load a list of tracks as the play queue, starting at `index`.
    PlayerLoad { queue: Vec<TrackId>, index: usize },
    /// Resume playback.
    PlayerPlay,
    /// Pause playback.
    PlayerPause,
    /// Advance to the next queued track.
    PlayerNext,
    /// Preload the next queued track while `current` is still playing.
    PlayerPreloadNext { current: TrackId },
    /// Go back to the previous queued track.
    PlayerPrev,
    /// Rebuild the dropped streaming session and resume the current track.
    PlayerReconnect,
    /// Seek to an absolute position.
    PlayerSeek(u32),
    /// Set absolute app (librespot) volume (0..=100).
    PlayerSetVolume(u16),
    /// Download + decode the best now-playing album-art candidate for the pane.
    LoadAlbumArt(Vec<AlbumArtImage>),
    /// Publish a now-playing snapshot to the IPC layer.
    PublishNowPlaying(PlaybackSnapshot),
}
