//! User configuration loaded from `config.toml`.
//!
//! The config file holds only non-secret settings: the Web API OAuth client id,
//! the loopback redirect port, keybindings, and theme colors. Secrets (tokens)
//! are never stored here — they live in the macOS Keychain (`crate::secrets`).
//! Every field has a serde default, so a missing or partial `config.toml` still
//! parses into a fully populated [`Config`] with the locked defaults from
//! `crate::auth`.

use anyhow::Context as _;
use anyhow::bail;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Application identifier used as the config-directory subfolder.
const APP_DIR: &str = "spot-defy";

/// Config file name within the application config directory.
const CONFIG_FILE: &str = "config.toml";

/// The default loopback redirect port for the Web API OAuth flow.
///
/// Distinct from the fixed streaming-flow port (8898) so the two sequential
/// logins never contend. A custom `client_id` must register
/// `http://127.0.0.1:8899/login` (or whatever `redirect_port` is set to).
const DEFAULT_REDIRECT_PORT: u16 = 8899;

/// Spotify brand green (#1DB954), used as the default TUI accent.
pub const SPOTIFY_GREEN: Color = Color::Rgb(0x1D, 0xB9, 0x54);

/// Spotify's dark playing-view fallback, used as the TUI background.
pub const SPOTIFY_BLACK: Color = Color::Rgb(0x19, 0x14, 0x14);

/// Secondary cyan accent for progress and numeric/control information.
pub const INFO_ACCENT: Color = Color::Rgb(0x5B, 0xD8, 0xE8);

/// Dimmed secondary accent for fading transient controls.
pub const INFO_ACCENT_DIM: Color = Color::Rgb(0x4F, 0x8F, 0x99);

/// Top-level user configuration.
///
/// Loaded from `<config_dir>/spot-defy/config.toml`. All fields default, so an
/// empty or absent file yields the locked defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Web API OAuth client id. Defaults to the shared extended-quota id; set
    /// this to your own registered Spotify app's client id to use your own quota
    /// (register `http://127.0.0.1:<redirect_port>/login` as its redirect URI).
    pub client_id: String,
    /// Loopback port the Web API PKCE redirect listener binds to.
    pub redirect_port: u16,
    /// Key bindings for navigation and playback control.
    pub keybindings: Keybindings,
    /// Foreground/accent theme colors (named, not RGB, for terminal safety).
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: crate::auth::webapi_client_id_default().to_owned(),
            redirect_port: DEFAULT_REDIRECT_PORT,
            keybindings: Keybindings::default(),
            theme: Theme::default(),
        }
    }
}

/// Single-character key bindings for the TUI.
///
/// Each binding is a single logical key the user presses in `Normal` mode.
/// Defaults follow common vim-style conventions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keybindings {
    /// Quit the application.
    pub quit: char,
    /// Move selection down.
    pub down: char,
    /// Move selection up.
    pub up: char,
    /// Toggle play/pause.
    pub play_pause: char,
    /// Skip to the next track.
    pub next: char,
    /// Return to the previous track.
    pub previous: char,
    /// Enter search/insert mode.
    pub search: char,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            quit: 'q',
            down: 'j',
            up: 'k',
            play_pause: ' ',
            next: 'n',
            previous: 'p',
            search: '/',
        }
    }
}

/// Named-color theme for the TUI.
///
/// Colors are stored as ratatui color names (e.g. `"green"`, `"cyan"`) so the
/// config stays terminal-palette friendly and human-editable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    /// Accent color for highlighted/selected rows.
    pub accent: String,
    /// Color of the now-playing progress bar.
    pub progress: String,
    /// Color used for secondary/dim text.
    pub dim: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: "spotify-green".to_owned(),
            progress: "info-cyan".to_owned(),
            dim: "gray".to_owned(),
        }
    }
}

/// Parsed runtime colors used by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColors {
    /// Accent color for highlighted/selected rows.
    pub accent: Color,
    /// Color of the now-playing progress bar.
    pub progress: Color,
    /// Color used for secondary/dim text and borders.
    pub dim: Color,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            accent: SPOTIFY_GREEN,
            progress: INFO_ACCENT,
            dim: Color::Gray,
        }
    }
}

