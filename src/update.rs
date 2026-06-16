//! Pure TEA reducer: `(Model, Message) -> (state mutation, Vec<Action>)`.
//!
//! This function performs no I/O. It mutates the model in place and returns the
//! side effects the event loop should run. Keeping it pure makes the UI
//! behavior unit-testable without network, keychain, or terminal.

use crate::ipc::NowPlayingPayload;
use crate::message::{Action, Message};
use crate::model::{PlaybackSnapshot, PlaybackState, TimeRange, TrackId, TrackListSource};
use crate::state::{LibraryTab, Mode, Model, PlaybackHealth, Screen, SearchTab};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

/// Debounce window (milliseconds) before a typed query is dispatched.
pub const SEARCH_DEBOUNCE_MS: u128 = 300;

/// Seek step in milliseconds for a single left/right key press.
pub const SEEK_STEP_MS: i32 = 5_000;

/// Volume step (percentage points) for a single volume key press.
pub const VOLUME_STEP: i16 = 5;

/// How long to keep the transient volume popup visible after user input.
const VOLUME_OVERLAY_MS: u64 = 1_200;

/// Consecutive unavailable tracks that trigger a streaming-session reconnect
/// rather than continuing to skip. One region-locked track is skipped; this
/// many failures in a row means the session is dead, not the tracks.
const RECONNECT_AFTER_FAILURES: u32 = 2;

/// Apply `msg` to `model`, returning the actions to execute.
///
/// # Returns
///
/// The list of [`Action`]s the caller must dispatch. An empty vector means no
/// side effect is required.
pub fn update(model: &mut Model, msg: Message) -> Vec<Action> {
    match msg {
        Message::Quit => quit(model),
        Message::Tick => tick(model),
        Message::KeyPress(key) => key_press(model, key),
        Message::EnterScreen(screen) => enter_screen(model, screen),
        Message::EnterInsertMode => enter_insert(model),
        Message::ExitInsertMode => exit_insert(model),
        Message::SearchInputChar(c) => search_input_char(model, c),
        Message::SearchBackspace => search_backspace(model),
        Message::SearchSubmit => search_submit(model),
        Message::SearchResults(result) => search_results(model, result),
        Message::SelectNext => select_next(model),
        Message::SelectPrevious => select_previous(model),
        Message::SelectFirst => select_first(model),
        Message::SelectLast => select_last(model),
        Message::ActivateSelection => activate_selection(model),
        Message::PlaylistsLoaded(result) => playlists_loaded(model, result),
        Message::TrackListLoaded { source, result } => tracks_loaded(model, &source, result),
        Message::SavedAlbumsLoaded(result) => albums_loaded(model, result),
        Message::TopArtistsLoaded(result) => top_artists_loaded(model, result),
        Message::PlaybackEvent(event) => playback_event(model, event),
        Message::TogglePlayPause => toggle_play_pause(model),
        Message::NextTrack => vec![Action::PlayerNext],
        Message::PrevTrack => vec![Action::PlayerPrev],
        Message::SeekRelative(delta) => seek_relative(model, delta),
        Message::VolumeDelta(delta) => volume_delta(model, delta),
        Message::NowPlayingRequested(reply) => now_playing_requested(model, reply),
        // Window resize is handled by the next draw, not the Model.
        Message::Resize(..) => Vec::new(),
        Message::Error(text) => {
            model.set_error(text);
            Vec::new()
        }
    }
}

/// Mark the model for shutdown.
fn quit(model: &mut Model) -> Vec<Action> {
    model.should_quit = true;
    Vec::new()
}

/// Enter text-insertion mode (only meaningful on the search screen).
fn enter_insert(model: &mut Model) -> Vec<Action> {
    model.mode = Mode::Insert;
    Vec::new()
}

/// Leave text-insertion mode.
fn exit_insert(model: &mut Model) -> Vec<Action> {
    model.mode = Mode::Normal;
    Vec::new()
}

/// Store the multi-lane search results and reset the selection cursor.
fn search_results(
    model: &mut Model,
    result: Result<crate::api::SearchResultset, crate::error::ApiError>,
) -> Vec<Action> {
    match result {
        Ok(results) => {
            model.search_results = results;
            // Only move the cursor if the user is still on this screen; a late
            // response must not clobber the selection of a screen they left.
            if model.screen == Screen::Search {
                model.reset_selection();
            }
        }
        Err(err) => model.set_error(format!("search failed: {err}")),
    }
    Vec::new()
}

