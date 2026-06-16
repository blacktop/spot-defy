//! Behavior tests for the pure `update` reducer.
//!
//! Tests here exercise the reducer through `spot_defy` types only; key-routing
//! and IPC-reply tests that need `crossterm`/`tokio` types directly live as unit
//! tests in `src/update.rs` (integration test crates cannot reach the library's
//! private dependencies).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use spot_defy::api::SearchResultset;
use spot_defy::message::{Action, Message};
use spot_defy::model::{
    AlbumId, AlbumItem, ArtistId, ArtistItem, PlaybackSnapshot, PlaybackState, PlaylistId,
    PlaylistItem, TimeRange, TrackId, TrackItem, TrackListSource,
};
use spot_defy::player::PlaybackEvent;
use spot_defy::state::{LibraryTab, Mode, Model, Screen, SearchTab};
use spot_defy::update::{SEARCH_DEBOUNCE_MS, update};
use std::time::{Duration, Instant};

/// Build a track fixture with a fixed two-minute duration.
fn track(id: &str, title: &str) -> TrackItem {
    TrackItem {
        id: TrackId(id.to_owned()),
        title: title.to_owned(),
        artist: "Artist".to_owned(),
        album: "Album".to_owned(),
        duration_ms: 120_000,
        album_art_images: Vec::new(),
    }
}

/// Build a playlist fixture.
fn playlist(id: &str, name: &str) -> PlaylistItem {
    PlaylistItem {
        id: PlaylistId(id.to_owned()),
        name: name.to_owned(),
        owner: "Owner".to_owned(),
        track_count: 3,
    }
}

/// Build an artist fixture.
fn artist(id: &str, name: &str) -> ArtistItem {
    ArtistItem {
        id: ArtistId(id.to_owned()),
        name: name.to_owned(),
    }
}

/// Build an album fixture.
fn album(id: &str, name: &str) -> AlbumItem {
    AlbumItem {
        id: AlbumId(id.to_owned()),
        name: name.to_owned(),
        artist: "Artist".to_owned(),
    }
}

#[test]
fn quit_message_sets_should_quit() {
    let mut model = Model::new();
    let actions = update(&mut model, Message::Quit);
    assert!(model.should_quit);
    assert!(actions.is_empty());
}

#[test]
fn esc_exits_insert_mode() {
    let mut model = Model::new();
    update(&mut model, Message::EnterInsertMode);
    assert_eq!(model.mode, Mode::Insert);
    update(&mut model, Message::ExitInsertMode);
    assert_eq!(model.mode, Mode::Normal);
}

#[test]
fn typing_arms_pending_search() {
    let mut model = Model::new();
    update(&mut model, Message::SearchInputChar('a'));
    assert_eq!(model.search.query, "a");
    assert!(model.search.pending);
}

#[test]
fn backspace_removes_last_char() {
    let mut model = Model::new();
    update(&mut model, Message::SearchInputChar('a'));
    update(&mut model, Message::SearchInputChar('b'));
    update(&mut model, Message::SearchBackspace);
    assert_eq!(model.search.query, "a");
}

#[test]
fn backspace_on_empty_query_is_noop() {
    let mut model = Model::new();
    let actions = update(&mut model, Message::SearchBackspace);
    assert!(model.search.query.is_empty());
    assert!(actions.is_empty());
}

#[test]
fn debounce_fires_search_after_threshold() {
    let mut model = Model::new();
    update(&mut model, Message::SearchInputChar('x'));
    // Simulate the debounce window having elapsed.
    let elapsed_ms = u64::try_from(SEARCH_DEBOUNCE_MS).unwrap_or(u64::MAX) + 50;
    model.search.last_input = Instant::now()
        .checked_sub(Duration::from_millis(elapsed_ms))
        .unwrap_or_else(Instant::now);

    let actions = update(&mut model, Message::Tick);
    assert!(!model.search.pending);
    assert_eq!(
        actions,
        vec![Action::Search {
            query: "x".to_owned(),
            limit: 10,
        }]
    );
}

