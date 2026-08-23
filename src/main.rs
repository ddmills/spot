mod api;
mod app;
mod audio_sink;
mod audio_tap;
mod auth;
mod client;
mod config;
#[cfg(windows)]
mod console_ctrl;
mod cover;
mod event;
mod radio;
#[cfg(windows)]
mod relaunch;
mod session;
mod ui;
mod viz;

use std::fs::File;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, EventStream};
use crossterm::terminal::SetTitle;
use futures::StreamExt;
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use parking_lot::RwLock;
use tokio::sync::mpsc::{self, UnboundedSender};
use unicode_width::UnicodeWidthChar;

use crate::api::Api;
use crate::app::command::AppCommand;
use crate::app::state::{self, AppState, LocalPlayback};
use crate::client::Client;

/// Below this the browse pane starts shedding columns and art; the UI still
/// draws, it just has less to say.
const MIN_COLS: u16 = 80;
const MIN_ROWS: u16 = 24;

/// How long the quit path waits for the client to silence both engines.
/// Generous, because it is only ever spent when something is already wrong —
/// a healthy shutdown answers in milliseconds.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // Before anything else touches the disk: a double-clicked spot hands
    // itself to Windows Terminal and this process is done. Doing it first
    // keeps the short-lived parent from truncating the log the child is about
    // to write.
    #[cfg(windows)]
    if relaunch::relaunch_in_windows_terminal() {
        return std::process::ExitCode::SUCCESS;
    }

    // Hard-exit rather than returning, on both paths below. Returning from
    // `main` drops the `#[tokio::main]` runtime, and that drop joins
    // librespot's player thread — which can be parked inside a sink write on a
    // device that has stopped draining. When it was, spot never exited: the
    // terminal was restored, the window was closed, and a detached radio thread
    // went on streaming from a process nobody could see. `run` has already
    // stopped both engines by this point, so there is nothing left to tear down
    // that is worth risking that on.
    match run().await {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            // Everything that can fail here fails before the TUI starts, or
            // after it has restored the terminal, so plain printing is safe.
            // The wait matters when spot was double-clicked from Explorer:
            // the console it opened closes with the process, and the error
            // would be gone before it could be read.
            eprintln!("\nspot: {e:#}");
            eprintln!("\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
            std::process::exit(1)
        }
    }
}

async fn run() -> Result<()> {
    init_logging()?;
    // Installed before anything can open an audio device, so closing the
    // window is never the one quit path that leaves a station playing.
    #[cfg(windows)]
    console_ctrl::install();
    warn_about_terminal();

    // Auth and session setup happen before the TUI takes the terminal, so
    // the OAuth flow can print instructions and block on the browser.
    // ASCII only: this prints before the TUI starts, possibly to a legacy
    // console codepage.
    if config::load_auth().is_none() {
        println!(
            "first run: a browser window will open so you can sign in to Spotify.\n\
             it happens twice - once for your library, once for playback. after\n\
             that spot signs itself in.\n"
        );
    }
    println!("spot - authenticating with Spotify...");
    let token = tokio::task::spawn_blocking(auth::obtain_web_token)
        .await
        .context("auth task panicked")??;

    println!(
        "starting playback engine (Connect device \"{}\")...",
        config::DEVICE_NAME
    );
    let (session, spirc, spirc_task, audio_tap, mixer, player_events) = session::build().await?;
    let _spirc_join = tokio::spawn(spirc_task);

    let api = Api::new(
        auth::to_rspotify_token(&token),
        session.device_id().to_string(),
    );
    tokio::spawn(token_refresh_loop(api.clone(), token.expires_at));

    let state = Arc::new(RwLock::new(AppState::new()));
    {
        let mut st = state.write();
        st.audio_tap = Arc::clone(&audio_tap);
        // Read before the first frame, so Home's Radio row can say how many
        // stations are behind it without waiting on anything.
        st.radio_favorites = config::load_radio();
    }
    let (tx, rx) = mpsc::unbounded_channel();
    // Playback truth for our own device, ahead of the Web API poll by a
    // second or more: librespot says when it really started and stopped.
    let local = Arc::new(LocalPlayback::default());
    tokio::spawn(player_event_loop(
        player_events,
        Arc::clone(&state),
        Arc::clone(&local),
        tx.clone(),
    ));
    // The radio player writes into the same tap librespot's sink does, so the
    // visualizer follows whichever engine is playing. The client is also the
    // only holder of the handles that can stop either engine, so quitting has
    // to ask it and wait for the answer — see the shutdown block below.
    let (client, shutdown_done) =
        Client::new(api, spirc, mixer, local, Arc::clone(&state), rx, audio_tap);
    tokio::spawn(client.run());

    let _ = tx.send(AppCommand::LoadPlaylists);
    let _ = tx.send(AppCommand::RefreshPlayback);

    let terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let result = run_tui(terminal, state, tx.clone()).await;

    // Before the terminal comes back, not after: the audio has to stop while
    // the UI still says spot is running, or quitting looks finished while a
    // station is still playing. `main` hard-exits once this returns, so this is
    // the only chance either engine gets to stop cleanly.
    if tx.send(AppCommand::Shutdown).is_ok() {
        // A timeout rather than a plain await: a client task that is stuck on
        // a network call must not strand the quit. The exit in `main` is what
        // actually guarantees silence; this is what makes it graceful.
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, shutdown_done)
            .await
            .is_err()
        {
            log::warn!("the client did not finish shutting down in time");
        }
    }

    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, SetTitle(""));
    ratatui::restore();
    result
}

