//! TEA orchestration: build services, run the `tokio::select!` loop, route
//! [`Message`]s to `update`, and call `view`.
//!
//! `terminal.draw` runs outside `select!` so every wake re-renders. `update`
//! is pure; the [`Action`]s it returns are dispatched as spawned tasks that
//! send further [`Message`]s back over the result channel.

use crate::api::SpotifyApi;
use crate::auth::{self, TokenSet};
use crate::config::Config;
use crate::message::{Action, Message};
use crate::model::{AlbumArtImage, PlaybackSnapshot, TrackListSource};
use crate::player::{Playback, PlaybackEvent};
use crate::state::Model;
use anyhow::Context as _;
use crossterm::event::{Event, EventStream};
use futures::StreamExt as _;
use librespot_core::authentication::Credentials;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use ratatui_image::FontSize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;

/// Tick interval driving search debounce and footer position interpolation.
const TICK_INTERVAL: Duration = Duration::from_millis(250);

/// Fallback cover-art target when the pane is not measured yet.
const DEFAULT_ART_EDGE_PX: u32 = 300;

/// Wiring for the running application.
pub struct App {
    model: Model,
    api: Arc<dyn SpotifyApi>,
    player: Arc<dyn Playback>,
    tx: UnboundedSender<Message>,
    rx: UnboundedReceiver<Message>,
    /// Latest now-playing snapshot published to the IPC `now-playing` server.
    now_playing_tx: watch::Sender<PlaybackSnapshot>,
    /// Web API token refresh context; `Some` enables the background refresh task.
    token_refresh: Option<TokenRefresh>,
    /// Terminal image-protocol picker, queried at startup (`None` = no graphics).
    picker: Option<Picker>,
    /// The decoded album art for the now-playing track, ready to render.
    album_art: Option<StatefulProtocol>,
    /// Spotify-provided album art candidates for the now-playing track.
    album_art_candidates: Vec<AlbumArtImage>,
    /// Last measured render area for the centered album art.
    album_art_area: Option<Rect>,
    /// Channel the background art loader delivers `(source_url, decoded art)` on.
    /// The URL tag lets the event loop discard stale, out-of-order results.
    art_tx: UnboundedSender<(String, Option<StatefulProtocol>)>,
    /// Receiver the event loop drains to install loaded album art.
    art_rx: UnboundedReceiver<(String, Option<StatefulProtocol>)>,
    /// The art URL currently loaded/loading, to skip redundant downloads and
    /// reject results that arrive after the track has already changed.
    art_url: Option<String>,
    /// Streaming credentials kept to rebuild the session after a broken pipe,
    /// reused while valid and re-minted only if a reconnect is rejected.
    streaming_token: TokenSet,
}

/// What the background refresh task needs to renew the Web API access token.
struct TokenRefresh {
    /// When the current Web API access token expires.
    expiry: Instant,
    /// Web API client id used to mint the refreshed token.
    client_id: String,
    /// Loopback redirect port for the refresh exchange.
    redirect_port: u16,
}

/// Renew the access token this long before it expires.
const REFRESH_LEAD: Duration = Duration::from_secs(60);

/// Back off this long after a failed refresh before retrying.
const REFRESH_RETRY: Duration = Duration::from_secs(30);

/// Build services and run the TUI to completion.
///
/// Installs the ratatui panic-restoring terminal (`ratatui::init` registers a
/// hook that restores the terminal on panic), runs the OAuth flow, constructs
/// the Web API and playback services, then drives the async event loop. The
/// terminal is always restored before returning.
///
/// # Errors
///
/// Returns an error if authentication, service construction, or the event loop
/// fails.
pub async fn run() -> anyhow::Result<()> {
    let config = Config::load().context("failed to load config")?;
    let streaming = Box::pin(auth::obtain_streaming_token())
        .await
        .context("spotify streaming login failed")?;
    let webapi = Box::pin(auth::obtain_webapi_token(
        &config.client_id,
        config.redirect_port,
    ))
    .await
    .context("spotify web api login failed")?;

    let mut app = Box::pin(build_app(&streaming, &webapi, &config))
        .await
        .context("failed to build application services")?;

    // Query the terminal's image protocol BEFORE ratatui takes over the
    // terminal (the query reads stdin). `None` (or a query failure) just means
    // no album art is shown.
    app.set_picker(Picker::from_query_stdio().ok());

    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal).await;
    ratatui::restore();
    result
}

