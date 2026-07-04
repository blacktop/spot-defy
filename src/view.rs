//! Render the `Model` with ratatui.
//!
//! Layout is header / body / now-playing footer / help bar, split vertically;
//! the now-playing footer and help bar are drawn on every screen. The Library
//! and Search screens add a sub-tab bar above their list. Widget state
//! (`ListState`/`TableState`) is borrowed from the model, never reconstructed
//! here. Spotify green is the primary accent; the now-playing progress bar is
//! drawn by hand so text over the fill flips to a dark, readable color.

use crate::config::{SPOTIFY_BLACK, ThemeColors};
use crate::model::{AlbumItem, ArtistItem, PlaybackState, PlaylistItem, TrackItem};
use crate::state::{LibraryTab, LoadPhase, Mode, Model, RepeatMode, Screen, SearchTab};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Gauge, HighlightSpacing, List, ListItem, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState,
};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FontSize, Resize, StatefulImage};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Minimum body width before the right-third artwork column appears.
const ARTWORK_MIN_TOTAL: u16 = 96;

/// Small adaptive viewport gutter so bordered UI never touches terminal edges.
const OUTER_MARGIN_HORIZONTAL: u16 = 1;
const OUTER_MARGIN_VERTICAL: u16 = 1;

/// Transient volume popup geometry.
const VOLUME_POPUP_WIDTH: u16 = 34;
const VOLUME_POPUP_HEIGHT: u16 = 5;
const VOLUME_FADE_WINDOW: Duration = Duration::from_millis(400);

/// The selection highlight: solid green bar with black text.
fn highlight_style(theme: ThemeColors) -> Style {
    Style::new().fg(SPOTIFY_BLACK).bg(theme.accent).bold()
}

/// The active-tab "pill" style: black on green.
fn active_tab_style(theme: ThemeColors) -> Style {
    Style::new().fg(SPOTIFY_BLACK).bg(theme.accent).bold()
}

fn dim_style(theme: ThemeColors) -> Style {
    Style::new().fg(theme.dim)
}

/// A bordered block with rounded corners, a dim border, and a green title.
fn panel(theme: ThemeColors, title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(SPOTIFY_BLACK))
        .border_style(dim_style(theme))
        .title(title.to_owned().fg(theme.accent).bold())
}