async fn run_tui(
    mut terminal: ratatui::DefaultTerminal,
    state: Arc<RwLock<AppState>>,
    tx: UnboundedSender<AppCommand>,
) -> Result<()> {
    // The player view animates its visualizer, so it redraws at ~20 fps;
    // everywhere else the slow tick keeps the app idle-cheap.
    const TICK_SLOW: Duration = Duration::from_millis(250);
    const TICK_FAST: Duration = Duration::from_millis(50);
    // How recently PCM must have arrived for the tick to stay fast. Wider
    // than [`TICK_SLOW`] on purpose: at the slow tick the loop only looks
    // every 250 ms, and a window that narrow could fall between two samples
    // and drop back to the slow tick under a record that is still playing.
    const AUDIO_LIVE: Duration = Duration::from_millis(600);

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK_SLOW);
    let mut fast = false;
    let mut last_title = String::new();
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            maybe_event = events.next() => match maybe_event {
                Some(Ok(ev)) => event::handle_event(ev, &state, &tx),
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
        }

        let mut st = state.write();
        if st.should_quit {
            break;
        }
        let title = window_title(st.playback.as_ref(), st.radio.as_ref());
        if title != last_title {
            let _ = crossterm::execute!(std::io::stdout(), SetTitle(&title));
            last_title = title;
        }
        terminal.draw(|frame| ui::draw(frame, &mut st))?;
        // The nav row's dot rides the audio's loudness on every screen, so
        // audio arriving is reason enough for the fast tick — at 250 ms it
        // would visibly step rather than breathe. Nothing playing still costs
        // four wakeups a second.
        let want_fast = st.show_player || st.audio_tap.is_fresh(AUDIO_LIVE);
        drop(st);
        // Rebuild outside the select! arm, where `tick` isn't borrowed.
        if want_fast != fast {
            fast = want_fast;
            tick = tokio::time::interval(if fast { TICK_FAST } else { TICK_SLOW });
        }
    }
    Ok(())
}

/// Warn, before the alternate screen hides the console, about the two terminal
/// properties the UI assumes.
///
/// Both are warnings rather than errors: the app still runs, it just looks
/// wrong, and refusing to start on a heuristic would be worse than a garbled
/// first frame. ASCII only — this may reach a legacy console codepage.
fn warn_about_terminal() {
    let mut warned = false;

    // The whole palette is truecolor (see ui::theme) and album art is drawn as
    // per-cell RGB half-blocks, so a 16-color terminal has nothing to fall
    // back to. Windows Terminal advertises itself through WT_SESSION; other
    // capable terminals generally set COLORTERM.
    let truecolor = std::env::var_os("WT_SESSION").is_some()
        || std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false);
    if !truecolor {
        println!(
            "warning: this terminal does not advertise 24-bit color. spot's colors\n\
             and album art need it - Windows Terminal is the safe choice:\n\
             https://aka.ms/terminal"
        );
        warned = true;
    }

    if let Ok((w, h)) = crossterm::terminal::size()
        && (w < MIN_COLS || h < MIN_ROWS)
    {
        println!(
            "warning: the window is {w}x{h}; spot expects at least {MIN_COLS}x{MIN_ROWS}.\n\
             Columns and album art will be dropped to fit."
        );
        warned = true;
    }

    if warned {
        // Long enough to read before the alternate screen takes over.
        std::thread::sleep(Duration::from_millis(2500));
    }
}