/// Construct the Web API and playback services from a fresh [`TokenSet`].
///
/// Establishes the librespot streaming session before handing control to the
/// event loop so the first playback command does not race the handshake.
async fn build_app(
    streaming: &TokenSet,
    webapi: &TokenSet,
    config: &Config,
) -> anyhow::Result<App> {
    let api = crate::api::rspotify_client::RspotifyApi::new(auth::to_rspotify_token(webapi));
    let mut player = crate::player::librespot_player::LibrespotPlayer::new();
    Box::pin(player.connect(auth::to_librespot_credentials(streaming)))
        .await
        .context("failed to connect streaming session")?;
    let mut app = App::new(Box::new(api), Box::new(player), streaming.clone(), config)?;
    app.schedule_token_refresh(
        webapi.expires_at,
        config.client_id.clone(),
        config.redirect_port,
    );
    Ok(app)
}

impl App {
    /// Assemble the application from its service dependencies.
    ///
    /// # Errors
    ///
    /// Returns an error if keybindings conflict or configured theme color names
    /// cannot be converted into renderer colors.
    pub fn new(
        api: Box<dyn SpotifyApi>,
        player: Box<dyn Playback>,
        streaming_token: TokenSet,
        config: &Config,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = unbounded_channel();
        let (now_playing_tx, _now_playing_rx) = watch::channel(PlaybackSnapshot::default());
        let (art_tx, art_rx) = unbounded_channel();
        Ok(Self {
            model: Model::from_config(config)?,
            api: Arc::from(api),
            player: Arc::from(player),
            tx,
            rx,
            now_playing_tx,
            token_refresh: None,
            picker: None,
            album_art: None,
            album_art_candidates: Vec::new(),
            album_art_area: None,
            art_tx,
            art_rx,
            art_url: None,
            streaming_token,
        })
    }

    /// Install the terminal image-protocol picker. Must be queried *before*
    /// the terminal is taken over by ratatui (see [`run`]).
    pub fn set_picker(&mut self, picker: Option<Picker>) {
        self.picker = picker;
    }

    /// Enable the background Web API token refresh task, renewing the token
    /// shortly before `expiry` using `client_id`/`redirect_port`.
    pub fn schedule_token_refresh(
        &mut self,
        expiry: Instant,
        client_id: String,
        redirect_port: u16,
    ) {
        self.token_refresh = Some(TokenRefresh {
            expiry,
            client_id,
            redirect_port,
        });
    }

    /// Borrow the Web API service handle.
    #[must_use]
    pub fn api(&self) -> &dyn SpotifyApi {
        self.api.as_ref()
    }

    /// Borrow the playback service handle.
    #[must_use]
    pub fn player(&self) -> &dyn Playback {
        self.player.as_ref()
    }

    /// Clone the message sender used by spawned action tasks.
    #[must_use]
    pub fn sender(&self) -> UnboundedSender<Message> {
        self.tx.clone()
    }

    /// Run the `tokio::select!` event loop, drawing on every wake.
    ///
    /// Sources are merged onto the single result channel: terminal events, a
    /// periodic tick, and the player event stream. `update` is the only place
    /// the model mutates; the [`Action`]s it returns are dispatched as tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if drawing or event reading fails fatally.
    pub async fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        self.spawn_player_bridge();
        self.spawn_now_playing_server();
        self.spawn_token_refresh();
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(TICK_INTERVAL);

