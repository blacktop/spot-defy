//! Real [`SpotifyApi`] implementation over rspotify's `AuthCodePkceSpotify`.
//!
//! Reuses the librespot access token via `AuthCodePkceSpotify::from_token`.
//! The token lives behind a tokio `Mutex` inside rspotify; this module must
//! clone the token and drop the guard before any `.await` (clippy
//! `await_holding_lock` is denied) — never hold the lock across an HTTP call.
//! Model-mapping helpers tolerate absent optional fields, and only still-live
//! endpoints are called (no recommendations / audio-features / featured).

use crate::api::{SEARCH_LIMIT_MAX, SpotifyApi};
use crate::error::ApiError;
use crate::model::{
    AlbumArtImage, AlbumId, AlbumItem, ArtistId, ArtistItem, PlaybackSnapshot, PlaybackState,
    PlaylistId, PlaylistItem, TimeRange, TrackId, TrackItem,
};
use async_trait::async_trait;
use chrono::Duration as ChronoDuration;
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::http::HttpError;
use rspotify::model::{
    AdditionalType, AlbumId as RsAlbumId, CurrentPlaybackContext, Device, FullAlbum, FullArtist,
    FullTrack, PlayableItem, PlaylistId as RsPlaylistId, SearchResult, SearchType, SimplifiedAlbum,
    SimplifiedArtist, SimplifiedPlaylist, SimplifiedTrack, TimeRange as RsTimeRange,
};
use rspotify::prelude::Id as _;
use rspotify::{AuthCodePkceSpotify, ClientError, Token};
use secrecy::{ExposeSecret as _, SecretString};

/// Page size for non-search library/discovery queries.
///
/// A single page is fetched per call; the UI lists do not paginate. Spotify
/// caps most of these endpoints at 50 items per request.
const PAGE_LIMIT: u32 = 50;

/// Spotify's hard cap for the recently-played endpoint.
const RECENTLY_PLAYED_MAX: u32 = 50;

/// Cap on paginated library fetches (playlists, saved albums): pages × 50 items.
const MAX_LIBRARY_PAGES: u32 = 40;

/// Maximum automatic retries on HTTP 429 before surfacing the error.
const MAX_RATE_LIMIT_RETRIES: u32 = 2;

/// Backoff used when a 429 response carries no `Retry-After` header.
const DEFAULT_RETRY_SECS: u64 = 2;

/// Cap on how long a single `Retry-After` is honored, to bound UI latency.
const MAX_RETRY_SECS: u64 = 60;

/// rspotify-backed Web API client.
pub struct RspotifyApi {
    client: AuthCodePkceSpotify,
}

impl RspotifyApi {
    /// Build a client from an existing rspotify [`Token`].
    ///
    /// Uses `from_token` so the librespot access token is reused directly.
    ///
    /// rspotify's built-in auto token-refresh is disabled: `from_token` has no
    /// `client_id`, so rspotify's refresh POST would return HTTP 400 and surface
    /// as a spurious request failure on every call (the token carries no expiry,
    /// so rspotify treats it as already expired). spot-defy owns the token
    /// lifecycle instead — see [`crate::app`]'s refresh task and
    /// [`SpotifyApi::set_access_token`](crate::api::SpotifyApi::set_access_token).
    #[must_use]
    pub fn new(token: Token) -> Self {
        let mut client = AuthCodePkceSpotify::from_token(token);
        client.config.token_refreshing = false;
        Self { client }
    }

    /// Borrow the underlying rspotify client (used by the Web API call sites).
    #[must_use]
    pub fn client(&self) -> &AuthCodePkceSpotify {
        &self.client
    }

    /// List the available Spotify Connect devices for this account.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if the request fails or the response is malformed.
    pub async fn devices(&self) -> Result<Vec<Device>, ApiError> {
        self.client.device().await.map_err(map_client_error)
    }

    /// Transfer playback to `device_id`, optionally forcing play.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if the transfer request fails.
    pub async fn transfer_playback(
        &self,
        device_id: &str,
        play: Option<bool>,
    ) -> Result<(), ApiError> {
        self.client
            .transfer_playback(device_id, play)
            .await
            .map_err(map_client_error)
    }

    /// Resume playback on the active (or given) device.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if the request fails (e.g. no active device, 404).
    pub async fn resume(&self, device_id: Option<&str>) -> Result<(), ApiError> {
        self.client
            .resume_playback(device_id, None)
            .await
            .map_err(map_client_error)
    }

