//! Render the `Model` with ratatui.
//!
//! Layout is header / body / now-playing footer / help bar, split vertically;
//! the now-playing footer and help bar are drawn on every screen. The Library
//! and Search screens add a sub-tab bar above their list. Widget state
//! (`ListState`/`TableState`) is borrowed from the model, never reconstructed
//! here. Spotify green is the primary accent; the now-playing progress bar is
//! drawn by hand so text over the fill flips to a dark, readable color.

use crate::model::{AlbumItem, ArtistItem, PlaybackState, PlaylistItem, TrackItem};
use crate::state::{LibraryTab, Mode, Model, Screen, SearchTab};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Gauge, HighlightSpacing, List, ListItem, Paragraph,
};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FontSize, Resize, StatefulImage};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Spotify brand green (#1DB954) — the primary accent. Defined as true-color RGB
/// so it renders the same regardless of the terminal's ANSI palette.
const SPOTIFY_GREEN: Color = Color::Rgb(0x1D, 0xB9, 0x54);

/// Spotify's recommended dark fallback for playing views when artwork color is
/// unavailable.
const SPOTIFY_BLACK: Color = Color::Rgb(0x19, 0x14, 0x14);

/// One extra accent for live numeric/control information. Keeping it narrow
/// preserves the green/white identity without making every datum compete.
const INFO_ACCENT: Color = Color::Rgb(0x5B, 0xD8, 0xE8);
const INFO_ACCENT_DIM: Color = Color::Rgb(0x4F, 0x8F, 0x99);

/// Minimum body width before the right-third artwork column appears.
const ARTWORK_MIN_TOTAL: u16 = 96;

/// Small adaptive viewport gutter so bordered UI never touches terminal edges.
const OUTER_MARGIN_HORIZONTAL: u16 = 1;
const OUTER_MARGIN_VERTICAL: u16 = 1;

/// Transient volume popup geometry.
const VOLUME_POPUP_WIDTH: u16 = 34;
const VOLUME_POPUP_HEIGHT: u16 = 5;
const VOLUME_FADE_WINDOW: Duration = Duration::from_millis(400);

/// Dim color for borders, so the green content stands out.
const BORDER: Color = Color::Rgb(0x45, 0x45, 0x45);

/// The selection highlight: solid green bar with black text.
fn highlight_style() -> Style {
    Style::new().fg(SPOTIFY_BLACK).bg(SPOTIFY_GREEN).bold()
}

/// The active-tab "pill" style: black on green.
fn active_tab_style() -> Style {
    Style::new().fg(SPOTIFY_BLACK).bg(SPOTIFY_GREEN).bold()
}

/// A bordered block with rounded corners, a dim border, and a green title.
fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(SPOTIFY_BLACK))
        .border_style(Style::new().fg(BORDER))
        .title(title.to_owned().fg(SPOTIFY_GREEN).bold())
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
    let block = panel(title);
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
        frame.render_widget(Paragraph::new(placeholder.dim()).centered(), image_area);
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
        Span::from(" SPOT-DEFY ").style(active_tab_style()),
        Span::from("  "),
    ];
    for (screen, label) in [
        (Screen::Search, "1 Search"),
        (Screen::Playlists, "2 Playlists"),
        (Screen::Library, "3 Library"),
    ] {
        let span = Span::from(format!(" {label} "));
        spans.push(if screen == active_tab(model.screen) {
            span.style(active_tab_style())
        } else {
            span.dim()
        });
        spans.push(Span::from(" "));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .style(Style::new().bg(SPOTIFY_BLACK))
        .border_style(Style::new().fg(SPOTIFY_GREEN));
    frame.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

/// Map the active screen onto its owning top-level tab.
fn active_tab(screen: Screen) -> Screen {
    match screen {
        Screen::Tracks => Screen::Playlists,
        Screen::Search | Screen::Playlists | Screen::Library => screen,
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
        "Search (typing)"
    } else {
        "Search (press / to edit)"
    };
    frame.render_widget(
        Paragraph::new(model.search.query.as_str()).block(panel(title)),
        input,
    );

    if model.mode == Mode::Insert {
        let offset = u16::try_from(model.search.query[..model.search.cursor].chars().count())
            .unwrap_or(u16::MAX);
        let cursor_x = input.x.saturating_add(1).saturating_add(offset);
        frame.set_cursor_position((cursor_x, input.y + 1));
    }

    let labels = SearchTab::ALL.map(SearchTab::label);
    render_subtab_bar(
        frame,
        tabs,
        &labels,
        active_index(&SearchTab::ALL, model.search_tab),
    );
    render_search_lane(model, frame, results);
}

/// Render the active search lane (tracks, albums, artists, or playlists).
fn render_search_lane(model: &mut Model, frame: &mut Frame, area: Rect) {
    let (title, items): (&str, Vec<ListItem<'static>>) = match model.search_tab {
        SearchTab::Tracks => (
            "Tracks",
            model.search_results.tracks.iter().map(track_row).collect(),
        ),
        SearchTab::Albums => (
            "Albums",
            model.search_results.albums.iter().map(album_row).collect(),
        ),
        SearchTab::Artists => (
            "Artists",
            model
                .search_results
                .artists
                .iter()
                .map(artist_row)
                .collect(),
        ),
        SearchTab::Playlists => (
            "Playlists",
            model
                .search_results
                .playlists
                .iter()
                .map(playlist_row)
                .collect(),
        ),
    };
    render_list(
        model,
        frame,
        area,
        title,
        items,
        "No results yet. Press / to search.",
    );
}

/// Render the playlists screen.
fn render_playlists(model: &mut Model, frame: &mut Frame, area: Rect) {
    let items = model.playlists.iter().map(playlist_row).collect();
    render_list(
        model,
        frame,
        area,
        "Playlists",
        items,
        "No playlists found.",
    );
}

/// Render the playlist-tracks screen.
fn render_tracks(model: &mut Model, frame: &mut Frame, area: Rect) {
    let items = model.tracks.iter().map(track_row).collect();
    render_list(
        model,
        frame,
        area,
        "Tracks",
        items,
        "This playlist has no tracks.",
    );
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
    );

    let empty = library_empty_hint(model.library_tab);
    let title = model.library_tab.label();
    match model.library_tab {
        LibraryTab::Albums => {
            let items = model.albums.iter().map(album_row).collect();
            render_list(model, frame, body, title, items, empty);
        }
        LibraryTab::TopArtists => {
            let items = model.artists.iter().map(artist_row).collect();
            render_list(model, frame, body, title, items, empty);
        }
        LibraryTab::TopTracks | LibraryTab::RecentlyPlayed | LibraryTab::Saved => {
            let items = model.tracks.iter().map(track_row).collect();
            render_list(model, frame, body, title, items, empty);
        }
    }
}