/// Window/tab title mirroring the now-playing line. Track metadata is
/// unrestricted remote data: control characters would terminate or corrupt
/// the OSC title sequence, so they are stripped, and the whole thing is
/// capped to a title-bar-friendly width.
fn window_title(
    playback: Option<&app::state::PlaybackSnapshot>,
    radio: Option<&app::state::RadioPlayback>,
) -> String {
    const MAX_WIDTH: usize = 80;
    // Radio wins, for the same reason the deck draws it first: while a station
    // is on, the Spotify snapshot is kept but paused, and naming it in the
    // taskbar would point at the wrong sound.
    let full = match (radio, playback) {
        // Spotify's spelling of the record where there is one, so the title
        // bar and the deck say the same thing; the station's own words where
        // there is not.
        (Some(r), _) => match (r.matched_track(), r.now_title()) {
            (Some(t), _) => format!("♫ {} — {} · {}", t.name, t.artists, r.station.name),
            (None, Some(title)) => format!("♫ {title} — {}", r.station.name),
            (None, None) => format!("♫ {}", r.station.name),
        },
        (None, Some(pb)) => format!("♫ {} — {}", pb.track_name, pb.artists),
        (None, None) => return "spot".to_string(),
    };
    let mut out = String::new();
    let mut used = 0;
    for c in full.chars().filter(|c| !c.is_control()) {
        let cw = c.width().unwrap_or(0);
        if used + cw > MAX_WIDTH - 1 {
            out.push('…');
            break;
        }
        out.push(c);
        used += cw;
    }
    out
}

