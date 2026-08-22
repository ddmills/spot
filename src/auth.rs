use std::collections::HashSet;
use std::time::Instant;

use anyhow::{Context, Result};
use librespot_oauth::{OAuthClientBuilder, OAuthToken};

use crate::config;

/// Client ID used for Web API calls: ncspot's, registered in extended quota
/// mode before Spotify's Nov 2024 API changes. The librespot/keymaster ID is
/// aggressively rate-limited (429) on api.spotify.com, so it can't be used
/// here — this split mirrors what spotify-player ships by default.
pub const WEB_CLIENT_ID: &str = "d420a117a32841c2b3474932e49fb54b";

/// Spotify's own desktop client ID (librespot's keymaster default), used only
/// to authenticate the librespot streaming session.
pub const SESSION_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

const REDIRECT_URI: &str = "http://127.0.0.1:8989/login";

/// Scope list mirrored from ncspot's NCSPOT_OAUTH_SCOPES (their PR #1772,
/// Feb 2026) — the exact set proven to pass authorization for this client ID
/// after Spotify's API restrictions. Anything beyond it (e.g.
/// "user-personalized", "app-remote-control") fails with `invalid_scope`.
const SCOPES: &[&str] = &[
    "streaming",
    "user-read-email",
    "user-read-private",
    "user-library-read",
    "user-library-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
    "playlist-read-private",
    "playlist-modify-public",
    "playlist-modify-private",
    "user-follow-read",
    "user-follow-modify",
    "user-top-read",
    "user-read-currently-playing",
    "user-read-recently-played",
];

fn client(client_id: &str) -> Result<librespot_oauth::OAuthClient> {
    OAuthClientBuilder::new(client_id, REDIRECT_URI, SCOPES.to_vec())
        .open_in_browser()
        .build()
        .context("failed to build OAuth client")
}

/// Interactive browser login for the Web API token (ncspot client ID).
/// Blocks on the local redirect listener; run before the TUI starts.
pub fn login_web_interactive() -> Result<OAuthToken> {
    let token = client(WEB_CLIENT_ID)?
        .get_access_token()
        .context("OAuth login failed")?;
    config::save_auth(&config::SavedAuth {
        client_id: WEB_CLIENT_ID.to_string(),
        refresh_token: token.refresh_token.clone(),
    })?;
    Ok(token)
}

/// Interactive browser login for the librespot session (keymaster client ID).
/// Only needed when no reusable credentials are cached yet; librespot stores
/// credentials after the first successful connect.
pub fn login_session_interactive() -> Result<OAuthToken> {
    client(SESSION_CLIENT_ID)?
        .get_access_token()
        .context("OAuth login for playback session failed")
}

/// Exchange the stored refresh token for a fresh Web API access token.
/// Blocking — call from spawn_blocking inside the TUI.
pub fn refresh_web(refresh_token: &str) -> Result<OAuthToken> {
    let mut token = client(WEB_CLIENT_ID)?
        .refresh_token(refresh_token)
        .context("token refresh failed")?;
    // Spotify may omit the refresh token from a refresh response (librespot
    // maps that to an empty string); the previous one is then still valid,
    // so keep it rather than persisting an unusable empty token.
    if token.refresh_token.is_empty() {
        token.refresh_token = refresh_token.to_string();
    }
    // It may also rotate: persist whichever token is current.
    config::save_auth(&config::SavedAuth {
        client_id: WEB_CLIENT_ID.to_string(),
        refresh_token: token.refresh_token.clone(),
    })?;
    Ok(token)
}

/// Get a usable Web API token: refresh if we have saved credentials for the
/// current client ID, otherwise run the interactive browser flow.
pub fn obtain_web_token() -> Result<OAuthToken> {
    if let Some(saved) = config::load_auth() {
        if saved.client_id == WEB_CLIENT_ID {
            match refresh_web(&saved.refresh_token) {
                Ok(tok) => return Ok(tok),
                Err(e) => {
                    log::warn!("token refresh failed ({e:#}), falling back to interactive login");
                    config::clear_auth();
                }
            }
        } else {
            // Token from an older build using a different client ID.
            config::clear_auth();
        }
    }
    login_web_interactive()
}

/// Convert a librespot OAuthToken into an rspotify Token so the Web API
/// client can use the same credentials.
pub fn to_rspotify_token(tok: &OAuthToken) -> rspotify::Token {
    let remaining = tok.expires_at.saturating_duration_since(Instant::now());
    let expires_in =
        chrono::Duration::from_std(remaining).unwrap_or_else(|_| chrono::Duration::seconds(3600));
    rspotify::Token {
        access_token: tok.access_token.clone(),
        expires_in,
        expires_at: Some(chrono::Utc::now() + expires_in),
        refresh_token: Some(tok.refresh_token.clone()),
        scopes: tok.scopes.iter().cloned().collect::<HashSet<_>>(),
    }
}
