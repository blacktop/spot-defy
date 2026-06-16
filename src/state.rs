//! The TEA `Model`: all UI state, including widget state carried across frames.
//!
//! `ListState` lives here (constructed once), never rebuilt in `crate::view`.
//! Screens and input modes are enums, not boolean flags.

use crate::api::SearchResultset;
use crate::config::{Config, Keybindings, ThemeColors};
use crate::model::{
    AlbumItem, ArtistItem, PlaybackSnapshot, PlaylistItem, TrackId, TrackItem, TrackListSource,
};
use ratatui::widgets::ListState;
use std::time::Instant;

/// Top-level screen the user is currently viewing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    /// Search across tracks/albums/artists/playlists.
    #[default]
    Search,
    /// The user's playlists.
    Playlists,
    /// Tracks of the selected playlist.
    Tracks,
    /// Library / top-items discovery.
    Library,
}

/// Input mode (vim-style): `Normal` for navigation, `Insert` for text entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Key presses navigate and trigger actions.
    #[default]
    Normal,
    /// Key presses edit the search query.
    Insert,
}

/// Health of the streaming playback session.
///
/// Distinguishes a single unplayable track (skip it) from a dead session where
/// every track fails (reconnect once, instead of skipping the whole queue).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackHealth {
    /// The last load succeeded, or none has been attempted: the normal state.
    #[default]
    Healthy,
    /// `n` consecutive tracks were unavailable, still below the reconnect
    /// threshold, so keep skipping.
    Skipping(u32),
    /// A reconnect has been requested; `n` counts failures skipped since, bounded
    /// so a fully-unplayable queue stops rather than skip-storming to the end.
    Reconnecting(u32),
}

/// Sub-views of the Library/discovery screen, cycled with Tab/Shift-Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibraryTab {
    /// The user's top tracks (`/me/top/tracks`).
    #[default]
    TopTracks,
    /// The user's saved albums (`/me/albums`).
    Albums,
    /// The user's top artists (`/me/top/artists`).
    TopArtists,
    /// Recently played tracks.
    RecentlyPlayed,
    /// Saved (liked) tracks.
    Saved,
}

impl LibraryTab {
    /// All tabs in display/cycle order.
    pub const ALL: [LibraryTab; 5] = [
        LibraryTab::TopTracks,
        LibraryTab::Albums,
        LibraryTab::TopArtists,
        LibraryTab::RecentlyPlayed,
        LibraryTab::Saved,
    ];

    /// Short label for the tab bar.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            LibraryTab::TopTracks => "Top Tracks",
            LibraryTab::Albums => "Albums",
            LibraryTab::TopArtists => "Top Artists",
            LibraryTab::RecentlyPlayed => "Recently Played",
            LibraryTab::Saved => "Saved",
        }
    }

    /// The next (`forward`) or previous tab, wrapping around.
    #[must_use]
    pub fn cycled(self, forward: bool) -> LibraryTab {
        cycle(&Self::ALL, self, forward)
    }
}

/// Result lanes of the Search screen, cycled with Tab/Shift-Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchTab {
    /// Track results (the primary lane).
    #[default]
    Tracks,
    /// Album results.
    Albums,
    /// Artist results.
    Artists,
    /// Playlist results.
    Playlists,
}

impl SearchTab {
    /// All lanes in display/cycle order.
    pub const ALL: [SearchTab; 4] = [
        SearchTab::Tracks,
        SearchTab::Albums,
        SearchTab::Artists,
        SearchTab::Playlists,
    ];

    /// Short label for the lane tab bar.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SearchTab::Tracks => "Tracks",
            SearchTab::Albums => "Albums",
            SearchTab::Artists => "Artists",
            SearchTab::Playlists => "Playlists",
        }
    }

    /// The next (`forward`) or previous lane, wrapping around.
    #[must_use]
    pub fn cycled(self, forward: bool) -> SearchTab {
        cycle(&Self::ALL, self, forward)
    }
}

/// Step to the next/previous variant in `all`, wrapping around.
fn cycle<T: Copy + PartialEq>(all: &[T], current: T, forward: bool) -> T {
    let len = all.len();
    let index = all.iter().position(|item| *item == current).unwrap_or(0);
    let next = if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    };
    all.get(next).copied().unwrap_or(current)
}