/// Render the entire UI for the current frame.
pub fn view(
    model: &mut Model,
    art: &mut Option<StatefulProtocol>,
    art_area: &mut Option<Rect>,
    font_size: Option<FontSize>,
    frame: &mut Frame,
) {
    *art_area = None;
    let frame_area = frame.area();
    frame.render_widget(
        Block::default().style(Style::new().bg(SPOTIFY_BLACK)),
        frame_area,
    );
    let app_area = content_area(frame_area);
    let [header, body, footer, help] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(app_area);

    render_header(model, frame, header);

    // On a wide-enough terminal, reserve the right third for album artwork.
    let (main, sidebar) = if body.width >= ARTWORK_MIN_TOTAL {
        let [main, _, side] = Layout::horizontal([
            Constraint::Fill(2),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);
        (main, Some(side))
    } else {
        (body, None)
    };

    match model.screen {
        Screen::Search => render_search(model, frame, main),
        Screen::Playlists => render_playlists(model, frame, main),
        Screen::Tracks => render_tracks(model, frame, main),
        Screen::Library => render_library(model, frame, main),
        Screen::Queue => render_queue(model, frame, main),
    }
    if let Some(sidebar) = sidebar {
        render_sidebar(model, art, art_area, font_size, frame, sidebar);
    }
    render_footer(model, frame, footer);
    render_help(model, frame, help);
    render_volume_overlay(model, frame, app_area);
}

/// Apply a tiny adaptive margin around the whole app surface.
fn content_area(area: Rect) -> Rect {
    let horizontal = if area.width > 40 {
        OUTER_MARGIN_HORIZONTAL
    } else {
        0
    };
    let vertical = if area.height > 16 {
        OUTER_MARGIN_VERTICAL
    } else {
        0
    };
    area.inner(Margin {
        horizontal,
        vertical,
    })
}

/// Render the now-playing sidebar: album art (or a placeholder) under a panel
/// titled with the current track name.
fn render_sidebar(
    model: &Model,
    art: &mut Option<StatefulProtocol>,
    art_area: &mut Option<Rect>,
    font_size: Option<FontSize>,
    frame: &mut Frame,
    area: Rect,
) {
    let title = model.now_playing.track.as_deref().unwrap_or("Now Playing");
    let block = panel(model.theme, title);
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let image_area = centered_album_art_area(inner, font_size);
    *art_area = Some(image_area);
    if image_area.width == 0 || image_area.height == 0 {
        return;
    }
    if let Some(protocol) = art {
        let image = StatefulImage::new().resize(Resize::Scale(None));
        frame.render_stateful_widget(image, image_area, protocol);
    } else {
        let placeholder = if model.now_playing.track.is_some() {
            "♪"
        } else {
            "no track playing"
        };
        frame.render_widget(
            Paragraph::new(Span::from(placeholder).style(dim_style(model.theme))).centered(),
            image_area,
        );
    }
}

/// Center a square-in-pixels image area inside a terminal-cell rectangle.
fn centered_album_art_area(area: Rect, font_size: Option<FontSize>) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }
    let font_size = font_size.unwrap_or_else(|| FontSize::new(1, 2));
    let cell_width = u32::from(font_size.width.max(1));
    let cell_height = u32::from(font_size.height.max(1));
    let width_px = u32::from(area.width) * cell_width;
    let height_px = u32::from(area.height) * cell_height;
    let side_px = width_px.min(height_px);
    if side_px == 0 {
        return area;
    }

    let width = cells_for_pixels(side_px, cell_width, area.width);
    let height = cells_for_pixels(side_px, cell_height, area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Convert source pixels to enough terminal cells, capped to the available pane.
fn cells_for_pixels(pixels: u32, cell_pixels: u32, max_cells: u16) -> u16 {
    let cells = pixels.div_ceil(cell_pixels).max(1);
    u16::try_from(cells).unwrap_or(max_cells).min(max_cells)
}

/// Draw the title/tab header with the active screen highlighted.
fn render_header(model: &Model, frame: &mut Frame, area: Rect) {
    let mut spans = vec![
        Span::from(" Spot[DEFY] ").style(active_tab_style(model.theme)),
        Span::from("  "),
    ];
    for (screen, label) in [
        (Screen::Search, "1 Search"),
        (Screen::Playlists, "2 Playlists"),
        (Screen::Library, "3 Library"),
        (Screen::Queue, "4 Queue"),
    ] {
        let span = Span::from(format!(" {label} "));
        spans.push(if screen == active_tab(model) {
            span.style(active_tab_style(model.theme))
        } else {
            span.style(dim_style(model.theme))
        });
        spans.push(Span::from(" "));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(SPOTIFY_BLACK))
        .border_style(Style::new().fg(model.theme.accent));
    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

/// Map the active screen onto its owning top-level tab. A drilled-in track
/// list highlights the tab it was entered from, not always Playlists.
fn active_tab(model: &Model) -> Screen {
    match model.screen {
        Screen::Tracks => match model.tracks_origin {
            Screen::Search => Screen::Search,
            Screen::Library => Screen::Library,
            Screen::Playlists | Screen::Tracks | Screen::Queue => Screen::Playlists,
        },
        Screen::Search | Screen::Playlists | Screen::Library | Screen::Queue => model.screen,
    }
}

/// Render the search screen: input box, lane tab bar, then the active lane.
fn render_search(model: &mut Model, frame: &mut Frame, area: Rect) {
    let [input, tabs, results] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);

    let title = if model.mode == Mode::Insert {
        "Search (typing)".to_owned()
    } else {
        format!(
            "Search (press {} to edit)",
            key_label(model.keybindings.search)
        )
    };
    // Measure by display width (CJK/emoji are two cells). When the query
    // outgrows the box in insert mode, scroll the text so the cursor stays
    // over the character actually being edited instead of pinning blind at
    // the right edge.
    let inner_width = usize::from(input.width.saturating_sub(2));
    let cursor_width = UnicodeWidthStr::width(&model.search.query[..model.search.cursor]);
    let scroll = if model.mode == Mode::Insert {
        cursor_width.saturating_sub(inner_width.saturating_sub(1))
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(model.search.query.as_str())
            .scroll((0, u16::try_from(scroll).unwrap_or(u16::MAX)))
            .block(panel(model.theme, &title)),
        input,
    );

    if model.mode == Mode::Insert {
        let offset = u16::try_from(cursor_width - scroll).unwrap_or(u16::MAX);
        let cursor_x = input
            .x
            .saturating_add(1)
            .saturating_add(offset)
            .min(input.right().saturating_sub(2));
        frame.set_cursor_position((cursor_x, input.y + 1));
    }

    let labels = SearchTab::ALL.map(SearchTab::label);
    render_subtab_bar(
        frame,
        tabs,
        &labels,
        active_index(&SearchTab::ALL, model.search_tab),
        model.theme,
    );
    render_search_lane(model, frame, results);
}

/// Render the active search lane (tracks, albums, artists, or playlists).
fn render_search_lane(model: &mut Model, frame: &mut Frame, area: Rect) {
    let (title, items): (&str, Vec<ListItem<'static>>) = match model.search_tab {
        SearchTab::Tracks => (
            "Tracks",
            model
                .search_results
                .tracks
                .iter()
                .map(|track| track_row(track, model.theme))
                .collect(),
        ),
        SearchTab::Albums => (
            "Albums",
            model
                .search_results
                .albums
                .iter()
                .map(|album| album_row(album, model.theme))
                .collect(),
        ),
        SearchTab::Artists => (
            "Artists",
            model
                .search_results
                .artists
                .iter()
                .map(|artist| artist_row(artist, model.theme))
                .collect(),
        ),
        SearchTab::Playlists => (
            "Playlists",
            model
                .search_results
                .playlists
                .iter()
                .map(|playlist| playlist_row(playlist, model.theme))
                .collect(),
        ),
    };
    // A lane whose query failed must not masquerade as a genuine zero-match.
    let lane_failed = model.search_results.failed_lanes.contains(&title);
    let empty = if lane_failed {
        search_retry_hint(model)
    } else {
        match model.search_phase {
            LoadPhase::Idle => "No results yet. Press / to search.",
            LoadPhase::Loading => "Searching…",
            LoadPhase::Loaded(_) => "No results for this query.",
            LoadPhase::Failed => search_retry_hint(model),
        }
    };
    render_list(model, frame, area, title, items, empty);
}

/// Retry hint for a failed search, honest about which key actually retries.
fn search_retry_hint(model: &Model) -> &'static str {
    if model.keybindings.uses('r') {
        "Search failed — edit the query and press Enter."
    } else {
        "Search failed — press r to retry."
    }
}

