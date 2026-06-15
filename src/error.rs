//! Internal `thiserror` error enums shared across modules.
//!
//! Application-boundary code (`crate::main`, `crate::app`) wraps these with
//! `anyhow::Context`; internal modules return these typed variants. No variant
//! ever carries a token or other secret in its `Display` output.

use thiserror::Error;

/// Errors raised by the OAuth / token lifecycle (`crate::auth`, `crate::secrets`).
#[derive(Debug, Error)]
pub enum AuthError {
    /// The interactive PKCE loopback flow failed to complete.
    #[error("oauth login failed: {0}")]
    Login(String),

    /// Minting a fresh access token from the stored refresh token failed.
    #[error("token refresh failed: {0}")]
    Refresh(String),

    /// Reading or writing the rotating refresh token in the Keychain failed.
    #[error("keychain access failed: {0}")]
    Keychain(String),

    /// A token field could not be mapped into the target credential type.
    #[error("token mapping failed: {0}")]
    Mapping(String),
}

/// Errors raised by the Web API layer (`crate::api`).
#[derive(Debug, Error)]
pub enum ApiError {
    /// The underlying HTTP request failed (network, TLS, timeout).
    #[error("spotify api request failed: {0}")]
    Request(String),

    /// The Spotify API returned an error status (auth, 4xx/5xx).
    #[error("spotify api returned an error: {0}")]
    Response(String),

    /// Spotify rate-limited the request (HTTP 429). `retry_after_secs` carries
    /// the server's `Retry-After` hint when present.
    #[error("rate limited by spotify (429); retried but still limited")]
    RateLimited { retry_after_secs: Option<u64> },

    /// A response payload could not be mapped into a `crate::model` type.
    #[error("failed to map spotify response: {0}")]
    Mapping(String),

    /// The requested capability is not yet implemented.
    #[error("api capability not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Errors raised by the streaming/playback layer (`crate::player`).
#[derive(Debug, Error)]
pub enum PlayerError {
    /// Establishing or refreshing the librespot streaming session failed.
    #[error("streaming session error: {0}")]
    Session(String),

    /// The account is not Premium, so streaming is unavailable.
    #[error("spotify premium is required for streaming")]
    PremiumRequired,

    /// A load/play/seek control command failed.
    #[error("playback control failed: {0}")]
    Control(String),

    /// The audio backend could not be initialized.
    #[error("audio backend error: {0}")]
    AudioBackend(String),

    /// The requested capability is not yet implemented.
    #[error("player capability not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Errors raised by the now-playing IPC layer (`crate::ipc`).
#[derive(Debug, Error)]
pub enum IpcError {
    /// Binding, connecting to, or unlinking the Unix socket failed.
    #[error("ipc socket error: {0}")]
    Socket(String),

    /// Serializing or deserializing the wire payload failed.
    #[error("ipc payload error: {0}")]
    Payload(String),

    /// The client timed out waiting for the server to respond.
    #[error("ipc request timed out")]
    Timeout,
}
