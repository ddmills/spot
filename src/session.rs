use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_playback::config::PlayerConfig;
use librespot_playback::mixer::{self, Mixer, MixerConfig};
use librespot_playback::player::{Player, PlayerEventChannel};

use crate::audio_sink::SpotSink;
use crate::audio_tap::{AudioTap, TapSink};
use crate::{auth, client, config};

/// How often the player reports its position while playing. This is what
/// keeps the progress bar anchored to the audio rather than to a guess.
const POSITION_UPDATE: Duration = Duration::from_millis(500);

/// Turn a failed session login into something a first-time user can act on.
///
/// The two failures worth naming are a non-Premium account and stale cached
/// credentials; librespot reports both as a login failure whose reason only
/// appears in the message text, so that is what we match on. Anything else
/// keeps the generic wrapper and the original error underneath.
fn explain_session_failure(e: librespot_core::Error) -> anyhow::Error {
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
    anyhow::anyhow!("failed to start the playback engine: {detail}{hint}")
}

/// Build the librespot session and the audio player spot drives directly.
/// Returns the session, the player, the PCM tap feeding the visualizer, the
/// soft mixer, and the player's event channel.
///
/// There is no Spirc and no Connect device: spot owns the queue and the
/// transport, so the session only has to stream audio and metadata. The
/// mixer holds the volume actually being applied, and the player events say
/// when playback really started, stopped or moved — all local and immediate.
///
/// Credentials come from librespot's cache when available (stored on the
/// first successful connect); otherwise this runs a one-time interactive
/// OAuth flow with the keymaster client ID.
pub async fn build() -> Result<(
    Session,
    Arc<Player>,
    Arc<AudioTap>,
    Arc<dyn Mixer>,
    PlayerEventChannel,
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

    // The volume the last session left, before the session is built — the
    // cache moves into it. Spirc used to restore this; now it is ours to do.
    let saved_volume = cache
        .volume()
        .unwrap_or_else(|| client::pct_to_raw(client::DEFAULT_VOLUME_PCT));

    let session_config = SessionConfig::default(); // keymaster client_id on desktop
    let session = Session::new(session_config, Some(cache));

    // The same login Spirc used to make on spot's behalf. Without it the
    // session has no connection for the player to fetch audio over.
    session
        .connect(credentials, true)
        .await
        .map_err(explain_session_failure)?;

    let mixer_fn = mixer::find(None).context("no mixer available")?;
    let mixer = mixer_fn(MixerConfig::default()).context("failed to open mixer")?;
    mixer.set_volume(saved_volume);

    let tap = Arc::new(AudioTap::new());
    let sink_tap = Arc::clone(&tap);
    // Second soft-volume handle for the tap, so it can undo the attenuation
    // the player applies before samples reach the sink.
    let tap_volume = mixer.get_soft_volume();
    let player_config = PlayerConfig {
        // Progress events while playing; the default is never to send them.
        position_update_interval: Some(POSITION_UPDATE),
        ..Default::default()
    };
    let player = Player::new(
        player_config,
        session.clone(),
        mixer.get_soft_volume(),
        move || {
            // The sink is built on librespot's own player thread and the
            // device is only opened here, so a failure has nowhere to be
            // returned to; librespot's own backends give up in the same place
            // for the same reason.
            let backend = SpotSink::open().expect("failed to open an audio output device");
            Box::new(TapSink::new(Box::new(backend), sink_tap, tap_volume))
        },
    );

    let player_events = player.get_player_event_channel();

    Ok((session, player, tap, mixer, player_events))
}