        loop {
            let font_size = self.picker.as_ref().map(Picker::font_size);
            {
                let model = &mut self.model;
                let art = &mut self.album_art;
                let art_area = &mut self.album_art_area;
                terminal
                    .draw(|frame| crate::view::view(model, art, art_area, font_size, frame))
                    .context("draw failed")?;
            }
            self.maybe_load_responsive_art();

            let message = tokio::select! {
                maybe_event = events.next() => message_from_event(maybe_event)?,
                _ = ticker.tick() => Some(Message::Tick),
                maybe_msg = self.rx.recv() => maybe_msg,
                maybe_art = self.art_rx.recv() => {
                    // Apply only if the result is for the still-current track,
                    // so a slow earlier download can't overwrite newer art.
                    if let Some((url, art)) = maybe_art {
                        if self.art_url.as_deref() == Some(url.as_str()) {
                            self.album_art = art;
                        }
                    }
                    None
                }
            };

            let Some(message) = message else { continue };
            let actions = crate::update::update(&mut self.model, message);
            for action in actions {
                self.dispatch_action(action);
            }
            if self.model.should_quit {
                break;
            }
        }
        Ok(())
    }

    /// Forward the player's [`PlaybackEvent`](crate::player::PlaybackEvent)
    /// stream onto the result channel.
    fn spawn_player_bridge(&self) {
        let mut rx = self.player.subscribe();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if tx.send(Message::PlaybackEvent(event)).is_err() {
                    break;
                }
            }
        });
    }

    /// Start the now-playing IPC server backed by the watch channel.
    ///
    /// A bind failure (e.g. another instance already running, no runtime dir)
    /// is logged and otherwise ignored: the TUI runs fine without the tmux
    /// status socket, so this must never abort startup.
    fn spawn_now_playing_server(&self) {
        let listener = match crate::ipc::server::bind() {
            Ok(listener) => listener,
            Err(err) => {
                tracing::warn!(error = %err, "now-playing IPC server disabled");
                return;
            }
        };
        let rx = self.now_playing_tx.subscribe();
        tokio::spawn(async move {
            if let Err(err) = crate::ipc::server::serve(listener, rx).await {
                tracing::warn!(error = %err, "now-playing IPC server stopped");
            }
        });
    }

    /// Spawn the background access-token refresh task, if one was scheduled.
    ///
    /// Renews the Web API token from the Keychain refresh token shortly before
    /// it expires. The librespot streaming session maintains its own connection
    /// and is not re-authenticated here.
    fn spawn_token_refresh(&self) {
        let Some(refresh) = self.token_refresh.as_ref() else {
            return;
        };
        let api = Arc::clone(&self.api);
        let client_id = refresh.client_id.clone();
        let redirect_port = refresh.redirect_port;
        let expiry = refresh.expiry;
        tokio::spawn(run_token_refresh(api, expiry, client_id, redirect_port));
    }

    /// Dispatch a single [`Action`].
    ///
    /// [`Action::PublishNowPlaying`] and [`Action::LoadAlbumArt`] are handled
    /// inline (IPC publish and art loading); every other action runs in a
    /// spawned task that returns a follow-up [`Message`].
    fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::PublishNowPlaying(snapshot) => {
                let _ = self.now_playing_tx.send(snapshot);
                return;
            }
            Action::LoadAlbumArt(candidates) => {
                self.set_album_art_candidates(candidates);
                return;
            }
            Action::PlayerReconnect => {
                self.spawn_reconnect();
                return;
            }
            _ => {}
        }
        let api = Arc::clone(&self.api);
        let player = Arc::clone(&self.player);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let future = Box::pin(run_action(api, player, action));
            if let Some(message) = future.await {
                let _ = tx.send(message);
            }
        });
    }

    /// Rebuild the dropped streaming session off the UI thread, reusing the
    /// in-memory streaming token. The current track resumes once the new
    /// handshake completes and emits its `Playing` event.
    fn spawn_reconnect(&self) {
        let player = Arc::clone(&self.player);
        let creds = auth::to_librespot_credentials(&self.streaming_token);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Some(err) = reconnect_session(player, creds).await {
                // Reset the reducer's reconnect state (via Stopped) so a
                // permanently failed reconnect does not wedge playback in
                // Reconnecting, then surface the failure.
                let _ = tx.send(Message::PlaybackEvent(PlaybackEvent::Stopped {
                    track: crate::model::TrackId(String::new()),
                }));
                let _ = tx.send(Message::Error(err));
            }
        });
    }

    /// Install the current track's Spotify art candidates and trigger the best
    /// responsive load for the latest measured pane.
    fn set_album_art_candidates(&mut self, candidates: Vec<AlbumArtImage>) {
        self.album_art_candidates = candidates;
        self.art_url = None;
        self.album_art = None;
        self.maybe_load_responsive_art();
    }

    /// Load the smallest candidate that satisfies the current pane size. This
    /// keeps normal terminals on Spotify's smaller images, while large panes
    /// upgrade to the wider cover only when it can actually be displayed.
    fn maybe_load_responsive_art(&mut self) {
        if self.album_art_candidates.is_empty() || self.album_art_area.is_none() {
            return;
        }
        let Some(picker) = self.picker.clone() else {
            return;
        };
        let target_edge_px = self.target_album_art_edge_px();
        if self.current_art_satisfies(target_edge_px) {
            return;
        }
        let Some(candidate) =
            select_album_art_candidate(&self.album_art_candidates, target_edge_px)
        else {
            return;
        };
        if self.art_url.as_deref() == Some(candidate.url.as_str()) {
            return;
        }
        let url = candidate.url.clone();
        self.art_url = Some(url.clone());
        self.album_art = None;
        let tx = self.art_tx.clone();
        tokio::spawn(async move {
            let art = load_album_art(&picker, &url).await;
            let _ = tx.send((url, art));
        });
    }

    /// Target edge length, in source pixels, for the measured art render area.
    fn target_album_art_edge_px(&self) -> u32 {
        let Some(area) = self.album_art_area else {
            return DEFAULT_ART_EDGE_PX;
        };
        let Some(picker) = self.picker.as_ref() else {
            return DEFAULT_ART_EDGE_PX;
        };
        pane_edge_px(area, picker.font_size()).unwrap_or(DEFAULT_ART_EDGE_PX)
    }

    /// Whether the already-loaded/loading URL is large enough for `target_edge_px`.
    fn current_art_satisfies(&self, target_edge_px: u32) -> bool {
        let Some(url) = self.art_url.as_deref() else {
            return false;
        };
        self.album_art_candidates
            .iter()
            .find(|candidate| candidate.url == url)
            .and_then(album_art_edge_px)
            .is_some_and(|edge| edge >= target_edge_px)
    }
}

