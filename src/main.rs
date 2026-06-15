//! spot-defy entry point.
//!
//! Parses the CLI, installs file logging and the Keychain default store, then
//! routes to the TUI, the now-playing IPC client, or the auth subcommands. All
//! real logic lives in the `spot_defy` library crate.

use anyhow::Context as _;
use clap::Parser as _;
use spot_defy::cli::{AuthCommand, Cli, Command};

/// Process entry point.
///
/// # Errors
///
/// Returns an error if logging/keychain setup fails or the dispatched command
/// fails.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _log_guard = spot_defy::logging::init().context("failed to initialize logging")?;
    let cli = Cli::parse();

    match cli.command {
        None => run_tui().await,
        Some(Command::NowPlaying) => spot_defy::ipc::now_playing_client::query_and_print().await,
        Some(Command::Auth(AuthCommand::Login)) => run_auth_login().await,
        Some(Command::Auth(AuthCommand::Logout)) => run_auth_logout(),
    }
}

/// Launch the interactive TUI.
async fn run_tui() -> anyhow::Result<()> {
    spot_defy::secrets::install_default_store().context("failed to install keychain store")?;
    spot_defy::app::run().await
}

/// Run both interactive OAuth login flows (streaming + Web API) and persist
/// their refresh tokens. The two flows use different client ids, so this is two
/// browser authorizations on first run (cached afterward).
async fn run_auth_login() -> anyhow::Result<()> {
    spot_defy::secrets::install_default_store().context("failed to install keychain store")?;
    let config = spot_defy::config::Config::load().context("failed to load config")?;
    spot_defy::auth::obtain_streaming_token()
        .await
        .context("spotify streaming login failed")?;
    spot_defy::auth::obtain_webapi_token(&config.client_id, config.redirect_port)
        .await
        .context("spotify web api login failed")?;
    Ok(())
}

/// Remove both stored refresh tokens (logout).
fn run_auth_logout() -> anyhow::Result<()> {
    spot_defy::secrets::install_default_store().context("failed to install keychain store")?;
    spot_defy::secrets::clear_refresh_token(spot_defy::secrets::TokenKind::Streaming)
        .context("failed to clear streaming token")?;
    spot_defy::secrets::clear_refresh_token(spot_defy::secrets::TokenKind::WebApi)
        .context("failed to clear web-api token")?;
    Ok(())
}