/// Render a one-line sub-tab bar with the active tab highlighted.
fn render_subtab_bar(frame: &mut Frame, area: Rect, labels: &[&str], active: usize) {
    let mut spans = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        let span = Span::from(format!(" {label} "));
        spans.push(if index == active {
            span.style(active_tab_style())
        } else {
            span.dim()
        });
        spans.push(Span::from(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Render a selectable list, or an empty-state hint when there are no rows.
fn render_list(
    model: &mut Model,
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: Vec<ListItem<'static>>,
    empty: &str,
) {
    if items.is_empty() {
        frame.render_widget(Paragraph::new(empty.dim()).block(panel(title)), area);
        return;
    }
    let list = List::new(items)
        .block(panel(title))
        .highlight_style(highlight_style())
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut model.list_state);
}

/// The index of `current` within `all` (defaulting to the first tab).
fn active_index<T: Copy + PartialEq>(all: &[T], current: T) -> usize {
    all.iter().position(|item| *item == current).unwrap_or(0)
}

/// Format a single playlist row as `Name · N tracks · Owner`.
fn playlist_row(playlist: &PlaylistItem) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::from(playlist.name.clone()),
        Span::from(format!("  ·  {} tracks  ·  ", playlist.track_count)).dim(),
        Span::from(playlist.owner.clone()).dim(),
    ]);
    ListItem::new(line)
}