/// Convert a terminal area plus font cell size into the square source-art edge
/// that can fill it without cropping.
fn pane_edge_px(area: Rect, font_size: FontSize) -> Option<u32> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let cell_width = u32::from(font_size.width.max(1));
    let cell_height = u32::from(font_size.height.max(1));
    let width_px = u32::from(area.width) * cell_width;
    let height_px = u32::from(area.height) * cell_height;
    Some(width_px.min(height_px))
}

/// Pick the smallest known Spotify cover that satisfies the target edge, or
/// the largest available cover when the pane is bigger than every candidate.
fn select_album_art_candidate(
    candidates: &[AlbumArtImage],
    target_edge_px: u32,
) -> Option<&AlbumArtImage> {
    candidates
        .iter()
        .filter_map(|image| album_art_edge_px(image).map(|edge| (image, edge)))
        .filter(|(_, edge)| *edge >= target_edge_px)
        .min_by_key(|(_, edge)| *edge)
        .map(|(image, _)| image)
        .or_else(|| {
            candidates
                .iter()
                .filter_map(|image| album_art_edge_px(image).map(|edge| (image, edge)))
                .max_by_key(|(_, edge)| *edge)
                .map(|(image, _)| image)
        })
        .or_else(|| candidates.first())
}

/// Conservative square edge for a source image. Spotify art is square in
/// practice, but optional dimensions mean callers must tolerate partial data.
fn album_art_edge_px(image: &AlbumArtImage) -> Option<u32> {
    match (image.width, image.height) {
        (Some(width), Some(height)) => Some(width.min(height)),
        (Some(width), None) => Some(width),
        (None, Some(height)) => Some(height),
        (None, None) => None,
    }
}

/// Download and decode album art into a renderable protocol, returning `None`
/// on any failure (network error, non-200, or undecodable image).
async fn load_album_art(picker: &Picker, url: &str) -> Option<StatefulProtocol> {
    let bytes = reqwest::get(url)
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .await
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?;
    Some(picker.new_resize_protocol(image))
}