/// Retry hint for a failed list load, honest about which key actually retries.
fn load_retry_hint(model: &Model) -> &'static str {
    if model.keybindings.uses('r') {
        "Couldn't load — leave and re-enter to retry."
    } else {
        "Couldn't load — press r to retry."
    }
}

/// Render the playlists screen.
fn render_playlists(model: &mut Model, frame: &mut Frame, area: Rect) {
    let items = model
        .playlists
        .iter()
        .map(|playlist| playlist_row(playlist, model.theme))
        .collect();
    let empty = match model.playlists_phase {
        LoadPhase::Idle | LoadPhase::Loading => "Loading playlists…",
        LoadPhase::Loaded(_) => "No playlists found.",
        LoadPhase::Failed => load_retry_hint(model),
    };
    render_list(model, frame, area, "Playlists", items, empty);
}

/// Render the drilled-in track-list screen (playlist or album).
fn render_tracks(model: &mut Model, frame: &mut Frame, area: Rect) {
    let items = model
        .tracks
        .iter()
        .map(|track| track_row(track, model.theme))
        .collect();
    let empty = match model.tracks_phase {
        LoadPhase::Idle | LoadPhase::Loading => "Loading tracks…",
        LoadPhase::Loaded(_) => "No tracks here.",
        LoadPhase::Failed => load_retry_hint(model),
    };
    render_list(model, frame, area, "Tracks", items, empty);
}

