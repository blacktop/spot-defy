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
    AlbumArtImage, AlbumId, AlbumItem, ArtistId, ArtistItem, PlaybackSnapshot, PlaybackState,
    PlaylistId, PlaylistItem, TimeRange, TrackId, TrackItem, TrackListSource,
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

fn set_current(model: &mut Model, id: &str) {
    model.now_playing_track = Some(TrackId(id.to_owned()));
}

fn track_ids(ids: &[&str]) -> Vec<TrackId> {
    ids.iter().map(|id| TrackId((*id).to_owned())).collect()
}

fn assert_loading_actions(actions: &[Action], title: &str, art: &[AlbumArtImage]) {
    assert!(matches!(&actions[0], Action::LoadAlbumArt(images) if images == art));
    assert!(
        matches!(&actions[1], Action::PublishNowPlaying(snapshot) if snapshot.track.as_deref() == Some(title) && snapshot.state == PlaybackState::Loading)
    );
}

fn assert_player_load(action: &Action, queue: &[&str], index: usize) {
    let expected_queue = track_ids(queue);
    assert!(matches!(
        action,
        Action::PlayerLoad {
            queue: actual_queue,
            index: actual_index,
        } if actual_queue == &expected_queue && *actual_index == index
    ));
}

fn assert_now_playing_identity(model: &Model, id: &str) {
    let expected = TrackId(id.to_owned());
    assert_eq!(model.now_playing_track.as_ref(), Some(&expected));
    assert_eq!(model.now_playing_metadata_track.as_ref(), Some(&expected));
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
    assert_loading_actions(&actions, "Song", &[]);
    assert_player_load(&actions[2], &["t1"], 0);
    assert_now_playing_identity(&model, "t1");
    assert_eq!(model.playback_queue, vec![track("t1", "Song")]);
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
fn next_prev_with_no_queue_are_noops() {
    let mut model = Model::new();
    assert!(update(&mut model, Message::NextTrack).is_empty());
    assert!(update(&mut model, Message::PrevTrack).is_empty());
}

#[test]
fn next_after_stop_restarts_the_loaded_queue() {
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = None; // stopped, queue still loaded
    let actions = update(&mut model, Message::NextTrack);
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(0))));
    assert_eq!(model.playback_queue_cursor, Some(0));
}

#[test]
fn manual_next_sets_expected_track_before_player_events() {
    let mut model = Model::new();
    let mut next = track("t2", "Next");
    next.album_art_images = vec![AlbumArtImage {
        url: "https://example.invalid/next.jpg".to_owned(),
        width: Some(300),
        height: Some(300),
    }];
    model.playback_queue = vec![track("t1", "Current"), next.clone()];
    model.playback_queue_cursor = Some(0);
    set_current(&mut model, "t1");

    let actions = update(&mut model, Message::NextTrack);
    assert_loading_actions(&actions, "Next", &next.album_art_images);
    assert!(matches!(actions[2], Action::PlayerPlayIndex(1)));
    assert_now_playing_identity(&model, "t2");
    assert_eq!(model.playback_queue_cursor, Some(1));

    let stale_end = PlaybackEvent::EndOfTrack {
        track: TrackId("t1".to_owned()),
    };
    assert!(update(&mut model, Message::PlaybackEvent(stale_end)).is_empty());
}

#[test]
fn manual_next_ignores_stale_position_update_while_loading_next_track() {
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "Current"), track("t2", "Next")];
    model.playback_queue_cursor = Some(0);
    model.now_playing.position_ms = 118_000;
    set_current(&mut model, "t1");

    let actions = update(&mut model, Message::NextTrack);

    assert!(matches!(actions[2], Action::PlayerPlayIndex(1)));
    assert_eq!(model.now_playing.track.as_deref(), Some("Next"));
    assert_eq!(model.now_playing.state, PlaybackState::Loading);
    assert_eq!(model.now_playing.position_ms, 0);

    let actions = update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::PositionUpdate {
            position_ms: 119_000,
        }),
    );

    assert!(actions.is_empty());
    assert_eq!(model.now_playing.position_ms, 0);
}

