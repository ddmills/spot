mod api;
mod app;
mod audio_sink;
mod audio_tap;
mod auth;
mod client;
mod config;
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

    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Everything that can fail here fails before the TUI starts, or
            // after it has restored the terminal, so plain printing is safe.
            // The wait matters when spot was double-clicked from Explorer:
            // the console it opened closes with the process, and the error
            // would be gone before it could be read.
            eprintln!("\nspot: {e:#}");
            eprintln!("\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    init_logging()?;
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
    ));
    // The radio player writes into the same tap librespot's sink does, so the
    // visualizer follows whichever engine is playing.
    tokio::spawn(Client::new(api, spirc, mixer, local, Arc::clone(&state), rx, audio_tap).run());

    let _ = tx.send(AppCommand::LoadPlaylists);
    let _ = tx.send(AppCommand::RefreshPlayback);

    let terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let result = run_tui(terminal, state, tx).await;
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
        let want_fast = st.show_player;
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
        (Some(r), _) => match r.now_title() {
            Some(title) => format!("♫ {title} — {}", r.station.name),
            None => format!("♫ {}", r.station.name),
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
/// Only our own device is followed. When something else is playing, librespot
/// is idle and has nothing to say about it.
async fn player_event_loop(
    mut events: PlayerEventChannel,
    state: Arc<RwLock<AppState>>,
    local: Arc<LocalPlayback>,
) {
    /// Re-anchor progress on a snapshot, but only if it describes our device.
    fn anchor(pb: &mut state::PlaybackSnapshot, position_ms: u32) {
        pb.progress_ms = u64::from(position_ms).min(pb.duration_ms);
        pb.fetched_at = Instant::now();
    }

    while let Some(event) = events.recv().await {
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