/// Render the library/discovery screen: sub-tab bar, then the active tab.
fn render_library(model: &mut Model, frame: &mut Frame, area: Rect) {
    let [tabs, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let labels = LibraryTab::ALL.map(LibraryTab::label);
    render_subtab_bar(
        frame,
        tabs,
        &labels,
        active_index(&LibraryTab::ALL, model.library_tab),
        model.theme,
    );

    let phase = match model.library_tab {
        LibraryTab::Albums => model.albums_phase,
        LibraryTab::TopArtists => model.artists_phase,
        LibraryTab::TopTracks | LibraryTab::RecentlyPlayed | LibraryTab::Saved => {
            model.tracks_phase
        }
    };
    let empty = library_empty_hint(model.library_tab, phase, load_retry_hint(model));
    let title = model.library_tab.label();
    match model.library_tab {
        LibraryTab::Albums => {
            let items = model
                .albums
                .iter()
                .map(|album| album_row(album, model.theme))
                .collect();
            render_list(model, frame, body, title, items, empty);
        }
        LibraryTab::TopArtists => {
            let items = model
                .artists
                .iter()
                .map(|artist| artist_row(artist, model.theme))
                .collect();
            render_list(model, frame, body, title, items, empty);
        }
        LibraryTab::TopTracks | LibraryTab::RecentlyPlayed | LibraryTab::Saved => {
            let items = model
                .tracks
                .iter()
                .map(|track| track_row(track, model.theme))
                .collect();
            render_list(model, frame, body, title, items, empty);
        }
    }
}

/// Render the live play queue, marking the now-playing row.
fn render_queue(model: &mut Model, frame: &mut Frame, area: Rect) {
    let cursor = model.playback_queue_cursor;
    let theme = model.theme;
    let items: Vec<ListItem<'static>> = model
        .playback_queue
        .iter()
        .enumerate()
        .map(|(index, track)| {
            if Some(index) == cursor {
                now_playing_row(track, theme)
            } else {
                track_row(track, theme)
            }
        })
        .collect();
    render_list(
        model,
        frame,
        area,
        "Queue",
        items,
        "Queue is empty — play a track (↵) or add one (a).",
    );
}

/// Format the queue row for the currently playing track: `♪` marker, accented.
fn now_playing_row(track: &TrackItem, theme: ThemeColors) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::from("♪ ").fg(theme.progress),
        Span::from(track.artist.clone()).fg(theme.accent).bold(),
        Span::from("  —  ").style(dim_style(theme)),
        Span::from(track.title.clone()).bold(),
        Span::from("   "),
        Span::from(fmt_ms(track.duration_ms)).fg(theme.progress),
    ]);
    ListItem::new(line)
}

/// Render a one-line sub-tab bar with the active tab highlighted.
fn render_subtab_bar(
    frame: &mut Frame,
    area: Rect,
    labels: &[&str],
    active: usize,
    theme: ThemeColors,
) {
    let mut spans = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        let span = Span::from(format!(" {label} "));
        spans.push(if index == active {
            span.style(active_tab_style(theme))
        } else {
            span.style(dim_style(theme))
        });
        spans.push(Span::from(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Render a selectable list, or an empty-state hint when there are no rows.
///
/// Non-empty lists get a `selected/total` counter in the bottom border and,
/// when the rows overflow the pane, a scrollbar on the right edge.
fn render_list(
    model: &mut Model,
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: Vec<ListItem<'static>>,
    empty: &str,
) {
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::from(empty.to_owned()).style(dim_style(model.theme)))
                .block(panel(model.theme, title)),
            area,
        );
        return;
    }
    let len = items.len();
    let selected = model.list_state.selected().unwrap_or(0).min(len - 1);
    let counter = Line::from(format!(" {}/{len} ", selected + 1))
        .right_aligned()
        .style(dim_style(model.theme));
    let list = List::new(items)
        .block(panel(model.theme, title).title_bottom(counter))
        .highlight_style(highlight_style(model.theme))
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut model.list_state);

    let viewport = usize::from(area.height.saturating_sub(2));
    if len > viewport && viewport > 0 {
        let mut scrollbar_state = ScrollbarState::new(len).position(selected);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight).style(dim_style(model.theme)),
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar_state,
        );
    }
}

/// The index of `current` within `all` (defaulting to the first tab).
fn active_index<T: Copy + PartialEq>(all: &[T], current: T) -> usize {
    all.iter().position(|item| *item == current).unwrap_or(0)
}