#[test]
fn manual_previous_sets_expected_track_before_player_events() {
    let mut model = Model::new();
    let mut previous = track("t1", "Previous");
    previous.album_art_images = vec![AlbumArtImage {
        url: "https://example.invalid/previous.jpg".to_owned(),
        width: Some(300),
        height: Some(300),
    }];
    model.playback_queue = vec![previous.clone(), track("t2", "Current")];
    model.playback_queue_cursor = Some(1);
    set_current(&mut model, "t2");

    let actions = update(&mut model, Message::PrevTrack);
    assert_loading_actions(&actions, "Previous", &previous.album_art_images);
    assert!(matches!(actions[2], Action::PlayerPlayIndex(0)));
    assert_now_playing_identity(&model, "t1");
    assert_eq!(model.playback_queue_cursor, Some(0));
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
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = Some(0);
    set_current(&mut model, "t1");
    let event = PlaybackEvent::EndOfTrack {
        track: TrackId("t1".to_owned()),
    };
    let actions = update(&mut model, Message::PlaybackEvent(event));
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(1))));
    assert_eq!(model.playback_queue_cursor, Some(1));
}

#[test]
fn end_of_track_with_repeat_one_replays_the_same_track() {
    use spot_defy::state::RepeatMode;
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = Some(0);
    model.repeat = RepeatMode::One;
    set_current(&mut model, "t1");
    let event = PlaybackEvent::EndOfTrack {
        track: TrackId("t1".to_owned()),
    };
    let actions = update(&mut model, Message::PlaybackEvent(event));
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(0))));
    assert_eq!(model.playback_queue_cursor, Some(0));
}

#[test]
fn end_of_queue_wraps_with_repeat_all_and_stops_with_repeat_off() {
    use spot_defy::state::RepeatMode;
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = Some(1);
    set_current(&mut model, "t2");
    let end = || {
        Message::PlaybackEvent(PlaybackEvent::EndOfTrack {
            track: TrackId("t2".to_owned()),
        })
    };

    model.repeat = RepeatMode::All;
    let actions = update(&mut model, end());
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(0))));

    // Reset to the queue end with repeat off: the queue stops.
    model.playback_queue_cursor = Some(1);
    set_current(&mut model, "t2");
    model.repeat = RepeatMode::Off;
    let actions = update(&mut model, end());
    assert_eq!(actions, vec![Action::PlayerStop]);
}

#[test]
fn manual_next_in_repeat_one_still_advances() {
    use spot_defy::state::RepeatMode;
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = Some(0);
    model.repeat = RepeatMode::One;
    set_current(&mut model, "t1");
    let actions = update(&mut model, Message::NextTrack);
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(1))));
}

#[test]
fn preload_hint_prefetches_next_queue_item() {
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = Some(0);
    let current = TrackId("t1".to_owned());
    model.now_playing_track = Some(current.clone());
    let event = PlaybackEvent::PreloadNext {
        track: current.clone(),
    };

    let actions = update(&mut model, Message::PlaybackEvent(event));

    assert_eq!(
        actions,
        vec![Action::PlayerPreload(TrackId("t2".to_owned()))]
    );
}

#[test]
fn preload_hint_respects_repeat_one() {
    use spot_defy::state::RepeatMode;
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = Some(0);
    model.repeat = RepeatMode::One;
    let current = TrackId("t1".to_owned());
    model.now_playing_track = Some(current.clone());

    let actions = update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::PreloadNext { track: current }),
    );

    // Repeat-one replays the already-loaded track: nothing new to preload.
    assert!(actions.is_empty());
}

#[test]
fn unavailable_event_skips_to_next_with_status() {
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A"), track("t2", "B")];
    model.playback_queue_cursor = Some(0);
    set_current(&mut model, "t1");
    let event = PlaybackEvent::Unavailable {
        track: TrackId("t1".to_owned()),
    };
    let actions = update(&mut model, Message::PlaybackEvent(event));
    // An unavailable track is skipped, not treated as a Premium failure.
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(1))));
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
    set_current(&mut model, "t");
    let unavailable = || {
        Message::PlaybackEvent(PlaybackEvent::Unavailable {
            track: TrackId("t".to_owned()),
        })
    };
    // First failure could be a single region-locked track: skip it (with no
    // queue loaded the skip itself is a no-op, but the streak is recorded).
    assert!(update(&mut model, unavailable()).is_empty());
    // A second failure in a row means the session is dead: reconnect instead of
    // skipping through the whole queue.
    assert_eq!(
        update(&mut model, unavailable()),
        vec![Action::PlayerReconnect]
    );
    // Further failures while a reconnect is in flight skip without re-triggering.
    assert!(!update(&mut model, unavailable()).contains(&Action::PlayerReconnect));
}

