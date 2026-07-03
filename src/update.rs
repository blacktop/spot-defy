//! Pure TEA reducer: `(Model, Message) -> (state mutation, Vec<Action>)`.
//!
//! This function performs no I/O. It mutates the model in place and returns the
//! side effects the event loop should run. Keeping it pure makes the UI
//! behavior unit-testable without network, keychain, or terminal.

use crate::message::{Action, Message};
use crate::model::{PlaybackSnapshot, PlaybackState, TimeRange, TrackId, TrackListSource};
use crate::state::{LibraryTab, LoadPhase, Mode, Model, PlaybackHealth, Screen, SearchTab};
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

/// Library/playlist data younger than this is reused on re-entry instead of
/// refetched, sparing the rate-limited shared client id; `r` forces a reload.
const LIBRARY_TTL: Duration = Duration::from_secs(300);

/// Rows jumped by one PageUp/PageDown press.
const PAGE_STEP: usize = 15;

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
        Message::NextTrack => next_track(model),
        Message::PrevTrack => previous_track(model),
        Message::SeekRelative(delta) => seek_relative(model, delta),
        Message::VolumeDelta(delta) => volume_delta(model, delta),
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
            // Partial failures still render, but never silently: name the
            // lanes that failed in the status bar.
            if !results.failed_lanes.is_empty() {
                let retry = if model.keybindings.uses('r') {
                    "edit the query and press Enter"
                } else {
                    "press r to retry"
                };
                model.set_error(format!(
                    "search failed for {}; {retry}",
                    results.failed_lanes.join(", ")
                ));
            }
            model.search_results = results;
            model.search_phase = LoadPhase::Loaded(Instant::now());
            // Only move the cursor if the user is still on this screen; a late
            // response must not clobber the selection of a screen they left.
            if model.screen == Screen::Search {
                model.reset_selection();
            }
        }
        Err(err) => {
            model.search_phase = LoadPhase::Failed;
            model.set_error(format!("search failed: {err}"));
        }
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
            model.artists_phase = LoadPhase::Loaded(Instant::now());
            if model.screen == Screen::Library && model.library_tab == LibraryTab::TopArtists {
                model.reset_selection();
            }
        }
        Err(err) => {
            model.artists_phase = LoadPhase::Failed;
            model.set_error(format!("loading top artists failed: {err}"));
        }
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
            model.albums_phase = LoadPhase::Loaded(Instant::now());
            if model.screen == Screen::Library && model.library_tab == LibraryTab::Albums {
                model.reset_selection();
            }
        }
        Err(err) => {
            model.albums_phase = LoadPhase::Failed;
            model.set_error(format!("loading albums failed: {err}"));
        }
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
            model.playlists_phase = LoadPhase::Loaded(Instant::now());
            if model.screen == Screen::Playlists {
                model.reset_selection();
            }
        }
        Err(err) => {
            model.playlists_phase = LoadPhase::Failed;
            model.set_error(format!("loading playlists failed: {err}"));
        }
    }
    Vec::new()
}

/// Store a loaded track list (playlist tracks, top tracks, recent, or saved).
///
/// A load error is always surfaced. Data (and its load phase) is stored when
/// `source` still owns `model.tracks` — a stale response for a source the user
/// has since navigated away from is dropped. The selection cursor is only
/// touched when the track view is actually on screen.
fn tracks_loaded(
    model: &mut Model,
    source: &TrackListSource,
    result: Result<Vec<crate::model::TrackItem>, crate::error::ApiError>,
) -> Vec<Action> {
    let owns = model.track_list_source.as_ref() == Some(source);
    match result {
        Ok(tracks) => {
            if owns {
                model.tracks = tracks;
                model.tracks_phase = LoadPhase::Loaded(Instant::now());
                if track_view_active(model, source) {
                    model.reset_selection();
                }
            }
        }
        Err(err) => {
            model.set_error(format!("loading tracks failed: {err}"));
            if owns {
                model.tracks_phase = LoadPhase::Failed;
            }
        }
    }
    Vec::new()
}