/// Format a single playlist row as `Name · N tracks · Owner`.
fn playlist_row(playlist: &PlaylistItem, theme: ThemeColors) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::from(playlist.name.clone()),
        Span::from(format!("  ·  {} tracks  ·  ", playlist.track_count)).style(dim_style(theme)),
        Span::from(playlist.owner.clone()).style(dim_style(theme)),
    ]);
    ListItem::new(line)
}

/// Format a single track row as `Artist — Title   M:SS`.
fn track_row(track: &TrackItem, theme: ThemeColors) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::from(track.artist.clone()).fg(theme.accent),
        Span::from("  —  ").style(dim_style(theme)),
        Span::from(track.title.clone()),
        Span::from("   "),
        Span::from(fmt_ms(track.duration_ms)).fg(theme.progress),
    ]);
    ListItem::new(line)
}

/// Format a single album row as `Artist — Album`.
fn album_row(album: &AlbumItem, theme: ThemeColors) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::from(album.artist.clone()).fg(theme.accent),
        Span::from("  —  ").style(dim_style(theme)),
        Span::from(album.name.clone()),
    ]);
    ListItem::new(line)
}

/// Format a single artist row.
fn artist_row(artist: &ArtistItem, theme: ThemeColors) -> ListItem<'static> {
    ListItem::new(Line::from(Span::from(artist.name.clone()).fg(theme.accent)))
}