#[test]
fn successful_play_resets_the_unavailable_streak() {
    let mut model = Model::new();
    model.playback_queue = vec![track("t", "Unavailable"), track("ok", "Ok")];
    model.playback_queue_cursor = Some(0);
    set_current(&mut model, "t");
    let unavailable = || {
        Message::PlaybackEvent(PlaybackEvent::Unavailable {
            track: TrackId("t".to_owned()),
        })
    };
    let actions = update(&mut model, unavailable());
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(1))));
    assert_eq!(model.now_playing_track, Some(TrackId("ok".to_owned())));
    update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::Playing {
            track: TrackId("ok".to_owned()),
            position_ms: 0,
        }),
    );
    // After a track plays, the next failure starts over at a skip, not reconnect.
    set_current(&mut model, "t");
    let actions = update(&mut model, unavailable());
    assert!(!actions.contains(&Action::PlayerReconnect));
    assert_eq!(
        model.playback_health,
        spot_defy::state::PlaybackHealth::Skipping(1)
    );
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
    set_current(&mut model, "t");
    let unavailable = || {
        Message::PlaybackEvent(PlaybackEvent::Unavailable {
            track: TrackId("t".to_owned()),
        })
    };
    // Drive into the post-reconnect state.
    assert!(!update(&mut model, unavailable()).contains(&Action::PlayerReconnect));
    assert_eq!(
        update(&mut model, unavailable()),
        vec![Action::PlayerReconnect]
    );
    // Post-reconnect failures skip a bounded number, then STOP rather than
    // skip-storming the rest of the queue track-by-track.
    assert!(!update(&mut model, unavailable()).contains(&Action::PlayerReconnect));
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
fn playing_event_for_preloaded_next_updates_metadata_art_and_highlight() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::Saved;
    let mut next = track("t2", "Next");
    next.album_art_images = vec![AlbumArtImage {
        url: "https://example.invalid/cover.jpg".to_owned(),
        width: Some(300),
        height: Some(300),
    }];
    model.tracks = vec![track("t1", "Current"), next.clone()];
    model.playback_queue = model.tracks.clone();
    model.playback_queue_cursor = Some(0);
    model.now_playing_track = Some(TrackId("t1".to_owned()));
    model.now_playing_metadata_track = Some(TrackId("t1".to_owned()));
    model.now_playing.track = Some("Current".to_owned());
    model.list_state.select(Some(0));

    let end_actions = update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::EndOfTrack {
            track: TrackId("t1".to_owned()),
        }),
    );
    assert_loading_actions(&end_actions, "Next", &next.album_art_images);
    assert!(matches!(end_actions[2], Action::PlayerPlayIndex(1)));
    assert_eq!(model.now_playing_track, Some(TrackId("t2".to_owned())));

    let actions = update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::Playing {
            track: TrackId("t2".to_owned()),
            position_ms: 250,
        }),
    );

    assert_eq!(model.now_playing.track.as_deref(), Some("Next"));
    assert_now_playing_identity(&model, "t2");
    assert_eq!(model.now_playing.position_ms, 250);
    assert_eq!(model.list_state.selected(), Some(1));
    assert_eq!(model.playback_queue_cursor, Some(1));
    assert!(
        matches!(actions.as_slice(), [Action::PublishNowPlaying(snapshot)] if snapshot.state == PlaybackState::Playing)
    );
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
    set_current(&mut model, "t1");
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
        ..SearchResultset::default()
    });
    update(&mut model, Message::SearchResults(result));
    assert_eq!(model.search_results.albums.len(), 1);
    assert_eq!(model.search_results.artists.len(), 1);
    assert_eq!(model.search_results.playlists.len(), 1);
}

#[test]
fn partially_failed_search_surfaces_the_failed_lanes() {
    let mut model = Model::new();
    // Tracks succeeded, playlists lane failed upstream: results still render,
    // but the failure is named in the status bar rather than swallowed.
    let result = Ok(SearchResultset {
        tracks: vec![track("t1", "A")],
        failed_lanes: vec!["Playlists"],
        ..SearchResultset::default()
    });
    update(&mut model, Message::SearchResults(result));
    assert_eq!(model.search_results.tracks.len(), 1);
    let status = model.status.expect("partial failure must set a status");
    assert!(status.contains("Playlists"), "status names the failed lane");
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
    assert_loading_actions(&actions, "Song", &[]);
    assert_player_load(&actions[2], &["t1"], 0);
    assert_now_playing_identity(&model, "t1");
    assert_eq!(model.playback_queue, vec![track("t1", "Song")]);
}