/// Whether the track view fed by `source` is what the user is looking at.
fn track_view_active(model: &Model, source: &TrackListSource) -> bool {
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
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
        return search_clear(model);
    }
    match key.code {
        KeyCode::Esc => exit_insert(model),
        KeyCode::Enter => {
            model.mode = Mode::Normal;
            search_submit(model)
        }
        KeyCode::Backspace => search_backspace(model),
        KeyCode::Delete => search_delete(model),
        KeyCode::Left => search_cursor_left(model),
        KeyCode::Right => search_cursor_right(model),
        KeyCode::Home => {
            model.search.cursor = 0;
            Vec::new()
        }
        KeyCode::End => {
            model.search.cursor = model.search.query.len();
            Vec::new()
        }
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
        // Force-reload the active view, bypassing the freshness cache. Yields
        // to a user binding so a configured `r` keeps its configured meaning
        // (the help hints stop advertising `r` in that case too).
        KeyCode::Char('r') if !model.keybindings.uses('r') => Some(refresh_active(model)),
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
        KeyCode::PageDown => Some(select_page(model, true)),
        KeyCode::PageUp => Some(select_page(model, false)),
        KeyCode::Enter => Some(activate_selection(model)),
        KeyCode::Esc => Some(escape(model)),
        _ => None,
    }
}

/// Jump the selection a page's worth of rows, clamped to the list bounds.
fn select_page(model: &mut Model, forward: bool) -> Vec<Action> {
    let len = model.active_len();
    if len == 0 {
        return Vec::new();
    }
    let current = model.list_state.selected().unwrap_or(0);
    let target = if forward {
        (current + PAGE_STEP).min(len - 1)
    } else {
        current.saturating_sub(PAGE_STEP)
    };
    model.list_state.select(Some(target));
    Vec::new()
}

