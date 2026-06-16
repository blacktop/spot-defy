//! Web API boundary: the [`SpotifyApi`] trait and its implementations.
//!
//! The trait decouples the TUI from rspotify so the UI can be driven by a mock
//! in tests. Only still-live Spotify endpoints are exposed: search, the user's
//! playlists, library/top-items discovery, and current player state. No
//! deprecated recommendations / audio-features / related-artists / featured
//! methods exist on this trait by design.

use crate::error::ApiError;
use crate::model::{
    AlbumId, AlbumItem, ArtistItem, PlaybackSnapshot, PlaylistId, PlaylistItem, TimeRange,
    TrackItem,
};
use async_trait::async_trait;
use secrecy::SecretString;

pub mod rspotify_client;

#[cfg(test)]
pub mod mock;

/// The maximum `limit` Spotify accepts for search (as of Feb 2026). Callers must
/// clamp to this value.
pub const SEARCH_LIMIT_MAX: u32 = 10;

/// The bundled results of a single multi-type search query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResultset {
    pub tracks: Vec<TrackItem>,
    pub albums: Vec<AlbumItem>,
    pub artists: Vec<ArtistItem>,
    pub playlists: Vec<PlaylistItem>,
}

/// The Spotify Web API surface used by spot-defy.
///
/// Every method returns [`ApiError`] on failure. Implementations must tolerate
/// absent optional fields in Spotify responses (the API strips fields over
/// time) and must never call deprecated endpoints.
#[async_trait]
pub trait SpotifyApi: Send + Sync {
    /// Search tracks matching `query`. `limit` is clamped to [`SEARCH_LIMIT_MAX`].
    async fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<TrackItem>, ApiError>;

    /// Search albums matching `query`. `limit` is clamped to [`SEARCH_LIMIT_MAX`].
    async fn search_albums(&self, query: &str, limit: u32) -> Result<Vec<AlbumItem>, ApiError>;

    /// Search artists matching `query`. `limit` is clamped to [`SEARCH_LIMIT_MAX`].
    async fn search_artists(&self, query: &str, limit: u32) -> Result<Vec<ArtistItem>, ApiError>;

    /// Search playlists matching `query`. `limit` is clamped to [`SEARCH_LIMIT_MAX`].
    async fn search_playlists(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<PlaylistItem>, ApiError>;

    /// List the current user's own and followed playlists.
    async fn current_user_playlists(&self) -> Result<Vec<PlaylistItem>, ApiError>;

    /// List the tracks of a single playlist.
    async fn playlist_tracks(&self, id: &PlaylistId) -> Result<Vec<TrackItem>, ApiError>;

    /// The user's top tracks for a time window (`/me/top/tracks`).
    async fn top_tracks(&self, range: TimeRange) -> Result<Vec<TrackItem>, ApiError>;

    /// The user's top artists for a time window (`/me/top/artists`).
    async fn top_artists(&self, range: TimeRange) -> Result<Vec<ArtistItem>, ApiError>;

    /// The user's recently played tracks (capped at the endpoint's 50-item max).
    async fn recently_played(&self) -> Result<Vec<TrackItem>, ApiError>;

    /// The user's saved (liked) tracks.
    async fn saved_tracks(&self) -> Result<Vec<TrackItem>, ApiError>;

    /// The user's saved albums (`/me/albums`), paginated across all pages.
    async fn saved_albums(&self) -> Result<Vec<AlbumItem>, ApiError>;

    /// The tracks of a single album (drilled into from the Albums tab).
    async fn album_tracks(&self, id: &AlbumId) -> Result<Vec<TrackItem>, ApiError>;

    /// The current playback state, or `None` when nothing is active.
    async fn current_playback(&self) -> Result<Option<PlaybackSnapshot>, ApiError>;

    /// Replace the bearer access token used for subsequent requests.
    ///
    /// Called by the background refresh task when the access token nears expiry.
    /// Implementations that do not authenticate (mocks) treat this as a no-op.
    async fn set_access_token(&self, access_token: SecretString);
}