/// Store loaded top artists (the Library "Top Artists" tab).
fn top_artists_loaded(
    model: &mut Model,
    result: Result<Vec<crate::model::ArtistItem>, crate::error::ApiError>,
) -> Vec<Action> {
    match result {
        Ok(artists) => {
            model.artists = artists;
            if model.screen == Screen::Library && model.library_tab == LibraryTab::TopArtists {
                model.reset_selection();
            }
        }
        Err(err) => model.set_error(format!("loading top artists failed: {err}")),
    }
    Vec::new()
}

/// Store loaded saved albums (the Library "Albums" tab).
fn albums_loaded(
    model: &mut Model,
    result: Result<Vec<crate::model::AlbumItem>, crate::error::ApiError>,
) -> Vec<Action> {
    match result {
        Ok(albums) => {
            model.albums = albums;
            if model.screen == Screen::Library && model.library_tab == LibraryTab::Albums {
                model.reset_selection();
            }
        }
        Err(err) => model.set_error(format!("loading albums failed: {err}")),
    }
    Vec::new()
}

/// Store loaded playlists and reset the selection cursor.
fn playlists_loaded(
    model: &mut Model,
    result: Result<Vec<crate::model::PlaylistItem>, crate::error::ApiError>,
) -> Vec<Action> {
    match result {
        Ok(playlists) => {
            model.playlists = playlists;
            if model.screen == Screen::Playlists {
                model.reset_selection();
            }
        }
        Err(err) => model.set_error(format!("loading playlists failed: {err}")),
    }
    Vec::new()
}

/// Store a loaded track list (playlist tracks, top tracks, recent, or saved).
///
/// A load error is always surfaced, but loaded tracks only replace the list
/// when the response still owns the active view — a stale response (the user
/// navigated away) must not overwrite what they are now looking at.
fn tracks_loaded(
    model: &mut Model,
    source: &TrackListSource,
    result: Result<Vec<crate::model::TrackItem>, crate::error::ApiError>,
) -> Vec<Action> {
    match result {
        Ok(tracks) => {
            if accept_track_list_response(model, source) {
                model.tracks = tracks;
                model.reset_selection();
            }
        }
        Err(err) => model.set_error(format!("loading tracks failed: {err}")),
    }
    Vec::new()
}

/// Accept only the track-list response that still owns the active track view.
fn accept_track_list_response(model: &Model, source: &TrackListSource) -> bool {
    if model.track_list_source.as_ref() != Some(source) {
        return false;
    }
    matches!(
        (model.screen, model.library_tab, source),
        (
            Screen::Tracks,
            _,
            TrackListSource::Playlist(_) | TrackListSource::Album(_)
        ) | (
            Screen::Library,
            LibraryTab::TopTracks,
            TrackListSource::TopTracks(_)
        ) | (
            Screen::Library,
            LibraryTab::RecentlyPlayed,
            TrackListSource::RecentlyPlayed
        ) | (
            Screen::Library,
            LibraryTab::Saved,
            TrackListSource::SavedTracks
        )
    )
}

/// Move selection to the next item in the active widget.
fn select_next(model: &mut Model) -> Vec<Action> {
    model.list_state.select_next();
    Vec::new()
}

/// Move selection to the previous item in the active widget.
fn select_previous(model: &mut Model) -> Vec<Action> {
    model.list_state.select_previous();
    Vec::new()
}

/// Move selection to the first item.
fn select_first(model: &mut Model) -> Vec<Action> {
    model.list_state.select_first();
    Vec::new()
}

/// Move selection to the last item.
fn select_last(model: &mut Model) -> Vec<Action> {
    model.list_state.select_last();
    Vec::new()
}

/// Route a raw key event through the active input mode.
fn key_press(model: &mut Model, key: KeyEvent) -> Vec<Action> {
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    match model.mode {
        Mode::Insert => key_press_insert(model, key),
        Mode::Normal => key_press_normal(model, key),
    }
}

/// Handle keys while editing the search box.
fn key_press_insert(model: &mut Model, key: KeyEvent) -> Vec<Action> {
    match key.code {
        KeyCode::Esc => exit_insert(model),
        KeyCode::Enter => {
            model.mode = Mode::Normal;
            search_submit(model)
        }
        KeyCode::Backspace => search_backspace(model),
        KeyCode::Char(c) => search_input_char(model, c),
        _ => Vec::new(),
    }
}