#[test]
fn stale_end_of_track_after_manual_selection_is_ignored() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::Saved;
    model.tracks = vec![
        track("old", "Old"),
        track("selected", "Selected"),
        track("next", "Next"),
    ];
    model.now_playing_track = Some(TrackId("old".to_owned()));
    model.list_state.select(Some(1));

    let actions = update(&mut model, Message::ActivateSelection);

    assert_loading_actions(&actions, "Selected", &[]);
    assert_player_load(&actions[2], &["old", "selected", "next"], 1);
    assert_now_playing_identity(&model, "selected");

    let stale_end = Message::PlaybackEvent(PlaybackEvent::EndOfTrack {
        track: TrackId("old".to_owned()),
    });
    assert!(update(&mut model, stale_end).is_empty());
}

#[test]
fn stale_playing_after_manual_selection_is_ignored() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::Saved;
    model.tracks = vec![
        track("old", "Old"),
        track("selected", "Selected"),
        track("next", "Next"),
    ];
    model.now_playing_track = Some(TrackId("old".to_owned()));
    model.list_state.select(Some(1));

    let actions = update(&mut model, Message::ActivateSelection);
    assert!(matches!(
        actions.as_slice(),
        [
            Action::LoadAlbumArt(_),
            Action::PublishNowPlaying(_),
            Action::PlayerLoad { index: 1, .. }
        ]
    ));

    let stale_playing = Message::PlaybackEvent(PlaybackEvent::Playing {
        track: TrackId("old".to_owned()),
        position_ms: 119_000,
    });
    assert!(update(&mut model, stale_playing).is_empty());
    assert_now_playing_identity(&model, "selected");
    assert_eq!(model.list_state.selected(), Some(1));
}