impl Theme {
    /// Convert configured color names into ratatui colors.
    ///
    /// # Errors
    ///
    /// Returns an error when any configured color name is unknown.
    pub fn colors(&self) -> anyhow::Result<ThemeColors> {
        Ok(ThemeColors {
            accent: parse_color("theme.accent", &self.accent)?,
            progress: parse_color("theme.progress", &self.progress)?,
            dim: parse_color("theme.dim", &self.dim)?,
        })
    }
}

/// Resolve the absolute path to the config file.
///
/// Returns `<config_dir>/spot-defy/config.toml`, where `<config_dir>` is the
/// platform config directory (`~/Library/Application Support` on macOS).
///
/// # Errors
///
/// Returns an error if the platform config directory cannot be resolved.
pub fn config_path() -> anyhow::Result<PathBuf> {
    let config_dir = dirs::config_dir().context("could not resolve platform config directory")?;
    Ok(config_dir.join(APP_DIR).join(CONFIG_FILE))
}

impl Config {
    /// Load the configuration from `config.toml`, falling back to defaults.
    ///
    /// When the file does not exist, the locked defaults are returned (this is
    /// the normal first-launch path). When the file exists but cannot be read or
    /// parsed, an error is returned so the user can fix it rather than silently
    /// running with surprising settings.
    ///
    /// # Errors
    ///
    /// Returns an error if the config path cannot be resolved, the file exists
    /// but cannot be read, or its contents are not valid TOML for [`Config`].
    pub fn load() -> anyhow::Result<Self> {
        let path = config_path()?;
        match std::fs::read_to_string(&path) {
            Ok(contents) => Self::from_toml(&contents)
                .with_context(|| format!("invalid config at {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => {
                Err(err).with_context(|| format!("could not read config at {}", path.display()))
            }
        }
    }

    /// Parse a [`Config`] from a TOML string.
    ///
    /// Missing fields fall back to their defaults; unknown fields are rejected so
    /// typos surface as errors instead of being silently ignored.
    ///
    /// # Errors
    ///
    /// Returns an error if `contents` is not valid TOML for [`Config`].
    pub fn from_toml(contents: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(contents).context("failed to parse config TOML")?;
        config.validate()?;
        Ok(config)
    }

    /// Validate cross-field config constraints that serde alone cannot express.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate keybindings or unsupported theme color
    /// names.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_keybindings(&self.keybindings)?;
        self.theme.colors()?;
        Ok(())
    }
}

fn validate_keybindings(keybindings: &Keybindings) -> anyhow::Result<()> {
    let bindings = [
        ("quit", keybindings.quit),
        ("down", keybindings.down),
        ("up", keybindings.up),
        ("play_pause", keybindings.play_pause),
        ("next", keybindings.next),
        ("previous", keybindings.previous),
        ("search", keybindings.search),
    ];

    for (index, (name, key)) in bindings.iter().enumerate() {
        if key.is_control() {
            bail!("unsupported control-character keybinding for {name}");
        }
        if let Some((other_name, _)) = bindings[..index]
            .iter()
            .find(|(_, other_key)| other_key == key)
        {
            bail!("duplicate keybinding `{key}` for {other_name} and {name}");
        }
    }
    Ok(())
}

fn parse_color(field: &str, value: &str) -> anyhow::Result<Color> {
    match value.trim().to_ascii_lowercase().as_str() {
        "spotify-green" => Ok(SPOTIFY_GREEN),
        "spotify-black" => Ok(SPOTIFY_BLACK),
        "info-cyan" => Ok(INFO_ACCENT),
        "info-cyan-dim" => Ok(INFO_ACCENT_DIM),
        "black" => Ok(Color::Black),
        "red" => Ok(Color::Red),
        "green" => Ok(Color::Green),
        "yellow" => Ok(Color::Yellow),
        "blue" => Ok(Color::Blue),
        "magenta" => Ok(Color::Magenta),
        "cyan" => Ok(Color::Cyan),
        "gray" | "grey" => Ok(Color::Gray),
        "darkgray" | "darkgrey" => Ok(Color::DarkGray),
        "lightred" | "light-red" => Ok(Color::LightRed),
        "lightgreen" | "light-green" => Ok(Color::LightGreen),
        "lightyellow" | "light-yellow" => Ok(Color::LightYellow),
        "lightblue" | "light-blue" => Ok(Color::LightBlue),
        "lightmagenta" | "light-magenta" => Ok(Color::LightMagenta),
        "lightcyan" | "light-cyan" => Ok(Color::LightCyan),
        "white" => Ok(Color::White),
        _ => bail!("unsupported {field} color `{value}`"),
    }
}