/// Follow librespot's own player, which knows what the audio is doing the
/// moment it changes.
///
/// Everything else about playback comes from `/me/player`, polled every three
/// seconds and lagging Spotify's backend besides. That is fine for what is
/// playing and useless for whether it is playing: a pause has to show on the
/// keypress, not a second later, and a snapshot that arrives mid-flight would
/// otherwise flip the pill back.
///
/// It also knows *what* is playing, which is the other half of the same
/// problem: two tracks of one album run into each other with no command in
/// between, so nothing arms the four-hundred-millisecond re-poll and the deck
/// keeps the finished record's title and sleeve until the next three-second
/// tick. `TrackChanged` carries the whole item — name, artists, album, length
/// and artwork — and arrives as the audio does. See
/// [`AppState::track_changed`].
///
/// Only our own device is followed. When something else is playing, librespot
/// is idle and has nothing to say about it.
async fn player_event_loop(
    mut events: PlayerEventChannel,
    state: Arc<RwLock<AppState>>,
    local: Arc<LocalPlayback>,
    tx: UnboundedSender<AppCommand>,
) {
    /// Re-anchor progress on a snapshot, but only if it describes our device.
    fn anchor(pb: &mut state::PlaybackSnapshot, position_ms: u32) {
        pb.progress_ms = u64::from(position_ms).min(pb.duration_ms);
        pb.fetched_at = Instant::now();
    }

    /// The track an event is about, as the URI spot spells track ids in.
    ///
    /// `None` for the events that are about the player rather than a track —
    /// a volume change, say, which applies whatever is on.
    fn subject(event: &PlayerEvent) -> Option<String> {
        use PlayerEvent::*;
        match event {
            Stopped { track_id, .. }
            | Loading { track_id, .. }
            | Preloading { track_id }
            | Playing { track_id, .. }
            | Paused { track_id, .. }
            | TimeToPreloadNextTrack { track_id, .. }
            | EndOfTrack { track_id, .. }
            | Unavailable { track_id, .. }
            | PositionCorrection { track_id, .. }
            | PositionChanged { track_id, .. }
            | Seeked { track_id, .. } => track_id.to_uri().ok(),
            _ => None,
        }
    }

    /// The deck's view of a librespot `AudioItem`.
    ///
    /// Episodes and local files go through `UniqueFields` variants that carry
    /// no artist list or album, so those read blank rather than being made up.
    fn now_playing(item: &AudioItem) -> state::NowPlaying {
        let (artists, album, artist_id) = match &item.unique_fields {
            UniqueFields::Track { artists, album, .. } => (
                artists
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                album.clone(),
                // The first credited artist, matching what the poll puts here
                // and what the deck's artist link opens. A bare id, not a URI:
                // the click site builds `spotify:artist:{id}` from it.
                artists.first().and_then(|a| a.id.to_id().ok()),
            ),
            _ => (String::new(), String::new(), None),
        };
        let covers: Vec<(&str, u32)> = item
            .covers
            .iter()
            .map(|c| (c.url.as_str(), c.width.max(0).min(c.height.max(0)) as u32))
            .collect();
        state::NowPlaying {
            track_uri: item.uri.clone(),
            track_name: item.name.clone(),
            artists,
            album,
            artist_id,
            duration_ms: u64::from(item.duration_ms),
            cover_url: cover::pick_sized(&covers),
        }
    }

    while let Some(event) = events.recv().await {
        // librespot is the authority on what is playing, and this is it saying
        // so — including when it is not what was asked for, which is what
        // shuffle does to a play that named a track.
        if let PlayerEvent::TrackChanged { audio_item } = &event {
            let now = now_playing(audio_item);
            let art = now.cover_url.clone();
            state.write().track_changed(now);
            // The client owns the fetch and its cache; it skips the work when
            // the sleeve is already up, which two tracks of one album is.
            let _ = tx.send(AppCommand::LoadPlayingCover { cover_url: art });
            continue;
        }

        // Across a switch, events about the track being left are still in
        // flight — starting with the `Paused` our own pause caused, carrying
        // that track's position. Applying those would undo the deck the click
        // just painted. Only those are dropped: anything else is news, and an
        // earlier version of this dropped everything that was not the track we
        // had asked for, which left the deck describing our guess while
        // something else played.
        {
            let st = state.read();
            if let (Some(pending), Some(about)) = (&st.pending_play, subject(&event)) {
                let leaving = pending.prev_uri.as_deref() == Some(about.as_str());
                let expected = pending.expect_uri.as_deref() == Some(about.as_str());
                if leaving && !expected {
                    continue;
                }
            }
        }

        let playing = match &event {
            PlayerEvent::Playing { .. } => Some(true),
            PlayerEvent::Paused { .. } | PlayerEvent::Stopped { .. } => Some(false),
            _ => None,
        };
        if let Some(playing) = playing {
            local.set_playing(playing);
        }

        let mut st = state.write();
        let Some(pb) = st.playback.as_mut().filter(|pb| pb.is_local_device) else {
            continue;
        };
        match event {
            PlayerEvent::Playing { position_ms, .. } => {
                pb.is_playing = true;
                anchor(pb, position_ms);
            }
            PlayerEvent::Paused { position_ms, .. } => {
                pb.is_playing = false;
                anchor(pb, position_ms);
            }
            PlayerEvent::Stopped { .. } => pb.is_playing = false,
            PlayerEvent::Seeked { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. } => anchor(pb, position_ms),
            // Covers our own volume keys and a change sent from another
            // Connect client, which Spirc applies to the mixer the same way.
            PlayerEvent::VolumeChanged { volume } => {
                pb.volume_percent = client::raw_to_pct(volume);
            }
            _ => {}
        }
    }
}

/// Keep the Web API token fresh for the lifetime of the app. The librespot
/// session refreshes its own credentials internally.
async fn token_refresh_loop(api: Api, mut expires_at: Instant) {
    loop {
        let wait = expires_at
            .saturating_duration_since(Instant::now())
            .saturating_sub(Duration::from_secs(120));
        tokio::time::sleep(wait).await;

        let Some(saved) = config::load_auth() else {
            log::error!("no saved refresh token; cannot refresh");
            return;
        };
        let refreshed =
            tokio::task::spawn_blocking(move || auth::refresh_web(&saved.refresh_token)).await;
        match refreshed {
            Ok(Ok(tok)) => {
                expires_at = tok.expires_at;
                api.update_token(auth::to_rspotify_token(&tok)).await;
                log::info!("access token refreshed");
            }
            Ok(Err(e)) => {
                log::error!("token refresh failed: {e:#}; retrying in 60s");
                expires_at = Instant::now() + Duration::from_secs(180);
            }
            Err(e) => {
                log::error!("refresh task panicked: {e}");
                return;
            }
        }
    }
}

fn init_logging() -> Result<()> {
    let log_path = config::log_file()?;
    // rspotify logs full request headers (Bearer token included) at INFO;
    // keep its output out of the log file entirely.
    let log_config = simplelog::ConfigBuilder::new()
        .add_filter_ignore_str("rspotify")
        .build();
    simplelog::WriteLogger::init(log::LevelFilter::Info, log_config, File::create(log_path)?)?;
    Ok(())
}
