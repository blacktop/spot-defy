//! In-memory mock [`SpotifyApi`] for tests.
//!
//! Returns canned data so the pure TEA `update`/`view` layers can be exercised
//! without network access. Network is the only boundary mocked here.

use crate::api::SpotifyApi;
use crate::error::ApiError;
use crate::model::{
    AlbumId, AlbumItem, ArtistItem, PlaybackSnapshot, PlaylistId, PlaylistItem, TimeRange,
    TrackItem,
};
use async_trait::async_trait;
use secrecy::SecretString;

/// A mock that returns preset results for every call.
#[derive(Debug, Clone, Default)]
pub struct MockApi {
    pub tracks: Vec<TrackItem>,
    pub albums: Vec<AlbumItem>,
    pub artists: Vec<ArtistItem>,
    pub playlists: Vec<PlaylistItem>,
    pub playback: Option<PlaybackSnapshot>,
}

#[async_trait]
impl SpotifyApi for MockApi {
    async fn search_tracks(&self, _query: &str, _limit: u32) -> Result<Vec<TrackItem>, ApiError> {
        Ok(self.tracks.clone())
    }

    async fn search_albums(&self, _query: &str, _limit: u32) -> Result<Vec<AlbumItem>, ApiError> {
        Ok(self.albums.clone())
    }

    async fn search_artists(&self, _query: &str, _limit: u32) -> Result<Vec<ArtistItem>, ApiError> {
        Ok(self.artists.clone())
    }

    async fn search_playlists(
        &self,
        _query: &str,
        _limit: u32,
    ) -> Result<Vec<PlaylistItem>, ApiError> {
        Ok(self.playlists.clone())
    }

    async fn current_user_playlists(&self) -> Result<Vec<PlaylistItem>, ApiError> {
        Ok(self.playlists.clone())
    }

    async fn playlist_tracks(&self, _id: &PlaylistId) -> Result<Vec<TrackItem>, ApiError> {
        Ok(self.tracks.clone())
    }

    async fn top_tracks(&self, _range: TimeRange) -> Result<Vec<TrackItem>, ApiError> {
        Ok(self.tracks.clone())
    }

    async fn top_artists(&self, _range: TimeRange) -> Result<Vec<ArtistItem>, ApiError> {
        Ok(self.artists.clone())
    }

    async fn recently_played(&self, _limit: u32) -> Result<Vec<TrackItem>, ApiError> {
        Ok(self.tracks.clone())
    }

    async fn saved_tracks(&self) -> Result<Vec<TrackItem>, ApiError> {
        Ok(self.tracks.clone())
    }

    async fn saved_albums(&self) -> Result<Vec<AlbumItem>, ApiError> {
        Ok(self.albums.clone())
    }

    async fn album_tracks(&self, _id: &AlbumId) -> Result<Vec<TrackItem>, ApiError> {
        Ok(self.tracks.clone())
    }

    async fn current_playback(&self) -> Result<Option<PlaybackSnapshot>, ApiError> {
        Ok(self.playback.clone())
    }

    async fn set_access_token(&self, _access_token: SecretString) {}
}