    /// Pause playback on the active (or given) device.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if the request fails.
    pub async fn pause(&self, device_id: Option<&str>) -> Result<(), ApiError> {
        self.client
            .pause_playback(device_id)
            .await
            .map_err(map_client_error)
    }

    /// Skip to the next track on the active (or given) device.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if the request fails.
    pub async fn next(&self, device_id: Option<&str>) -> Result<(), ApiError> {
        self.client
            .next_track(device_id)
            .await
            .map_err(map_client_error)
    }

    /// Skip to the previous track on the active (or given) device.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if the request fails.
    pub async fn previous(&self, device_id: Option<&str>) -> Result<(), ApiError> {
        self.client
            .previous_track(device_id)
            .await
            .map_err(map_client_error)
    }

    /// Seek the active track to `position_ms`.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if the request fails or `position_ms` overflows the
    /// signed-millisecond range used by the underlying API.
    pub async fn seek(&self, position_ms: u32, device_id: Option<&str>) -> Result<(), ApiError> {
        let position =
            ChronoDuration::try_milliseconds(i64::from(position_ms)).ok_or_else(|| {
                ApiError::Mapping(format!("seek position out of range: {position_ms}"))
            })?;
        self.client
            .seek_track(position, device_id)
            .await
            .map_err(map_client_error)
    }

    /// Set the output volume (0..=100) on the active (or given) device.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError`] if `volume` exceeds 100 or the request fails.
    pub async fn set_volume(&self, volume: u16, device_id: Option<&str>) -> Result<(), ApiError> {
        let percent = u8::try_from(volume)
            .ok()
            .filter(|v| *v <= 100)
            .ok_or_else(|| ApiError::Mapping(format!("volume must be 0..=100, got {volume}")))?;
        self.client
            .volume(percent, device_id)
            .await
            .map_err(map_client_error)
    }
}

#[async_trait]
impl SpotifyApi for RspotifyApi {
    async fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<TrackItem>, ApiError> {
        let result = self.search(query, SearchType::Track, limit).await?;
        match result {
            SearchResult::Tracks(page) => Ok(page.items.iter().map(map_full_track).collect()),
            other => Err(unexpected_search_result("tracks", &other)),
        }
    }

    async fn search_albums(&self, query: &str, limit: u32) -> Result<Vec<AlbumItem>, ApiError> {
        let result = self.search(query, SearchType::Album, limit).await?;
        match result {
            SearchResult::Albums(page) => {
                Ok(page.items.iter().filter_map(map_simplified_album).collect())
            }
            other => Err(unexpected_search_result("albums", &other)),
        }
    }

    async fn search_artists(&self, query: &str, limit: u32) -> Result<Vec<ArtistItem>, ApiError> {
        let result = self.search(query, SearchType::Artist, limit).await?;
        match result {
            SearchResult::Artists(page) => Ok(page.items.iter().map(map_full_artist).collect()),
            other => Err(unexpected_search_result("artists", &other)),
        }
    }

