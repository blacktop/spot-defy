//! File-based `tracing` logging setup.
//!
//! The TUI owns the alternate screen, so logs must never go to stdout/stderr.
//! Logs roll daily into the platform data directory. The returned
//! [`WorkerGuard`] must be held for the lifetime of the program or buffered log
//! lines are dropped.

use anyhow::Context as _;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

/// Crates that log access/refresh tokens or the OAuth authorization code — but
/// only ever at `TRACE`. `TRACE` events from these targets are dropped so
/// secrets can never reach the on-disk log, while `DEBUG` stays available for
/// troubleshooting (e.g. `RUST_LOG=librespot_playback=debug,librespot_core=debug`
/// to see why a track failed to load).
const SECRET_BEARING_TARGETS: &[&str] = &["librespot_core", "librespot_oauth", "librespot_connect"];

/// Targets that emit high-volume, low-value spam. The MP3 demuxer logs a line
/// per junk byte range when a stream fails to decrypt (a single failed track
/// can produce tens of thousands of lines), so events from these targets below
/// `ERROR` are dropped to keep the log readable.
const NOISY_TARGETS: &[&str] = &["symphonia"];

/// Initialize the file logging subscriber.
///
/// Logs are written to `<data_dir>/spot-defy/logs/spot-defy.log.<date>` with a
/// daily rotation. Levels follow `RUST_LOG` (default `info`); `TRACE` from
/// token-bearing targets is always dropped (see [`SECRET_BEARING_TARGETS`]).
///
/// # Returns
///
/// A [`WorkerGuard`] that flushes the non-blocking writer on drop. Keep it alive
/// for the entire program; dropping it early loses buffered log output.
///
/// # Errors
///
/// Returns an error if the platform data directory cannot be resolved or the
/// log directory cannot be created.
pub fn init() -> anyhow::Result<WorkerGuard> {
    let data_dir = dirs::data_dir().context("could not resolve platform data directory")?;
    let log_dir = data_dir.join("spot-defy").join("logs");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("could not create log directory at {}", log_dir.display()))?;

    let appender = tracing_appender::rolling::daily(&log_dir, "spot-defy.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let writer_layer = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_filter(filter_fn(|meta| {
            let target = meta.target();
            let level = *meta.level();
            !is_redacted(target, level) && !is_noise(target, level)
        }));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(writer_layer)
        .init();

    Ok(guard)
}

/// Whether a log event is a token-bearing `TRACE` event that must be dropped.
fn is_redacted(target: &str, level: Level) -> bool {
    level == Level::TRACE
        && SECRET_BEARING_TARGETS
            .iter()
            .any(|prefix| target.starts_with(prefix))
}

/// Whether a log event is high-volume noise to drop below `ERROR`.
///
/// In `tracing`, a more verbose level compares greater, so `level > ERROR`
/// matches `WARN`/`INFO`/`DEBUG`/`TRACE` while keeping genuine errors.
fn is_noise(target: &str, level: Level) -> bool {
    level > Level::ERROR
        && NOISY_TARGETS
            .iter()
            .any(|prefix| target.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use crate::logging::{is_noise, is_redacted};
    use tracing::Level;

    #[test]
    fn drops_trace_from_secret_targets() {
        assert!(is_redacted("librespot_core::token", Level::TRACE));
        assert!(is_redacted("librespot_oauth", Level::TRACE));
        assert!(is_redacted("librespot_connect::spirc", Level::TRACE));
    }

    #[test]
    fn keeps_debug_and_non_secret_targets() {
        // DEBUG diagnostics are kept — tokens are only ever logged at TRACE.
        assert!(!is_redacted("librespot_core::token", Level::DEBUG));
        assert!(!is_redacted("librespot_playback::player", Level::TRACE));
        assert!(!is_redacted("spot_defy::auth", Level::TRACE));
    }

    #[test]
    fn drops_symphonia_spam_below_error() {
        assert!(is_noise("symphonia_bundle_mp3::demuxer", Level::WARN));
        assert!(is_noise("symphonia", Level::INFO));
    }

    #[test]
    fn keeps_symphonia_errors_and_other_targets() {
        // Real errors survive; unrelated targets are never treated as noise.
        assert!(!is_noise("symphonia_bundle_mp3::demuxer", Level::ERROR));
        assert!(!is_noise("librespot_playback::player", Level::WARN));
    }
}
