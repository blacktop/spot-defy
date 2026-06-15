//! spot-defy: a privacy-first Spotify player TUI with embedded streaming.
//!
//! The binary (`src/main.rs`) is a thin shell over this library so integration
//! tests can exercise the pure modules (`update`, `state`, `model`) directly.
//! See the per-module docs for the TEA architecture, the two service traits
//! ([`api::SpotifyApi`], [`player::Playback`]), and the now-playing IPC.

// Panic-prevention lints are denied in production code (see Cargo.toml) but
// unwrap/expect/panic are the idiomatic way to assert in tests; relax them for
// the in-crate `#[cfg(test)]` modules only.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod api;
pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod error;
pub mod ipc;
pub mod logging;
pub mod message;
pub mod model;
pub mod player;
pub mod secrets;
pub mod state;
pub mod update;
pub mod view;
