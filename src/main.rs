mod api;
mod app;
mod audio_sink;
mod audio_tap;
mod auth;
mod cli;
mod client;
mod clipboard;
mod config;
#[cfg(windows)]
mod console_ctrl;
mod cover;
mod event;
#[cfg(windows)]
mod ipc;
mod link;
#[cfg(windows)]
mod protocol;
mod radio;
#[cfg(windows)]
mod relaunch;
mod session;
mod ui;
mod update;
mod viz;

use std::fs::File;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, EventStream};
use crossterm::terminal::SetTitle;
use futures::StreamExt;
use librespot_core::cache::Cache;
use librespot_playback::mixer::Mixer;
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use parking_lot::RwLock;
use tokio::sync::mpsc::{self, UnboundedSender};
use unicode_width::UnicodeWidthChar;

use crate::api::Api;
use crate::app::command::AppCommand;
use crate::app::state::{AppState, SpotifyState};
use crate::audio_tap::AudioTap;
use crate::client::{Client, Engine, Spotify};

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
    // Answered before anything else, and before the relaunch below: every one
    // of these prints and exits, and a new Windows Terminal window would close
    // with the process, taking what it printed with it.
    let link = match answer_the_command_line() {
        Ok(link) => link,
        Err(code) => return code,
    };

    // A link belongs to the spot that is already running: a second player
    // would fight this one for the audio device and for the Spotify session.
    // Only a launch carrying a link asks — a second window opened on purpose
    // is still the user's to open.
    #[cfg(windows)]
    if let Some(target) = &link
        && ipc::forward(target).await
    {
        return std::process::ExitCode::SUCCESS;
    }

    // Before anything else touches the disk: a double-clicked spot hands
    // itself to Windows Terminal and this process is done. Doing it first
    // keeps the short-lived parent from truncating the log the child is about
    // to write. The link goes with it, or a link clicked into a legacy console
    // would be lost on the way across.
    #[cfg(windows)]
    if relaunch::relaunch_in_windows_terminal(link.as_ref()) {
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
    match run(link).await {
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

/// Do whatever the command line asked that is not "start spot", and say
/// whether there is anything left to do.
///
/// `Ok` carries the link to follow, if one came with the launch. `Err` carries
/// the code to exit with: the work is over, and it was done here.
///
/// This is the only place the protocol registration is reached from other than
/// the Home row, which is what keeps a claim on the `spotify:` scheme to an
/// act the user asked for — see [`crate::protocol`].
fn answer_the_command_line() -> Result<Option<link::Link>, std::process::ExitCode> {
    use std::process::ExitCode;

    match cli::parse(std::env::args().skip(1)) {
        cli::Invocation::Run(link) => Ok(link),
        cli::Invocation::Help => {
            println!("{}", cli::HELP);
            Err(ExitCode::SUCCESS)
        }
        cli::Invocation::Version => {
            println!("spot {}", env!("CARGO_PKG_VERSION"));
            Err(ExitCode::SUCCESS)
        }
        cli::Invocation::Rejected(why) => {
            eprintln!("spot: {why}");
            Err(ExitCode::FAILURE)
        }
        #[cfg(windows)]
        cli::Invocation::Register { force } => Err(register_protocol(force)),
        #[cfg(windows)]
        cli::Invocation::Unregister => Err(unregister_protocol()),
        #[cfg(not(windows))]
        cli::Invocation::Register { .. } | cli::Invocation::Unregister => {
            eprintln!("spot: only Windows routes Spotify links to an app.");
            Err(ExitCode::FAILURE)
        }
    }
}

/// ASCII only on both of these: they may reach a legacy console codepage.
#[cfg(windows)]
fn register_protocol(force: bool) -> std::process::ExitCode {
    match protocol::register(force) {
        Ok(now) if now.in_force() => {
            println!(
                "spot now opens Spotify links.\n\n\
                 To give them back, run spot {} or use the Links row on Home.",
                cli::UNREGISTER
            );
            std::process::ExitCode::SUCCESS
        }
        // Written, but Windows' own default-apps choice outranks it. Saying
        // this plainly is the whole point: a registration that silently does
        // nothing is the worst outcome here.
        Ok(now) => {
            println!(
                "spot registered itself, but {}.\n\n\
                 Windows keeps the answer you gave it once, and no app can change\n\
                 that on your behalf. Open Settings > Apps > Default apps, find\n\
                 spot, and set it for the SPOTIFY link type.",
                now.describe()
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "spot: {e:#}.\n\n\
                 Nothing was changed. Add --force to replace it anyway; spot keeps\n\
                 what it replaced and puts it back on {}.",
                cli::UNREGISTER
            );
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn unregister_protocol() -> std::process::ExitCode {
    match protocol::unregister() {
        Ok(()) => {
            println!(
                "spot no longer opens Spotify links. {}.",
                protocol::status().describe()
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("spot: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The audio path that exists whether or not Spotify does: the on-disk cache,
/// the soft mixer, and the PCM tap the visualizer reads.
///
/// Kept together because a sign-in needs all three to build the streaming
/// engine, and the frame loop carries them only to hand them on.
#[derive(Clone)]
struct AudioPath {
    cache: Cache,
    mixer: Arc<dyn Mixer>,
    tap: Arc<AudioTap>,
}

impl AudioPath {
    fn open() -> Result<Self> {
        let (cache, mixer, tap) = session::audio()?;
        Ok(Self { cache, mixer, tap })
    }
}

async fn run(link: Option<link::Link>) -> Result<()> {
    init_logging()?;
    // Only ever a repair, and only for a registration the user already asked
    // for: spot is one portable file that gets moved, and a handler naming a
    // path that no longer holds it opens nothing. See [`crate::protocol`].
    #[cfg(windows)]
    protocol::repair_path();
    // Installed before anything can open an audio device, so closing the
    // window is never the one quit path that leaves a station playing.
    #[cfg(windows)]
    console_ctrl::install();
    warn_about_terminal();

    // No account is needed to reach the first frame. These three are the whole
    // of the audio path radio plays through, and none of them needs a login;
    // Spotify is connected from Home afterwards, or never.
    let audio = AudioPath::open()?;

    let state = Arc::new(RwLock::new(AppState::new()));
    {
        let mut st = state.write();
        st.audio_tap = Arc::clone(&audio.tap);
        // Read before the first frame, so Home's Links row says where a
        // clicked Spotify link goes rather than saying nothing until it is
        // pressed. A read, never a write — see [`crate::protocol`].
        #[cfg(windows)]
        protocol::refresh(&mut st);
        // Read before the first frame, so Home's Radio row can say how many
        // stations are behind it without waiting on anything.
        st.radio_favorites = config::load_radio();
        // Read before the first `playlists()` call, so a warm start has the
        // add-to-playlist box's marks before the box can be opened. What is
        // stale is dropped by `load_playlists` when the snapshots come back.
        st.playlist_tracks = config::load_playlist_tracks();
        // A saved refresh token is a sign-in that has already happened. It is
        // spent in the background below: no browser, and nothing to press.
        if config::load_auth().is_some() {
            st.spotify = SpotifyState::Connecting;
        }
    }
    let (tx, rx) = mpsc::unbounded_channel();
    // The radio player writes into the same tap librespot's sink does, so the
    // visualizer follows whichever engine is playing. The client is also the
    // only holder of the handles that can stop either engine, so quitting has
    // to ask it and wait for the answer — see the shutdown block below.
    let (client, shutdown_done) = Client::new(
        audio.cache.clone(),
        Arc::clone(&audio.mixer),
        Arc::clone(&state),
        rx,
        tx.clone(),
        Arc::clone(&audio.tap),
    );
    tokio::spawn(client.run());

    // The executable a previous run replaced is still on disk, because Windows
    // would not let that run delete the image it was executing. This one can.
    update::clean_previous();
    let _ = tx.send(AppCommand::CheckForUpdate);

    // The link this launch carried, and every link a later launch hands over.
    // Both queue behind the sign-in and are served the moment Spotify is up.
    if let Some(target) = link {
        let _ = tx.send(AppCommand::OpenLink(target));
    }
    #[cfg(windows)]
    ipc::listen(tx.clone());

    if state.read().spotify == SpotifyState::Connecting {
        tokio::spawn(connect_spotify(
            Arc::clone(&state),
            tx.clone(),
            audio.clone(),
            false,
        ));
    }

    let terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let result = run_tui(terminal, Arc::clone(&state), tx.clone(), audio).await;
    let restart = state.read().restart_request;

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

    // After the terminal is back and before `main` exits: the new copy needs
    // the console this one is giving up, and it must not start while the old
    // one still owns the audio device.
    if restart {
        #[cfg(windows)]
        relaunch::restart();
    }
    result
}

/// Sign in to Spotify and hand what it got to the client.
///
/// Two halves and two outcomes. The Web API comes first and any account has
/// it: a station's announcement is looked up in it, so a free account still
/// gets its records named. The streaming engine comes second and only Premium
/// has it, which is what tells [`SpotifyState::Ready`] from
/// [`SpotifyState::Limited`].
///
/// `interactive` decides whether a browser may open. A run that already has a
/// refresh token spends it silently at startup; only the Home row asks for a
/// login the user has to answer.
async fn connect_spotify(
    state: Arc<RwLock<AppState>>,
    tx: UnboundedSender<AppCommand>,
    audio: AudioPath,
    interactive: bool,
) {
    state.write().spotify = SpotifyState::Connecting;

    let token = if interactive {
        tokio::task::spawn_blocking(auth::obtain_web_token).await
    } else {
        let Some(saved) = config::load_auth() else {
            state.write().spotify = SpotifyState::Off;
            return;
        };
        tokio::task::spawn_blocking(move || auth::refresh_web(&saved.refresh_token)).await
    };
    let token = match token {
        Ok(Ok(token)) => token,
        Ok(Err(e)) => {
            log::error!("Spotify sign-in failed: {e:#}");
            let mut st = state.write();
            st.spotify = SpotifyState::Off;
            st.toast(format!("sign-in failed: {e}"));
            return;
        }
        Err(e) => {
            log::error!("sign-in task panicked: {e}");
            state.write().spotify = SpotifyState::Off;
            return;
        }
    };

    let api = Api::new(auth::to_rspotify_token(&token));
    tokio::spawn(token_refresh_loop(api.clone(), token.expires_at));

    // Spotify has been withdrawing the subscription level from the account
    // endpoint. When it reports one, a free account is spared a browser window
    // and a login that would only be refused; when it reports none, the login
    // itself is the test.
    //
    // The id off the same answer is what the Playlists page tells your own
    // playlists from the ones you follow by.
    let premium = match api.account().await {
        Ok(account) => {
            state.write().me_id = Some(account.id);
            account.premium
        }
        Err(e) => {
            log::warn!("could not read the account: {e:#}");
            None
        }
    };
    // The login is only attempted where a refusal cannot cost the terminal.
    // librespot 0.8's `Session::check_catalogue` calls `process::exit(1)` for
    // an account it will not stream for, and nothing upstream of it can
    // intervene — so the attempt is made either with the console already given
    // back (the Home row, see [`sign_in`]) or on cached credentials, which
    // exist only because this account has streamed here before. It is the
    // level above that decides in practice; this is what stands if Spotify
    // stops reporting one.
    let may_connect = interactive || audio.cache.credentials().is_some();
    let (engine, limit) = if premium == Some(false) {
        (None, Some("no Premium".to_string()))
    } else if !may_connect {
        (None, Some("sign in to play".to_string()))
    } else {
        let login = if interactive {
            session::Login::Interactive
        } else {
            session::Login::Saved
        };
        match session::connect(&audio.cache, &audio.mixer, &audio.tap, login).await {
            Ok((session, player, events)) => {
                tokio::spawn(player_event_loop(events, Arc::clone(&state), tx.clone()));
                (Some(Engine { session, player }), None)
            }
            Err(e) => {
                log::error!("playback engine unavailable: {e:#}");
                let refused = e.to_string().contains("Premium");
                state.write().toast(format!("no playback: {e}"));
                let reason = if refused { "no Premium" } else { "no playback" };
                (None, Some(reason.to_string()))
            }
        }
    };

    let ready = engine.is_some();
    let _ = tx.send(AppCommand::SpotifyConnected(Spotify { api, engine }));
    state.write().spotify = match limit {
        Some(reason) => SpotifyState::Limited(reason),
        None => SpotifyState::Ready,
    };
    if ready {
        let _ = tx.send(AppCommand::LoadPlaylists);
    }
}

/// Run an interactive sign-in with the terminal handed back to the console.
///
/// The OAuth flow prints the authorization URL and blocks on the browser, so
/// the alternate screen has to go for the length of it — a stray `println!`
/// over a drawn frame scrolls the screen out from under the diff ratatui
/// draws against. Radio plays throughout; only the picture stops.
async fn sign_in(
    terminal: ratatui::DefaultTerminal,
    state: &Arc<RwLock<AppState>>,
    tx: &UnboundedSender<AppCommand>,
    audio: &AudioPath,
) -> ratatui::DefaultTerminal {
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    drop(terminal);
    ratatui::restore();

    // ASCII only: this may reach a legacy console codepage.
    println!(
        "spot - connecting to Spotify.\n\n\
         a browser window opens for your library, and a second one for playback.\n\
         both are Spotify's own login page, and both are one-time. spot comes back\n\
         when they are done.\n"
    );
    connect_spotify(Arc::clone(state), tx.clone(), audio.clone(), true).await;

    let terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    terminal
}

async fn run_tui(
    mut terminal: ratatui::DefaultTerminal,
    state: Arc<RwLock<AppState>>,
    tx: UnboundedSender<AppCommand>,
    audio: AudioPath,
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

        // The whole frame under one lock, and the lock ended before the await
        // below: the sign-in takes seconds, and every other task writes this
        // state.
        let (want_fast, connect) = {
            let mut st = state.write();
            if st.should_quit {
                break;
            }
            // The playing Spotify track is the queue's current row — but only
            // once something has actually played (see `AppState::playback`).
            let playing = st
                .playback
                .as_ref()
                .and(st.queue.as_ref())
                .and_then(|q| q.current());
            // Off air the station is on screen but the record is what plays,
            // so the taskbar names the record like any other.
            let title = window_title(playing, st.radio.as_ref().filter(|r| !r.off_air));
            if title != last_title {
                let _ = crossterm::execute!(std::io::stdout(), SetTitle(&title));
                last_title = title;
            }
            terminal.draw(|frame| ui::draw(frame, &mut st))?;
            // The nav row's dot rides the audio's loudness on every screen, so
            // audio arriving is reason enough for the fast tick — at 250 ms it
            // would visibly step rather than breathe. Nothing playing still
            // costs four wakeups a second.
            // A spinner on screen is the third reason, and the only one that
            // holds while nothing is playing at all — which is exactly when
            // there is one.
            let want_fast =
                st.show_player || st.audio_tap.is_fresh(AUDIO_LIVE) || ui::is_animating(&st);
            (want_fast, std::mem::take(&mut st.connect_request))
        };
        // Here and nowhere else: the sign-in prints to the console, so it can
        // only run where the terminal can be given back — see [`sign_in`].
        if connect {
            terminal = sign_in(terminal, &state, &tx, &audio).await;
        }
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
    playing: Option<&app::state::Track>,
    radio: Option<&app::state::RadioPlayback>,
) -> String {
    const MAX_WIDTH: usize = 80;
    // Radio wins, for the same reason the deck draws it first: while a station
    // is on, the Spotify queue is kept but paused, and naming it in the
    // taskbar would point at the wrong sound.
    let full = match (radio, playing) {
        // Spotify's spelling of the record where there is one, so the title
        // bar and the deck say the same thing; the station's own words where
        // there is not.
        (Some(r), _) => match (r.matched_track(), r.now_title()) {
            (Some(t), _) => format!("♫ {} — {} · {}", t.name, t.artists, r.station.name),
            (None, Some(title)) => format!("♫ {title} — {}", r.station.name),
            (None, None) => format!("♫ {}", r.station.name),
        },
        (None, Some(t)) => format!("♫ {} — {}", t.name, t.artists),
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
/// spot owns the queue, so this loop does not decide *what* is playing — the
/// client writes that into the queue before it asks the player for anything.
/// What the player knows first is what the audio is *doing*: when it really
/// started, paused, moved, or ran out, and what artwork the loaded metadata
/// carries.
async fn player_event_loop(
    mut events: PlayerEventChannel,
    state: Arc<RwLock<AppState>>,
    tx: UnboundedSender<AppCommand>,
) {
    /// Re-anchor progress on the transport state, if there is one yet.
    fn anchor(state: &RwLock<AppState>, position_ms: u32, playing: Option<bool>) {
        let mut st = state.write();
        let Some(pb) = st.playback.as_mut() else {
            return;
        };
        if let Some(playing) = playing {
            pb.is_playing = playing;
        }
        pb.anchor(u64::from(position_ms));
    }

    while let Some(event) = events.recv().await {
        match event {
            // The metadata librespot loaded for the playing track, artwork
            // included. The queue's own row may have arrived without a cover
            // URL (an album's track list does not repeat the album object per
            // row), so this is what fills the sleeve for those. The client
            // owns the fetch and its cache; it skips the work when the same
            // sleeve is already up, which two tracks of one album is.
            PlayerEvent::TrackChanged { audio_item } => {
                let covers: Vec<(&str, u32)> = audio_item
                    .covers
                    .iter()
                    .map(|c| (c.url.as_str(), c.width.max(0).min(c.height.max(0)) as u32))
                    .collect();
                let _ = tx.send(AppCommand::LoadPlayingCover {
                    cover_url: cover::pick_sized(&covers),
                });
            }
            // The track ran out. The client owns both the queue and the
            // player, so it is the one that advances and loads — one owner,
            // no race.
            PlayerEvent::EndOfTrack { .. } => {
                let _ = tx.send(AppCommand::TrackEnded);
            }
            // A load that failed. librespot leaves its own player parked in
            // `Loading` when this fires, so nothing further arrives on this
            // channel for that track and nothing decodes: without this arm the
            // transport claims to play for the rest of the run, over silence.
            PlayerEvent::Unavailable { track_id, .. } => {
                if let Ok(uri) = track_id.to_uri() {
                    let _ = tx.send(AppCommand::TrackUnavailable { uri });
                }
            }
            PlayerEvent::TimeToPreloadNextTrack { .. } => {
                let _ = tx.send(AppCommand::PreloadNext);
            }
            PlayerEvent::Playing { position_ms, .. } => {
                // The only place spot hears librespot start making sound for
                // any reason — including a load that landed after a station
                // took the output device. The client decides what to do about
                // it; this loop only reports it, because it cannot see the
                // radio engine. See [`AppCommand::YieldToRadio`].
                let _ = tx.send(AppCommand::YieldToRadio);
                anchor(&state, position_ms, Some(true));
            }
            PlayerEvent::Paused { position_ms, .. } => {
                anchor(&state, position_ms, Some(false));
            }
            PlayerEvent::Stopped { .. } => {
                if let Some(pb) = state.write().playback.as_mut() {
                    pb.is_playing = false;
                }
            }
            PlayerEvent::Seeked { position_ms, .. }
            | PlayerEvent::PositionChanged { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. } => {
                anchor(&state, position_ms, None);
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