/// Convert a crossterm event poll into a [`Message`], if any.
///
/// # Errors
///
/// Returns an error only when the event stream yields a fatal I/O error.
fn message_from_event(
    maybe_event: Option<std::io::Result<Event>>,
) -> anyhow::Result<Option<Message>> {
    match maybe_event {
        Some(Ok(Event::Key(key))) => Ok(Some(Message::KeyPress(key))),
        Some(Ok(Event::Resize(w, h))) => Ok(Some(Message::Resize(w, h))),
        Some(Ok(_)) | None => Ok(None),
        Some(Err(err)) => Err(anyhow::Error::new(err).context("terminal event stream failed")),
    }
}

/// Execute one [`Action`] against the services, producing a follow-up message.
///
/// Returns `None` for fire-and-forget commands that need no reply. API errors
/// are wrapped into the corresponding `*Loaded`/`SearchResults` message so the
/// reducer can surface them; player errors become [`Message::Error`].
async fn run_action(
    api: Arc<dyn SpotifyApi>,
    player: Arc<dyn Playback>,
    action: Action,
) -> Option<Message> {
    match action {
        Action::Search { query, limit } => Some(Message::SearchResults(
            search(api.as_ref(), &query, limit).await,
        )),
        Action::LoadPlaylists => Some(Message::PlaylistsLoaded(api.current_user_playlists().await)),
        Action::LoadPlaylistTracks(id) => {
            let result = api.playlist_tracks(&id).await;
            Some(Message::TrackListLoaded {
                source: TrackListSource::Playlist(id),
                result,
            })
        }
        Action::LoadTopTracks(range) => {
            let result = api.top_tracks(range).await;
            Some(Message::TrackListLoaded {
                source: TrackListSource::TopTracks(range),
                result,
            })
        }
        Action::LoadTopArtists(range) => {
            Some(Message::TopArtistsLoaded(api.top_artists(range).await))
        }
        Action::LoadRecentlyPlayed => {
            let result = api.recently_played().await;
            Some(Message::TrackListLoaded {
                source: TrackListSource::RecentlyPlayed,
                result,
            })
        }
        Action::LoadSavedTracks => {
            let result = api.saved_tracks().await;
            Some(Message::TrackListLoaded {
                source: TrackListSource::SavedTracks,
                result,
            })
        }
        Action::LoadSavedAlbums => Some(Message::SavedAlbumsLoaded(api.saved_albums().await)),
        Action::LoadAlbumTracks(id) => {
            let result = api.album_tracks(&id).await;
            Some(Message::TrackListLoaded {
                source: TrackListSource::Album(id),
                result,
            })
        }
        Action::PlayerLoad { queue, index } => {
            player_result(player.load_queue(&queue, index, true))
        }
        Action::PlayerPlay => player_result(player.play()),
        Action::PlayerPause => player_result(player.pause()),
        Action::PlayerNext => player_result(player.next()),
        Action::PlayerPreloadNext { current } => player_result(player.preload_next(&current)),
        Action::PlayerPrev => player_result(player.previous()),
        Action::PlayerSeek(position_ms) => player_result(player.seek(position_ms)),
        Action::PlayerSetVolume(volume) => player_result(player.set_volume(volume)),
        // Handled inline in `dispatch_action` (IPC publish, art loading, and the
        // session reconnect); the background token refresh runs as its own task.
        Action::PublishNowPlaying(_) | Action::LoadAlbumArt(_) | Action::PlayerReconnect => None,
    }
}

/// Run a multi-type search, populating every lane of a
/// [`SearchResultset`](crate::api::SearchResultset).
///
/// The four type queries run concurrently; a failure in any lane fails the
/// whole search so the error surfaces rather than showing partial results.
async fn search(
    api: &dyn SpotifyApi,
    query: &str,
    limit: u32,
) -> Result<crate::api::SearchResultset, crate::error::ApiError> {
    let (tracks, albums, artists, playlists) = tokio::try_join!(
        api.search_tracks(query, limit),
        api.search_albums(query, limit),
        api.search_artists(query, limit),
        api.search_playlists(query, limit),
    )?;
    Ok(crate::api::SearchResultset {
        tracks,
        albums,
        artists,
        playlists,
    })
}

