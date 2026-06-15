//! Tests for OAuth token mapping and secret redaction.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use secrecy::SecretString;
use spot_defy::auth::{TokenSet, scopes, to_rspotify_token};
use std::time::Instant;

fn sample_tokens() -> TokenSet {
    TokenSet {
        access: SecretString::from("access-abc"),
        refresh: SecretString::from("refresh-xyz"),
        expires_at: Instant::now(),
    }
}

#[test]
fn to_rspotify_token_maps_access_and_refresh() {
    let tokens = sample_tokens();
    let token = to_rspotify_token(&tokens);
    assert_eq!(token.access_token, "access-abc");
    assert_eq!(token.refresh_token.as_deref(), Some("refresh-xyz"));
}

#[test]
fn to_rspotify_token_carries_all_scopes() {
    let tokens = sample_tokens();
    let token = to_rspotify_token(&tokens);
    for scope in scopes() {
        assert!(
            token.scopes.contains(scope),
            "missing scope {scope} in mapped token"
        );
    }
}

#[test]
fn scopes_cover_the_used_read_endpoints() {
    let scopes = scopes();
    assert!(scopes.contains(&"playlist-read-private"));
    assert!(scopes.contains(&"user-library-read"));
    assert!(scopes.contains(&"user-top-read"));
    assert!(scopes.contains(&"user-read-recently-played"));
}

#[test]
fn scopes_are_minimal_and_read_only() {
    // Privacy guard: the Web API token must never request write, playback-control,
    // profile, streaming, or deprecated-endpoint scopes — only the read scopes the
    // features use. Adding a broader scope here is a deliberate decision that must
    // update this test.
    let scopes = scopes();
    assert_eq!(scopes.len(), 4, "unexpected scope added: {scopes:?}");
    for scope in &scopes {
        assert!(!scope.contains("modify"), "no write scope allowed: {scope}");
    }
    assert!(!scopes.contains(&"streaming"));
    assert!(!scopes.contains(&"user-read-private"));
    assert!(!scopes.iter().any(|s| s.contains("playback-state")));
    assert!(!scopes.iter().any(|s| s.contains("currently-playing")));
    assert!(
        !scopes
            .iter()
            .any(|s| s.contains("recommend") || s.contains("featured"))
    );
}

#[test]
fn token_set_debug_redacts_secrets() {
    let tokens = sample_tokens();
    let rendered = format!("{tokens:?}");
    assert!(
        !rendered.contains("access-abc"),
        "access token leaked into Debug output: {rendered}"
    );
    assert!(
        !rendered.contains("refresh-xyz"),
        "refresh token leaked into Debug output: {rendered}"
    );
}
