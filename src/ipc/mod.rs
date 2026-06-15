//! Now-playing IPC contract: socket path resolution and the wire payload.
//!
//! The running TUI serves a Unix socket; `spot-defy now-playing` connects,
//! reads one snapshot line, and prints a tmux-safe status. The payload is a
//! small JSON object so the client never needs to start librespot or the TUI.
//!
//! # tmux integration
//!
//! The `now-playing` subcommand only reads the running app's Unix socket; it
//! never starts librespot or the TUI, so it is safe to poll on a short
//! interval. tmux runs `#(...)` with its own `PATH`, so prefer an absolute
//! path to the binary. Add to `~/.tmux.conf`:
//!
//! ```tmux
//! set -g status-interval 5
//! set -g status-right-length 100
//! set -g status-right "#(/path/to/spot-defy now-playing)"
//! ```
//!
//! When nothing is playing, auth is missing, or the app is not running, the
//! command prints an empty line and exits `0`, so the status bar simply shows
//! nothing rather than an error. Track and artist names are sanitized to a
//! single line and any `#` is escaped as `##` so a song title can never inject
//! a tmux format directive. See `docs/tmux.md` for the full setup guide.

use crate::error::IpcError;
use crate::model::{PlaybackSnapshot, PlaybackState};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod now_playing_client;
pub mod server;

/// The Unix socket filename under the runtime/cache directory.
const SOCKET_FILE: &str = "spot-defy.sock";

/// The JSON payload exchanged over the now-playing socket.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NowPlayingPayload {
    pub track: Option<String>,
    pub artist: Option<String>,
    pub state: PlaybackState,
    pub position_ms: u32,
    pub duration_ms: u32,
}

impl From<&PlaybackSnapshot> for NowPlayingPayload {
    /// Project a full [`PlaybackSnapshot`] onto the narrower IPC wire payload.
    ///
    /// The volume is dropped because the tmux status line never renders it; the
    /// payload stays minimal so the client transfer is a single tiny JSON blob.
    fn from(snapshot: &PlaybackSnapshot) -> Self {
        Self {
            track: snapshot.track.clone(),
            artist: snapshot.artist.clone(),
            state: snapshot.state,
            position_ms: snapshot.position_ms,
            duration_ms: snapshot.duration_ms,
        }
    }
}

/// Resolve the now-playing Unix socket path.
///
/// Prefers the platform runtime directory and falls back to the cache
/// directory; the file is named [`SOCKET_FILE`].
///
/// # Errors
///
/// Returns [`IpcError::Socket`] if no suitable base directory can be resolved.
pub fn socket_path() -> Result<PathBuf, IpcError> {
    let base = dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .ok_or_else(|| IpcError::Socket("no runtime or cache directory available".to_owned()))?;
    Ok(base.join("spot-defy").join(SOCKET_FILE))
}