/// Reconnect the streaming session, re-minting streaming credentials once if
/// the cached token is rejected (e.g. it expired during a long session).
///
/// Returns `None` on success (the resumed track's events update the UI) and an
/// error message only on hard failure.
async fn reconnect_session(player: Arc<dyn Playback>, creds: Credentials) -> Option<String> {
    if player.reconnect(creds).await.is_ok() {
        return None;
    }
    match auth::obtain_streaming_token().await {
        Ok(fresh) => match player
            .reconnect(auth::to_librespot_credentials(&fresh))
            .await
        {
            Ok(()) => None,
            Err(err) => Some(format!("streaming reconnect failed: {err}")),
        },
        Err(err) => Some(format!("streaming reconnect failed: {err}")),
    }
}

/// Turn a player control result into an error message, or nothing on success.
fn player_result(result: Result<(), crate::error::PlayerError>) -> Option<Message> {
    match result {
        Ok(()) => None,
        Err(err) => Some(Message::Error(err.to_string())),
    }
}

/// Renew the Web API access token shortly before each expiry, indefinitely.
///
/// Loads the rotating refresh token from the Keychain, mints a fresh access
/// token, applies it to the API client, then waits for the next expiry. A
/// failed refresh backs off and retries rather than aborting, so a transient
/// network error never permanently stops the refresh loop.
async fn run_token_refresh(
    api: Arc<dyn SpotifyApi>,
    mut expiry: Instant,
    client_id: String,
    redirect_port: u16,
) {
    loop {
        let wake = expiry.checked_sub(REFRESH_LEAD).unwrap_or(expiry);
        tokio::time::sleep_until(tokio::time::Instant::from_std(wake)).await;
        match Box::pin(auth::refresh_webapi_from_keychain(
            &client_id,
            redirect_port,
        ))
        .await
        {
            Ok(tokens) => {
                api.set_access_token(tokens.access).await;
                expiry = tokens.expires_at;
                tracing::info!("refreshed spotify web-api access token");
            }
            Err(err) => {
                tracing::warn!(error = %err, "web-api token refresh failed; retrying soon");
                tokio::time::sleep(REFRESH_RETRY).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{album_art_edge_px, pane_edge_px, select_album_art_candidate};
    use crate::model::AlbumArtImage;
    use ratatui::layout::Rect;
    use ratatui_image::FontSize;

    fn art(url: &str, size: Option<u32>) -> AlbumArtImage {
        AlbumArtImage {
            url: url.to_owned(),
            width: size,
            height: size,
        }
    }

    #[test]
    fn selects_smallest_art_that_satisfies_target() {
        let candidates = vec![
            art("https://example.test/640.jpg", Some(640)),
            art("https://example.test/300.jpg", Some(300)),
            art("https://example.test/64.jpg", Some(64)),
        ];

        let selected = select_album_art_candidate(&candidates, 280);

        assert_eq!(
            selected.map(|image| image.url.as_str()),
            Some("https://example.test/300.jpg")
        );
    }

    #[test]
    fn selects_largest_art_when_target_exceeds_available() {
        let candidates = vec![
            art("https://example.test/640.jpg", Some(640)),
            art("https://example.test/300.jpg", Some(300)),
            art("https://example.test/64.jpg", Some(64)),
        ];

        let selected = select_album_art_candidate(&candidates, 900);

        assert_eq!(
            selected.map(|image| image.url.as_str()),
            Some("https://example.test/640.jpg")
        );
    }

    #[test]
    fn falls_back_to_first_art_without_dimensions() {
        let candidates = vec![art("https://example.test/unknown.jpg", None)];

        let selected = select_album_art_candidate(&candidates, 300);

        assert_eq!(
            selected.map(|image| image.url.as_str()),
            Some("https://example.test/unknown.jpg")
        );
    }

    #[test]
    fn art_edge_uses_smaller_dimension_when_rectangular() {
        let image = AlbumArtImage {
            url: "https://example.test/art.jpg".to_owned(),
            width: Some(640),
            height: Some(300),
        };

        assert_eq!(album_art_edge_px(&image), Some(300));
    }

    #[test]
    fn pane_edge_px_uses_terminal_cell_dimensions() {
        let area = Rect::new(0, 0, 30, 20);
        let font = FontSize::new(8, 16);

        assert_eq!(pane_edge_px(area, font), Some(240));
    }
}