    async fn search_playlists(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<PlaylistItem>, ApiError> {
        let result = self.search(query, SearchType::Playlist, limit).await?;
        match result {
            SearchResult::Playlists(page) => {
                Ok(page.items.iter().map(map_simplified_playlist).collect())
            }
            other => Err(unexpected_search_result("playlists", &other)),
        }
    }

    async fn current_user_playlists(&self) -> Result<Vec<PlaylistItem>, ApiError> {
        let mut playlists = Vec::new();
        // Paginate by explicit offset: Spotify's `next` link currently points at
        // a removed endpoint (403), so it must never be followed.
        for page_index in 0..MAX_LIBRARY_PAGES {
            let offset = page_index * PAGE_LIMIT;
            let page = self
                .retrying(|| {
                    self.client
                        .current_user_playlists_manual(Some(PAGE_LIMIT), Some(offset))
                })
                .await?;
            let count = page.items.len();
            playlists.extend(page.items.iter().map(map_simplified_playlist));
            if count < PAGE_LIMIT as usize {
                break;
            }
        }
        Ok(playlists)
    }

    async fn playlist_tracks(&self, id: &PlaylistId) -> Result<Vec<TrackItem>, ApiError> {
        let playlist_id = RsPlaylistId::from_id(id.0.as_str())
            .map_err(|e| ApiError::Mapping(format!("invalid playlist id {}: {e}", id.0)))?;
        let page = self
            .retrying(|| {
                self.client.playlist_items_manual(
                    playlist_id.clone(),
                    None,
                    None,
                    Some(PAGE_LIMIT),
                    Some(0),
                )
            })
            .await?;
        Ok(page.items.iter().filter_map(map_playlist_item).collect())
    }

    async fn top_tracks(&self, range: TimeRange) -> Result<Vec<TrackItem>, ApiError> {
        let page = self
            .retrying(|| {
                self.client.current_user_top_tracks_manual(
                    Some(to_rs_time_range(range)),
                    Some(PAGE_LIMIT),
                    Some(0),
                )
            })
            .await?;
        Ok(page.items.iter().map(map_full_track).collect())
    }

    async fn top_artists(&self, range: TimeRange) -> Result<Vec<ArtistItem>, ApiError> {
        let page = self
            .retrying(|| {
                self.client.current_user_top_artists_manual(
                    Some(to_rs_time_range(range)),
                    Some(PAGE_LIMIT),
                    Some(0),
                )
            })
            .await?;
        Ok(page.items.iter().map(map_full_artist).collect())
    }

    async fn recently_played(&self, limit: u32) -> Result<Vec<TrackItem>, ApiError> {
        let limit = limit.min(RECENTLY_PLAYED_MAX);
        let page = self
            .retrying(|| self.client.current_user_recently_played(Some(limit), None))
            .await?;
        Ok(page
            .items
            .iter()
            .map(|h| map_full_track(&h.track))
            .collect())
    }

    async fn saved_tracks(&self) -> Result<Vec<TrackItem>, ApiError> {
        let page = self
            .retrying(|| {
                self.client
                    .current_user_saved_tracks_manual(None, Some(PAGE_LIMIT), Some(0))
            })
            .await?;
        Ok(page
            .items
            .iter()
            .map(|s| map_full_track(&s.track))
            .collect())
    }

    async fn saved_albums(&self) -> Result<Vec<AlbumItem>, ApiError> {
        let mut albums = Vec::new();
        for page_index in 0..MAX_LIBRARY_PAGES {
            let offset = page_index * PAGE_LIMIT;
            let page = self
                .retrying(|| {
                    self.client.current_user_saved_albums_manual(
                        None,
                        Some(PAGE_LIMIT),
                        Some(offset),
                    )
                })
                .await?;
            let count = page.items.len();
            albums.extend(page.items.iter().map(|saved| map_full_album(&saved.album)));
            if count < PAGE_LIMIT as usize {
                break;
            }
        }
        Ok(albums)
    }

    async fn album_tracks(&self, id: &AlbumId) -> Result<Vec<TrackItem>, ApiError> {
        let album_id = RsAlbumId::from_id(id.0.as_str())
            .map_err(|e| ApiError::Mapping(format!("invalid album id {}: {e}", id.0)))?;
        let album = self
            .retrying(|| self.client.album(album_id.clone(), None))
            .await?;
        let art = album_art_images(&album.images);
        Ok(album
            .tracks
            .items
            .iter()
            .map(|track| map_album_track(track, &album.name, &art))
            .collect())
    }

    async fn current_playback(&self) -> Result<Option<PlaybackSnapshot>, ApiError> {
        let additional = [AdditionalType::Track];
        let context = self
            .retrying(|| self.client.current_playback(None, Some(&additional)))
            .await?;
        Ok(context.as_ref().map(map_playback_context))
    }

    async fn set_access_token(&self, access_token: SecretString) {
        let token_lock = self.client.get_token();
        let Ok(mut guard) = token_lock.lock().await else {
            tracing::warn!("could not lock token to apply the refreshed access token");
            return;
        };
        if let Some(token) = guard.as_mut() {
            access_token
                .expose_secret()
                .clone_into(&mut token.access_token);
        }
    }
}

impl RspotifyApi {
    /// Run a single-type search clamped to [`SEARCH_LIMIT_MAX`].
    async fn search(
        &self,
        query: &str,
        kind: SearchType,
        limit: u32,
    ) -> Result<SearchResult, ApiError> {
        let limit = limit.clamp(1, SEARCH_LIMIT_MAX);
        self.retrying(|| {
            self.client
                .search(query, kind, None, None, Some(limit), Some(0))
        })
        .await
    }

