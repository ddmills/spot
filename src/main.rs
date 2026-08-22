mod api;
mod app;
mod audio_tap;
mod auth;
mod client;
mod config;
mod cover;
mod event;
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
use parking_lot::RwLock;
use tokio::sync::mpsc::{self, UnboundedSender};
use unicode_width::UnicodeWidthChar;

use crate::api::Api;
use crate::app::command::AppCommand;
use crate::app::state::AppState;
use crate::client::Client;

#[tokio::main]
async fn main() -> Result<()> {
    init_logging()?;

    // Auth and session setup happen before the TUI takes the terminal, so
    // the OAuth flow can print instructions and block on the browser.
    // ASCII only: this prints before the TUI starts, possibly to a legacy
    // console codepage.
    println!("spot - authenticating with Spotify...");
    let token = tokio::task::spawn_blocking(auth::obtain_web_token)
        .await
        .context("auth task panicked")??;

    println!(
        "starting playback engine (Connect device \"{}\")...",
        config::DEVICE_NAME
    );
    let (session, spirc, spirc_task, audio_tap) = session::build().await?;
    let _spirc_join = tokio::spawn(spirc_task);

    let api = Api::new(
        auth::to_rspotify_token(&token),
        session.device_id().to_string(),
    );
    tokio::spawn(token_refresh_loop(api.clone(), token.expires_at));

    let state = Arc::new(RwLock::new(AppState::new()));
    state.write().audio_tap = audio_tap;
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(Client::new(api, spirc, Arc::clone(&state), rx).run());

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
        let title = window_title(st.playback.as_ref());
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

/// Window/tab title mirroring the now-playing line. Track metadata is
/// unrestricted remote data: control characters would terminate or corrupt
/// the OSC title sequence, so they are stripped, and the whole thing is
/// capped to a title-bar-friendly width.
fn window_title(playback: Option<&app::state::PlaybackSnapshot>) -> String {
    const MAX_WIDTH: usize = 80;
    let Some(pb) = playback else {
        return "spot".to_string();
    };
    let full = format!("♫ {} — {}", pb.track_name, pb.artists);
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