#[test]
fn debounce_does_not_fire_before_threshold() {
    let mut model = Model::new();
    update(&mut model, Message::SearchInputChar('x'));
    let actions = update(&mut model, Message::Tick);
    assert!(model.search.pending);
    assert!(actions.is_empty());
}

#[test]
fn submit_bypasses_debounce() {
    let mut model = Model::new();
    update(&mut model, Message::SearchInputChar('q'));
    let actions = update(&mut model, Message::SearchSubmit);
    assert_eq!(
        actions,
        vec![Action::Search {
            query: "q".to_owned(),
            limit: 10,
        }]
    );
}

#[test]
fn entering_playlists_screen_requests_load() {
    let mut model = Model::new();
    let actions = update(&mut model, Message::EnterScreen(Screen::Playlists));
    assert_eq!(model.screen, Screen::Playlists);
    assert_eq!(actions, vec![Action::LoadPlaylists]);
}

#[test]
fn select_next_advances_list_selection() {
    let mut model = Model::new();
    update(&mut model, Message::SelectNext);
    assert_eq!(model.list_state.selected(), Some(0));
}

#[test]
fn entering_library_requests_top_tracks() {
    let mut model = Model::new();
    let actions = update(&mut model, Message::EnterScreen(Screen::Library));
    assert_eq!(actions, vec![Action::LoadTopTracks(TimeRange::MediumTerm)]);
}

#[test]
fn activate_playlist_drills_into_tracks() {
    let mut model = Model::new();
    model.screen = Screen::Playlists;
    model.playlists = vec![playlist("pl1", "Mix")];
    model.list_state.select(Some(0));
    let actions = update(&mut model, Message::ActivateSelection);
    assert_eq!(model.screen, Screen::Tracks);
    assert_eq!(
        actions,
        vec![Action::LoadPlaylistTracks(PlaylistId("pl1".to_owned()))]
    );
}

#[test]
fn activate_track_loads_player() {
    let mut model = Model::new();
    model.screen = Screen::Tracks;
    model.tracks = vec![track("t1", "Song")];
    model.list_state.select(Some(0));
    let actions = update(&mut model, Message::ActivateSelection);
    assert_eq!(
        actions,
        vec![Action::PlayerLoad {
            queue: vec![TrackId("t1".to_owned())],
            index: 0,
        }]
    );
}

#[test]
fn activate_with_no_selection_is_noop() {
    let mut model = Model::new();
    model.screen = Screen::Tracks;
    model.tracks = vec![track("t1", "Song")];
    let actions = update(&mut model, Message::ActivateSelection);
    assert!(actions.is_empty());
}

#[test]
fn toggle_play_pause_depends_on_state() {
    let mut model = Model::new();
    model.now_playing.state = PlaybackState::Playing;
    assert_eq!(
        update(&mut model, Message::TogglePlayPause),
        vec![Action::PlayerPause]
    );
    model.now_playing.state = PlaybackState::Paused;
    assert_eq!(
        update(&mut model, Message::TogglePlayPause),
        vec![Action::PlayerPlay]
    );
    model.now_playing.state = PlaybackState::Stopped;
    assert!(update(&mut model, Message::TogglePlayPause).is_empty());
}

#[test]
fn next_prev_messages_emit_actions() {
    let mut model = Model::new();
    assert_eq!(
        update(&mut model, Message::NextTrack),
        vec![Action::PlayerNext]
    );
    assert_eq!(
        update(&mut model, Message::PrevTrack),
        vec![Action::PlayerPrev]
    );
}