/// Handle keys in navigation mode, common to every screen.
fn key_press_normal(model: &mut Model, key: KeyEvent) -> Vec<Action> {
    model.clear_status();
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return quit(model);
    }
    if let Some(actions) = tab_key(model, key.code) {
        return actions;
    }
    if let Some(actions) = global_key(model, key.code) {
        return actions;
    }
    if let Some(actions) = navigation_key(model, key.code) {
        return actions;
    }
    transport_key(model, key.code)
}

/// Cycle the active sub-tab with Tab/Shift-Tab on the Library and Search screens.
fn tab_key(model: &mut Model, code: KeyCode) -> Option<Vec<Action>> {
    let forward = match code {
        KeyCode::Tab => true,
        KeyCode::BackTab => false,
        _ => return None,
    };
    match model.screen {
        Screen::Library => {
            model.library_tab = model.library_tab.cycled(forward);
            model.reset_selection();
            Some(library_load_action(model, model.library_tab))
        }
        Screen::Search => {
            model.search_tab = model.search_tab.cycled(forward);
            model.reset_selection();
            Some(Vec::new())
        }
        Screen::Playlists | Screen::Tracks => None,
    }
}

/// Screen-switching, quit, and search-entry keys available everywhere.
fn global_key(model: &mut Model, code: KeyCode) -> Option<Vec<Action>> {
    if char_key(code, model.keybindings.quit) {
        return Some(quit(model));
    }
    if char_key(code, model.keybindings.search) {
        model.screen = Screen::Search;
        return Some(enter_insert(model));
    }
    match code {
        KeyCode::Char('1') => Some(enter_screen(model, Screen::Search)),
        KeyCode::Char('2') => Some(enter_screen(model, Screen::Playlists)),
        KeyCode::Char('3') => Some(enter_screen(model, Screen::Library)),
        _ => None,
    }
}

/// List navigation and selection keys.
fn navigation_key(model: &mut Model, code: KeyCode) -> Option<Vec<Action>> {
    if matches!(code, KeyCode::Down) || char_key(code, model.keybindings.down) {
        return Some(select_next(model));
    }
    if matches!(code, KeyCode::Up) || char_key(code, model.keybindings.up) {
        return Some(select_previous(model));
    }
    match code {
        KeyCode::Home | KeyCode::Char('g') => Some(select_first(model)),
        KeyCode::End | KeyCode::Char('G') => Some(select_last(model)),
        KeyCode::Enter => Some(activate_selection(model)),
        KeyCode::Esc => Some(escape(model)),
        _ => None,
    }
}

/// Playback transport keys (play/pause, skip, seek).
fn transport_key(model: &mut Model, code: KeyCode) -> Vec<Action> {
    if char_key(code, model.keybindings.play_pause) {
        return toggle_play_pause(model);
    }
    if char_key(code, model.keybindings.next) {
        return vec![Action::PlayerNext];
    }
    if char_key(code, model.keybindings.previous) {
        return vec![Action::PlayerPrev];
    }
    match code {
        KeyCode::Right | KeyCode::Char('l') => seek_relative(model, SEEK_STEP_MS),
        KeyCode::Left | KeyCode::Char('h') => seek_relative(model, -SEEK_STEP_MS),
        KeyCode::Char('+' | '=') => volume_delta(model, VOLUME_STEP),
        KeyCode::Char('-' | '_') => volume_delta(model, -VOLUME_STEP),
        _ => Vec::new(),
    }
}

fn char_key(code: KeyCode, binding: char) -> bool {
    matches!(code, KeyCode::Char(key) if key == binding)
}

/// `Esc` in navigation mode steps back out of a drilled-in playlist.
fn escape(model: &mut Model) -> Vec<Action> {
    if model.screen == Screen::Tracks {
        return enter_screen(model, Screen::Playlists);
    }
    Vec::new()
}

/// Handle the periodic tick: fire a debounced search once the window elapses.
fn tick(model: &mut Model) -> Vec<Action> {
    expire_volume_overlay(model);
    if model.search.pending && model.search.last_input.elapsed().as_millis() >= SEARCH_DEBOUNCE_MS {
        model.search.pending = false;
        return search_action(model);
    }
    Vec::new()
}

