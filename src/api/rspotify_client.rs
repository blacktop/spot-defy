//! Real [`SpotifyApi`] implementation over rspotify's `AuthCodePkceSpotify`.
//!
//! Reuses the librespot access token via `AuthCodePkceSpotify::from_token`.
//! The token lives behind a tokio `Mutex` inside rspotify; this module must
//! clone the token and drop the guard before any `.await` (clippy
//! `await_holding_lock` is denied) — never hold the lock across an HTTP call.
//!
//! Track lists are fetched as raw JSON (through rspotify's authenticated HTTP
//! layer) and parsed with the lenient `Raw*` models below: rspotify's typed
//! `FullTrack` demands fields Spotify legitimately omits (a local playlist
//! file has no `external_ids`), and one such row used to fail an entire page.
//! Only still-live endpoints are called (no recommendations / audio-features /
//! featured).

use crate::api::{SEARCH_LIMIT_MAX, SpotifyApi};
use crate::error::ApiError;
use crate::model::{
    AlbumArtImage, AlbumId, AlbumItem, ArtistId, ArtistItem, PlaylistId, PlaylistItem, TimeRange,
    TrackId, TrackItem,
};
use async_trait::async_trait;
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::http::{HttpError, Query};
use rspotify::model::{
    AlbumId as RsAlbumId, FullAlbum, FullArtist, LibraryId, PlaylistId as RsPlaylistId,
    SearchResult, SearchType, SimplifiedAlbum, SimplifiedArtist, SimplifiedPlaylist,
    SimplifiedTrack, TimeRange as RsTimeRange, TrackId as RsTrackId,
};
use rspotify::prelude::Id as _;
use rspotify::{AuthCodePkceSpotify, ClientError, Token};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

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

    /// GET `endpoint` through rspotify's authenticated HTTP layer (with the
    /// usual 429 retry) and deserialize with one of the lenient `Raw*` models.
    ///
    /// `api_get` is a stable-but-doc-hidden `BaseClient` method; rspotify is
    /// exact-pinned, so relying on it is safe until the next deliberate bump.
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        let query: Query<'_> = params.iter().copied().collect();
        let body = self
            .retrying(|| self.client.api_get(endpoint, &query))
            .await?;
        serde_json::from_str(&body)
            .map_err(|e| ApiError::Mapping(format!("unexpected {endpoint} payload: {e}")))
    }
}

