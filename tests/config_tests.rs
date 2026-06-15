//! Behavior tests for config defaults, TOML parsing, and path resolution.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use spot_defy::config::{Config, config_path};

#[test]
fn default_config_uses_webapi_client_id_and_port() {
    let config = Config::default();
    assert_eq!(
        config.client_id,
        spot_defy::auth::webapi_client_id_default()
    );
    assert_eq!(config.redirect_port, 8899);
}

#[test]
fn default_keybindings_match_vim_conventions() {
    let config = Config::default();
    assert_eq!(config.keybindings.quit, 'q');
    assert_eq!(config.keybindings.down, 'j');
    assert_eq!(config.keybindings.up, 'k');
    assert_eq!(config.keybindings.search, '/');
}

#[test]
fn empty_toml_yields_defaults() {
    let config = Config::from_toml("").expect("empty config should parse to defaults");
    assert_eq!(config, Config::default());
}

#[test]
fn partial_toml_fills_missing_fields_from_defaults() {
    let config = Config::from_toml("redirect_port = 9000").expect("partial config should parse");
    assert_eq!(config.redirect_port, 9000);
    // Unspecified fields keep their defaults.
    assert_eq!(
        config.client_id,
        spot_defy::auth::webapi_client_id_default()
    );
    assert_eq!(config.keybindings.quit, 'q');
}

#[test]
fn full_toml_round_trips_all_sections() {
    let toml = r#"
client_id = "custom-client"
redirect_port = 12345

[keybindings]
quit = "x"
down = "s"
up = "w"
play_pause = "p"
next = "l"
previous = "h"
search = "f"

[theme]
accent = "magenta"
progress = "yellow"
dim = "darkgray"
"#;
    let config = Config::from_toml(toml).expect("full config should parse");
    assert_eq!(config.client_id, "custom-client");
    assert_eq!(config.redirect_port, 12345);
    assert_eq!(config.keybindings.quit, 'x');
    assert_eq!(config.keybindings.next, 'l');
    assert_eq!(config.theme.accent, "magenta");
    assert_eq!(config.theme.dim, "darkgray");
}

#[test]
fn unknown_top_level_field_is_rejected() {
    let result = Config::from_toml("totally_made_up = true");
    assert!(
        result.is_err(),
        "unknown fields must be rejected, not ignored"
    );
}

#[test]
fn unknown_nested_field_is_rejected() {
    let toml = "[keybindings]\nfly = \"z\"";
    let result = Config::from_toml(toml);
    assert!(result.is_err(), "unknown nested fields must be rejected");
}

#[test]
fn malformed_toml_is_rejected() {
    let result = Config::from_toml("redirect_port = = =");
    assert!(result.is_err(), "malformed TOML must error");
}

#[test]
fn multi_char_keybinding_is_rejected() {
    // A char field cannot hold a multi-character string.
    let toml = "[keybindings]\nquit = \"esc\"";
    let result = Config::from_toml(toml);
    assert!(result.is_err(), "a char binding must be a single character");
}

#[test]
fn config_path_ends_with_app_dir_and_file() {
    let path = config_path().expect("config dir should resolve on macOS");
    assert!(
        path.ends_with("spot-defy/config.toml"),
        "unexpected path: {}",
        path.display()
    );
    assert!(
        path.is_absolute(),
        "config path must be absolute: {}",
        path.display()
    );
}