/// Switch screens and kick off any load action the screen needs.
fn enter_screen(model: &mut Model, screen: Screen) -> Vec<Action> {
    let changed = model.screen != screen;
    model.screen = screen;
    if changed {
        model.reset_selection();
    }
    if model.mode == Mode::Insert && screen != Screen::Search {
        model.mode = Mode::Normal;
    }
    match screen {
        Screen::Playlists => {
            model.track_list_source = None;
            vec![Action::LoadPlaylists]
        }
        Screen::Library => library_load_action(model, model.library_tab),
        Screen::Search => {
            model.track_list_source = None;
            Vec::new()
        }
        Screen::Tracks => Vec::new(),
    }
}

/// The load action that populates a given Library sub-tab.
fn library_load_action(model: &mut Model, tab: LibraryTab) -> Vec<Action> {
    match tab {
        LibraryTab::TopTracks => {
            let range = TimeRange::default();
            model.track_list_source = Some(TrackListSource::TopTracks(range));
            vec![Action::LoadTopTracks(range)]
        }
        LibraryTab::Albums => {
            model.track_list_source = None;
            vec![Action::LoadSavedAlbums]
        }
        LibraryTab::TopArtists => {
            model.track_list_source = None;
            vec![Action::LoadTopArtists(TimeRange::default())]
        }
        LibraryTab::RecentlyPlayed => {
            model.track_list_source = Some(TrackListSource::RecentlyPlayed);
            vec![Action::LoadRecentlyPlayed]
        }
        LibraryTab::Saved => {
            model.track_list_source = Some(TrackListSource::SavedTracks);
            vec![Action::LoadSavedTracks]
        }
    }
}

/// Insert a character into the search buffer and arm the debounce.
fn search_input_char(model: &mut Model, c: char) -> Vec<Action> {
    model.search.query.insert(model.search.cursor, c);
    model.search.cursor += c.len_utf8();
    model.search.last_input = std::time::Instant::now();
    model.search.pending = !model.search.query.is_empty();
    Vec::new()
}

/// Delete the character before the cursor and re-arm the debounce.
fn search_backspace(model: &mut Model) -> Vec<Action> {
    if model.search.cursor == 0 {
        return Vec::new();
    }
    let prev = model.search.query[..model.search.cursor]
        .chars()
        .next_back()
        .map_or(0, char::len_utf8);
    let start = model.search.cursor - prev;
    model
        .search
        .query
        .replace_range(start..model.search.cursor, "");
    model.search.cursor = start;
    model.search.last_input = std::time::Instant::now();
    model.search.pending = !model.search.query.is_empty();
    Vec::new()
}

/// Submit the search immediately, bypassing the debounce.
fn search_submit(model: &mut Model) -> Vec<Action> {
    model.search.pending = false;
    search_action(model)
}

/// Build the search action for the current query, or nothing when it is empty.
fn search_action(model: &Model) -> Vec<Action> {
    if model.search.query.is_empty() {
        return Vec::new();
    }
    vec![Action::Search {
        query: model.search.query.clone(),
        limit: crate::api::SEARCH_LIMIT_MAX,
    }]
}

/// Activate the selected row: open a playlist or play a track.
fn activate_selection(model: &mut Model) -> Vec<Action> {
    let Some(index) = model.list_state.selected() else {
        return Vec::new();
    };
    match model.screen {
        Screen::Playlists => activate_playlist(model, index),
        Screen::Tracks => activate_track(model, index),
        Screen::Library => activate_library(model, index),
        Screen::Search => activate_search(model, index),
    }
}

/// Drill into the selected playlist and request its tracks.
fn activate_playlist(model: &mut Model, index: usize) -> Vec<Action> {
    let Some(playlist) = model.playlists.get(index) else {
        return Vec::new();
    };
    let id = playlist.id.clone();
    model.track_list_source = Some(TrackListSource::Playlist(id.clone()));
    model.screen = Screen::Tracks;
    model.reset_selection();
    vec![Action::LoadPlaylistTracks(id)]
}

/// Play the selected track from the playlist/library track list, queueing the
/// whole list so next/previous walk it.
fn activate_track(model: &Model, index: usize) -> Vec<Action> {
    play_from(&model.tracks, index)
}