    /// Run an rspotify call, backing off and retrying on HTTP 429.
    ///
    /// Honors the server's `Retry-After` (capped at [`MAX_RETRY_SECS`]) for up
    /// to [`MAX_RATE_LIMIT_RETRIES`] attempts; any non-429 error is returned
    /// immediately. The call runs in a background task, so the backoff sleep
    /// never blocks the UI thread.
    async fn retrying<T, F, Fut>(&self, mut call: F) -> Result<T, ApiError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, ClientError>>,
    {
        let mut attempt: u32 = 0;
        loop {
            let error = match Box::pin(call()).await {
                Ok(value) => return Ok(value),
                Err(error) => map_client_error(error),
            };
            let ApiError::RateLimited { retry_after_secs } = error else {
                return Err(error);
            };
            if attempt >= MAX_RATE_LIMIT_RETRIES {
                return Err(ApiError::RateLimited { retry_after_secs });
            }
            attempt += 1;
            let secs = retry_after_secs
                .unwrap_or(DEFAULT_RETRY_SECS)
                .min(MAX_RETRY_SECS);
            tracing::warn!(secs, attempt, "rate limited by spotify; backing off");
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    }
}

/// Map our [`TimeRange`] to rspotify's enum.
fn to_rs_time_range(range: TimeRange) -> RsTimeRange {
    match range {
        TimeRange::ShortTerm => RsTimeRange::ShortTerm,
        TimeRange::MediumTerm => RsTimeRange::MediumTerm,
        TimeRange::LongTerm => RsTimeRange::LongTerm,
    }
}

/// Join the names of `artists` into a display string (`", "`-separated).
fn join_artists(artists: &[SimplifiedArtist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Map a [`FullTrack`] into a [`TrackItem`].
///
/// Local tracks lack an id; they are surfaced with an empty [`TrackId`] so the
/// row still renders rather than being silently dropped. Duration is taken from
/// the chrono `Duration` and clamped into `u32` milliseconds.
fn map_full_track(track: &FullTrack) -> TrackItem {
    let id = track
        .id
        .as_ref()
        .map(|i| i.id().to_owned())
        .unwrap_or_default();
    let duration_ms = u32::try_from(track.duration.num_milliseconds().max(0)).unwrap_or(u32::MAX);
    TrackItem {
        id: TrackId(id),
        title: track.name.clone(),
        artist: join_artists(&track.artists),
        album: track.album.name.clone(),
        duration_ms,
        album_art_images: album_art_images(&track.album.images),
    }
}

/// Preserve Spotify's album-cover candidates so the TUI can choose the smallest
/// image that satisfies the current terminal art pane.
fn album_art_images(images: &[rspotify::model::Image]) -> Vec<AlbumArtImage> {
    images
        .iter()
        .map(|image| AlbumArtImage {
            url: image.url.clone(),
            width: image.width,
            height: image.height,
        })
        .collect()
}

/// Map a [`FullAlbum`] into an [`AlbumItem`].
fn map_full_album(album: &FullAlbum) -> AlbumItem {
    AlbumItem {
        id: AlbumId(album.id.id().to_owned()),
        name: album.name.clone(),
        artist: join_artists(&album.artists),
    }
}

/// Map an album's [`SimplifiedTrack`] into a [`TrackItem`], carrying the album
/// name and cover candidates (a simplified track lacks its own album metadata).
fn map_album_track(track: &SimplifiedTrack, album_name: &str, art: &[AlbumArtImage]) -> TrackItem {
    let id = track
        .id
        .as_ref()
        .map(|i| i.id().to_owned())
        .unwrap_or_default();
    let duration_ms = u32::try_from(track.duration.num_milliseconds().max(0)).unwrap_or(u32::MAX);
    TrackItem {
        id: TrackId(id),
        title: track.name.clone(),
        artist: join_artists(&track.artists),
        album: album_name.to_owned(),
        duration_ms,
        album_art_images: art.to_vec(),
    }
}

/// Map a [`SimplifiedAlbum`] into an [`AlbumItem`].
///
/// Returns `None` when the album has no id (Spotify occasionally returns
/// id-less placeholders); such rows cannot be acted on and are dropped.
fn map_simplified_album(album: &SimplifiedAlbum) -> Option<AlbumItem> {
    let id = album.id.as_ref()?.id().to_owned();
    Some(AlbumItem {
        id: AlbumId(id),
        name: album.name.clone(),
        artist: join_artists(&album.artists),
    })
}

/// Map a [`FullArtist`] into an [`ArtistItem`].
fn map_full_artist(artist: &FullArtist) -> ArtistItem {
    ArtistItem {
        id: ArtistId(artist.id.id().to_owned()),
        name: artist.name.clone(),
    }
}

/// Map a [`SimplifiedPlaylist`] into a [`PlaylistItem`].
///
/// Uses the migration-aware `items` count and falls back to the owner's
/// display name (then the owner id) for the owner column.
fn map_simplified_playlist(playlist: &SimplifiedPlaylist) -> PlaylistItem {
    let owner = playlist
        .owner
        .display_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| playlist.owner.id.id().to_owned());
    PlaylistItem {
        id: PlaylistId(playlist.id.id().to_owned()),
        name: playlist.name.clone(),
        owner,
        track_count: playlist.items.total,
    }
}

/// Map a playlist row into a [`TrackItem`], dropping non-track and id-less rows.
fn map_playlist_item(item: &rspotify::model::PlaylistItem) -> Option<TrackItem> {
    match item.item.as_ref()? {
        PlayableItem::Track(track) => Some(map_full_track(track)),
        PlayableItem::Episode(_) | PlayableItem::Unknown(_) => None,
    }
}

/// Map the current playback context into a [`PlaybackSnapshot`].
fn map_playback_context(context: &CurrentPlaybackContext) -> PlaybackSnapshot {
    let track = match context.item.as_ref() {
        Some(PlayableItem::Track(t)) => Some(t),
        Some(PlayableItem::Episode(_) | PlayableItem::Unknown(_)) | None => None,
    };
    let duration_ms = track.map_or(0, |t| {
        u32::try_from(t.duration.num_milliseconds().max(0)).unwrap_or(u32::MAX)
    });
    let position_ms = context.progress.map_or(0, |p| {
        u32::try_from(p.num_milliseconds().max(0)).unwrap_or(u32::MAX)
    });
    let state = if context.is_playing {
        PlaybackState::Playing
    } else if track.is_some() {
        PlaybackState::Paused
    } else {
        PlaybackState::Stopped
    };
    let volume = u16::try_from(context.device.volume_percent.unwrap_or(0).min(100)).unwrap_or(100);
    PlaybackSnapshot {
        track: track.map(|t| t.name.clone()),
        artist: track.map(|t| join_artists(&t.artists)),
        state,
        position_ms,
        duration_ms,
        volume,
    }
}

/// Build a [`ApiError`] for a search response whose payload variant did not
/// match the requested type (defensive — Spotify echoes the requested `type`).
fn unexpected_search_result(expected: &str, got: &SearchResult) -> ApiError {
    ApiError::Mapping(format!(
        "search returned unexpected payload (expected {expected}, got {got:?})"
    ))
}

/// Map an rspotify [`ClientError`] into our [`ApiError`], distinguishing
/// transport failures from HTTP error statuses and adding context for the
/// expired-token (401) and rate-limit (429) cases the caller must handle.
fn map_client_error(error: ClientError) -> ApiError {
    match error {
        ClientError::InvalidToken => {
            ApiError::Response("access token expired or invalid; refresh required".to_owned())
        }
        ClientError::Http(http) => map_http_error(&http),
        ClientError::ParseJson(e) => ApiError::Mapping(e.to_string()),
        ClientError::Model(e) => ApiError::Mapping(e.to_string()),
        ClientError::Io(e) => ApiError::Request(e.to_string()),
        ClientError::ParseUrl(e) => ApiError::Request(e.to_string()),
        ClientError::CacheFile(msg) | ClientError::AuthCodeListenerParse(msg) => {
            ApiError::Request(msg)
        }
        ClientError::TokenCallbackFn(e) => ApiError::Response(e.to_string()),
        ClientError::AuthCodeListenerBind { addr, e } => {
            ApiError::Request(format!("oauth listener bind failed on {addr}: {e}"))
        }
        ClientError::AuthCodeListenerTerminated
        | ClientError::AuthCodeListenerRead
        | ClientError::AuthCodeListenerWrite => {
            ApiError::Request("oauth loopback listener failed".to_owned())
        }
    }
}

/// Map an rspotify [`HttpError`] into our [`ApiError`] using the real status
/// code, attaching actionable context for the expired-token (401) and
/// rate-limit (429) cases. Transport failures (no status) become
/// [`ApiError::Request`].
fn map_http_error(http: &HttpError) -> ApiError {
    match http {
        HttpError::StatusCode(response) => {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            classify_status(response.status().as_u16(), retry_after)
        }
        HttpError::Client(e) => ApiError::Request(e.to_string()),
    }
}

/// Turn an HTTP status code into an [`ApiError`] with caller-facing context.
///
/// `retry_after_secs` is the parsed `Retry-After` header, forwarded on 429 so
/// the retry layer can honor the server's backoff hint.
fn classify_status(status: u16, retry_after_secs: Option<u64>) -> ApiError {
    match status {
        401 => ApiError::Response(
            "unauthorized (401); access token expired, refresh required".to_owned(),
        ),
        429 => ApiError::RateLimited { retry_after_secs },
        other => ApiError::Response(format!("spotify returned status code {other}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::rspotify_client::{
        album_art_images, classify_status, join_artists, map_full_artist, map_full_track,
        map_playback_context, map_playlist_item, map_simplified_album, map_simplified_playlist,
        to_rs_time_range,
    };
    use crate::error::ApiError;
    use crate::model::{PlaybackState, TimeRange};
    use rspotify::model::{
        CurrentPlaybackContext, FullArtist, FullTrack, PlaylistItem as RsPlaylistItem,
        SimplifiedAlbum, SimplifiedArtist, SimplifiedPlaylist, TimeRange as RsTimeRange,
    };

    fn full_track_json() -> serde_json::Value {
        serde_json::json!({
            "album": {
                "album_type": "album",
                "artists": [{"external_urls": {}, "href": null,
                    "id": "0TnOYISbd1XYRBk9myaseg", "name": "Pitbull", "type": "artist"}],
                "external_urls": {}, "href": null,
                "id": "5xYZXIgVAd2u4Qm5pmnYmw",
                "images": [
                    {"height": 640, "url": "https://i.scdn.co/image/large", "width": 640},
                    {"height": 300, "url": "https://i.scdn.co/image/medium", "width": 300},
                    {"height": 64, "url": "https://i.scdn.co/image/small", "width": 64}
                ],
                "name": "Global Warming",
                "release_date": "2012-11-16", "release_date_precision": "day", "type": "album"
            },
            "artists": [
                {"external_urls": {}, "href": null, "id": "0TnOYISbd1XYRBk9myaseg",
                    "name": "Pitbull", "type": "artist"},
                {"external_urls": {}, "href": null, "id": "1l7ZsJRRS8wlW3WfJfPfNS",
                    "name": "Christina Aguilera", "type": "artist"}
            ],
            "disc_number": 1, "duration_ms": 229_400, "explicit": false,
            "external_ids": {}, "external_urls": {}, "href": null,
            "id": "06AKEBrKUckW0KREUWRnvT", "is_local": false,
            "name": "Feel This Moment", "preview_url": null,
            "track_number": 4, "type": "track"
        })
    }

    fn parse<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
        serde_json::from_value(v).expect("sample json must deserialize")
    }

    #[test]
    fn maps_full_track_fields() {
        let track: FullTrack = parse(full_track_json());
        let item = map_full_track(&track);
        assert_eq!(item.id.0, "06AKEBrKUckW0KREUWRnvT");
        assert_eq!(item.title, "Feel This Moment");
        assert_eq!(item.artist, "Pitbull, Christina Aguilera");
        assert_eq!(item.album, "Global Warming");
        assert_eq!(item.duration_ms, 229_400);
        assert_eq!(item.album_art_images.len(), 3);
        assert_eq!(item.album_art_images[0].width, Some(640));
        assert_eq!(
            item.album_art_images[1].url,
            "https://i.scdn.co/image/medium"
        );
    }

    #[test]
    fn maps_local_track_without_id_to_empty_id() {
        let mut json = full_track_json();
        json["id"] = serde_json::Value::Null;
        json["is_local"] = serde_json::Value::Bool(true);
        let track: FullTrack = parse(json);
        let item = map_full_track(&track);
        assert_eq!(item.id.0, "");
        assert_eq!(item.title, "Feel This Moment");
    }

    #[test]
    fn join_artists_uses_comma_separator() {
        let artists: Vec<SimplifiedArtist> = vec![
            parse(serde_json::json!({"external_urls": {}, "href": null,
                "id": "0TnOYISbd1XYRBk9myaseg", "name": "A"})),
            parse(serde_json::json!({"external_urls": {}, "href": null,
                "id": "1l7ZsJRRS8wlW3WfJfPfNS", "name": "B"})),
        ];
        assert_eq!(join_artists(&artists), "A, B");
    }

    #[test]
    fn album_art_images_preserves_spotify_dimensions() {
        let images = vec![
            rspotify::model::Image {
                height: Some(640),
                url: "large".to_owned(),
                width: Some(640),
            },
            rspotify::model::Image {
                height: None,
                url: "unknown".to_owned(),
                width: None,
            },
        ];

        let mapped = album_art_images(&images);

        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].url, "large");
        assert_eq!(mapped[0].height, Some(640));
        assert_eq!(mapped[1].width, None);
    }

    #[test]
    fn maps_simplified_album_and_drops_idless() {
        let album: SimplifiedAlbum = parse(serde_json::json!({
            "album_type": "album",
            "artists": [{"external_urls": {}, "href": null,
                "id": "0TnOYISbd1XYRBk9myaseg", "name": "Daft Punk", "type": "artist"}],
            "external_urls": {}, "href": null, "id": "4m2880jivSbbyEGAKfITt1",
            "images": [], "name": "Random Access Memories"
        }));
        let mapped = map_simplified_album(&album).expect("album with id maps");
        assert_eq!(mapped.id.0, "4m2880jivSbbyEGAKfITt1");
        assert_eq!(mapped.name, "Random Access Memories");
        assert_eq!(mapped.artist, "Daft Punk");

        let idless: SimplifiedAlbum = parse(serde_json::json!({
            "album_type": "album", "artists": [], "external_urls": {},
            "href": null, "id": null, "images": [], "name": "Untitled"
        }));
        assert!(map_simplified_album(&idless).is_none());
    }

    #[test]
    fn maps_full_artist() {
        let artist: FullArtist = parse(serde_json::json!({
            "external_urls": {}, "href": "https://api.spotify.com/v1/artists/x",
            "id": "0OdUWJ0sBjDrqHygGUXeCF", "images": [], "name": "Band of Horses"
        }));
        let mapped = map_full_artist(&artist);
        assert_eq!(mapped.id.0, "0OdUWJ0sBjDrqHygGUXeCF");
        assert_eq!(mapped.name, "Band of Horses");
    }

    #[test]
    fn maps_simplified_playlist_with_owner_and_count() {
        let playlist: SimplifiedPlaylist = parse(serde_json::json!({
            "collaborative": false, "external_urls": {},
            "href": "https://api.spotify.com/v1/playlists/p", "id": "37i9dQZF1DXcBWIGoYBM5M",
            "images": [], "name": "Today's Top Hits",
            "owner": {"display_name": "Spotify", "external_urls": {},
                "href": "https://api.spotify.com/v1/users/spotify", "id": "spotify"},
            "public": true, "snapshot_id": "abc",
            "tracks": {"href": "h", "total": 50}, "items": {"href": "h", "total": 50}
        }));
        let mapped = map_simplified_playlist(&playlist);
        assert_eq!(mapped.id.0, "37i9dQZF1DXcBWIGoYBM5M");
        assert_eq!(mapped.name, "Today's Top Hits");
        assert_eq!(mapped.owner, "Spotify");
        assert_eq!(mapped.track_count, 50);
    }

    #[test]
    fn playlist_owner_falls_back_to_id_when_no_display_name() {
        let playlist: SimplifiedPlaylist = parse(serde_json::json!({
            "collaborative": false, "external_urls": {}, "href": "h",
            "id": "37i9dQZF1DXcBWIGoYBM5M", "images": [], "name": "Mix",
            "owner": {"display_name": null, "external_urls": {}, "href": "h", "id": "user-123"},
            "public": false, "snapshot_id": "abc",
            "tracks": {"href": "h", "total": 3}, "items": {"href": "h", "total": 3}
        }));
        assert_eq!(map_simplified_playlist(&playlist).owner, "user-123");
    }

    #[test]
    fn maps_playlist_track_item_and_drops_episode() {
        let track_item: RsPlaylistItem = parse(serde_json::json!({
            "added_at": "2020-01-01T00:00:00Z", "added_by": null, "is_local": false,
            "track": full_track_json(), "item": full_track_json()
        }));
        let mapped = map_playlist_item(&track_item).expect("track item maps");
        assert_eq!(mapped.title, "Feel This Moment");

        let empty: RsPlaylistItem = parse(serde_json::json!({
            "added_at": null, "added_by": null, "is_local": false,
            "track": null, "item": null
        }));
        assert!(map_playlist_item(&empty).is_none());
    }

    #[test]
    fn maps_playback_context_playing() {
        let context: CurrentPlaybackContext = parse(serde_json::json!({
            "device": {"id": "d1", "is_active": true, "is_private_session": false,
                "is_restricted": false, "name": "Mac", "type": "Computer",
                "volume_percent": 73},
            "repeat_state": "off", "shuffle_state": false, "context": null,
            "timestamp": 1_700_000_000_000_i64, "progress_ms": 42_000,
            "is_playing": true, "item": full_track_json(),
            "currently_playing_type": "track",
            "actions": {"disallows": {}}
        }));
        let snap = map_playback_context(&context);
        assert_eq!(snap.state, PlaybackState::Playing);
        assert_eq!(snap.track.as_deref(), Some("Feel This Moment"));
        assert_eq!(snap.artist.as_deref(), Some("Pitbull, Christina Aguilera"));
        assert_eq!(snap.position_ms, 42_000);
        assert_eq!(snap.duration_ms, 229_400);
        assert_eq!(snap.volume, 73);
    }

    #[test]
    fn maps_playback_context_paused_when_not_playing() {
        let context: CurrentPlaybackContext = parse(serde_json::json!({
            "device": {"id": "d1", "is_active": true, "is_private_session": false,
                "is_restricted": false, "name": "Mac", "type": "Computer",
                "volume_percent": 10},
            "repeat_state": "off", "shuffle_state": false, "context": null,
            "timestamp": 1_700_000_000_000_i64, "progress_ms": 1000,
            "is_playing": false, "item": full_track_json(),
            "currently_playing_type": "track",
            "actions": {"disallows": {}}
        }));
        assert_eq!(map_playback_context(&context).state, PlaybackState::Paused);
    }

    #[test]
    fn time_range_maps_to_rspotify() {
        assert!(matches!(
            to_rs_time_range(TimeRange::ShortTerm),
            RsTimeRange::ShortTerm
        ));
        assert!(matches!(
            to_rs_time_range(TimeRange::MediumTerm),
            RsTimeRange::MediumTerm
        ));
        assert!(matches!(
            to_rs_time_range(TimeRange::LongTerm),
            RsTimeRange::LongTerm
        ));
    }

    #[test]
    fn classify_status_detects_known_codes() {
        match classify_status(401, None) {
            ApiError::Response(msg) => assert!(msg.contains("401")),
            other => panic!("expected 401 response error, got {other:?}"),
        }
        match classify_status(429, Some(5)) {
            ApiError::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, Some(5)),
            other => panic!("expected rate-limited error, got {other:?}"),
        }
        match classify_status(503, None) {
            ApiError::Response(msg) => assert!(msg.contains("503")),
            other => panic!("expected generic response error, got {other:?}"),
        }
    }

    #[test]
    fn new_disables_rspotify_internal_token_refresh() {
        // We own the token lifecycle; rspotify's auto-refresh (which would 400
        // because `from_token` has no client_id) must stay off.
        let api = crate::api::rspotify_client::RspotifyApi::new(rspotify::Token::default());
        assert!(!api.client().config.token_refreshing);
    }

    #[tokio::test]
    async fn set_access_token_replaces_the_stored_bearer() {
        use crate::api::SpotifyApi as _;
        use rspotify::clients::BaseClient as _;
        use secrecy::SecretString;

        let api = crate::api::rspotify_client::RspotifyApi::new(rspotify::Token {
            access_token: "old".to_owned(),
            ..rspotify::Token::default()
        });
        api.set_access_token(SecretString::from("new")).await;

        let stored = api
            .client()
            .get_token()
            .lock()
            .await
            .expect("token lock")
            .clone()
            .expect("token present");
        assert_eq!(stored.access_token, "new");
    }
}
