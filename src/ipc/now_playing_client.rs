//! Thin client for `spot-defy now-playing`.
//!
//! Connects to the running TUI's Unix socket with a hard timeout, reads one
//! snapshot, and prints a single tmux-safe line. On any error (no server,
//! timeout, idle) it prints an empty line and exits successfully so tmux never
//! shows a stack trace or hangs.

use crate::ipc::NowPlayingPayload;
use crate::model::PlaybackState;
use std::io::Write as _;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio::net::UnixStream;

/// Hard timeout for the whole query; tmux refreshes frequently so fail fast.
const QUERY_TIMEOUT: Duration = Duration::from_millis(400);

/// Connect, read the snapshot, print one line, and return.
///
/// Always prints exactly one line to stdout and returns `Ok(())`; transport
/// failures degrade to an empty line rather than an error.
///
/// # Errors
///
/// Returns an error only if writing to stdout itself fails.
pub async fn query_and_print() -> anyhow::Result<()> {
    let line = match read_snapshot().await {
        Some(payload) => format_line(&payload),
        None => String::new(),
    };
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{line}")?;
    Ok(())
}

/// Attempt to read a snapshot within the timeout; `None` on any failure.
async fn read_snapshot() -> Option<NowPlayingPayload> {
    let path = crate::ipc::socket_path().ok()?;
    let fut = async {
        let mut stream = UnixStream::connect(&path).await.ok()?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.ok()?;
        serde_json::from_slice::<NowPlayingPayload>(&buf).ok()
    };
    tokio::time::timeout(QUERY_TIMEOUT, fut)
        .await
        .ok()
        .flatten()
}

/// Placeholder shown when a track or artist name is absent.
const MISSING_FIELD: &str = "—";

/// Maximum number of characters kept from each of the artist and track names.
///
/// tmux truncates the status itself via `status-right-length`, but capping each
/// field here keeps a pathological title from dominating the line and bounds
/// the work done formatting it.
const MAX_FIELD_CHARS: usize = 40;

/// Format a payload into a one-line, tmux-safe status string.
///
/// The output is guaranteed to be a single line: control characters (including
/// newlines and tabs) in track and artist names are replaced with spaces, each
/// field is truncated to [`MAX_FIELD_CHARS`], and `#` is escaped as `##` so a
/// song title can never inject a tmux format directive. A stopped player yields
/// an empty string so the status bar shows nothing.
fn format_line(payload: &NowPlayingPayload) -> String {
    let symbol = match payload.state {
        PlaybackState::Playing => "▶",
        PlaybackState::Paused => "⏸",
        PlaybackState::Loading => "…",
        PlaybackState::Stopped => return String::new(),
    };
    let artist = sanitize_field(payload.artist.as_deref());
    let track = sanitize_field(payload.track.as_deref());
    format!("{symbol} {artist} — {track}").replace('#', "##")
}

/// Sanitize one name field for single-line, tmux-safe output.
///
/// Replaces control characters with spaces, collapses leading/trailing
/// whitespace, truncates to [`MAX_FIELD_CHARS`] (appending an ellipsis when
/// shortened), and falls back to [`MISSING_FIELD`] when the field is absent or
/// empty after cleanup.
fn sanitize_field(value: Option<&str>) -> String {
    let Some(raw) = value else {
        return MISSING_FIELD.to_owned();
    };
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return MISSING_FIELD.to_owned();
    }
    truncate_chars(cleaned, MAX_FIELD_CHARS)
}