/// Activate a Library row: play/drill into music content, or no-op on artists.
fn activate_library(model: &mut Model, index: usize) -> Vec<Action> {
    match model.library_tab {
        LibraryTab::TopArtists => Vec::new(),
        LibraryTab::Albums => activate_album(model, index),
        LibraryTab::TopTracks | LibraryTab::RecentlyPlayed | LibraryTab::Saved => {
            activate_track(model, index)
        }
    }
}

/// Activate a Search row based on the active lane.
fn activate_search(model: &mut Model, index: usize) -> Vec<Action> {
    match model.search_tab {
        SearchTab::Tracks => play_from(&model.search_results.tracks, index),
        SearchTab::Playlists => activate_search_playlist(model, index),
        SearchTab::Albums => activate_search_album(model, index),
        SearchTab::Artists => Vec::new(),
    }
}

/// Drill into an album from the user's library.
fn activate_album(model: &mut Model, index: usize) -> Vec<Action> {
    let Some(album) = model.albums.get(index) else {
        return Vec::new();
    };
    let id = album.id.clone();
    model.track_list_source = Some(TrackListSource::Album(id.clone()));
    model.screen = Screen::Tracks;
    model.reset_selection();
    vec![Action::LoadAlbumTracks(id)]
}

/// Drill into a playlist from the search results.
fn activate_search_playlist(model: &mut Model, index: usize) -> Vec<Action> {
    let Some(playlist) = model.search_results.playlists.get(index) else {
        return Vec::new();
    };
    let id = playlist.id.clone();
    model.track_list_source = Some(TrackListSource::Playlist(id.clone()));
    model.screen = Screen::Tracks;
    model.reset_selection();
    vec![Action::LoadPlaylistTracks(id)]
}

/// Drill into an album from the search results.
fn activate_search_album(model: &mut Model, index: usize) -> Vec<Action> {
    let Some(album) = model.search_results.albums.get(index) else {
        return Vec::new();
    };
    let id = album.id.clone();
    model.track_list_source = Some(TrackListSource::Album(id.clone()));
    model.screen = Screen::Tracks;
    model.reset_selection();
    vec![Action::LoadAlbumTracks(id)]
}

/// Build a player-load action that queues `tracks` and starts at `index`.
fn play_from(tracks: &[crate::model::TrackItem], index: usize) -> Vec<Action> {
    if index >= tracks.len() {
        return Vec::new();
    }
    let queue = tracks.iter().map(|track| track.id.clone()).collect();
    vec![Action::PlayerLoad { queue, index }]
}

/// Toggle between play and pause based on the current playback state.
fn toggle_play_pause(model: &Model) -> Vec<Action> {
    match model.now_playing.state {
        PlaybackState::Playing => vec![Action::PlayerPause],
        PlaybackState::Paused => vec![Action::PlayerPlay],
        PlaybackState::Loading | PlaybackState::Stopped => Vec::new(),
    }
}

/// Translate a relative seek into an absolute seek action, clamped to `[0, dur]`.
fn seek_relative(model: &Model, delta: i32) -> Vec<Action> {
    let np = &model.now_playing;
    if np.duration_ms == 0 {
        return Vec::new();
    }
    let target = i64::from(np.position_ms) + i64::from(delta);
    let clamped = target.clamp(0, i64::from(np.duration_ms));
    let position = u32::try_from(clamped).unwrap_or(0);
    vec![Action::PlayerSeek(position)]
}

/// Translate a relative volume change into an absolute set, clamped to `0..=100`.
fn volume_delta(model: &mut Model, delta: i16) -> Vec<Action> {
    let current = i32::from(model.now_playing.volume);
    let target = (current + i32::from(delta)).clamp(0, 100);
    let volume = u16::try_from(target).unwrap_or(0);
    model.now_playing.volume = volume;
    model.volume_overlay_until = Some(Instant::now() + Duration::from_millis(VOLUME_OVERLAY_MS));
    vec![Action::PlayerSetVolume(volume)]
}

/// Hide the transient volume popup once its display window has elapsed.
fn expire_volume_overlay(model: &mut Model) {
    if model
        .volume_overlay_until
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        model.volume_overlay_until = None;
    }
}

