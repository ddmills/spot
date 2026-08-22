use std::future::Future;
use std::sync::Arc;

use anyhow::{Context, Result};
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::{DeviceType, SessionConfig};
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, MixerConfig};
use librespot_playback::player::Player;

use crate::audio_tap::{AudioTap, TapSink};
use crate::{auth, config};

/// Turn a failed Connect handshake into something a first-time user can act on.
///
/// The two failures worth naming are a non-Premium account and stale cached
/// credentials; librespot reports both as a login failure whose reason only
/// appears in the message text, so that is what we match on. Anything else
/// keeps the generic wrapper and the original error underneath.
fn explain_connect_failure(e: librespot_core::Error) -> anyhow::Error {
    let detail = e.to_string();
    let hint = if detail.contains("Premium account required") {
        "\nspot streams audio itself through librespot, which Spotify only \
         permits for Premium accounts."
    } else if detail.contains("Bad credentials") || detail.contains("validate credentials") {
        "\nThe saved login is no longer valid. Delete the `creds` file in \
         %LOCALAPPDATA%\\spot and start spot again to log in fresh."
    } else {
        ""
    };
    anyhow::anyhow!("failed to start Spotify Connect device: {detail}{hint}")
}

/// Build the librespot session, audio player and Spirc Connect device.
/// Returns the session, the Spirc control handle, the Spirc event-loop
/// future (which the caller must spawn), and the PCM tap feeding the
/// visualizer.
///
/// Credentials come from librespot's cache when available (stored on the
/// first successful connect); otherwise this runs a one-time interactive
/// OAuth flow with the keymaster client ID.
pub async fn build() -> Result<(
    Session,
    Spirc,
    impl Future<Output = ()> + use<>,
    Arc<AudioTap>,
)> {
    let cache_root = config::cache_dir()?;
    let cache = Cache::new(
        Some(cache_root.join("creds")),
        Some(cache_root.join("volume")),
        Some(cache_root.join("audio")),
        // Cap the audio file cache at 2 GiB.
        Some(2 * 1024 * 1024 * 1024),
    )
    .context("failed to create librespot cache")?;

    let credentials = match cache.credentials() {
        Some(creds) => creds,
        None => {
            println!("first run: authorizing the playback engine...");
            let token = tokio::task::spawn_blocking(auth::login_session_interactive)
                .await
                .context("auth task panicked")??;
            Credentials::with_access_token(token.access_token)
        }
    };

    let session_config = SessionConfig::default(); // keymaster client_id on desktop
    let session = Session::new(session_config, Some(cache));

    let mixer_fn = mixer::find(None).context("no mixer available")?;
    let mixer = mixer_fn(MixerConfig::default()).context("failed to open mixer")?;

    let backend = audio_backend::find(None).context("no audio backend available")?;
    let tap = Arc::new(AudioTap::new());
    let sink_tap = Arc::clone(&tap);
    // Second soft-volume handle for the tap, so it can undo the attenuation
    // the player applies before samples reach the sink.
    let tap_volume = mixer.get_soft_volume();
    let player = Player::new(
        PlayerConfig::default(),
        session.clone(),
        mixer.get_soft_volume(),
        move || {
            Box::new(TapSink::new(
                backend(None, AudioFormat::default()),
                sink_tap,
                tap_volume,
            ))
        },
    );

    let connect_config = ConnectConfig {
        name: config::DEVICE_NAME.to_string(),
        device_type: DeviceType::Computer,
        ..Default::default()
    };

    let (spirc, spirc_task) = Spirc::new(
        connect_config,
        session.clone(),
        credentials,
        player,
        Arc::clone(&mixer),
    )
    .await
    .map_err(explain_connect_failure)?;

    Ok((session, spirc, spirc_task, tap))
}