#[test]
fn playing_event_for_manual_selection_updates_stale_metadata_without_loading() {
    let mut model = Model::new();
    model.screen = Screen::Library;
    model.library_tab = LibraryTab::Saved;
    let mut selected = track("selected", "Selected");
    selected.album_art_images = vec![AlbumArtImage {
        url: "https://example.invalid/selected.jpg".to_owned(),
        width: Some(300),
        height: Some(300),
    }];
    model.tracks = vec![track("old", "Old"), selected.clone(), track("next", "Next")];
    model.now_playing_track = Some(TrackId("old".to_owned()));
    model.now_playing_metadata_track = Some(TrackId("old".to_owned()));
    model.now_playing.track = Some("Old".to_owned());
    model.list_state.select(Some(1));

    let actions = update(&mut model, Message::ActivateSelection);
    assert_loading_actions(&actions, "Selected", &selected.album_art_images);
    assert_player_load(&actions[2], &["old", "selected", "next"], 1);
    assert_eq!(model.now_playing.track.as_deref(), Some("Selected"));
    assert_now_playing_identity(&model, "selected");

    let actions = update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::Playing {
            track: TrackId("selected".to_owned()),
            position_ms: 100,
        }),
    );

    assert_eq!(model.now_playing.track.as_deref(), Some("Selected"));
    assert_now_playing_identity(&model, "selected");
    assert_eq!(model.list_state.selected(), Some(1));
    assert!(
        matches!(actions.as_slice(), [Action::PublishNowPlaying(snapshot)] if snapshot.state == PlaybackState::Playing)
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

#[test]
fn reentering_playlists_reuses_fresh_data() {
    use spot_defy::state::LoadPhase;
    let mut model = Model::new();
    // First entry requests a load.
    let actions = update(&mut model, Message::EnterScreen(Screen::Playlists));
    assert_eq!(actions, vec![Action::LoadPlaylists]);
    // While the request is in flight, re-entering does not stack another.
    assert!(update(&mut model, Message::EnterScreen(Screen::Playlists)).is_empty());
    // Once the response lands, re-entry within the TTL reuses it.
    let result = Ok(vec![playlist("p1", "Mix")]);
    update(&mut model, Message::PlaylistsLoaded(result));
    assert!(matches!(model.playlists_phase, LoadPhase::Loaded(_)));
    assert!(update(&mut model, Message::EnterScreen(Screen::Playlists)).is_empty());
}

#[test]
fn failed_playlist_load_retries_on_reentry() {
    use spot_defy::error::ApiError;
    use spot_defy::state::LoadPhase;
    let mut model = Model::new();
    update(&mut model, Message::EnterScreen(Screen::Playlists));
    let result = Err(ApiError::Request("network down".to_owned()));
    update(&mut model, Message::PlaylistsLoaded(result));
    assert_eq!(model.playlists_phase, LoadPhase::Failed);
    // A failed load is not fresh: entering the screen again retries.
    let actions = update(&mut model, Message::EnterScreen(Screen::Playlists));
    assert_eq!(actions, vec![Action::LoadPlaylists]);
}

#[test]
fn drilling_into_a_playlist_shows_loading_not_stale_tracks() {
    use spot_defy::state::LoadPhase;
    let mut model = Model::new();
    model.screen = Screen::Playlists;
    model.playlists = vec![playlist("p1", "Mix")];
    model.tracks = vec![track("old", "Left over from another list")];
    model.list_state.select(Some(0));

    let actions = update(&mut model, Message::ActivateSelection);

    assert_eq!(
        actions,
        vec![Action::LoadPlaylistTracks(PlaylistId("p1".to_owned()))]
    );
    assert!(
        model.tracks.is_empty(),
        "stale tracks must not show while the new list loads"
    );
    assert_eq!(model.tracks_phase, LoadPhase::Loading);
    assert_eq!(model.screen, Screen::Tracks);
}

#[test]
fn reopening_the_same_playlist_within_ttl_is_instant() {
    let mut model = Model::new();
    model.screen = Screen::Playlists;
    model.playlists = vec![playlist("p1", "Mix")];
    model.list_state.select(Some(0));
    update(&mut model, Message::ActivateSelection);
    update(
        &mut model,
        Message::TrackListLoaded {
            source: TrackListSource::Playlist(PlaylistId("p1".to_owned())),
            result: Ok(vec![track("t1", "Song")]),
        },
    );
    // Esc back out, then re-open the same playlist: served from cache.
    update(&mut model, Message::EnterScreen(Screen::Playlists));
    model.list_state.select(Some(0));
    let actions = update(&mut model, Message::ActivateSelection);
    assert!(
        actions.is_empty(),
        "fresh same-source drill-in needs no load"
    );
    assert_eq!(model.screen, Screen::Tracks);
    assert_eq!(model.tracks.len(), 1);
}

#[test]
fn track_response_landing_after_esc_is_kept_for_reentry() {
    let mut model = Model::new();
    model.screen = Screen::Playlists;
    model.playlists = vec![playlist("p1", "Mix"), playlist("p2", "Other")];
    model.list_state.select(Some(1));
    update(&mut model, Message::ActivateSelection);
    // The user bails back to Playlists before the tracks arrive.
    update(&mut model, Message::EnterScreen(Screen::Playlists));
    model.list_state.select(Some(0));
    update(
        &mut model,
        Message::TrackListLoaded {
            source: TrackListSource::Playlist(PlaylistId("p2".to_owned())),
            result: Ok(vec![track("t9", "Nine")]),
        },
    );
    // The late response must not clobber the Playlists cursor…
    assert_eq!(model.list_state.selected(), Some(0));
    // …but the data is cached, so re-opening that playlist is instant.
    model.list_state.select(Some(1));
    let actions = update(&mut model, Message::ActivateSelection);
    assert!(actions.is_empty());
    assert_eq!(model.tracks.len(), 1);
}

#[test]
fn shuffle_toggle_permutes_and_restores_the_queue() {
    let mut model = Model::new();
    let original: Vec<_> = (0..8)
        .map(|i| track(&format!("t{i}"), &format!("T{i}")))
        .collect();
    model.playback_queue = original.clone();
    model.playback_queue_cursor = Some(3);
    set_current(&mut model, "t3");

    let actions = update(&mut model, Message::ToggleShuffle);
    assert!(model.shuffle);
    // Same tracks, cursor still pointing at the playing track.
    assert_eq!(model.playback_queue.len(), original.len());
    for item in &original {
        assert!(model.playback_queue.contains(item));
    }
    let cursor = model.playback_queue_cursor.expect("cursor kept");
    assert_eq!(model.playback_queue[cursor].id.0, "t3");
    // The player's mirrored queue is synced to the new order.
    assert!(matches!(
        &actions[..],
        [Action::PlayerSetQueue { queue, cursor: Some(c) }]
            if queue.len() == original.len() && queue[*c].0 == "t3"
    ));

    // Toggling off restores the original order exactly.
    update(&mut model, Message::ToggleShuffle);
    assert!(!model.shuffle);
    assert_eq!(model.playback_queue, original);
    assert_eq!(model.playback_queue_cursor, Some(3));
}

#[test]
fn shuffle_with_no_queue_is_a_noop_with_status() {
    let mut model = Model::new();
    let actions = update(&mut model, Message::ToggleShuffle);
    assert!(actions.is_empty());
    assert!(!model.shuffle);
    assert!(model.status.is_some());
}

#[test]
fn toggle_like_targets_the_now_playing_track() {
    let mut model = Model::new();
    set_current(&mut model, "t1");
    let actions = update(&mut model, Message::ToggleLike);
    assert_eq!(actions, vec![Action::ToggleSaved(TrackId("t1".to_owned()))]);

    // Without a playing track it explains itself instead of failing silently.
    let mut idle = Model::new();
    assert!(update(&mut idle, Message::ToggleLike).is_empty());
    assert!(idle.status.is_some());
}

#[test]
fn saved_toggled_invalidates_the_saved_tab_cache() {
    use spot_defy::state::LoadPhase;
    use std::time::Instant;
    let mut model = Model::new();
    model.track_list_source = Some(TrackListSource::SavedTracks);
    model.tracks_phase = LoadPhase::Loaded(Instant::now());

    update(&mut model, Message::SavedToggled(Ok(true)));

    assert!(model.status.expect("status set").contains("saved"));
    assert_eq!(
        model.tracks_phase,
        LoadPhase::Idle,
        "the Saved tab must refetch after a like/unlike"
    );
}

#[test]
fn queue_selected_appends_and_syncs_the_player() {
    let mut model = Model::new();
    model.screen = Screen::Tracks;
    model.tracks = vec![track("t1", "A"), track("t9", "New")];
    model.playback_queue = vec![track("t1", "A")];
    model.playback_queue_cursor = Some(0);
    model.list_state.select(Some(1));

    let actions = update(&mut model, Message::QueueSelected);

    assert_eq!(model.playback_queue.len(), 2);
    assert_eq!(model.playback_queue[1].id.0, "t9");
    assert!(matches!(
        &actions[..],
        [Action::PlayerSetQueue { queue, .. }] if queue.len() == 2
    ));

    // With nothing playing, queueing explains itself and does nothing.
    let mut idle = Model::new();
    idle.screen = Screen::Tracks;
    idle.tracks = vec![track("t1", "A")];
    idle.list_state.select(Some(0));
    assert!(update(&mut idle, Message::QueueSelected).is_empty());
    assert!(idle.status.is_some());
}

#[test]
fn remove_queued_adjusts_cursor_and_protects_the_playing_row() {
    let mut model = Model::new();
    model.screen = Screen::Queue;
    model.playback_queue = vec![track("t1", "A"), track("t2", "B"), track("t3", "C")];
    model.playback_queue_cursor = Some(1);
    set_current(&mut model, "t2");

    // Removing a row before the cursor shifts the cursor left.
    model.list_state.select(Some(0));
    let actions = update(&mut model, Message::RemoveQueued);
    assert_eq!(model.playback_queue.len(), 2);
    assert_eq!(model.playback_queue_cursor, Some(0));
    assert!(matches!(&actions[..], [Action::PlayerSetQueue { .. }]));

    // The playing row itself refuses to be removed.
    model.list_state.select(Some(0));
    assert!(update(&mut model, Message::RemoveQueued).is_empty());
    assert_eq!(model.playback_queue.len(), 2);
}

#[test]
fn queue_screen_enter_jumps_to_the_selected_entry() {
    let mut model = Model::new();
    model.screen = Screen::Queue;
    model.playback_queue = vec![track("t1", "A"), track("t2", "B"), track("t3", "C")];
    model.playback_queue_cursor = Some(0);
    set_current(&mut model, "t1");
    model.list_state.select(Some(2));

    let actions = update(&mut model, Message::ActivateSelection);

    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(2))));
    assert_eq!(model.playback_queue_cursor, Some(2));
    assert_eq!(model.now_playing_track, Some(TrackId("t3".to_owned())));
}

