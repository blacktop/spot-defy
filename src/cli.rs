//! Command-line interface definition (clap derive).
//!
//! The same binary serves three modes: the default TUI, a `now-playing`
//! one-line status reader for the tmux status bar, and `auth login` to run the
//! interactive OAuth flow once.

use clap::{Parser, Subcommand};

/// Privacy-first Spotify player TUI with embedded streaming.
#[derive(Debug, Parser)]
#[command(name = "spot-defy", version, about)]
pub struct Cli {
    /// Optional subcommand; when omitted the interactive TUI launches.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print a one-line now-playing status for the tmux status bar, then exit.
    NowPlaying,

    /// Authentication management.
    #[command(subcommand)]
    Auth(AuthCommand),
}

/// `spot-defy auth ...` subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Run the interactive OAuth login flow and store the refresh token.
    Login,

    /// Remove the stored refresh token from the Keychain (logout).
    Logout,
}