/// Lenient track object: only the fields the UI renders, everything defaulted,
/// so a stripped-down row (local file, relinked ghost) never fails a page.
#[derive(Debug, Default, Deserialize)]
struct RawTrack {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<RawArtist>,
    #[serde(default)]
    album: RawAlbumRef,
    #[serde(default)]
    duration_ms: u64,
    /// `"track"` or `"episode"`; playlists can contain both.
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawArtist {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawAlbumRef {
    #[serde(default)]
    name: String,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(Debug, Default, Deserialize)]
struct RawImage {
    #[serde(default)]
    url: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

/// A page of bare track objects (`/me/top/tracks`, search's `tracks.items`).
#[derive(Debug, Default, Deserialize)]
struct RawTrackPage {
    #[serde(default)]
    items: Vec<RawTrack>,
}

/// A page of wrapped track rows (`{"items": [{"track": {…}}]}`): playlist
/// items, saved tracks, and recently played all share this shape.
#[derive(Debug, Default, Deserialize)]
struct RawTrackEntries {
    #[serde(default)]
    items: Vec<RawTrackEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct RawTrackEntry {
    #[serde(default)]
    track: Option<RawTrack>,
}

/// The `tracks` lane of a search response.
#[derive(Debug, Default, Deserialize)]
struct RawSearchTracks {
    #[serde(default)]
    tracks: RawTrackPage,
}

/// Map a lenient raw track into the UI's [`TrackItem`]. Local tracks keep an
/// empty id (still rendered, not playable) — same policy as before.
fn lenient_track_item(track: RawTrack) -> TrackItem {
    TrackItem {
        id: TrackId(track.id.unwrap_or_default()),
        title: track.name,
        artist: track
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        album: track.album.name,
        duration_ms: u32::try_from(track.duration_ms).unwrap_or(u32::MAX),
        album_art_images: track
            .album
            .images
            .into_iter()
            .map(|image| AlbumArtImage {
                url: image.url,
                width: image.width,
                height: image.height,
            })
            .collect(),
    }
}

/// Flatten wrapped track rows, dropping empty slots and non-track rows
/// (episodes) the way the typed mapping used to.
fn lenient_track_items(entries: RawTrackEntries) -> Vec<TrackItem> {
    entries
        .items
        .into_iter()
        .filter_map(|entry| entry.track)
        .filter(|track| track.kind.as_deref() == Some("track"))
        .map(lenient_track_item)
        .collect()
}

#[async_trait]
impl SpotifyApi for RspotifyApi {
    async fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<TrackItem>, ApiError> {
        let limit = limit.clamp(1, SEARCH_LIMIT_MAX).to_string();
        let page: RawSearchTracks = self
            .get_json(
                "search",
                &[
                    ("q", query),
                    ("type", "track"),
                    ("limit", &limit),
                    ("offset", "0"),
                ],
            )
            .await?;
        Ok(page
            .tracks
            .items
            .into_iter()
            .map(lenient_track_item)
            .collect())
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
            // Terminate on the page's own `next` marker (or an empty page), not a
            // short page: Spotify can return fewer than the requested items on a
            // non-final page. We read `next` only as a "more pages" flag and still
            // paginate by manual offset (the `next` URL itself 403s).
            let is_last = page.next.is_none() || page.items.is_empty();
            playlists.extend(page.items.iter().map(map_simplified_playlist));
            if is_last {
                break;
            }
        }
        Ok(playlists)
    }

    async fn playlist_tracks(&self, id: &PlaylistId) -> Result<Vec<TrackItem>, ApiError> {
        // Validate the id shape before splicing it into the endpoint path.
        let playlist_id = RsPlaylistId::from_id(id.0.as_str())
            .map_err(|e| ApiError::Mapping(format!("invalid playlist id {}: {e}", id.0)))?;
        let endpoint = format!("playlists/{}/tracks", playlist_id.id());
        let limit = PAGE_LIMIT.to_string();
        let page: RawTrackEntries = self
            .get_json(
                &endpoint,
                &[
                    ("limit", &limit),
                    ("offset", "0"),
                    ("additional_types", "track"),
                ],
            )
            .await?;
        Ok(lenient_track_items(page))
    }

    async fn top_tracks(&self, range: TimeRange) -> Result<Vec<TrackItem>, ApiError> {
        let limit = PAGE_LIMIT.to_string();
        let page: RawTrackPage = self
            .get_json(
                "me/top/tracks",
                &[
                    ("time_range", time_range_param(range)),
                    ("limit", &limit),
                    ("offset", "0"),
                ],
            )
            .await?;
        Ok(page.items.into_iter().map(lenient_track_item).collect())
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

    async fn recently_played(&self) -> Result<Vec<TrackItem>, ApiError> {
        let limit = RECENTLY_PLAYED_MAX.to_string();
        let page: RawTrackEntries = self
            .get_json("me/player/recently-played", &[("limit", &limit)])
            .await?;
        Ok(lenient_track_items(page))
    }

    async fn saved_tracks(&self) -> Result<Vec<TrackItem>, ApiError> {
        let limit = PAGE_LIMIT.to_string();
        let page: RawTrackEntries = self
            .get_json("me/tracks", &[("limit", &limit), ("offset", "0")])
            .await?;
        Ok(lenient_track_items(page))
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
            let is_last = page.next.is_none() || page.items.is_empty();
            albums.extend(page.items.iter().map(|saved| map_full_album(&saved.album)));
            if is_last {
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

    async fn is_track_saved(&self, id: &TrackId) -> Result<bool, ApiError> {
        let track_id = rs_track_id(id)?;
        let contained = self
            .retrying(|| {
                self.client
                    .library_contains([LibraryId::Track(track_id.clone())])
            })
            .await?;
        Ok(contained.first().copied().unwrap_or(false))
    }

    async fn save_track(&self, id: &TrackId) -> Result<(), ApiError> {
        let track_id = rs_track_id(id)?;
        self.retrying(|| {
            self.client
                .library_add([LibraryId::Track(track_id.clone())])
        })
        .await
    }

    async fn remove_saved_track(&self, id: &TrackId) -> Result<(), ApiError> {
        let track_id = rs_track_id(id)?;
        self.retrying(|| {
            self.client
                .library_remove([LibraryId::Track(track_id.clone())])
        })
        .await
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

/// Parse an app [`TrackId`] into rspotify's validated track id type.
fn rs_track_id(id: &TrackId) -> Result<RsTrackId<'static>, ApiError> {
    RsTrackId::from_id(id.0.clone())
        .map_err(|e| ApiError::Mapping(format!("invalid track id {}: {e}", id.0)))
}

/// Map our [`TimeRange`] to rspotify's enum.
fn to_rs_time_range(range: TimeRange) -> RsTimeRange {
    match range {
        TimeRange::ShortTerm => RsTimeRange::ShortTerm,
        TimeRange::MediumTerm => RsTimeRange::MediumTerm,
        TimeRange::LongTerm => RsTimeRange::LongTerm,
    }
}

/// Map our [`TimeRange`] to the Web API's query-string value.
fn time_range_param(range: TimeRange) -> &'static str {
    match range {
        TimeRange::ShortTerm => "short_term",
        TimeRange::MediumTerm => "medium_term",
        TimeRange::LongTerm => "long_term",
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
        RawSearchTracks, RawTrackEntries, album_art_images, classify_status, join_artists,
        lenient_track_item, lenient_track_items, map_full_artist, map_simplified_album,
        map_simplified_playlist, time_range_param, to_rs_time_range,
    };
    use crate::error::ApiError;
    use crate::model::TimeRange;
    use rspotify::model::{
        FullArtist, SimplifiedAlbum, SimplifiedArtist, SimplifiedPlaylist, TimeRange as RsTimeRange,
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

    /// A local playlist file the way Spotify actually returns it: no
    /// `external_ids`, no id, barely any album metadata. This exact shape
    /// used to fail whole pages under rspotify's strict `FullTrack`.
    fn local_track_json() -> serde_json::Value {
        serde_json::json!({
            "album": {"album_type": null, "artists": [], "external_urls": {},
                "href": null, "id": null, "images": [], "name": "",
                "release_date": null, "type": "album"},
            "artists": [{"external_urls": {}, "href": null, "id": null,
                "name": "Basement Tape", "type": "artist"}],
            "disc_number": 0, "duration_ms": 187_000, "explicit": false,
            "external_urls": {}, "href": null, "id": null, "is_local": true,
            "name": "Old Demo", "track_number": 0, "type": "track"
        })
    }

    fn parse<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
        serde_json::from_value(v).expect("sample json must deserialize")
    }

    #[test]
    fn lenient_track_maps_all_rendered_fields() {
        let item = lenient_track_item(parse(full_track_json()));
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
    fn playlist_page_with_local_track_missing_external_ids_still_parses() {
        // Regression: one local file (no `external_ids`) must not fail the
        // page — the real track and the local row both map, episodes drop.
        let page: RawTrackEntries = parse(serde_json::json!({
            "items": [
                {"track": full_track_json()},
                {"track": local_track_json()},
                {"track": {"type": "episode", "name": "Some Podcast",
                    "duration_ms": 100, "external_urls": {}}},
                {"track": null}
            ],
            "next": null
        }));

        let items = lenient_track_items(page);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Feel This Moment");
        assert_eq!(items[1].title, "Old Demo");
        assert_eq!(items[1].id.0, "", "local files keep an empty id");
        assert_eq!(items[1].artist, "Basement Tape");
    }

    #[test]
    fn search_tracks_page_parses_leniently() {
        let page: RawSearchTracks = parse(serde_json::json!({
            "tracks": {"items": [full_track_json()], "total": 1}
        }));
        assert_eq!(page.tracks.items.len(), 1);
        let item = lenient_track_item(
            page.tracks
                .items
                .into_iter()
                .next()
                .expect("one search row"),
        );
        assert_eq!(item.title, "Feel This Moment");
    }

    #[test]
    fn time_range_param_matches_web_api_values() {
        assert_eq!(time_range_param(TimeRange::ShortTerm), "short_term");
        assert_eq!(time_range_param(TimeRange::MediumTerm), "medium_term");
        assert_eq!(time_range_param(TimeRange::LongTerm), "long_term");
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