/// Empty-state hint for a Library sub-tab, honest about its load phase.
/// `retry` is the caller-selected copy for the failed state (see
/// [`load_retry_hint`]).
fn library_empty_hint(tab: LibraryTab, phase: LoadPhase, retry: &'static str) -> &'static str {
    match phase {
        LoadPhase::Idle | LoadPhase::Loading => match tab {
            LibraryTab::TopTracks => "Loading your top tracks…",
            LibraryTab::Albums => "Loading your albums…",
            LibraryTab::TopArtists => "Loading your top artists…",
            LibraryTab::RecentlyPlayed => "Loading recently played…",
            LibraryTab::Saved => "Loading your saved tracks…",
        },
        LoadPhase::Loaded(_) => match tab {
            LibraryTab::TopTracks => "No top tracks yet.",
            LibraryTab::Albums => "No saved albums.",
            LibraryTab::TopArtists => "No top artists yet.",
            LibraryTab::RecentlyPlayed => "Nothing played recently.",
            LibraryTab::Saved => "No saved tracks.",
        },
        LoadPhase::Failed => retry,
    }
}

/// Render the persistent now-playing footer as a hand-drawn progress bar.
///
/// The track line is drawn first, then the filled portion of the bar is
/// overlaid directly on the buffer with a green background and black text, so
/// the title stays readable where it crosses the progress fill.
fn render_footer(model: &Model, frame: &mut Frame, area: Rect) {
    let np = &model.now_playing;
    let block = panel(model.theme, "Now Playing");
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let symbol = state_symbol(np.state);
    let track = np.track.as_deref().unwrap_or("—");
    let artist = np.artist.as_deref().unwrap_or("—");
    let left = format!("{symbol} {artist} — {track}");
    let modes = mode_indicators(model);
    let right = format!(
        "{modes}{} / {}",
        fmt_ms(np.position_ms),
        fmt_ms(np.duration_ms)
    );
    let line = now_playing_line(inner.width as usize, &left, &right);
    frame.render_widget(Paragraph::new(line), inner);

    let filled = progress_cells(inner.width, np.position_ms, np.duration_ms);
    let buf = frame.buffer_mut();
    for y in inner.top()..inner.bottom() {
        for x in inner.left()..inner.left().saturating_add(filled) {
            let cell = &mut buf[(x, y)];
            cell.set_bg(model.theme.progress);
            cell.set_fg(SPOTIFY_BLACK);
        }
    }
}

/// Render a short-lived volume popup after `+`/`-` input.
fn render_volume_overlay(model: &Model, frame: &mut Frame, area: Rect) {
    let Some(deadline) = model.volume_overlay_until else {
        return;
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining == Duration::ZERO || area.width < 24 || area.height < 8 {
        return;
    }

    let accent = if remaining <= VOLUME_FADE_WINDOW {
        model.theme.dim
    } else {
        model.theme.progress
    };
    let popup = volume_popup_area(area);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(SPOTIFY_BLACK))
        .border_style(Style::new().fg(accent))
        .title("Volume".fg(accent).bold());
    let inner = block.inner(popup).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height < 2 {
        return;
    }

    let [label, gauge_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    let label_line = Line::from(vec![
        Span::from("vol ").style(dim_style(model.theme)),
        Span::from(format!("{}%", model.now_playing.volume))
            .fg(accent)
            .bold(),
    ]);
    frame.render_widget(Paragraph::new(label_line), label);
    let gauge = Gauge::default()
        .percent(model.now_playing.volume.min(100))
        .gauge_style(Style::new().fg(accent).bg(model.theme.dim));
    frame.render_widget(gauge, gauge_area);
}

/// Bottom-right popup area, positioned above the persistent footer/help bars.
fn volume_popup_area(area: Rect) -> Rect {
    let width = VOLUME_POPUP_WIDTH.min(area.width);
    let height = VOLUME_POPUP_HEIGHT.min(area.height);
    let right_gap = u16::from(area.width > width + 1);
    let x = area.right().saturating_sub(width + right_gap);
    let y = area.bottom().saturating_sub(height + 5).max(area.y);
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Number of filled progress cells across `width` columns (integer math, so the
/// fill never truncates oddly and stays within `0..=width`).
fn progress_cells(width: u16, position_ms: u32, duration_ms: u32) -> u16 {
    if duration_ms == 0 {
        return 0;
    }
    let position = u64::from(position_ms.min(duration_ms));
    let filled = u64::from(width) * position / u64::from(duration_ms);
    u16::try_from(filled).unwrap_or(width).min(width)
}

/// Compose a fixed-`width` line with `left` label and `right` timing
/// right-aligned, padded between. Widths are terminal *display* widths (so
/// CJK/emoji titles align correctly), and both fields are truncated to fit.
fn now_playing_line(width: usize, left: &str, right: &str) -> String {
    if width == 0 {
        return String::new();
    }
    let right = truncate_to_width(right, width);
    let right_w = UnicodeWidthStr::width(right.as_str());
    let left_budget = width.saturating_sub(right_w).saturating_sub(1);
    let left = truncate_to_width(left, left_budget);
    let left_w = UnicodeWidthStr::width(left.as_str());
    let pad = width.saturating_sub(left_w + right_w);
    let mut out = String::with_capacity(width);
    out.push_str(&left);
    out.extend(std::iter::repeat_n(' ', pad));
    out.push_str(&right);
    out
}

/// Truncate `text` so its terminal display width is at most `max_width`.
fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > max_width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// Shuffle/repeat glyphs for the footer's right segment (empty when both off).
fn mode_indicators(model: &Model) -> &'static str {
    match (model.shuffle, model.repeat) {
        (false, RepeatMode::Off) => "",
        (false, RepeatMode::All) => "⟳ · ",
        (false, RepeatMode::One) => "⟳1 · ",
        (true, RepeatMode::Off) => "⇄ · ",
        (true, RepeatMode::All) => "⇄ ⟳ · ",
        (true, RepeatMode::One) => "⇄ ⟳1 · ",
    }
}

/// Glyph for a playback state.
fn state_symbol(state: PlaybackState) -> &'static str {
    match state {
        PlaybackState::Playing => "▶",
        PlaybackState::Paused => "⏸",
        PlaybackState::Loading => "…",
        PlaybackState::Stopped => "■",
    }
}

/// Render the bottom help/keybindings bar (or a transient status message).
fn render_help(model: &Model, frame: &mut Frame, area: Rect) {
    if let Some(status) = &model.status {
        let line = Line::from(format!(" {status} ")).black().on_red();
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let hint = match model.mode {
        Mode::Insert => " Enter submit · Esc cancel ".to_owned(),
        Mode::Normal => keys_for(model),
    };
    frame.render_widget(
        Paragraph::new(Line::from(hint).style(dim_style(model.theme))),
        area,
    );
}

/// Keybinding hint string for the active screen in navigation mode.
fn keys_for(model: &Model) -> String {
    let keys = &model.keybindings;
    let play = key_label(keys.play_pause);
    let skip = format!("{}/{}", key_label(keys.next), key_label(keys.previous));
    let quit = key_label(keys.quit);
    // Only advertise hardcoded keys while they are actually free.
    let refresh = if keys.uses('r') { "" } else { "r refresh · " };
    let add = if keys.uses('a') { "" } else { "a queue · " };
    let modes = match (keys.uses('s'), keys.uses('R')) {
        (false, false) => "s/R modes · ",
        (false, true) => "s shuffle · ",
        (true, false) => "R repeat · ",
        (true, true) => "",
    };
    match model.screen {
        Screen::Search => format!(
            "{} edit · Tab lane · ↵ play · {add}{refresh}{} pause · {skip} skip · -/+ vol · {quit} quit",
            key_label(keys.search),
            play,
        ),
        Screen::Playlists => format!(
            "↵ open · {}/{} move · {refresh}{} pause · {skip} skip · ←/→ seek · -/+ vol · {quit} quit",
            key_label(keys.down),
            key_label(keys.up),
            play,
        ),
        Screen::Tracks => format!(
            "↵ play · Esc back · {add}{refresh}{play} pause · {skip} skip · ←/→ seek · -/+ vol · {quit} quit",
        ),
        Screen::Library => format!(
            "Tab tab · ↵ play · {add}{refresh}{play} pause · {skip} skip · ←/→ seek · -/+ vol · {quit} quit",
        ),
        Screen::Queue => {
            let remove = if keys.uses('x') { "" } else { "x remove · " };
            format!(
                "↵ jump · {remove}{modes}{play} pause · {skip} skip · ←/→ seek · -/+ vol · {quit} quit",
            )
        }
    }
}

fn key_label(key: char) -> String {
    match key {
        ' ' => "Space".to_owned(),
        '\t' => "Tab".to_owned(),
        '\n' => "Enter".to_owned(),
        _ => key.to_string(),
    }
}

/// Format milliseconds as `M:SS`.
fn fmt_ms(ms: u32) -> String {
    let total_secs = ms / 1000;
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

#[cfg(test)]
mod tests {
    use crate::view::{centered_album_art_area, now_playing_line, progress_cells};
    use ratatui::layout::Rect;
    use ratatui_image::FontSize;

    #[test]
    fn progress_cells_spans_zero_to_full() {
        assert_eq!(progress_cells(100, 0, 200), 0);
        assert_eq!(progress_cells(100, 100, 200), 50);
        assert_eq!(progress_cells(100, 200, 200), 100);
    }

    #[test]
    fn progress_cells_handles_zero_duration_and_overrun() {
        assert_eq!(progress_cells(100, 50, 0), 0);
        // Position past the end is clamped to a full bar.
        assert_eq!(progress_cells(40, 999, 200), 40);
    }

    #[test]
    fn now_playing_line_is_exactly_width_with_right_aligned_timing() {
        let line = now_playing_line(30, "Artist — Title", "1:00 / 3:00");
        assert_eq!(line.chars().count(), 30);
        assert!(line.starts_with("Artist — Title"));
        assert!(line.ends_with("1:00 / 3:00"));
    }

    #[test]
    fn now_playing_line_truncates_an_overlong_label() {
        let line = now_playing_line(20, "A really long artist and title here", "0:10 / 4:00");
        assert_eq!(line.chars().count(), 20);
        assert!(line.ends_with("0:10 / 4:00"));
    }

    #[test]
    fn now_playing_line_measures_wide_chars_by_display_width() {
        use unicode_width::UnicodeWidthStr;
        // CJK glyphs are two cells wide; the line must still be exactly `width`
        // display cells and keep the timing flush right.
        let line = now_playing_line(24, "日本語のアーティスト — 曲名", "1:00 / 3:00");
        assert_eq!(UnicodeWidthStr::width(line.as_str()), 24);
        assert!(line.ends_with("1:00 / 3:00"));
    }

    #[test]
    fn centered_album_art_area_keeps_pixel_square_centered() {
        let area = Rect::new(10, 4, 80, 80);
        let font_size = Some(FontSize::new(8, 16));

        let centered = centered_album_art_area(area, font_size);

        assert_eq!(centered, Rect::new(10, 24, 80, 40));
    }

    #[test]
    fn centered_album_art_area_caps_to_available_area() {
        let area = Rect::new(2, 3, 30, 20);
        let font_size = Some(FontSize::new(8, 16));

        let centered = centered_album_art_area(area, font_size);

        assert_eq!(centered, Rect::new(2, 5, 30, 15));
    }
}
