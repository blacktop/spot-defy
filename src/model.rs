//! Shared domain types and newtypes used across the UI and service layers.
//!
//! These types form the contract between the TEA UI (`crate::state`,
//! `crate::update`, `crate::view`) and the two service traits
//! (`crate::api::SpotifyApi`, `crate::player::Playback`). All identifiers are
//! newtypes over `String` so the type system distinguishes a track id from an
//! album id at compile time.

use serde::{Deserialize, Serialize};

/// A Spotify track identifier (the base-62 id, not the full `spotify:track:` URI).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackId(pub String);

/// A Spotify album identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlbumId(pub String);

/// A Spotify artist identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtistId(pub String);

/// A Spotify playlist identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlaylistId(pub String);

/// The request that currently owns `Model::tracks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackListSource {
    /// Tracks for a playlist drilled into from Playlists/Search.
    Playlist(PlaylistId),
    /// The user's top tracks for a time window.
    TopTracks(TimeRange),
    /// The user's recently played tracks.
    RecentlyPlayed,
    /// The user's saved/liked tracks.
    SavedTracks,
    /// Tracks for an album drilled into from Library/Search.
    Album(AlbumId),
}

/// A single track row as rendered in lists and tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackItem {
    pub id: TrackId,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Track duration in milliseconds.
    pub duration_ms: u32,
    /// Album cover images in Spotify's returned order, usually widest first.
    pub album_art_images: Vec<AlbumArtImage>,
}

/// A Spotify album-art candidate with its source dimensions, when provided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumArtImage {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// A single album row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumItem {
    pub id: AlbumId,
    pub name: String,
    pub artist: String,
}

/// A single artist row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistItem {
    pub id: ArtistId,
    pub name: String,
}

/// A single playlist row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub id: PlaylistId,
    pub name: String,
    pub owner: String,
    pub track_count: u32,
}

/// High-level playback lifecycle, modeled as a state machine rather than flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlaybackState {
    /// No track loaded.
    #[default]
    Stopped,
    /// A track is buffering/loading before audio starts.
    Loading,
    /// Audio is actively playing.
    Playing,
    /// A track is loaded but paused.
    Paused,
}

/// A point-in-time snapshot of what is playing, shared with the IPC layer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlaybackSnapshot {
    pub track: Option<String>,
    pub artist: Option<String>,
    pub state: PlaybackState,
    pub position_ms: u32,
    pub duration_ms: u32,
    /// App (librespot) volume as a 0..=100 percentage.
    pub volume: u16,
}

/// Time window for top-items discovery queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TimeRange {
    /// Approximately the last four weeks.
    ShortTerm,
    /// Approximately the last six months.
    #[default]
    MediumTerm,
    /// Calculated from several years of listening history.
    LongTerm,
}