#[test]
fn duplicate_track_ids_keep_the_explicit_cursor_instance() {
    // Queue [A, B, A]: jumping to the SECOND A must not snap the cursor back
    // to the first A (that caused an infinite A→B loop with repeat off).
    let mut model = Model::new();
    model.screen = Screen::Queue;
    model.playback_queue = vec![track("a", "A"), track("b", "B"), track("a", "A")];
    model.playback_queue_cursor = Some(0);
    set_current(&mut model, "a");
    model.list_state.select(Some(2));

    let actions = update(&mut model, Message::ActivateSelection);
    assert!(matches!(actions.last(), Some(Action::PlayerPlayIndex(2))));
    assert_eq!(
        model.playback_queue_cursor,
        Some(2),
        "the cursor must stay on the jumped-to instance"
    );

    // The second A ending must end the queue (repeat off), not replay B.
    let actions = update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::EndOfTrack {
            track: TrackId("a".to_owned()),
        }),
    );
    assert_eq!(actions, vec![Action::PlayerStop]);
}

#[test]
fn shuffle_can_always_be_turned_off() {
    // Shuffle on with a queue, then the queue empties (e.g. removed row by
    // row after playback stopped): 's' must still disable shuffle.
    let mut model = Model::new();
    model.playback_queue = vec![track("t1", "A")];
    update(&mut model, Message::ToggleShuffle);
    assert!(model.shuffle);
    model.playback_queue.clear();
    model.shuffle_order = Some(Vec::new());

    update(&mut model, Message::ToggleShuffle);
    assert!(!model.shuffle, "shuffle must never get stuck on");
}

