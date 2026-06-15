//! Two OAuth flows: one bootstraps the librespot streaming session, the other
//! authenticates the rspotify Web API.
//!
//! Spotify now returns HTTP 429 for ALL Web API calls authenticated with the
//! librespot keymaster client id, so the Web API runs on a SEPARATE client id
//! with extended quota (overridable via `config.client_id`), exactly as ncspot
//! and spotify-player do. The keymaster id is used ONLY to connect the librespot
//! streaming Session. Each flow stores its own rotating refresh token in the
//! Keychain (distinct accounts); access tokens live in memory as [`SecretString`]
//! and are never logged.

use crate::error::AuthError;
use crate::secrets::{self, TokenKind};
use librespot_core::authentication::Credentials;
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthToken};
use secrecy::{ExposeSecret as _, SecretString};
use std::collections::HashSet;
use std::time::Instant;

/// librespot's keymaster client id. Used ONLY to connect the streaming session:
/// Spotify 429s every Web API call made with it, so the Web API uses a different
/// client id (see [`webapi_client_id_default`]).
const STREAMING_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

/// Default Web API client id — the extended-quota id shared by ncspot and
/// spotify-player. Overridable via `config.client_id` to point at your own app.
const WEBAPI_CLIENT_ID: &str = "d420a117a32841c2b3474932e49fb54b";

/// Loopback redirect for the streaming (keymaster) flow.
const STREAMING_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

/// HTML returned to the browser after each authorization, so the user knows to
/// return to the terminal.
const BROWSER_MESSAGE: &str = concat!(
    "<!doctype html><html><body>",
    "<h1>spot-defy is authorized</h1>",
    "<p>You can close this tab and return to your terminal.</p>",
    "</body></html>"
);

/// The full set of in-memory tokens produced by one OAuth exchange.
///
/// All token fields are [`SecretString`] so `Debug`/`tracing` render them as
/// `[REDACTED]` and the backing memory is zeroized on drop.
#[derive(Debug, Clone)]
pub struct TokenSet {
    /// Short-lived (~1h) access token.
    pub access: SecretString,
    /// Rotating, single-use refresh token persisted to the Keychain.
    pub refresh: SecretString,
    /// Monotonic instant at which `access` expires.
    pub expires_at: Instant,
}

/// The default Web API client id (extended quota). Used as the config default.
#[must_use]
pub fn webapi_client_id_default() -> &'static str {
    WEBAPI_CLIENT_ID
}

/// The Web API OAuth scopes — the minimal **read-only** set the app's features
/// actually use: reading your playlists, top items, recently played, and saved
/// tracks. Search needs no scope. No write/modify scopes, no playback-control
/// scopes (playback is local via librespot), no profile/email scope, and no
/// deprecated-endpoint scopes are requested.
#[must_use]
pub fn scopes() -> Vec<&'static str> {
    vec![
        "playlist-read-private",
        "user-library-read",
        "user-top-read",
        "user-read-recently-played",
    ]
}

/// Scopes for the streaming (keymaster) session bootstrap. librespot's own OAuth
/// example requests this set; `["streaming"]` alone can leave the access-point
/// session under-provisioned for loading tracks after the first.
fn streaming_scopes() -> Vec<&'static str> {
    vec![
        "streaming",
        "user-read-private",
        "user-read-playback-state",
        "user-modify-playback-state",
        "user-read-currently-playing",
    ]
}

/// The loopback redirect URI for the Web API flow on `port`.
fn webapi_redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/login")
}

/// Obtain the streaming access token (keymaster id) used to connect librespot.
///
/// # Errors
///
/// Returns [`AuthError`] if the OAuth flow or Keychain access fails.
pub async fn obtain_streaming_token() -> Result<TokenSet, AuthError> {
    run_flow(
        STREAMING_CLIENT_ID,
        STREAMING_REDIRECT_URI,
        streaming_scopes(),
        TokenKind::Streaming,
    )
    .await
}

/// Obtain the Web API access token from `client_id` (default extended-quota id).
///
/// # Errors
///
/// Returns [`AuthError`] if the OAuth flow or Keychain access fails.
pub async fn obtain_webapi_token(
    client_id: &str,
    redirect_port: u16,
) -> Result<TokenSet, AuthError> {
    let redirect = webapi_redirect_uri(redirect_port);
    run_flow(client_id, &redirect, scopes(), TokenKind::WebApi).await
}

/// Run one OAuth flow: prefer the stored refresh token, else the browser; then
/// persist the rotated refresh token under `kind`.
async fn run_flow(
    client_id: &str,
    redirect_uri: &str,
    scopes: Vec<&str>,
    kind: TokenKind,
) -> Result<TokenSet, AuthError> {
    let client = build_client(client_id, redirect_uri, scopes)?;
    let tokens = match secrets::load_refresh_token(kind)? {
        Some(refresh) => obtain_via_refresh(&client, &refresh).await?,
        None => obtain_via_browser(&client).await?,
    };
    secrets::store_refresh_token(kind, &tokens.refresh)?;
    Ok(tokens)
}