/// Fold a streaming event into the now-playing snapshot.
fn playback_event(model: &mut Model, event: crate::player::PlaybackEvent) -> Vec<Action> {
    use crate::player::PlaybackEvent as Ev;
    match event {
        Ev::Loading { track } => {
            apply_loading(model, &track);
            let art = model
                .find_track(&track)
                .map(|track| track.album_art_images.clone())
                .unwrap_or_default();
            return vec![
                Action::LoadAlbumArt(art),
                Action::PublishNowPlaying(model.now_playing.clone()),
            ];
        }
        Ev::Playing { position_ms, .. } => {
            model.now_playing.state = PlaybackState::Playing;
            model.now_playing.position_ms = position_ms;
            // A successful play clears any in-progress skip/reconnect streak.
            model.playback_health = PlaybackHealth::Healthy;
        }
        Ev::Paused { position_ms, .. } => {
            model.now_playing.state = PlaybackState::Paused;
            model.now_playing.position_ms = position_ms;
        }
        Ev::PositionUpdate { position_ms } => model.now_playing.position_ms = position_ms,
        Ev::VolumeChanged { volume } => model.now_playing.volume = volume,
        Ev::Stopped { .. } => apply_stopped(model),
        Ev::PreloadNext { track } => {
            return vec![Action::PlayerPreloadNext { current: track }];
        }
        Ev::EndOfTrack { .. } => return vec![Action::PlayerNext],
        Ev::Unavailable { .. } => return track_unavailable(model),
        Ev::SessionDisconnected => return session_disconnected(model),
    }
    vec![Action::PublishNowPlaying(model.now_playing.clone())]
}

/// Clear now-playing metadata after playback stops at queue end or by command.
fn apply_stopped(model: &mut Model) {
    let volume = model.now_playing.volume;
    model.now_playing = PlaybackSnapshot {
        volume,
        ..PlaybackSnapshot::default()
    };
    model.playback_health = PlaybackHealth::Healthy;
}

/// React to a track the streaming engine could not play.
///
/// librespot reports a track it cannot load (usually region-restricted or
/// relinked) as Unavailable — it does NOT mean Premium is missing. An isolated
/// failure is skipped, but [`RECONNECT_AFTER_FAILURES`] in a row means the
/// session is dead (a broken pipe drops the audio-key channel, so every track
/// fails), so reconnect once instead of skipping through the entire queue.
fn track_unavailable(model: &mut Model) -> Vec<Action> {
    match model.playback_health {
        PlaybackHealth::Healthy => {
            model.playback_health = PlaybackHealth::Skipping(1);
            model.set_error("track unavailable here — skipping".to_owned());
            vec![Action::PlayerNext]
        }
        PlaybackHealth::Skipping(count) => {
            let count = count + 1;
            if count >= RECONNECT_AFTER_FAILURES {
                model.playback_health = PlaybackHealth::Reconnecting(0);
                model.set_error("streaming connection lost — reconnecting".to_owned());
                vec![Action::PlayerReconnect]
            } else {
                model.playback_health = PlaybackHealth::Skipping(count);
                model.set_error("track unavailable here — skipping".to_owned());
                vec![Action::PlayerNext]
            }
        }
        // After a reconnect, a few more failures means the tracks themselves are
        // unplayable (not the session): skip a bounded number, then stop rather
        // than skip-storming the rest of the queue.
        PlaybackHealth::Reconnecting(count) => {
            let count = count + 1;
            if count >= RECONNECT_AFTER_FAILURES {
                model.playback_health = PlaybackHealth::Healthy;
                model.now_playing.state = PlaybackState::Stopped;
                model.set_error("no playable tracks here — stopped".to_owned());
                Vec::new()
            } else {
                model.playback_health = PlaybackHealth::Reconnecting(count);
                vec![Action::PlayerNext]
            }
        }
    }
}

/// React to the streaming session dropping: reconnect once, marking stopped.
fn session_disconnected(model: &mut Model) -> Vec<Action> {
    model.now_playing.state = PlaybackState::Stopped;
    if matches!(model.playback_health, PlaybackHealth::Reconnecting(_)) {
        // A reconnect is already in flight; don't stack another.
        model.set_error("streaming session disconnected".to_owned());
        return Vec::new();
    }
    model.playback_health = PlaybackHealth::Reconnecting(0);
    model.set_error("streaming session lost — reconnecting".to_owned());
    vec![Action::PlayerReconnect]
}

/// Populate the now-playing metadata for a newly loading track.
fn apply_loading(model: &mut Model, track: &TrackId) {
    model.now_playing.state = PlaybackState::Loading;
    model.now_playing.position_ms = 0;
    follow_now_playing(model, track);
    let Some((title, artist, duration_ms)) = model
        .find_track(track)
        .map(|item| (item.title.clone(), item.artist.clone(), item.duration_ms))
    else {
        return;
    };
    model.now_playing.track = Some(title);
    model.now_playing.artist = Some(artist);
    model.now_playing.duration_ms = duration_ms;
}