#[test]
fn seek_clamps_within_track_bounds() {
    let mut model = Model::new();
    model.now_playing = PlaybackSnapshot {
        duration_ms: 10_000,
        position_ms: 2_000,
        ..PlaybackSnapshot::default()
    };
    // Seeking back further than the start clamps to zero.
    let back = update(&mut model, Message::SeekRelative(-9_000));
    assert_eq!(back, vec![Action::PlayerSeek(0)]);
    // Seeking past the end clamps to the duration.
    let fwd = update(&mut model, Message::SeekRelative(50_000));
    assert_eq!(fwd, vec![Action::PlayerSeek(10_000)]);
}

#[test]
fn seek_with_no_track_is_noop() {
    let mut model = Model::new();
    let actions = update(&mut model, Message::SeekRelative(5_000));
    assert!(actions.is_empty());
}

#[test]
fn volume_delta_clamps_and_updates_model() {
    let mut model = Model::new();
    model.now_playing.volume = 98;
    let actions = update(&mut model, Message::VolumeDelta(10));
    assert_eq!(model.now_playing.volume, 100);
    assert!(model.volume_overlay_until.is_some());
    assert_eq!(actions, vec![Action::PlayerSetVolume(100)]);

    model.now_playing.volume = 3;
    let actions = update(&mut model, Message::VolumeDelta(-10));
    assert_eq!(model.now_playing.volume, 0);
    assert!(model.volume_overlay_until.is_some());
    assert_eq!(actions, vec![Action::PlayerSetVolume(0)]);
}

#[test]
fn tick_clears_expired_volume_overlay() {
    let mut model = Model::new();
    model.volume_overlay_until = Some(
        Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("test instant can be shifted backward"),
    );

    let actions = update(&mut model, Message::Tick);

    assert!(actions.is_empty());
    assert!(model.volume_overlay_until.is_none());
}

#[test]
fn playback_loading_event_fills_now_playing() {
    let mut model = Model::new();
    model.tracks = vec![track("t1", "Song")];
    let event = PlaybackEvent::Loading {
        track: TrackId("t1".to_owned()),
    };
    let actions = update(&mut model, Message::PlaybackEvent(event));
    assert_eq!(model.now_playing.state, PlaybackState::Loading);
    assert_eq!(model.now_playing.track.as_deref(), Some("Song"));
    assert_eq!(model.now_playing.duration_ms, 120_000);
    // Loading also requests album art (empty here — the fixture has no art).
    assert_eq!(actions.len(), 2);
    assert!(matches!(&actions[0], Action::LoadAlbumArt(images) if images.is_empty()));
    assert!(matches!(actions[1], Action::PublishNowPlaying(_)));
}

#[test]
fn end_of_track_advances_queue() {
    let mut model = Model::new();
    let event = PlaybackEvent::EndOfTrack {
        track: TrackId("t1".to_owned()),
    };
    let actions = update(&mut model, Message::PlaybackEvent(event));
    assert_eq!(actions, vec![Action::PlayerNext]);
}

#[test]
fn preload_hint_prefetches_next_queue_item() {
    let mut model = Model::new();
    let current = TrackId("t1".to_owned());
    let event = PlaybackEvent::PreloadNext {
        track: current.clone(),
    };

    let actions = update(&mut model, Message::PlaybackEvent(event));

    assert_eq!(actions, vec![Action::PlayerPreloadNext { current }]);
}

#[test]
fn unavailable_event_skips_to_next_with_status() {
    let mut model = Model::new();
    let event = PlaybackEvent::Unavailable {
        track: TrackId("t1".to_owned()),
    };
    let actions = update(&mut model, Message::PlaybackEvent(event));
    // An unavailable track is skipped, not treated as a Premium failure.
    assert_eq!(actions, vec![Action::PlayerNext]);
    assert!(
        model
            .status
            .unwrap_or_default()
            .to_lowercase()
            .contains("unavailable")
    );
}