/// Format a single track row as `Artist — Title   M:SS`.
fn track_row(track: &TrackItem) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::from(track.artist.clone()).fg(SPOTIFY_GREEN),
        Span::from("  —  ").dim(),
        Span::from(track.title.clone()),
        Span::from("   "),
        Span::from(fmt_ms(track.duration_ms)).fg(INFO_ACCENT),
    ]);
    ListItem::new(line)
}

/// Format a single album row as `Artist — Album`.
fn album_row(album: &AlbumItem) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::from(album.artist.clone()).fg(SPOTIFY_GREEN),
        Span::from("  —  ").dim(),
        Span::from(album.name.clone()),
    ]);
    ListItem::new(line)
}

/// Format a single artist row.
fn artist_row(artist: &ArtistItem) -> ListItem<'static> {
    ListItem::new(Line::from(
        Span::from(artist.name.clone()).fg(SPOTIFY_GREEN),
    ))
}

/// Empty-state hint for a Library sub-tab while its data loads.
fn library_empty_hint(tab: LibraryTab) -> &'static str {
    match tab {
        LibraryTab::TopTracks => "Loading your top tracks…",
        LibraryTab::Albums => "Loading your albums…",
        LibraryTab::TopArtists => "Loading your top artists…",
        LibraryTab::RecentlyPlayed => "Loading recently played…",
        LibraryTab::Saved => "Loading your saved tracks…",
    }
}

/// Render the persistent now-playing footer as a hand-drawn progress bar.
///
/// The track line is drawn first, then the filled portion of the bar is
/// overlaid directly on the buffer with a green background and black text, so
/// the title stays readable where it crosses the progress fill.
fn render_footer(model: &Model, frame: &mut Frame, area: Rect) {
    let np = &model.now_playing;
    let block = panel("Now Playing");
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
    let right = format!("{} / {}", fmt_ms(np.position_ms), fmt_ms(np.duration_ms));
    let line = now_playing_line(inner.width as usize, &left, &right);
    frame.render_widget(Paragraph::new(line), inner);

    let filled = progress_cells(inner.width, np.position_ms, np.duration_ms);
    let buf = frame.buffer_mut();
    for y in inner.top()..inner.bottom() {
        for x in inner.left()..inner.left().saturating_add(filled) {
            let cell = &mut buf[(x, y)];
            cell.set_bg(SPOTIFY_GREEN);
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
        INFO_ACCENT_DIM
    } else {
        INFO_ACCENT
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
        Span::from("vol ").dim(),
        Span::from(format!("{}%", model.now_playing.volume))
            .fg(accent)
            .bold(),
    ]);
    frame.render_widget(Paragraph::new(label_line), label);
    let gauge = Gauge::default()
        .percent(model.now_playing.volume.min(100))
        .gauge_style(Style::new().fg(accent).bg(BORDER));
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
        Mode::Insert => " Enter submit · Esc cancel ",
        Mode::Normal => keys_for(model.screen),
    };
    frame.render_widget(Paragraph::new(Line::from(hint).dim()), area);
}

/// Keybinding hint string for the active screen in navigation mode.
fn keys_for(screen: Screen) -> &'static str {
    match screen {
        Screen::Search => {
            "/ edit · Tab lane · ↵ play · ␣ pause · n/p skip · ←/→ seek · -/+ vol · q quit"
        }
        Screen::Playlists => "↵ open · j/k move · ␣ pause · n/p skip · ←/→ seek · -/+ vol · q quit",
        Screen::Tracks => "↵ play · Esc back · ␣ pause · n/p skip · ←/→ seek · -/+ vol · q quit",
        Screen::Library => "Tab tab · ↵ play · ␣ pause · n/p skip · ←/→ seek · -/+ vol · q quit",
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