/// Move the list selection to follow the now-playing `track` when it appears in
/// the active screen's track list, so an auto-advanced track stays highlighted.
fn follow_now_playing(model: &mut Model, track: &TrackId) {
    let position = active_track_list(model)
        .iter()
        .position(|item| &item.id == track);
    if let Some(index) = position {
        model.list_state.select(Some(index));
    }
}

/// The track list currently shown on screen, or an empty slice when the active
/// screen/tab shows no playable track list.
fn active_track_list(model: &Model) -> &[crate::model::TrackItem] {
    match model.screen {
        Screen::Tracks => &model.tracks,
        Screen::Library
            if matches!(
                model.library_tab,
                LibraryTab::TopTracks | LibraryTab::RecentlyPlayed | LibraryTab::Saved
            ) =>
        {
            &model.tracks
        }
        Screen::Search if model.search_tab == SearchTab::Tracks => &model.search_results.tracks,
        Screen::Playlists | Screen::Library | Screen::Search => &[],
    }
}

/// Answer an IPC now-playing request from the cached snapshot.
fn now_playing_requested(
    model: &Model,
    reply: tokio::sync::oneshot::Sender<NowPlayingPayload>,
) -> Vec<Action> {
    let _ = reply.send(NowPlayingPayload::from(&model.now_playing));
    Vec::new()
}

#[cfg(test)]
mod tests {
    //! Key-routing and IPC-reply behavior. These live in-crate because they use
    //! `crossterm`/`tokio` types that an integration test crate cannot reach.

    use crate::config::Keybindings;
    use crate::message::{Action, Message};
    use crate::model::PlaybackState;
    use crate::state::{Mode, Model, Screen};
    use crate::update::update;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    /// A navigation-mode key press.
    fn key(code: KeyCode) -> Message {
        Message::KeyPress(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn slash_key_enters_search_insert_mode() {
        let mut model = Model::new();
        model.screen = Screen::Library;
        update(&mut model, key(KeyCode::Char('/')));
        assert_eq!(model.screen, Screen::Search);
        assert_eq!(model.mode, Mode::Insert);
    }

    #[test]
    fn number_keys_switch_screens() {
        let mut model = Model::new();
        update(&mut model, key(KeyCode::Char('2')));
        assert_eq!(model.screen, Screen::Playlists);
        update(&mut model, key(KeyCode::Char('3')));
        assert_eq!(model.screen, Screen::Library);
        update(&mut model, key(KeyCode::Char('1')));
        assert_eq!(model.screen, Screen::Search);
    }

    #[test]
    fn insert_mode_typing_then_esc_exits() {
        let mut model = Model::new();
        update(&mut model, Message::EnterInsertMode);
        update(&mut model, key(KeyCode::Char('h')));
        update(&mut model, key(KeyCode::Char('i')));
        assert_eq!(model.search.query, "hi");
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.mode, Mode::Normal);
    }

    #[test]
    fn insert_mode_enter_submits_and_returns_to_normal() {
        let mut model = Model::new();
        update(&mut model, Message::EnterInsertMode);
        update(&mut model, key(KeyCode::Char('x')));
        let actions = update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.mode, Mode::Normal);
        assert_eq!(
            actions,
            vec![Action::Search {
                query: "x".to_owned(),
                limit: 10,
            }]
        );
    }

    #[test]
    fn ctrl_c_quits() {
        let mut model = Model::new();
        let msg = Message::KeyPress(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        update(&mut model, msg);
        assert!(model.should_quit);
    }

    #[test]
    fn key_release_is_ignored() {
        let mut model = Model::new();
        let mut event = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        event.kind = KeyEventKind::Release;
        update(&mut model, Message::KeyPress(event));
        assert!(!model.should_quit);
    }

    #[test]
    fn esc_in_tracks_returns_to_playlists() {
        let mut model = Model::new();
        model.screen = Screen::Tracks;
        let actions = update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.screen, Screen::Playlists);
        assert_eq!(actions, vec![Action::LoadPlaylists]);
    }