/// Playback transport keys (play/pause, skip, seek).
fn transport_key(model: &mut Model, code: KeyCode) -> Vec<Action> {
    if char_key(code, model.keybindings.play_pause) {
        return toggle_play_pause(model);
    }
    if char_key(code, model.keybindings.next) {
        return next_track(model);
    }
    if char_key(code, model.keybindings.previous) {
        return previous_track(model);
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

/// `Esc` in navigation mode steps back out of a drilled-in track list, to
/// whichever screen the user drilled in from (playlists, library, or search).
fn escape(model: &mut Model) -> Vec<Action> {
    if model.screen == Screen::Tracks {
        return enter_screen(model, model.tracks_origin);
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

/// Switch screens and kick off any load action the screen needs. Loads are
/// skipped when the target's data is already fresh or in flight (see
/// [`LIBRARY_TTL`]); `r` forces a reload via [`refresh_active`].
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
        Screen::Playlists => request_playlists(model, false),
        Screen::Library => library_load_action(model, model.library_tab),
        Screen::Search | Screen::Tracks => Vec::new(),
    }
}

/// The load action that populates a given Library sub-tab (cache-aware).
fn library_load_action(model: &mut Model, tab: LibraryTab) -> Vec<Action> {
    library_tab_request(model, tab, false)
}

/// Request a Library sub-tab's data, honoring the freshness cache unless forced.
fn library_tab_request(model: &mut Model, tab: LibraryTab, force: bool) -> Vec<Action> {
    match tab {
        LibraryTab::TopTracks => track_tab_load(
            model,
            TrackListSource::TopTracks(TimeRange::default()),
            force,
        ),
        LibraryTab::Albums => request_albums(model, force),
        LibraryTab::TopArtists => request_artists(model, force),
        LibraryTab::RecentlyPlayed => track_tab_load(model, TrackListSource::RecentlyPlayed, force),
        LibraryTab::Saved => track_tab_load(model, TrackListSource::SavedTracks, force),
    }
}

/// Whether a dataset in `phase` needs a request (not in flight, not fresh).
fn should_request(phase: LoadPhase, force: bool) -> bool {
    force || !(phase == LoadPhase::Loading || phase.is_fresh(LIBRARY_TTL))
}

/// Request the user's playlists unless they are already fresh or loading.
fn request_playlists(model: &mut Model, force: bool) -> Vec<Action> {
    if !should_request(model.playlists_phase, force) {
        return Vec::new();
    }
    model.playlists_phase = LoadPhase::Loading;
    vec![Action::LoadPlaylists]
}

/// Request saved albums unless they are already fresh or loading.
fn request_albums(model: &mut Model, force: bool) -> Vec<Action> {
    if !should_request(model.albums_phase, force) {
        return Vec::new();
    }
    model.albums_phase = LoadPhase::Loading;
    vec![Action::LoadSavedAlbums]
}

/// Request top artists unless they are already fresh or loading.
fn request_artists(model: &mut Model, force: bool) -> Vec<Action> {
    if !should_request(model.artists_phase, force) {
        return Vec::new();
    }
    model.artists_phase = LoadPhase::Loading;
    vec![Action::LoadTopArtists(TimeRange::default())]
}

/// Point `model.tracks` at `source`, loading it unless the same source is
/// already fresh or in flight. Switching sources clears the stale list so the
/// view shows a loading state instead of the previous source's rows.
fn track_tab_load(model: &mut Model, source: TrackListSource, force: bool) -> Vec<Action> {
    let same = model.track_list_source.as_ref() == Some(&source);
    if same && !should_request(model.tracks_phase, force) {
        return Vec::new();
    }
    if !same {
        model.tracks.clear();
    }
    model.track_list_source = Some(source.clone());
    model.tracks_phase = LoadPhase::Loading;
    vec![source_load_action(source)]
}

/// The [`Action`] that fetches a given track-list source.
fn source_load_action(source: TrackListSource) -> Action {
    match source {
        TrackListSource::Playlist(id) => Action::LoadPlaylistTracks(id),
        TrackListSource::Album(id) => Action::LoadAlbumTracks(id),
        TrackListSource::TopTracks(range) => Action::LoadTopTracks(range),
        TrackListSource::RecentlyPlayed => Action::LoadRecentlyPlayed,
        TrackListSource::SavedTracks => Action::LoadSavedTracks,
    }
}

/// Force-reload whatever the active screen/tab is showing (`r`).
fn refresh_active(model: &mut Model) -> Vec<Action> {
    match model.screen {
        Screen::Playlists => request_playlists(model, true),
        Screen::Library => library_tab_request(model, model.library_tab, true),
        Screen::Search => search_submit(model),
        Screen::Tracks => {
            let Some(source) = model.track_list_source.clone() else {
                return Vec::new();
            };
            model.tracks_phase = LoadPhase::Loading;
            vec![source_load_action(source)]
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

/// Delete the character at (after) the cursor and re-arm the debounce.
fn search_delete(model: &mut Model) -> Vec<Action> {
    let Some(next) = model.search.query[model.search.cursor..].chars().next() else {
        return Vec::new();
    };
    let end = model.search.cursor + next.len_utf8();
    model
        .search
        .query
        .replace_range(model.search.cursor..end, "");
    model.search.last_input = std::time::Instant::now();
    model.search.pending = !model.search.query.is_empty();
    Vec::new()
}

/// Clear the whole query (Ctrl-U), leaving the results untouched.
fn search_clear(model: &mut Model) -> Vec<Action> {
    model.search.query.clear();
    model.search.cursor = 0;
    model.search.pending = false;
    Vec::new()
}

/// Move the edit cursor one character left.
fn search_cursor_left(model: &mut Model) -> Vec<Action> {
    let step = model.search.query[..model.search.cursor]
        .chars()
        .next_back()
        .map_or(0, char::len_utf8);
    model.search.cursor -= step;
    Vec::new()
}

/// Move the edit cursor one character right.
fn search_cursor_right(model: &mut Model) -> Vec<Action> {
    let step = model.search.query[model.search.cursor..]
        .chars()
        .next()
        .map_or(0, char::len_utf8);
    model.search.cursor += step;
    Vec::new()
}

/// Submit the search immediately, bypassing the debounce.
fn search_submit(model: &mut Model) -> Vec<Action> {
    model.search.pending = false;
    search_action(model)
}

/// Build the search action for the current query, or nothing when it is empty.
fn search_action(model: &mut Model) -> Vec<Action> {
    if model.search.query.is_empty() {
        return Vec::new();
    }
    model.search_phase = LoadPhase::Loading;
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
    drill_into_tracks(model, Screen::Playlists, TrackListSource::Playlist(id))
}

/// Enter the Tracks screen from `origin`, loading `source` unless it is the
/// still-fresh current source (re-opening the same playlist/album is instant).
fn drill_into_tracks(model: &mut Model, origin: Screen, source: TrackListSource) -> Vec<Action> {
    model.tracks_origin = origin;
    model.screen = Screen::Tracks;
    let actions = track_tab_load(model, source, false);
    model.reset_selection();
    actions
}

/// Play the selected track from the playlist/library track list, queueing the
/// whole list so next/previous walk it.
fn activate_track(model: &mut Model, index: usize) -> Vec<Action> {
    play_from(model, model.tracks.clone(), index)
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
        SearchTab::Tracks => play_from(model, model.search_results.tracks.clone(), index),
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
    drill_into_tracks(model, Screen::Library, TrackListSource::Album(id))
}

/// Drill into a playlist from the search results.
fn activate_search_playlist(model: &mut Model, index: usize) -> Vec<Action> {
    let Some(playlist) = model.search_results.playlists.get(index) else {
        return Vec::new();
    };
    let id = playlist.id.clone();
    drill_into_tracks(model, Screen::Search, TrackListSource::Playlist(id))
}

/// Drill into an album from the search results.
fn activate_search_album(model: &mut Model, index: usize) -> Vec<Action> {
    let Some(album) = model.search_results.albums.get(index) else {
        return Vec::new();
    };
    let id = album.id.clone();
    drill_into_tracks(model, Screen::Search, TrackListSource::Album(id))
}

/// Advance playback to the next queue entry, updating the expected track before
/// the player emits events so stale events from the old track are ignored.
fn next_track(model: &mut Model) -> Vec<Action> {
    let mut actions = if expect_next_track(model) {
        stage_expected_track(model)
    } else {
        Vec::new()
    };
    actions.push(Action::PlayerNext);
    actions
}

/// Move playback to the previous queue entry, updating the expected track
/// before the player emits events so stale events from the old track are
/// ignored.
fn previous_track(model: &mut Model) -> Vec<Action> {
    let mut actions = if expect_previous_track(model) {
        stage_expected_track(model)
    } else {
        Vec::new()
    };
    actions.push(Action::PlayerPrev);
    actions
}

/// Build a player-load action that queues `tracks` and starts at `index`.
fn play_from(model: &mut Model, tracks: Vec<crate::model::TrackItem>, index: usize) -> Vec<Action> {
    if index >= tracks.len() {
        return Vec::new();
    }
    model.playback_queue = tracks;
    let Some(selected) = model
        .playback_queue
        .get(index)
        .map(|track| track.id.clone())
    else {
        return Vec::new();
    };
    model.now_playing_track = Some(selected);
    model.playback_queue_cursor = Some(index);
    let queue = model
        .playback_queue
        .iter()
        .map(|track| track.id.clone())
        .collect();
    let mut actions = stage_expected_track(model);
    actions.push(Action::PlayerLoad { queue, index });
    actions
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
            if !accept_track_event(model, &track) {
                return Vec::new();
            }
            let changed = model.now_playing_metadata_track.as_ref() != Some(&track);
            apply_loading(model, &track);
            if changed {
                return track_identity_actions(model, &track);
            }
        }
        Ev::Playing { track, position_ms } => {
            if !accept_track_event(model, &track) {
                return Vec::new();
            }
            let changed = sync_track_if_needed(model, &track);
            model.now_playing.state = PlaybackState::Playing;
            model.now_playing.position_ms = position_ms;
            // A successful play clears any in-progress skip/reconnect streak.
            model.playback_health = PlaybackHealth::Healthy;
            if changed {
                return track_identity_actions(model, &track);
            }
        }
        Ev::Paused { track, position_ms } => {
            if !accept_track_event(model, &track) {
                return Vec::new();
            }
            let changed = sync_track_if_needed(model, &track);
            model.now_playing.state = PlaybackState::Paused;
            model.now_playing.position_ms = position_ms;
            if changed {
                return track_identity_actions(model, &track);
            }
        }
        Ev::PositionUpdate { position_ms } => {
            if model.now_playing.state == PlaybackState::Loading {
                return Vec::new();
            }
            model.now_playing.position_ms = position_ms;
        }
        Ev::VolumeChanged { volume } => model.now_playing.volume = volume,
        Ev::Stopped { track } => {
            if track.0.is_empty() || is_current_track(model, &track) {
                apply_stopped(model);
            } else {
                return Vec::new();
            }
        }
        Ev::PreloadNext { track } => {
            if is_current_track(model, &track) {
                return vec![Action::PlayerPreloadNext { current: track }];
            }
            return Vec::new();
        }
        Ev::EndOfTrack { track } => {
            if is_current_track(model, &track) {
                return next_track(model);
            }
            return Vec::new();
        }
        Ev::Unavailable { track } => {
            if is_current_track(model, &track) {
                return track_unavailable(model);
            }
            return Vec::new();
        }
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
    model.now_playing_track = None;
    model.now_playing_metadata_track = None;
    model.playback_queue_cursor = None;
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
            let _ = expect_next_track(model);
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
                let _ = expect_next_track(model);
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
                let _ = expect_next_track(model);
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
    sync_now_playing_track(model, track);
}

/// Apply metadata/list-position updates when a playback event points at a new
/// track. Returns whether the visible now-playing identity changed.
fn sync_track_if_needed(model: &mut Model, track: &TrackId) -> bool {
    if model.now_playing_metadata_track.as_ref() == Some(track) {
        return false;
    }
    sync_now_playing_track(model, track);
    true
}

/// Update the now-playing identity from the current app-side queue or visible
/// lists, clearing stale metadata if the track cannot be resolved.
fn sync_now_playing_track(model: &mut Model, track: &TrackId) {
    model.now_playing_track = Some(track.clone());
    model.now_playing_metadata_track = Some(track.clone());
    if let Some(index) = model
        .playback_queue
        .iter()
        .position(|item| &item.id == track)
    {
        model.playback_queue_cursor = Some(index);
    }
    follow_now_playing(model, track);
    let Some((title, artist, duration_ms)) = find_playback_track(model, track)
        .map(|item| (item.title.clone(), item.artist.clone(), item.duration_ms))
    else {
        model.now_playing.track = None;
        model.now_playing.artist = None;
        model.now_playing.duration_ms = 0;
        return;
    };
    model.now_playing.track = Some(title);
    model.now_playing.artist = Some(artist);
    model.now_playing.duration_ms = duration_ms;
}

fn stage_expected_track(model: &mut Model) -> Vec<Action> {
    let Some(track) = model.now_playing_track.clone() else {
        return Vec::new();
    };
    apply_loading(model, &track);
    track_identity_actions(model, &track)
}

fn track_identity_actions(model: &Model, track: &TrackId) -> Vec<Action> {
    vec![
        Action::LoadAlbumArt(album_art_for(model, track)),
        Action::PublishNowPlaying(model.now_playing.clone()),
    ]
}

fn album_art_for(model: &Model, track: &TrackId) -> Vec<crate::model::AlbumArtImage> {
    find_playback_track(model, track)
        .map(|track| track.album_art_images.clone())
        .unwrap_or_default()
}

fn is_current_track(model: &Model, track: &TrackId) -> bool {
    model.now_playing_track.as_ref() == Some(track)
}

fn accept_track_event(model: &Model, track: &TrackId) -> bool {
    model
        .now_playing_track
        .as_ref()
        .is_none_or(|expected| expected == track)
}

fn expect_next_track(model: &mut Model) -> bool {
    let Some(cursor) = model.playback_queue_cursor else {
        return false;
    };
    let next = cursor + 1;
    let Some(track) = model.playback_queue.get(next) else {
        return false;
    };
    model.playback_queue_cursor = Some(next);
    model.now_playing_track = Some(track.id.clone());
    true
}

fn expect_previous_track(model: &mut Model) -> bool {
    let Some(cursor) = model.playback_queue_cursor else {
        return false;
    };
    let Some(previous) = cursor.checked_sub(1) else {
        return false;
    };
    let Some(track) = model.playback_queue.get(previous) else {
        return false;
    };
    model.playback_queue_cursor = Some(previous);
    model.now_playing_track = Some(track.id.clone());
    true
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

/// Resolve metadata for the active playback queue first, then any currently
/// visible/loaded track list. This prevents a previous list with the same id
/// from supplying stale title/art for a newly launched queue.
fn find_playback_track<'a>(
    model: &'a Model,
    track: &TrackId,
) -> Option<&'a crate::model::TrackItem> {
    model
        .playback_queue
        .iter()
        .find(|item| &item.id == track)
        .or_else(|| {
            active_track_list(model)
                .iter()
                .find(|item| &item.id == track)
        })
        .or_else(|| model.find_track(track))
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

#[cfg(test)]
mod tests {
    //! Key-routing behavior. These live in-crate because they use `crossterm`
    //! types that an integration test crate cannot reach.

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
    fn esc_in_tracks_returns_to_the_drill_in_origin() {
        let mut model = Model::new();
        model.screen = Screen::Tracks;
        model.tracks_origin = Screen::Playlists;
        let actions = update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.screen, Screen::Playlists);
        assert_eq!(actions, vec![Action::LoadPlaylists]);

        // Drilled in from Search: Esc returns there without any load.
        model.screen = Screen::Tracks;
        model.tracks_origin = Screen::Search;
        let actions = update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.screen, Screen::Search);
        assert!(actions.is_empty());
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
    fn tab_cycles_library_tabs_and_loads_each_once() {
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

        // Cycling back to a tab whose request is still in flight does NOT
        // refire the load: the freshness cache dedups it.
        let actions = update(&mut model, key(KeyCode::BackTab));
        assert_eq!(model.library_tab, LibraryTab::TopArtists);
        assert!(actions.is_empty());
    }

    #[test]
    fn refresh_key_forces_a_reload_of_fresh_data() {
        use crate::state::LoadPhase;
        use std::time::Instant;
        let mut model = Model::new();
        model.screen = Screen::Playlists;
        model.playlists_phase = LoadPhase::Loaded(Instant::now());

        // Fresh data: re-entering the screen loads nothing…
        assert!(update(&mut model, key(KeyCode::Char('2'))).is_empty());
        // …but `r` bypasses the cache.
        let actions = update(&mut model, key(KeyCode::Char('r')));
        assert_eq!(actions, vec![Action::LoadPlaylists]);
    }

    #[test]
    fn page_keys_jump_selection_by_a_page() {
        let mut model = Model::new();
        model.screen = Screen::Tracks;
        model.tracks = (0..40)
            .map(|i| crate::model::TrackItem {
                id: crate::model::TrackId(format!("t{i}")),
                title: format!("T{i}"),
                artist: "A".to_owned(),
                album: "L".to_owned(),
                duration_ms: 1000,
                album_art_images: Vec::new(),
            })
            .collect();
        model.list_state.select(Some(0));

        update(&mut model, key(KeyCode::PageDown));
        assert_eq!(model.list_state.selected(), Some(15));
        update(&mut model, key(KeyCode::PageDown));
        assert_eq!(model.list_state.selected(), Some(30));
        // Clamped at the end, then a page back up.
        update(&mut model, key(KeyCode::PageDown));
        assert_eq!(model.list_state.selected(), Some(39));
        update(&mut model, key(KeyCode::PageUp));
        assert_eq!(model.list_state.selected(), Some(24));
    }

    #[test]
    fn insert_mode_cursor_keys_edit_mid_query() {
        let mut model = Model::new();
        update(&mut model, Message::EnterInsertMode);
        update(&mut model, key(KeyCode::Char('a')));
        update(&mut model, key(KeyCode::Char('b')));
        // Move left and insert: cursor editing, not append-only.
        update(&mut model, key(KeyCode::Left));
        update(&mut model, key(KeyCode::Char('c')));
        assert_eq!(model.search.query, "acb");
        // Home + Delete removes the first character.
        update(&mut model, key(KeyCode::Home));
        update(&mut model, key(KeyCode::Delete));
        assert_eq!(model.search.query, "cb");
        // End puts the cursor back at the tail for appends.
        update(&mut model, key(KeyCode::End));
        update(&mut model, key(KeyCode::Char('d')));
        assert_eq!(model.search.query, "cbd");
        // Ctrl-U clears the whole query.
        let ctrl_u = Message::KeyPress(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        update(&mut model, ctrl_u);
        assert_eq!(model.search.query, "");
        assert_eq!(model.search.cursor, 0);
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