#[test]
fn repeated_unavailable_triggers_reconnect_not_endless_skip() {
    let mut model = Model::new();
    let unavailable = || {
        Message::PlaybackEvent(PlaybackEvent::Unavailable {
            track: TrackId("t".to_owned()),
        })
    };
    // First failure could be a single region-locked track: skip it.
    assert_eq!(update(&mut model, unavailable()), vec![Action::PlayerNext]);
    // A second failure in a row means the session is dead: reconnect instead of
    // skipping through the whole queue.
    assert_eq!(
        update(&mut model, unavailable()),
        vec![Action::PlayerReconnect]
    );
    // Further failures while a reconnect is in flight skip without re-triggering.
    assert_eq!(update(&mut model, unavailable()), vec![Action::PlayerNext]);
}

#[test]
fn successful_play_resets_the_unavailable_streak() {
    let mut model = Model::new();
    let unavailable = || {
        Message::PlaybackEvent(PlaybackEvent::Unavailable {
            track: TrackId("t".to_owned()),
        })
    };
    update(&mut model, unavailable());
    update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::Playing {
            track: TrackId("ok".to_owned()),
            position_ms: 0,
        }),
    );
    // After a track plays, the next failure starts over at a skip, not reconnect.
    assert_eq!(update(&mut model, unavailable()), vec![Action::PlayerNext]);
}

#[test]
fn session_disconnect_requests_reconnect_once() {
    let mut model = Model::new();
    let disconnect = || Message::PlaybackEvent(PlaybackEvent::SessionDisconnected);
    assert_eq!(
        update(&mut model, disconnect()),
        vec![Action::PlayerReconnect]
    );
    // A second disconnect while already reconnecting does not pile on.
    assert_eq!(update(&mut model, disconnect()), Vec::new());
}

#[test]
fn unavailable_after_reconnect_is_bounded_then_stops() {
    let mut model = Model::new();
    let unavailable = || {
        Message::PlaybackEvent(PlaybackEvent::Unavailable {
            track: TrackId("t".to_owned()),
        })
    };
    // Drive into the post-reconnect state.
    assert_eq!(update(&mut model, unavailable()), vec![Action::PlayerNext]);
    assert_eq!(
        update(&mut model, unavailable()),
        vec![Action::PlayerReconnect]
    );
    // Post-reconnect failures skip a bounded number, then STOP rather than
    // skip-storming the rest of the queue track-by-track.
    assert_eq!(update(&mut model, unavailable()), vec![Action::PlayerNext]);
    let stopped = update(&mut model, unavailable());
    assert!(stopped.is_empty(), "the storm must stop, not keep skipping");
    assert_eq!(model.now_playing.state, PlaybackState::Stopped);
}

#[test]
fn stopped_event_unwedges_a_failed_reconnect() {
    let mut model = Model::new();
    let disconnect = || Message::PlaybackEvent(PlaybackEvent::SessionDisconnected);
    assert_eq!(
        update(&mut model, disconnect()),
        vec![Action::PlayerReconnect]
    );
    // A permanently failed reconnect emits Stopped, which clears the streak so
    // the state machine is not wedged in Reconnecting forever.
    update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::Stopped {
            track: TrackId(String::new()),
        }),
    );
    // A later disconnect now retries instead of being silently swallowed.
    assert_eq!(
        update(&mut model, disconnect()),
        vec![Action::PlayerReconnect]
    );
}

#[test]
fn late_search_result_does_not_reset_another_screens_selection() {
    let mut model = Model::new();
    model.screen = Screen::Playlists;
    model.playlists = vec![
        playlist("p1", "a"),
        playlist("p2", "b"),
        playlist("p3", "c"),
    ];
    model.list_state.select(Some(2));
    // A search dispatched earlier completes after the user moved to Playlists.
    update(
        &mut model,
        Message::SearchResults(Ok(SearchResultset::default())),
    );
    assert_eq!(
        model.list_state.selected(),
        Some(2),
        "a stale search response must not snap another screen's cursor to the top"
    );
}