/// Mint a Web API access token from the Keychain refresh token, non-interactively.
///
/// Used by the background refresh task; performs no browser fallback. The rotated
/// refresh token is persisted before returning.
///
/// # Errors
///
/// Returns [`AuthError::Refresh`] if no refresh token is stored or the exchange
/// fails, or [`AuthError::Keychain`] if the rotated token cannot be persisted.
pub async fn refresh_webapi_from_keychain(
    client_id: &str,
    redirect_port: u16,
) -> Result<TokenSet, AuthError> {
    let refresh = secrets::load_refresh_token(TokenKind::WebApi)?
        .ok_or_else(|| AuthError::Refresh("no stored web-api refresh token".to_owned()))?;
    let redirect = webapi_redirect_uri(redirect_port);
    let client = build_client(client_id, &redirect, scopes())?;
    let token = Box::pin(client.refresh_token_async(refresh.expose_secret()))
        .await
        .map_err(|err| AuthError::Refresh(err.to_string()))?;
    let tokens = token_set_from(token);
    secrets::store_refresh_token(TokenKind::WebApi, &tokens.refresh)?;
    Ok(tokens)
}

/// Mint an access token from a stored refresh token, falling back to the browser
/// flow if the refresh is rejected (revoked/rotated), so a stale entry never
/// bricks login.
async fn obtain_via_refresh(
    client: &OAuthClient,
    refresh: &SecretString,
) -> Result<TokenSet, AuthError> {
    let exchange = Box::pin(client.refresh_token_async(refresh.expose_secret()));
    match exchange.await {
        Ok(token) => Ok(token_set_from(token)),
        Err(err) => {
            tracing::warn!(error = %err, "refresh token exchange failed; re-running browser login");
            obtain_via_browser(client).await
        }
    }
}

/// Run the interactive PKCE loopback flow: open the browser, capture the redirect
/// code on the loopback listener, and exchange it for tokens.
async fn obtain_via_browser(client: &OAuthClient) -> Result<TokenSet, AuthError> {
    let exchange = Box::pin(client.get_access_token_async());
    let token = exchange
        .await
        .map_err(|err| AuthError::Login(err.to_string()))?;
    Ok(token_set_from(token))
}

/// Build a librespot OAuth client for `client_id`/`redirect_uri`/`scopes`.
fn build_client(
    client_id: &str,
    redirect_uri: &str,
    scopes: Vec<&str>,
) -> Result<OAuthClient, AuthError> {
    OAuthClientBuilder::new(client_id, redirect_uri, scopes)
        .open_in_browser()
        .with_custom_message(BROWSER_MESSAGE)
        .build()
        .map_err(|err| AuthError::Login(format!("failed to build oauth client: {err}")))
}

/// Convert a librespot [`OAuthToken`] into an in-memory [`TokenSet`], wrapping
/// both token strings in [`SecretString`] immediately.
fn token_set_from(token: OAuthToken) -> TokenSet {
    TokenSet {
        access: SecretString::from(token.access_token),
        refresh: SecretString::from(token.refresh_token),
        expires_at: token.expires_at,
    }
}

/// Map an in-memory Web API [`TokenSet`] into an rspotify [`Token`].
///
/// The access token is exposed only at this HTTP-boundary call site.
#[must_use]
pub fn to_rspotify_token(tokens: &TokenSet) -> rspotify::Token {
    rspotify::Token {
        access_token: tokens.access.expose_secret().to_owned(),
        refresh_token: Some(tokens.refresh.expose_secret().to_owned()),
        scopes: scopes()
            .into_iter()
            .map(str::to_owned)
            .collect::<HashSet<_>>(),
        ..rspotify::Token::default()
    }
}

/// Build librespot streaming [`Credentials`] from an in-memory streaming
/// [`TokenSet`]. The access token is exposed only at this streaming-boundary
/// call site.
#[must_use]
pub fn to_librespot_credentials(tokens: &TokenSet) -> Credentials {
    Credentials::with_access_token(tokens.access.expose_secret())
}

#[cfg(test)]
mod tests {
    use crate::auth::{scopes, to_rspotify_token, token_set_from};
    use librespot_oauth::OAuthToken;
    use secrecy::ExposeSecret as _;
    use std::time::{Duration, Instant};

    fn sample_oauth_token() -> OAuthToken {
        OAuthToken {
            access_token: "access-123".to_string(),
            refresh_token: "refresh-456".to_string(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            token_type: "Bearer".to_string(),
            scopes: scopes().into_iter().map(str::to_owned).collect(),
        }
    }

    #[test]
    fn token_set_from_wraps_both_tokens_as_secrets() {
        let set = token_set_from(sample_oauth_token());
        assert_eq!(set.access.expose_secret(), "access-123");
        assert_eq!(set.refresh.expose_secret(), "refresh-456");
    }

    #[test]
    fn token_set_from_preserves_the_expiry_instant() {
        let oauth = sample_oauth_token();
        let expected = oauth.expires_at;
        let set = token_set_from(oauth);
        assert_eq!(set.expires_at, expected);
    }

    #[test]
    fn to_rspotify_token_maps_tokens_and_scopes() {
        let set = token_set_from(sample_oauth_token());
        let token = to_rspotify_token(&set);
        assert_eq!(token.access_token, "access-123");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-456"));
        assert!(token.scopes.contains("user-top-read"));
    }
}