#[test]
fn unshuffle_restores_exact_order_after_shuffled_edits() {
    // Craft a known permutation: shuffled [C, B, A] came from original
    // [A, B, C] via order [2, 1, 0]. Removing the shuffled row 0 (original C)
    // then unshuffling must yield [A, B] — instance-exact, not id-guessed.
    let mut model = Model::new();
    model.screen = Screen::Queue;
    model.playback_queue = vec![track("c", "C"), track("b", "B"), track("a", "A")];
    model.shuffle = true;
    model.shuffle_order = Some(vec![2, 1, 0]);
    model.playback_queue_cursor = Some(1); // playing B
    set_current(&mut model, "b");

    model.list_state.select(Some(0));
    update(&mut model, Message::RemoveQueued);
    assert_eq!(model.playback_queue.len(), 2);

    update(&mut model, Message::ToggleShuffle);
    assert!(!model.shuffle);
    let ids: Vec<&str> = model
        .playback_queue
        .iter()
        .map(|t| t.id.0.as_str())
        .collect();
    assert_eq!(ids, vec!["a", "b"], "original order minus the removed row");
    // The cursor followed the playing instance to its original position.
    assert_eq!(model.playback_queue_cursor, Some(1));
}

#[test]
fn unavailable_single_track_repeat_all_stops_without_reconnect() {
    use spot_defy::state::{PlaybackHealth, RepeatMode};
    let mut model = Model::new();
    model.playback_queue = vec![track("dead", "Dead")];
    model.playback_queue_cursor = Some(0);
    model.repeat = RepeatMode::All;
    set_current(&mut model, "dead");

    let actions = update(
        &mut model,
        Message::PlaybackEvent(PlaybackEvent::Unavailable {
            track: TrackId("dead".to_owned()),
        }),
    );

    // Wrapping the skip back onto the same dead track must stop playback, not
    // replay it or escalate into a full session reconnect.
    assert_eq!(actions, vec![Action::PlayerStop]);
    assert_eq!(model.playback_health, PlaybackHealth::Healthy);
    assert_eq!(model.now_playing.state, PlaybackState::Stopped);
}

#[test]
fn like_toggle_is_single_flight() {
    let mut model = Model::new();
    set_current(&mut model, "t1");
    let first = update(&mut model, Message::ToggleLike);
    assert_eq!(first, vec![Action::ToggleSaved(TrackId("t1".to_owned()))]);
    // A second press while the round trip is in flight must not race it.
    let second = update(&mut model, Message::ToggleLike);
    assert!(second.is_empty());
    // The reply re-arms the toggle.
    update(&mut model, Message::SavedToggled(Ok(true)));
    let third = update(&mut model, Message::ToggleLike);
    assert_eq!(third, vec![Action::ToggleSaved(TrackId("t1".to_owned()))]);
}