#[test]
fn loading_event_moves_selection_to_now_playing_track() {
    let mut model = Model::new();
    model.screen = Screen::Library; // default Library tab is Top Tracks (a track list)
    model.tracks = vec![track("t1", "A"), track("t2", "B"), track("t3", "C")];
    model.list_state.select(Some(0));
    // The player auto-advanced to the third track.
    let event = PlaybackEvent::Loading {
        track: TrackId("t3".to_owned()),
    };
    update(&mut model, Message::PlaybackEvent(event));
    assert_eq!(model.list_state.selected(), Some(2));
}

#[test]
fn position_update_advances_footer() {
    let mut model = Model::new();
    let event = PlaybackEvent::PositionUpdate { position_ms: 7_500 };
    update(&mut model, Message::PlaybackEvent(event));
    assert_eq!(model.now_playing.position_ms, 7_500);
}

#[test]
fn stopped_event_clears_now_playing_and_publishes_snapshot() {
    let mut model = Model::new();
    model.now_playing = PlaybackSnapshot {
        track: Some("Song".to_owned()),
        artist: Some("Artist".to_owned()),
        state: PlaybackState::Playing,
        position_ms: 5_000,
        duration_ms: 120_000,
        volume: 42,
    };
    let event = PlaybackEvent::Stopped {
        track: TrackId("t1".to_owned()),
    };

    let actions = update(&mut model, Message::PlaybackEvent(event));

    assert_eq!(model.now_playing.state, PlaybackState::Stopped);
    assert_eq!(model.now_playing.track, None);
    assert_eq!(model.now_playing.artist, None);
    assert_eq!(model.now_playing.position_ms, 0);
    assert_eq!(model.now_playing.duration_ms, 0);
    assert_eq!(model.now_playing.volume, 42);
    assert!(
        matches!(actions.as_slice(), [Action::PublishNowPlaying(snapshot)] if snapshot.state == PlaybackState::Stopped)
    );
}

#[test]
fn stale_track_list_response_is_ignored() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::RecentlyPlayed;
    model.track_list_source = Some(TrackListSource::RecentlyPlayed);
    model.tracks = vec![track("current", "Current")];

    update(
        &mut model,
        Message::TrackListLoaded {
            source: TrackListSource::TopTracks(TimeRange::MediumTerm),
            result: Ok(vec![track("stale", "Stale")]),
        },
    );

    assert_eq!(model.tracks[0].id, TrackId("current".to_owned()));
}

#[test]
fn current_track_list_response_populates_tracks() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::RecentlyPlayed;
    model.track_list_source = Some(TrackListSource::RecentlyPlayed);

    update(
        &mut model,
        Message::TrackListLoaded {
            source: TrackListSource::RecentlyPlayed,
            result: Ok(vec![track("recent", "Recent")]),
        },
    );

    assert_eq!(model.tracks[0].id, TrackId("recent".to_owned()));
}

#[test]
fn loading_event_does_not_follow_tracks_on_albums_tab() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::Albums;
    model.albums = vec![album("al1", "Album A"), album("al2", "Album B")];
    model.tracks = vec![track("t1", "Song A"), track("t2", "Song B")];
    model.list_state.select(Some(0));

    update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::Loading {
            track: TrackId("t2".to_owned()),
        }),
    );

    assert_eq!(model.list_state.selected(), Some(0));
}

#[test]
fn search_results_reset_selection_to_first() {
    let mut model = Model::new();
    model.list_state.select(Some(5));
    let result = Ok(SearchResultset {
        tracks: vec![track("t1", "A"), track("t2", "B")],
        ..SearchResultset::default()
    });
    update(&mut model, Message::SearchResults(result));
    assert_eq!(model.search_results.tracks.len(), 2);
    assert_eq!(model.list_state.selected(), Some(0));
}