/// Search input buffer and debounce bookkeeping.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// Current query text.
    pub query: String,
    /// Cursor byte offset within `query`.
    pub cursor: usize,
    /// When the last character was entered (for debounce).
    pub last_input: Instant,
    /// Whether a debounced search is pending dispatch.
    pub pending: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            last_input: Instant::now(),
            pending: false,
        }
    }
}

/// The complete UI model.
#[derive(Default)]
pub struct Model {
    /// User-configured normal-mode keybindings.
    pub keybindings: Keybindings,
    /// User-configured renderer colors.
    pub theme: ThemeColors,
    /// Active screen.
    pub screen: Screen,
    /// Active input mode.
    pub mode: Mode,
    /// Search buffer and debounce state.
    pub search: SearchState,
    /// Active result lane on the Search screen.
    pub search_tab: SearchTab,
    /// Active sub-tab on the Library screen.
    pub library_tab: LibraryTab,
    /// Selection state for the active list.
    pub list_state: ListState,
    /// Loaded playlists (Playlists screen).
    pub playlists: Vec<PlaylistItem>,
    /// Loaded tracks (playlist tracks or the track-based Library tabs).
    pub tracks: Vec<TrackItem>,
    /// Source that most recently requested ownership of `tracks`.
    pub track_list_source: Option<TrackListSource>,
    /// Loaded artists (the Library "Top Artists" tab).
    pub artists: Vec<ArtistItem>,
    /// Loaded saved albums (the Library "Albums" tab).
    pub albums: Vec<AlbumItem>,
    /// The latest multi-lane search results (Search screen).
    pub search_results: SearchResultset,
    /// Latest now-playing snapshot rendered in the footer.
    pub now_playing: PlaybackSnapshot,
    /// Streaming-session health, tracking consecutive unavailable tracks so a
    /// dead session reconnects instead of skipping the whole queue.
    pub playback_health: PlaybackHealth,
    /// Transient error/status message shown in the footer, if any.
    pub status: Option<String>,
    /// Deadline for the transient volume popup after a user volume change.
    pub volume_overlay_until: Option<Instant>,
    /// Set when the event loop should exit.
    pub should_quit: bool,
}

impl Model {
    /// Create a fresh model with default state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a fresh model using the loaded user config.
    ///
    /// # Errors
    ///
    /// Returns an error if keybindings conflict or configured theme color names
    /// are invalid.
    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self {
            keybindings: config.keybindings.clone(),
            theme: config.theme.colors()?,
            ..Self::default()
        })
    }

    /// Record a transient status/error message for the footer.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
    }

    /// Clear any transient status/error message.
    pub fn clear_status(&mut self) {
        self.status = None;
    }

    /// Reset the selection cursor when the active list contents change.
    ///
    /// Selects the first row when the visible list is non-empty, otherwise
    /// clears the selection so the highlight does not point past the end.
    pub fn reset_selection(&mut self) {
        let selected = (self.active_len() > 0).then_some(0);
        self.list_state.select(selected);
    }

    /// Number of rows in the list currently visible for the active screen/tab.
    #[must_use]
    pub fn active_len(&self) -> usize {
        match self.screen {
            Screen::Playlists => self.playlists.len(),
            Screen::Tracks => self.tracks.len(),
            Screen::Library => match self.library_tab {
                LibraryTab::Albums => self.albums.len(),
                LibraryTab::TopArtists => self.artists.len(),
                LibraryTab::TopTracks | LibraryTab::RecentlyPlayed | LibraryTab::Saved => {
                    self.tracks.len()
                }
            },
            Screen::Search => match self.search_tab {
                SearchTab::Tracks => self.search_results.tracks.len(),
                SearchTab::Albums => self.search_results.albums.len(),
                SearchTab::Artists => self.search_results.artists.len(),
                SearchTab::Playlists => self.search_results.playlists.len(),
            },
        }
    }

    /// Find a loaded track by id across the playlist/library and search lanes.
    ///
    /// Playback can be started from either the track lists or the search
    /// results, so now-playing metadata is resolved from both.
    #[must_use]
    pub fn find_track(&self, id: &TrackId) -> Option<&TrackItem> {
        self.tracks
            .iter()
            .chain(self.search_results.tracks.iter())
            .find(|track| &track.id == id)
    }
}