/// Truncate `value` to at most `max` characters, appending `…` when cut.
fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use crate::ipc::NowPlayingPayload;
    use crate::ipc::now_playing_client::{MAX_FIELD_CHARS, MISSING_FIELD, format_line};
    use crate::model::PlaybackState;

    fn payload(
        state: PlaybackState,
        artist: Option<&str>,
        track: Option<&str>,
    ) -> NowPlayingPayload {
        NowPlayingPayload {
            track: track.map(str::to_owned),
            artist: artist.map(str::to_owned),
            state,
            position_ms: 0,
            duration_ms: 0,
        }
    }

    #[test]
    fn playing_renders_play_symbol_artist_and_track() {
        let line = format_line(&payload(
            PlaybackState::Playing,
            Some("Daft Punk"),
            Some("Aerodynamic"),
        ));
        assert_eq!(line, "▶ Daft Punk — Aerodynamic");
    }

    #[test]
    fn paused_renders_pause_symbol() {
        let line = format_line(&payload(
            PlaybackState::Paused,
            Some("Air"),
            Some("La Femme"),
        ));
        assert!(
            line.starts_with("⏸ "),
            "expected pause symbol prefix, got {line:?}"
        );
    }

    #[test]
    fn loading_renders_ellipsis_symbol() {
        let line = format_line(&payload(
            PlaybackState::Loading,
            Some("Boards"),
            Some("Roygbiv"),
        ));
        assert!(
            line.starts_with("… "),
            "expected loading symbol prefix, got {line:?}"
        );
    }

    #[test]
    fn stopped_renders_empty_line() {
        let line = format_line(&payload(
            PlaybackState::Stopped,
            Some("ignored"),
            Some("ignored"),
        ));
        assert!(line.is_empty());
    }

    #[test]
    fn missing_fields_fall_back_to_placeholder() {
        let line = format_line(&payload(PlaybackState::Playing, None, None));
        assert_eq!(line, format!("▶ {MISSING_FIELD} — {MISSING_FIELD}"));
    }

    #[test]
    fn blank_field_after_trim_uses_placeholder() {
        let line = format_line(&payload(
            PlaybackState::Playing,
            Some("   "),
            Some("Real Track"),
        ));
        assert_eq!(line, format!("▶ {MISSING_FIELD} — Real Track"));
    }

    #[test]
    fn hash_is_escaped_so_tmux_cannot_interpret_a_directive() {
        let line = format_line(&payload(
            PlaybackState::Playing,
            Some("P!nk"),
            Some("#1 Crush"),
        ));
        assert!(
            line.contains("##1 Crush"),
            "expected escaped hash, got {line:?}"
        );
        // Every `#` must be part of a doubled `##`; no lone `#` may survive.
        assert!(
            !line.replace("##", "").contains('#'),
            "unescaped hash leaked through, got {line:?}",
        );
    }

    #[test]
    fn newlines_and_tabs_are_flattened_to_a_single_line() {
        let line = format_line(&payload(
            PlaybackState::Playing,
            Some("Multi\nLine"),
            Some("Tab\tName\rHere"),
        ));
        assert!(
            !line.contains('\n'),
            "output must be single line, got {line:?}"
        );
        assert!(!line.contains('\t'), "tabs must be stripped, got {line:?}");
        assert!(
            !line.contains('\r'),
            "carriage returns must be stripped, got {line:?}"
        );
        assert!(line.contains("Multi Line"));
        assert!(line.contains("Tab Name Here"));
    }

    #[test]
    fn long_title_is_truncated_with_an_ellipsis() {
        let long = "A".repeat(120);
        let line = format_line(&payload(
            PlaybackState::Playing,
            Some("Artist"),
            Some(&long),
        ));
        let track = line
            .rsplit(" — ")
            .next()
            .expect("track segment present after separator");
        assert_eq!(track.chars().count(), MAX_FIELD_CHARS);
        assert!(
            track.ends_with('…'),
            "truncated field must end with ellipsis"
        );
    }

    #[test]
    fn field_at_exactly_the_limit_is_not_truncated() {
        let exact = "B".repeat(MAX_FIELD_CHARS);
        let line = format_line(&payload(
            PlaybackState::Playing,
            Some("Artist"),
            Some(&exact),
        ));
        assert!(
            line.ends_with(&exact),
            "exact-length field must be kept whole"
        );
        assert!(!line.contains('…'));
    }
}