#[test]
fn search_results_populate_every_lane() {
    let mut model = Model::new();
    let result = Ok(SearchResultset {
        tracks: vec![track("t1", "A")],
        albums: vec![album("al1", "Album")],
        artists: vec![artist("ar1", "Artist")],
        playlists: vec![playlist("pl1", "Mix")],
    });
    update(&mut model, Message::SearchResults(result));
    assert_eq!(model.search_results.albums.len(), 1);
    assert_eq!(model.search_results.artists.len(), 1);
    assert_eq!(model.search_results.playlists.len(), 1);
}

#[test]
fn top_artists_loaded_populates_artists() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::TopArtists;
    let result = Ok(vec![artist("ar1", "Boards of Canada")]);
    update(&mut model, Message::TopArtistsLoaded(result));
    assert_eq!(model.artists.len(), 1);
    assert_eq!(model.list_state.selected(), Some(0));
}

#[test]
fn entering_library_on_artists_tab_loads_artists() {
    let mut model = Model::new();
    model.library_tab = LibraryTab::TopArtists;
    let actions = update(&mut model, Message::EnterScreen(Screen::Library));
    assert_eq!(actions, vec![Action::LoadTopArtists(TimeRange::MediumTerm)]);
}

#[test]
fn activate_search_track_plays_from_results() {
    let mut model = Model::new();
    model.screen = Screen::Search;
    model.search_tab = SearchTab::Tracks;
    model.search_results = SearchResultset {
        tracks: vec![track("t1", "Song")],
        ..SearchResultset::default()
    };
    model.list_state.select(Some(0));
    let actions = update(&mut model, Message::ActivateSelection);
    assert_eq!(
        actions,
        vec![Action::PlayerLoad {
            queue: vec![TrackId("t1".to_owned())],
            index: 0,
        }]
    );
}

#[test]
fn activate_search_playlist_drills_into_tracks() {
    let mut model = Model::new();
    model.screen = Screen::Search;
    model.search_tab = SearchTab::Playlists;
    model.search_results = SearchResultset {
        playlists: vec![playlist("pl1", "Mix")],
        ..SearchResultset::default()
    };
    model.list_state.select(Some(0));
    let actions = update(&mut model, Message::ActivateSelection);
    assert_eq!(model.screen, Screen::Tracks);
    assert_eq!(
        actions,
        vec![Action::LoadPlaylistTracks(PlaylistId("pl1".to_owned()))]
    );
}

#[test]
fn activate_library_artist_is_noop() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::TopArtists;
    model.artists = vec![artist("ar1", "Air")];
    model.list_state.select(Some(0));
    let actions = update(&mut model, Message::ActivateSelection);
    assert!(actions.is_empty());
}

#[test]
fn entering_library_on_albums_tab_loads_saved_albums() {
    let mut model = Model::new();
    model.library_tab = LibraryTab::Albums;

    let actions = update(&mut model, Message::EnterScreen(Screen::Library));

    assert_eq!(actions, vec![Action::LoadSavedAlbums]);
}

#[test]
fn activate_library_album_drills_into_tracks() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::Albums;
    model.albums = vec![album("al1", "Album")];
    model.list_state.select(Some(0));

    let actions = update(&mut model, Message::ActivateSelection);

    assert_eq!(model.screen, Screen::Tracks);
    assert_eq!(
        actions,
        vec![Action::LoadAlbumTracks(AlbumId("al1".to_owned()))]
    );
}

#[test]
fn search_results_clear_selection_when_empty() {
    let mut model = Model::new();
    model.list_state.select(Some(2));
    let result = Ok(SearchResultset::default());
    update(&mut model, Message::SearchResults(result));
    assert_eq!(model.list_state.selected(), None);
}

#[test]
fn api_error_surfaces_status_message() {
    let mut model = Model::new();
    let result = Err(spot_defy::error::ApiError::Response("403".to_owned()));
    update(&mut model, Message::PlaylistsLoaded(result));
    assert!(model.status.is_some());
}