    #[test]
    fn space_key_toggles_playback() {
        let mut model = Model::new();
        model.now_playing.state = PlaybackState::Playing;
        let actions = update(&mut model, key(KeyCode::Char(' ')));
        assert_eq!(actions, vec![Action::PlayerPause]);
    }

    #[test]
    fn configured_keys_override_default_character_bindings() {
        let mut model = Model::new();
        model.keybindings = Keybindings {
            quit: 'x',
            down: 's',
            up: 'w',
            play_pause: 'b',
            next: 'l',
            previous: 'h',
            search: 'f',
        };

        update(&mut model, key(KeyCode::Char('q')));
        assert!(!model.should_quit);
        update(&mut model, key(KeyCode::Char('x')));
        assert!(model.should_quit);

        model.should_quit = false;
        model.now_playing.state = PlaybackState::Playing;
        assert_eq!(
            update(&mut model, key(KeyCode::Char('b'))),
            vec![Action::PlayerPause]
        );
        assert_eq!(
            update(&mut model, key(KeyCode::Char('l'))),
            vec![Action::PlayerNext]
        );
        assert_eq!(
            update(&mut model, key(KeyCode::Char('h'))),
            vec![Action::PlayerPrev]
        );
        update(&mut model, key(KeyCode::Char('f')));
        assert_eq!(model.screen, Screen::Search);
        assert_eq!(model.mode, Mode::Insert);
    }

    #[test]
    fn arrow_keys_seek() {
        let mut model = Model::new();
        model.now_playing.duration_ms = 60_000;
        model.now_playing.position_ms = 30_000;
        let fwd = update(&mut model, key(KeyCode::Right));
        assert_eq!(fwd, vec![Action::PlayerSeek(35_000)]);
        let back = update(&mut model, key(KeyCode::Left));
        assert_eq!(back, vec![Action::PlayerSeek(25_000)]);
    }

    #[test]
    fn plus_minus_keys_change_volume() {
        let mut model = Model::new();
        model.now_playing.volume = 50;
        let up = update(&mut model, key(KeyCode::Char('+')));
        assert_eq!(up, vec![Action::PlayerSetVolume(55)]);
        let down = update(&mut model, key(KeyCode::Char('-')));
        assert_eq!(down, vec![Action::PlayerSetVolume(50)]);
    }

    #[test]
    fn jk_keys_move_selection() {
        let mut model = Model::new();
        update(&mut model, key(KeyCode::Char('j')));
        assert_eq!(model.list_state.selected(), Some(0));
    }

    #[test]
    fn now_playing_request_replies_with_snapshot() {
        let mut model = Model::new();
        model.now_playing.track = Some("Song".to_owned());
        model.now_playing.state = PlaybackState::Playing;
        let (tx, rx) = tokio::sync::oneshot::channel();
        update(&mut model, Message::NowPlayingRequested(tx));
        let payload = rx.blocking_recv().expect("reply should be sent");
        assert_eq!(payload.track.as_deref(), Some("Song"));
        assert_eq!(payload.state, PlaybackState::Playing);
    }

    #[test]
    fn tab_cycles_library_tabs_and_loads_each() {
        use crate::model::TimeRange;
        use crate::state::LibraryTab;
        let mut model = Model::new();
        model.screen = Screen::Library;

        let actions = update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.library_tab, LibraryTab::Albums);
        assert_eq!(actions, vec![Action::LoadSavedAlbums]);

        let actions = update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.library_tab, LibraryTab::TopArtists);
        assert_eq!(actions, vec![Action::LoadTopArtists(TimeRange::MediumTerm)]);

        let actions = update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.library_tab, LibraryTab::RecentlyPlayed);
        assert_eq!(actions, vec![Action::LoadRecentlyPlayed]);

        let actions = update(&mut model, key(KeyCode::BackTab));
        assert_eq!(model.library_tab, LibraryTab::TopArtists);
        assert_eq!(actions, vec![Action::LoadTopArtists(TimeRange::MediumTerm)]);
    }

    #[test]
    fn tab_cycles_search_lanes_without_loading() {
        use crate::state::SearchTab;
        let mut model = Model::new();
        model.screen = Screen::Search;

        let actions = update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.search_tab, SearchTab::Albums);
        assert!(actions.is_empty());

        let actions = update(&mut model, key(KeyCode::BackTab));
        assert_eq!(model.search_tab, SearchTab::Tracks);
        assert!(actions.is_empty());
    }
}
