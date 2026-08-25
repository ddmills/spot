use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt;
use librespot_core::cache::Cache as LibrespotCache;
use librespot_core::session::Session;
use librespot_core::spotify_uri::SpotifyUri;
use librespot_playback::mixer::Mixer;
use librespot_playback::player::Player;
use parking_lot::{Mutex, RwLock};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use crate::api::{Api, PAGE_LIMIT};
use crate::app::command::{AppCommand, FetchSource};
use crate::app::queue::Queue;
use crate::app::state::{
    self, AppState, ArtistTab, ArtistView, MainView, Playback, TrackList, TrackListKind,
    UpdateState,
};
use crate::cover::{Cover, CoverCache};
use crate::radio::api::RadioApi;
use crate::radio::player::RadioPlayer;

/// Where a streamed track fetch pulls its pages from.
///
/// Holds everything the fetch was asked for, not only what the endpoints
/// need: an album's artists and sleeve are carried so [`Self::open_command`]
/// can ask for the page again without reading a view that failed and
/// therefore has nothing to read.
enum TrackSource {
    Liked,
    Playlist(String),
    Album {
        id: String,
        name: String,
        artists: String,
        year: String,
        cover_url: Option<String>,
    },
}

impl TrackSource {
    /// Doubles as the view's identity on the back stack — see
    /// [`crate::app::state::ViewKey`]. Spelled there so the navigation layer,
    /// which has to name a page from a command's id before any of this has
    /// run, cannot drift from what gets stamped on the view.
    fn cache_key(&self) -> String {
        match self {
            TrackSource::Liked => state::liked_key(),
            TrackSource::Playlist(id) => state::playlist_key(id),
            TrackSource::Album { id, .. } => state::album_key(id),
        }
    }

    /// The command that opens this page from nothing — what a failed load
    /// hands to its `↻ try again`.
    fn open_command(&self) -> AppCommand {
        match self {
            TrackSource::Liked => AppCommand::LoadLikedSongs,
            TrackSource::Playlist(id) => AppCommand::LoadPlaylistTracks {
                playlist_id: id.clone(),
            },
            TrackSource::Album {
                id,
                name,
                artists,
                year,
                cover_url,
            } => AppCommand::OpenAlbum {
                id: id.clone(),
                name: name.clone(),
                artists: artists.clone(),
                year: year.clone(),
                cover_url: cover_url.clone(),
            },
        }
    }
}

/// A finished fetch, kept so reopening the same view is instant.
struct CachedTracks {
    /// Playlist snapshot the tracks belong to; None for liked songs.
    snapshot_id: Option<String>,
    tracks: Vec<crate::app::state::Track>,
}

type TrackCache = HashMap<String, CachedTracks>;

/// How often the client wakes without a command, for the one job that needs
/// a clock: matching what a station announces against Spotify.
const RADIO_TICK: Duration = Duration::from_secs(3);

/// How many stations a seek tries before it gives up.
///
/// Kept small because the cost is time, not requests: a station that will not
/// answer holds the walk for the whole connect timeout, and a run of five
/// would be over a minute of silence with nothing to look at.
const SEEK_ATTEMPTS: u8 = 3;

/// How long after a seek landed on a station its dying still counts as part of
/// that seek.
///
/// A station that drops in its first moments is one the walk should carry on
/// past. One that has been on for half an hour is the thing you chose to
/// listen to, and jumping you to a stranger is not what the control you last
/// pressed meant.
const SEEK_CHAIN_WITHIN: Duration = Duration::from_secs(120);
const COVER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const COVER_TIMEOUT: Duration = Duration::from_secs(10);
/// Playlists walked at once by the marks prefetch. Low on purpose: it runs
/// over a whole library at sign-in, behind whatever you are actually doing.
const PREFETCH_CONCURRENCY: usize = 4;

/// Write the cached playlist contents to disk, quietly.
///
/// Nobody asked for this and nothing on screen depends on it landing — a run
/// that cannot write its cache walks the playlists again next time, which is
/// the first-run case and not an error worth a toast.
fn save_playlist_tracks(st: &AppState) {
    if let Err(e) = crate::config::save_playlist_tracks(&st.playlist_tracks) {
        log::warn!("could not save the playlist cache: {e:#}");
    }
}

/// Drop the cached contents of every playlist that is gone or has moved on.
///
/// A `snapshot_id` that no longer matches is Spotify saying the contents
/// changed, and what was true of the old contents says nothing about the new.
/// Answers whether anything went, so the file is rewritten only when it holds
/// something it should not.
fn drop_stale_playlist_tracks(
    cache: &mut HashMap<String, state::PlaylistContents>,
    playlists: &[state::Playlist],
) -> bool {
    let snapshots: HashMap<&str, &str> = playlists
        .iter()
        .map(|p| (p.id.as_str(), p.snapshot_id.as_str()))
        .collect();
    let before = cache.len();
    cache.retain(|id, contents| snapshots.get(id.as_str()) == Some(&contents.snapshot_id.as_str()));
    cache.len() != before
}

/// The playlists the prefetch still has to walk: the ones you own that
/// nothing holds the contents of.
///
/// Only the ones you own, because those are the only ones the box offers —
/// Spotify refuses an add to a playlist you merely follow, so walking one
/// would buy a mark nothing draws.
fn uncached_playlists(
    cache: &HashMap<String, state::PlaylistContents>,
    playlists: &[state::Playlist],
    me: Option<&str>,
) -> Vec<String> {
    let Some(me) = me else {
        return Vec::new();
    };
    playlists
        .iter()
        .filter(|p| p.owner_id == me)
        .filter(|p| !cache.contains_key(&p.id))
        .map(|p| p.id.clone())
        .collect()
}

/// Put a track id into a cached playlist's contents, or take it out.
///
/// A playlist nothing has walked is left alone: a set holding one id would
/// read as a playlist holding one track, and the box would draw every other
/// record as off it.
/// The line under a playlist's name: who made it, and how it is shared.
///
/// The sharing rides on the same line rather than taking one of its own, the
/// way an album's year rides beside its artist. Only the answers worth a word
/// appear — a public playlist is simply a playlist, and saying so on every
/// page would be a label carried by all to inform on a handful.
fn playlist_subtitle(owner: &str, public: Option<bool>, collaborative: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !owner.is_empty() {
        parts.push(format!("by {owner}"));
    }
    if collaborative {
        parts.push("collaborative".to_string());
    } else if public == Some(false) {
        parts.push("private".to_string());
    }
    parts.join(" · ")
}

fn set_cached_membership(st: &mut AppState, playlist_id: &str, track_id: &str, on: bool) {
    let Some(contents) = st.playlist_tracks.get_mut(playlist_id) else {
        return;
    };
    if on {
        contents.track_ids.insert(track_id.to_string());
    } else {
        contents.track_ids.remove(track_id);
    }
}

/// The volume a first run starts at, before anything has been saved.
pub const DEFAULT_VOLUME_PCT: u8 = 50;

/// The Spotify streaming engine. Only a Premium account gets one.
#[derive(Clone)]
pub struct Engine {
    /// Held for the shutdown: stopping the player does not close the
    /// connection underneath it.
    pub session: Session,
    pub player: Arc<Player>,
}

/// Everything a finished sign-in hands the client.
///
/// The two halves arrive together and are of different worth: the Web API is
/// what a station's announcement is looked up in, and any account has it; the
/// engine is what plays a record, and only Premium has it.
#[derive(Clone)]
pub struct Spotify {
    pub api: Api,
    pub engine: Option<Engine>,
}

impl std::fmt::Debug for Spotify {
    /// Hand-written because neither librespot's session nor its player is
    /// `Debug`, and [`crate::app::command::AppCommand`] is.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spotify")
            .field("engine", &self.engine.is_some())
            .finish()
    }
}

/// Background task: consumes UI commands, owns the queue, and drives
/// librespot's player directly — spot decides the order, the shuffle and the
/// transport, and Spotify supplies the audio, the metadata and the art.
///
/// Spotify is optional and arrives late, or never: spot starts with radio
/// alone and takes [`AppCommand::SpotifyConnected`] whenever a sign-in
/// finishes. Every Spotify command therefore begins by asking for the half it
/// needs, and returns quietly when it is not there — the screen offers no way
/// to those commands meanwhile.
pub struct Client {
    api: Option<Api>,
    engine: Option<Engine>,
    /// librespot's on-disk cache. The client only writes the volume into it;
    /// the credentials and the audio are librespot's own business. Held
    /// directly rather than through the session, which need not exist.
    volume_cache: LibrespotCache,
    /// librespot's soft mixer: the volume actually being applied, readable
    /// and writable without a round trip. It needs no session, so radio has
    /// one too.
    mixer: Arc<dyn Mixer>,
    state: Arc<RwLock<AppState>>,
    rx: UnboundedReceiver<AppCommand>,
    /// The other end of [`Self::rx`], for work that has to leave the command
    /// loop and come back. The radio seek is the only user: it reads the
    /// directory, which is a request the loop must not wait on, and what it
    /// finds is a `PlayStation` like any other.
    tx: UnboundedSender<AppCommand>,
    /// Completed track fetches by cache key; shared with fetch tasks.
    cache: Arc<Mutex<TrackCache>>,
    /// Shared with cover-fetch tasks, so the connection to Spotify's image
    /// CDN stays warm across track changes.
    http: reqwest::Client,
    covers: Arc<Mutex<CoverCache>>,
    /// The playing track we last asked the saved-check about, so loading the
    /// same track twice does not ask twice. It is asked once per track and
    /// the answer only changes when we change it.
    liked_probe: Option<String>,
    /// The playlists whose contents are being read, so a second ask for one
    /// already in flight does not walk it twice.
    membership_probe: HashSet<String>,
    /// The announcement we last ran a Spotify lookup for.
    ///
    /// The radio twin of [`Self::liked_probe`], and set *before* the search
    /// goes out rather than when it answers: a lookup that fails must cost one
    /// request per announced title, not one per tick for as long as the record
    /// is on. Cleared when the station changes or stops, so the same string
    /// announced by a different station is asked about again.
    radio_probe: Option<String>,
    /// Which tune-in the deck is on, counted up on every station started.
    ///
    /// A tune-in reports its failure over the command channel, so a slow one
    /// from a station already abandoned can arrive after the deck has moved
    /// on. The station's uuid does not tell the two apart — a retry puts the
    /// same uuid back on the deck — so the deck carries this instead.
    tune_seq: u64,
    /// The radio directory. Shares [`Self::http`], which already carries the
    /// user agent Radio Browser asks for.
    radio_api: RadioApi,
    /// The radio audio thread. Held whether or not radio is in use; the thread
    /// is idle and the output device is not opened until a station plays.
    radio_player: RadioPlayer,
    /// Fired once both engines are silent, so the quit path can wait for it.
    /// Taken on use — a shutdown happens once and ends the loop.
    shutdown_ack: Option<oneshot::Sender<()>>,
}

impl Client {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        volume_cache: LibrespotCache,
        mixer: Arc<dyn Mixer>,
        state: Arc<RwLock<AppState>>,
        rx: UnboundedReceiver<AppCommand>,
        tx: UnboundedSender<AppCommand>,
        audio_tap: Arc<crate::audio_tap::AudioTap>,
    ) -> (Self, oneshot::Receiver<()>) {
        // Returned rather than taken as an argument: the receiver has exactly
        // one caller, the quit path, and pairing it with the client here keeps
        // the two ends from being wired up wrongly.
        let (shutdown_ack, shutdown_done) = oneshot::channel();
        // Timeouts rather than defaults: a stalled CDN request would otherwise
        // leave a generation guard armed indefinitely.
        let http = reqwest::Client::builder()
            .connect_timeout(COVER_CONNECT_TIMEOUT)
            .timeout(COVER_TIMEOUT)
            .user_agent(concat!("spot/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        let client = Self {
            api: None,
            engine: None,
            volume_cache,
            mixer,
            state,
            rx,
            tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            radio_api: RadioApi::new(http.clone()),
            radio_player: RadioPlayer::new(audio_tap),
            http,
            covers: Arc::new(Mutex::new(CoverCache::default())),
            liked_probe: None,
            membership_probe: HashSet::new(),
            radio_probe: None,
            tune_seq: 0,
            shutdown_ack: Some(shutdown_ack),
        };
        (client, shutdown_done)
    }

    pub async fn run(mut self) {
        // The one clock left in the client: a station's announcements arrive
        // on the decoder thread, and this is where they get looked up. There
        // is no playback poll any more — playback is local, and librespot's
        // events drive the state the moment anything changes.
        let mut tick = tokio::time::interval(RADIO_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    // Handled here rather than in `handle`: it is the one
                    // command that ends the loop.
                    Some(AppCommand::Shutdown) => {
                        self.shutdown();
                        break;
                    }
                    Some(cmd) => {
                        if let Err(e) = self.handle(cmd).await {
                            log::error!("command failed: {e:#}");
                            self.state.write().toast(format!("error: {e}"));
                        }
                    }
                    None => break,
                },
                _ = tick.tick() => {
                    self.check_radio_stream();
                    self.resolve_radio_track();
                }
            }
        }
    }

    /// The player, when an account that can stream is signed in.
    fn player(&self) -> Option<&Arc<Player>> {
        self.engine.as_ref().map(|e| &e.player)
    }

    async fn handle(&mut self, cmd: AppCommand) -> Result<()> {
        use AppCommand::*;
        match cmd {
            SpotifyConnected(spotify) => {
                self.api = Some(spotify.api);
                self.engine = spotify.engine;
            }
            // Whichever engine owns the device owns the transport. Radio is
            // checked first everywhere below for that reason: while a station
            // is on, the Spotify player is paused and driving it would start
            // Spotify playing underneath the stream.
            // A station that would not play has nothing to resume, and
            // pressing play on something that is not playing plainly means
            // "try again". Checked before the toggle, or the deck would flip
            // to claiming it was on.
            PlayPause if self.state.read().radio.as_ref().is_some_and(|r| r.failed()) => {
                let Some(station) = self.state.read().radio.as_ref().map(|r| r.station.clone())
                else {
                    return Ok(());
                };
                self.play_station(station, 0);
            }
            PlayPause if self.radio_live() => {
                let toggled = {
                    let mut st = self.state.write();
                    st.radio.as_mut().map(|r| {
                        r.is_playing = !r.is_playing;
                        r.is_playing
                    })
                };
                match toggled {
                    Some(true) => self.radio_player.resume(),
                    Some(false) => self.radio_player.pause(),
                    // A live engine with no deck to pause, which is what a
                    // station change that fails after an earlier station was
                    // playing leaves behind. There is no station on screen to
                    // pause, so the key means stop; doing nothing here strands
                    // a stream that can only be ended by killing spot.
                    None => self.stop_radio(),
                }
            }
            PlayPause => {
                let mut st = self.state.write();
                let AppState {
                    playback, queue, ..
                } = &mut *st;
                if let Some(pb) = playback.as_mut()
                    && let Some(player) = self.player()
                {
                    let duration = queue
                        .as_ref()
                        .and_then(|q| q.current())
                        .map(|t| t.duration_ms)
                        .unwrap_or(0);
                    if pb.is_playing {
                        player.pause();
                    } else {
                        player.play();
                    }
                    // Flip on the keypress; the player's own event re-anchors
                    // a moment later with the exact position.
                    pb.progress_ms = pb.interpolated_progress_ms(duration);
                    pb.anchored_at = Instant::now();
                    pb.is_playing = !pb.is_playing;
                }
            }
            VolumeRel(delta) if self.radio_live() => {
                let current = self.playback_volume();
                self.set_radio_volume((i16::from(current) + i16::from(delta)).clamp(0, 100) as u8);
            }
            SetVolume(pct) if self.radio_live() => self.set_radio_volume(pct.min(100)),
            // A broadcast has no position to seek to and no queue to shuffle,
            // so these have nothing to do while a station is on. The key
            // handler already turns them into a toast, but it is not the only
            // way in: a mouse click, a media key, or a hit rect left over from
            // the Spotify deck all arrive here directly.
            SeekRel(_) | SeekTo(_) | ToggleShuffle | JumpTo(_) | TrackEnded
                if self.radio_live() => {}
            // Previous and next mean the station either side of this one, not
            // the track: see [`Self::radio_back`].
            Prev if self.radio_live() => self.radio_back(),
            Next if self.radio_live() => self.radio_forward(),
            Next => {
                if self.advance_queue() {
                    self.load_current();
                }
            }
            Prev => {
                let stepped = {
                    let mut st = self.state.write();
                    st.queue.as_mut().and_then(|q| q.back()).is_some()
                };
                if stepped {
                    self.load_current();
                }
            }
            TrackEnded => {
                if self.advance_queue() {
                    self.load_current();
                }
            }
            PreloadNext => self.preload_next(),
            JumpTo(i) => {
                let jumped = {
                    let mut st = self.state.write();
                    st.queue.as_mut().and_then(|q| q.jump(i)).is_some()
                };
                if jumped {
                    self.load_current();
                }
            }
            SeekRel(delta_ms) => {
                if let Some(target) =
                    self.seek_target(|pos, dur| (pos as i64 + delta_ms).clamp(0, dur as i64) as u64)
                {
                    self.seek_to(target);
                }
            }
            SeekTo(ms) => {
                if let Some(target) = self.seek_target(|_, dur| ms.min(dur)) {
                    self.seek_to(target);
                }
            }
            VolumeRel(delta) => {
                // Stepped off the mixer: the volume actually being applied,
                // with no round trip behind it.
                let current = self.local_volume_pct();
                let new_pct = (current as i16 + delta as i16).clamp(0, 100) as u8;
                self.set_volume(new_pct);
            }
            SetVolume(pct) => self.set_volume(pct.min(100)),
            ToggleShuffle => {
                let mut st = self.state.write();
                let AppState {
                    playback, queue, ..
                } = &mut *st;
                if let Some(pb) = playback.as_mut() {
                    let on = !pb.shuffle;
                    if let Some(q) = queue.as_mut() {
                        q.shuffle(on);
                    }
                    pb.shuffle = on;
                }
                // The rows moved under the selection; keep it on the playing
                // one, which is the row the ear is on.
                st.queue_index = st.queue.as_ref().map_or(0, |q| q.index());
            }
            Play {
                tracks,
                start,
                name,
                key,
                loading,
                shuffle,
            } => self.play(tracks, start, name, key, loading, shuffle),
            PlayFetched {
                source,
                name,
                shuffle,
            } => self.play_fetched(source, name, shuffle).await,
            QueueInsertNext(track) => {
                let queued = {
                    let mut st = self.state.write();
                    match st.queue.as_mut() {
                        Some(q) => {
                            q.insert_next(track.clone());
                            st.toast(format!("up next: {}", track.name));
                            true
                        }
                        None => false,
                    }
                };
                // Nothing playing: `a` becomes a play of one track, which is
                // the nearest honest reading of "put this on next".
                if !queued {
                    self.play(vec![track], 0, "Queue".to_string(), None, false, false);
                }
            }
            SetLiked { uri, liked } => self.set_liked(uri, liked).await,
            SetOnPlaylist {
                playlist_id,
                uri,
                on,
                seq,
            } => self.set_on_playlist(playlist_id, uri, on, seq).await,
            SetPlaylistSaved { id, saved } => self.set_playlist_saved(id, saved).await,
            EditPlaylistDetails {
                id,
                name,
                description,
                seq,
            } => self.edit_playlist_details(id, name, description, seq).await,
            CachePlaylistTracks { playlist_ids } => self.cache_playlist_tracks(playlist_ids).await,
            Search(query) => self.search(query).await,
            LoadPlaylists => self.load_playlists().await,
            LoadLikedSongs => self.load_liked_view(false),
            LoadPlaylistTracks { playlist_id } => self.load_playlist_view(playlist_id, false),
            OpenAlbum {
                id,
                name,
                artists,
                year,
                cover_url,
            } => self.load_album_view(id, name, artists, year, cover_url, false),
            LoadViewCover { cover_url } => self.load_view_cover(cover_url),
            LoadPlayingCover { cover_url } => {
                // Same URL as what is already up means the same record: two
                // tracks off one album change the title and not the sleeve, and
                // refetching would blink the art off and back for no reason.
                let installed = self.state.read().cover.as_ref().map(|c| c.url.clone());
                if installed.as_deref() != cover_url.as_deref() {
                    self.load_cover(cover_url);
                }
            }
            OpenArtist { id, uri, name } => self.load_artist_view(id, uri, name, false),
            LoadArtistArt => self.load_artist_art(),
            Refresh => {
                // Fresh playlists first: snapshot_ids are how playlist changes
                // are detected.
                self.load_playlists().await;
                let artist = match &self.state.read().main {
                    MainView::Artist(v) => Some((v.id.clone(), v.uri.clone(), v.name.clone())),
                    _ => None,
                };
                if let Some((id, uri, name)) = artist {
                    self.load_artist_view(id, uri, name, true);
                    return Ok(());
                }
                let key = match &self.state.read().main {
                    MainView::Tracks(list) => list.cache_key.clone(),
                    _ => None,
                };
                match key {
                    Some(key) => {
                        self.cache.lock().remove(&key);
                        if key == state::liked_key() {
                            self.load_liked_view(true);
                        } else if let Some(id) = key.strip_prefix("playlist:") {
                            self.load_playlist_view(id.to_string(), true);
                        } else if let Some(id) = key.strip_prefix("album:") {
                            // Rebuild the album metadata from the view itself.
                            let meta = match &self.state.read().main {
                                MainView::Tracks(list) => Some((
                                    list.header.name.clone(),
                                    list.header.cover_url.clone(),
                                    list.items
                                        .first()
                                        .map(|t| (t.artists.clone(), t.release_year.clone()))
                                        .unwrap_or_default(),
                                )),
                                _ => None,
                            };
                            if let Some((name, cover, (artists, year))) = meta {
                                self.load_album_view(
                                    id.to_string(),
                                    name,
                                    artists,
                                    year,
                                    cover,
                                    true,
                                );
                            }
                        }
                    }
                    // The pages that are not track lists ask for themselves
                    // again from what they already hold. `R` used to stop at
                    // the playlists on all three, which read as the key doing
                    // nothing on the very pages a failed fetch leaves blank.
                    None => {
                        // Read and released before either call: `search` takes
                        // the write lock, and holding a read one across it
                        // would deadlock the client.
                        let again = {
                            let st = self.state.read();
                            match &st.main {
                                MainView::Search(results) if !results.query.is_empty() => {
                                    Some(Search(results.query.clone()))
                                }
                                MainView::Radio(v) => Some(LoadRadio {
                                    scope: v.scope.clone(),
                                }),
                                _ => None,
                            }
                        };
                        match again {
                            Some(Search(query)) => self.search(query).await,
                            Some(LoadRadio { scope }) => self.load_radio(scope),
                            _ => self.state.write().toast("playlists refreshed"),
                        }
                    }
                }
            }
            LoadRadio { scope } => self.load_radio(scope),
            PlayStation { station, attempt } => {
                // Before the new station is installed, so what is recorded is
                // what was actually playing. A tune-in that fails leaves the
                // entry standing, which is right: it is still the last thing
                // you heard.
                //
                // Only the first station of a seek: the walk past a station
                // that would not play is not a step you took, and putting the
                // dead ones in the path would have previous walk back through
                // silence.
                if attempt <= 1 {
                    self.state.write().record_listen();
                }
                self.play_station(*station, attempt);
            }
            StopRadio => self.stop_radio(),
            RadioFailed {
                station,
                reason,
                tune_seq,
            } => self.radio_failed(*station, reason, tune_seq),
            ToggleSavedStation(station) => self.toggle_saved_station(*station),
            YieldToRadio => self.yield_to_radio(),
            CheckForUpdate => self.check_for_update(),
            InstallUpdate => self.install_update(),
            // Intercepted by `run`, which is the only place that can end the
            // loop. Listed rather than swept into a catch-all so a new command
            // still has to be handled deliberately.
            Shutdown => {}
        }
        Ok(())
    }

    /// Install a new queue and start its `start` row.
    ///
    /// `tracks` is the caller's display order and becomes the play order —
    /// what you see is what plays. When the list came from a re-fetchable
    /// source still paging in (`key` + `loading`), the freshest copy of the
    /// rows is taken off the main view under the same lock the page fetches
    /// append under, so no page can fall between the click and the install;
    /// later pages then land through the extend in [`Self::start_track_fetch`].
    fn play(
        &mut self,
        tracks: Vec<crate::app::state::Track>,
        start: usize,
        name: String,
        key: Option<String>,
        loading: bool,
        shuffle: bool,
    ) {
        self.yield_to_spotify();
        {
            let mut st = self.state.write();
            let mut tracks = tracks;
            let mut start = start;
            let mut loading = loading;
            if let (Some(k), true) = (key.as_deref(), loading)
                && let MainView::Tracks(list) = &st.main
                && list.cache_key.as_deref() == Some(k)
                && list.sort.is_natural()
            {
                let started = tracks.get(start).map(|t| t.uri.clone());
                tracks = list.items.clone();
                loading = list.loading;
                // The clicked row's position in the fuller copy. Fetch order
                // is stable, so it is where it was — this only matters if a
                // page landed between the click and here.
                if let Some(uri) = started
                    && let Some(pos) = tracks.iter().position(|t| t.uri == uri)
                {
                    start = pos;
                }
            }
            if tracks.is_empty() {
                return;
            }
            // A shuffle control asks for the mode, not just one mixed queue:
            // record it in playback so the deck agrees and later plays
            // inherit it. Before anything has played there is no playback to
            // stamp, so install one; `load_current` rebuilds it right after,
            // inheriting the bit.
            if shuffle {
                match st.playback.as_mut() {
                    Some(pb) => pb.shuffle = true,
                    None => {
                        st.playback = Some(Playback::started(self.local_volume_pct(), true));
                    }
                }
            }
            let shuffle = shuffle || st.playback.as_ref().is_some_and(|pb| pb.shuffle);
            let mut q = Queue::new(tracks, start, name);
            q.source_key = key;
            q.loading = loading;
            // Shuffle is a mode, not a property of one queue: left on, a new
            // play comes up shuffled too, with the clicked track playing
            // first — exactly what `Queue::shuffle` guarantees.
            if shuffle {
                q.shuffle(true);
            }
            st.set_queue(Some(q));
        }
        self.check_queue_liked();
        self.load_current();
    }

    /// Ask whether the queue's rows are saved, so the player's list can draw
    /// an honest `★` on each of them.
    ///
    /// Only the playing track is probed elsewhere (see [`Self::load_current`]),
    /// which would leave every other row of the list reading "not saved". URIs
    /// already answered are dropped, so replaying a list costs nothing.
    fn check_queue_liked(&self) {
        let Some(api) = self.api.clone() else { return };
        let uris: Vec<String> = {
            let st = self.state.read();
            let Some(q) = st.queue.as_ref() else { return };
            q.rows()
                .iter()
                .filter(|t| !st.liked.contains_key(&t.uri))
                .map(|t| t.uri.clone())
                .collect()
        };
        spawn_liked_check(api, self.state.clone(), uris);
    }

    /// Play a source whose rows are not in hand — a playlist row's `x`, an
    /// album card's ▶. The first page is fetched inline so play starts as
    /// soon as it lands; the rest stream into the queue behind it.
    async fn play_fetched(&mut self, source: FetchSource, name: String, shuffle: bool) {
        let Some(api) = self.api.clone() else { return };
        let source = match source {
            FetchSource::Playlist { id } => TrackSource::Playlist(id),
            // Artists and sleeve are left blank: this source fills a queue
            // rather than a page, so nothing here will ever ask it to open
            // one.
            FetchSource::Album { id, year } => TrackSource::Album {
                id,
                name: name.clone(),
                artists: String::new(),
                year,
                cover_url: None,
            },
        };
        let key = source.cache_key();

        // Cache hit: the whole list at once. No snapshot check — a play is
        // not a browse, and a copy at most one `R` old is a fair trade for
        // starting instantly.
        let cached = self.cache.lock().get(&key).map(|e| e.tracks.clone());
        if let Some(tracks) = cached {
            self.play(tracks, 0, name, Some(key), false, shuffle);
            return;
        }

        let first = match &source {
            TrackSource::Liked => api.liked_songs_page(0).await,
            TrackSource::Playlist(id) => api.playlist_tracks_page(id, 0).await,
            TrackSource::Album { id, name, year, .. } => {
                api.album_tracks_page(id, name, year, 0).await
            }
        };
        let (tracks, has_more, _) = match first {
            Ok(page) => page,
            Err(e) => {
                self.state.write().toast(format!("could not play: {e}"));
                return;
            }
        };
        if tracks.is_empty() {
            self.state.write().toast("nothing to play");
            return;
        }
        self.play(
            tracks.clone(),
            0,
            name,
            Some(key.clone()),
            has_more,
            shuffle,
        );
        if !has_more {
            self.cache.lock().insert(
                key,
                CachedTracks {
                    snapshot_id: None,
                    tracks,
                },
            );
            return;
        }

        // The rest of the pages, streamed into the queue it was started for.
        // The generation stamp is what stops a queue installed later from
        // being appended to by this task.
        let generation = self.state.read().queue.as_ref().map(|q| q.generation);
        let Some(generation) = generation else { return };
        let state = self.state.clone();
        let cache = self.cache.clone();
        let mut all = tracks;
        tokio::spawn(async move {
            let mut offset = PAGE_LIMIT;
            loop {
                let result = match &source {
                    TrackSource::Liked => api.liked_songs_page(offset).await,
                    TrackSource::Playlist(id) => api.playlist_tracks_page(id, offset).await,
                    TrackSource::Album { id, name, year, .. } => {
                        api.album_tracks_page(id, name, year, offset).await
                    }
                };
                let mut finished = false;
                // The rows this page brought, so their `★` can be answered the
                // way the first page's were.
                let mut fresh: Vec<String>;
                {
                    let mut st = state.write();
                    let Some(q) = st.queue.as_mut().filter(|q| q.generation == generation) else {
                        return;
                    };
                    match result {
                        Ok((page, has_more, _)) => {
                            all.extend(page.iter().cloned());
                            fresh = page.iter().map(|t| t.uri.clone()).collect();
                            q.extend(page);
                            if !has_more {
                                q.loading = false;
                                finished = true;
                            }
                        }
                        Err(e) => {
                            q.loading = false;
                            st.toast(format!("queue load failed: {e}"));
                            return;
                        }
                    }
                    fresh.retain(|uri| !st.liked.contains_key(uri));
                }
                spawn_liked_check(api.clone(), state.clone(), fresh);
                if finished {
                    cache.lock().insert(
                        key,
                        CachedTracks {
                            snapshot_id: None,
                            tracks: all,
                        },
                    );
                    return;
                }
                offset += PAGE_LIMIT;
            }
        });
    }

    /// Step the queue forward, wrapping at the end (repeat-all).
    fn advance_queue(&self) -> bool {
        let mut st = self.state.write();
        st.queue.as_mut().and_then(|q| q.advance()).is_some()
    }

    /// Load the queue's current track into the player and stamp the state to
    /// match, in the same breath. There is no round trip: the deck describes
    /// the new track on the next frame, and the audio follows as fast as
    /// librespot can fetch it.
    fn load_current(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let track = {
            let mut st = self.state.write();
            let Some(track) = st.queue.as_ref().and_then(|q| q.current()).cloned() else {
                return;
            };
            let shuffle = st.playback.as_ref().is_some_and(|pb| pb.shuffle);
            st.playback = Some(Playback::started(self.local_volume_pct(), shuffle));
            // Every deck checks radio before Spotify, so a station left set
            // keeps drawing over the track that is starting. `yield_to_spotify`
            // stops the engine itself on the play path.
            st.radio = None;
            // The status word tells "playing" from "still fetching" by whether
            // samples are arriving, and the old track's are about to stop.
            st.audio_tap.clear();
            track
        };

        match SpotifyUri::from_uri(&track.uri) {
            Ok(uri) => engine.player.load(uri, true, 0),
            Err(e) => {
                log::error!("unplayable uri {}: {e}", track.uri);
                self.state.write().toast("that track cannot be played");
                return;
            }
        }

        // The deck draws a liked mark for the playing track, which is not
        // necessarily in any loaded list, so its saved state is asked for on
        // its own — once per track; after that the map owns the answer and
        // `set_liked` keeps it.
        let unchecked = {
            let st = self.state.read();
            (!st.liked.contains_key(&track.uri)
                && self.liked_probe.as_deref() != Some(track.uri.as_str()))
            .then(|| track.uri.clone())
        };
        if let Some(uri) = unchecked
            && let Some(api) = self.api.clone()
        {
            self.liked_probe = Some(uri.clone());
            spawn_liked_check(api, self.state.clone(), vec![uri]);
        }

        // The sleeve, when the row carried one; rows off an album's own track
        // list do not, and for those `TrackChanged` brings the artwork with
        // the metadata a moment later. Served from the cover cache instantly
        // when the record was seen before.
        if track.cover_url.is_some() {
            let installed = self.state.read().cover.as_ref().map(|c| c.url.clone());
            if installed != track.cover_url {
                self.load_cover(track.cover_url);
            }
        }
    }

    /// Hand the player the next track ahead of the gap, so track boundaries
    /// are seamless.
    fn preload_next(&self) {
        let next = {
            let st = self.state.read();
            st.queue.as_ref().and_then(|q| {
                // A one-track queue's next is itself; preloading what is
                // already loaded does nothing useful.
                (q.len() > 1).then(|| q.rows()[(q.index() + 1) % q.len()].uri.clone())
            })
        };
        if let Some(uri) = next
            && let Ok(uri) = SpotifyUri::from_uri(&uri)
            && let Some(player) = self.player()
        {
            player.preload(uri);
        }
    }

    /// The seek target under the current transport state, or `None` when
    /// nothing is playing. `f` gets (interpolated position, duration).
    fn seek_target(&self, f: impl FnOnce(u64, u64) -> u64) -> Option<u64> {
        let st = self.state.read();
        let pb = st.playback.as_ref()?;
        let duration = st
            .queue
            .as_ref()
            .and_then(|q| q.current())
            .map(|t| t.duration_ms)?;
        Some(f(pb.interpolated_progress_ms(duration), duration))
    }

    fn seek_to(&self, target: u64) {
        let Some(player) = self.player() else { return };
        player.seek(target.min(u32::MAX as u64) as u32);
        if let Some(pb) = self.state.write().playback.as_mut() {
            pb.anchor(target);
        }
    }

    /// Apply a volume to the mixer, the state and the cache in one move.
    ///
    /// The mixer is the truth the audio path reads; the state is what the
    /// slider draws; the cache is what the next run starts at.
    fn set_volume(&self, pct: u8) {
        let raw = pct_to_raw(pct);
        self.mixer.set_volume(raw);
        if let Some(pb) = self.state.write().playback.as_mut() {
            pb.volume_percent = pct;
        }
        self.volume_cache.save_volume(raw);
    }

    /// Answer one query out of both catalogues.
    ///
    /// Spotify and the station directory are two hosts, so the two round trips
    /// overlap rather than queue: the directory is asked from a spawned task
    /// while this one waits on Spotify. Neither half can be allowed to hold up
    /// the other, and in practice the directory is the slower of the two.
    ///
    /// The view is installed empty and loading straight away, exactly as
    /// [`Self::load_radio`] does it, so the query and the tab strip are on
    /// screen the instant Enter is pressed. Both halves then merge into that
    /// view under a `load_generation` guard — a half whose query the user has
    /// already replaced writes nothing.
    async fn search(&mut self, query: String) {
        let generation = {
            let mut st = self.state.write();
            st.load_generation += 1;
            let generation = st.load_generation;
            st.loading = true;
            st.main = MainView::Search(state::SearchResults {
                query: query.clone(),
                stations_loading: true,
                generation,
                ..Default::default()
            });
            st.main_to_top();
            generation
        };

        // The station half, off on its own so the Spotify await below starts
        // immediately.
        let radio_api = self.radio_api.clone();
        let state = self.state.clone();
        let station_query = query.clone();
        tokio::spawn(async move {
            let found = radio_api.search(&station_query).await;
            let mut st = state.write();
            let MainView::Search(results) = &mut st.main else {
                return;
            };
            if results.generation != generation {
                return;
            }
            results.stations_loading = false;
            match found {
                Ok(stations) => {
                    results.stations = stations.into();
                    // The station half lands after the other four, so it
                    // arrives on a page that may already be sorted.
                    st.resort_main();
                }
                // Logged, not toasted, unlike the Spotify half below. The
                // directory being unreachable is not *this search* failing —
                // four tabs of perfectly good results are on screen — and a
                // toast thrown over them would say that it was. The Stations
                // tab going from "searching…" to "no stations" is where that
                // news belongs, and it is only news to someone looking at it.
                Err(e) => {
                    log::error!("station search failed: {e:#}");
                    results.stations_error = Some(state::LoadError::new(
                        e.to_string(),
                        AppCommand::Search(station_query),
                    ));
                }
            }
        });

        // The four Spotify tabs are on the strip only for an account that can
        // play what they list — see [`AppState::search_tabs`]. Where they are
        // not, the station half above is the whole answer, and asking Spotify
        // would spend a request on a tab nobody can open.
        let ready = self.state.read().spotify == state::SpotifyState::Ready;
        let Some(api) = self.api.clone().filter(|_| ready) else {
            self.state.write().loading = false;
            return;
        };

        let result = api.search(&query).await;
        let mut st = self.state.write();
        st.loading = false;
        // The station half may already have landed in the view, so the Spotify
        // vecs are moved *into* it rather than replacing it wholesale.
        let MainView::Search(results) = &mut st.main else {
            return;
        };
        if results.generation != generation {
            return;
        }
        match result {
            Ok(found) => {
                results.tracks = found.tracks;
                results.albums = found.albums;
                results.artists = found.artists;
                results.playlists = found.playlists;
                let uris: Vec<String> =
                    results.tracks.items.iter().map(|t| t.uri.clone()).collect();
                drop(st);
                spawn_liked_check(api, self.state.clone(), uris);
            }
            Err(e) => {
                results.error = Some(state::LoadError::new(
                    e.to_string(),
                    AppCommand::Search(query),
                ));
                st.toast(format!("search failed: {e}"));
            }
        }
    }

    /// Fill a radio page.
    ///
    /// The view is installed empty and loading straight away, so the trail and
    /// the tab strip are on screen while the directory is still answering, and
    /// the fetch is guarded by `load_generation` exactly as a track page is:
    /// a page the user has already navigated away from writes nothing.
    ///
    /// Saved stations are the one scope that never touches the network — they
    /// are already in memory, read from `radio.json` at startup.
    fn load_radio(&self, scope: state::RadioScope) {
        use state::{RadioRow, RadioScope, RadioView};

        let generation = {
            let mut st = self.state.write();
            st.load_generation += 1;
            let generation = st.load_generation;
            let mut view = RadioView::new(scope.clone(), generation);
            if scope == RadioScope::Favorites {
                view.rows = st
                    .radio_favorites
                    .iter()
                    .cloned()
                    .map(RadioRow::Station)
                    .collect();
                view.loading = false;
            }
            st.main = MainView::Radio(view);
            st.main_to_top();
            generation
        };
        if scope == RadioScope::Favorites {
            return;
        }

        let api = self.radio_api.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            let rows = match &scope {
                RadioScope::Popular => api.top_voted().await.map(into_station_rows),
                RadioScope::Country(code) => api.by_country(code).await.map(into_station_rows),
                RadioScope::Genre(tag) => api.by_tag(tag).await.map(into_station_rows),
                RadioScope::Countries => api.countries().await.map(into_facet_rows),
                RadioScope::Genres => api.genres().await.map(into_facet_rows),
                // Handled above, without a fetch.
                RadioScope::Favorites => Ok(Vec::new()),
            };

            let mut st = state.write();
            // Someone navigated on while the directory was answering.
            let MainView::Radio(view) = &mut st.main else {
                return;
            };
            if view.generation != generation {
                return;
            }
            view.loading = false;
            match rows {
                Ok(rows) => {
                    view.rows = rows.into();
                    st.main_to_top();
                }
                Err(e) => {
                    log::error!("radio directory load failed: {e:#}");
                    view.error = Some(state::LoadError::new(
                        e.to_string(),
                        AppCommand::LoadRadio { scope },
                    ));
                    st.toast(format!("could not reach the radio directory: {e}"));
                }
            }
        });
    }

    /// Start a station, having first got Spotify out of the way.
    ///
    /// The two engines share one output device, so this is the only place that
    /// may decide which of them owns it. The player is paused rather than
    /// stopped: pausing is instant and local, and the queue stays where it
    /// was so stopping the stream puts the last track straight back.
    ///
    /// The connecting is done on a task of its own, and this returns as soon as
    /// the deck is drawn. Connecting takes seconds — a directory address to
    /// resolve, a stream to reach and prefetch, a codec to identify — and
    /// awaiting it here holds the command loop for all of them. Pause, stop
    /// and the next station all arrive on that loop, so a slow station makes a
    /// player that answers nothing, and a station that never connects makes a
    /// player that never answers again.
    fn play_station(&mut self, station: state::Station, attempt: u8) {
        if let Some(player) = self.player() {
            player.pause();
        }
        // The station being left goes silent on the press, not when the one
        // replacing it is ready. Connecting takes seconds — a directory
        // address, a stream to reach, five seconds of prefetch — and hearing
        // the station you asked to leave through all of them is the deck
        // reporting one thing and the speakers another. `hush` rather than
        // `stop`: the device stays open for the station arriving.
        self.radio_player.hush();
        let volume = self.playback_volume();
        self.tune_seq += 1;
        let tune_seq = self.tune_seq;

        {
            let mut st = self.state.write();
            let mut playback =
                state::RadioPlayback::new(station.clone(), volume, self.radio_player.title());
            playback.seek_attempt = attempt;
            playback.tune_seq = tune_seq;
            st.radio = Some(playback);
            // Whatever the last station was announcing is not this station's
            // business. Without this, moving to a station that happens to be
            // playing the same record would find the probe already set and
            // never look it up.
            self.radio_probe = None;
            // The Spotify bar must not keep claiming to be playing behind the
            // station; the queue itself is kept so stopping radio puts the
            // last track straight back.
            if let Some(pb) = st.playback.as_mut() {
                pb.is_playing = false;
            }
        }

        let player = self.player().cloned();
        let radio = self.radio_player.clone();
        let api = self.radio_api.clone();
        let state = Arc::clone(&self.state);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            // Whether this task still speaks for the deck. Asked afresh at
            // every step rather than once at the end: the two awaits below are
            // a network round trip and a whole connect, the deck's previous
            // and next controls make overlapping tasks ordinary rather than
            // rare, and a task that no longer speaks for the deck must touch
            // neither engine — not the radio it would strand, and not the
            // Spotify a later step may have handed the device back to.
            let ours = {
                let state = Arc::clone(&state);
                move || {
                    state
                        .read()
                        .radio
                        .as_ref()
                        .is_some_and(|r| r.tune_seq == tune_seq)
                }
            };

            // The directory's ranking runs on these, and it also hands back
            // the stream URL it believes in, which is fresher than the one in
            // the row.
            let url = api
                .click(&station.uuid)
                .await
                .unwrap_or_else(|| station.url.clone());
            if !ours() {
                return;
            }

            // Again, right before the stream starts: a load the player was
            // still working through can start making sound after the pause
            // above, and this is the last point before the stream at which to
            // catch it.
            if let Some(player) = &player {
                player.pause();
            }
            let outcome = radio.play(&url, volume).await;
            let ours = ours();
            if let Err(e) = outcome {
                log::error!("could not play {}: {e:#}", station.name);
                // Reported rather than acted on. Whether this is the end of
                // the road or one step of a seek is the command loop's to
                // decide, and it is also the only place that may write the
                // deck. The engine is left alone: `hush` above already
                // silenced it, and this station never reached the thread.
                if ours {
                    let _ = tx.send(AppCommand::RadioFailed {
                        station: Box::new(station.clone()),
                        reason: e.to_string(),
                        tune_seq,
                    });
                }
                return;
            }
            // And once more now the station is audible. `play` above spends
            // several seconds connecting and prefetching; anything that
            // started during that window is caught on the far side of it —
            // with `YieldToRadio` behind it as the backstop for anything later
            // still. Only while this task still speaks for the deck: previous
            // can have handed the device back to Spotify meanwhile, and this
            // pause would silence it under a bar that says it is playing.
            if ours {
                if let Some(player) = &player {
                    player.pause();
                }
                let mut st = state.write();
                if let Some(r) = st.radio.as_mut() {
                    // The walk that reached this station is over, and nothing
                    // is failing any more.
                    r.failure = None;
                    r.seek_attempt = 0;
                }
                st.toast(format!("playing {}", station.name));
            }
        });
    }

    fn stop_radio(&self) {
        self.radio_player.stop();
        self.state.write().radio = None;
    }

    /// The radio deck's `◂◂ previous`: back to the last thing you were
    /// listening to.
    ///
    /// A station has no track before it, but it does have something before
    /// it — an earlier station, or the Spotify queue it interrupted. The
    /// history is walked here rather than recorded here: `step_back_listen`
    /// has already handed what is playing to the forward path, so the tune-in
    /// below must not record it a second time.
    fn radio_back(&mut self) {
        let stepped = self.state.write().step_back_listen();
        match stepped {
            Some(state::Listened::Station(s)) => self.play_station(*s, 0),
            Some(state::Listened::Spotify) => self.resume_spotify(),
            None => self
                .state
                .write()
                .toast("nothing was playing before this station"),
        }
    }

    /// The radio deck's `next ▸▸` / `seek ▸▸`.
    ///
    /// Forward through what [`Self::radio_back`] stepped out of while there is
    /// any; once that path is spent the control is offering the rest of the
    /// station's own country instead, which is what the row says it is.
    fn radio_forward(&mut self) {
        let stepped = self.state.write().step_forward_listen();
        match stepped {
            Some(state::Listened::Station(s)) => self.play_station(*s, 0),
            Some(state::Listened::Spotify) => self.resume_spotify(),
            None => self.seek_station(1),
        }
    }

    /// Hand the output device back to Spotify, where the station left it.
    ///
    /// Starting a station pauses librespot rather than stopping it and keeps
    /// the queue, so there is nothing to reload: stopping the stream and
    /// letting the player go on puts the track back where it was. The anchor
    /// is reset because the paused snapshot is now the position again — a
    /// station may have been on for an hour.
    fn resume_spotify(&mut self) {
        self.stop_radio();
        let resumed = {
            let mut st = self.state.write();
            match st.playback.as_mut() {
                Some(pb) => {
                    pb.anchor(pb.progress_ms);
                    pb.is_playing = true;
                    st.queue
                        .as_ref()
                        .and_then(|q| q.current())
                        .map(|t| t.name.clone())
                }
                None => None,
            }
        };
        match resumed {
            Some(name) => {
                if let Some(player) = self.player() {
                    player.play();
                }
                self.state.write().toast(format!("back to {name}"));
            }
            None => self
                .state
                .write()
                .toast("nothing was playing before this station"),
        }
    }

    /// Move down the playing station's country, in the directory's own order.
    ///
    /// The country page is asked for rather than read from anywhere: nothing
    /// caches stations, and the station may have been reached from the chart,
    /// a genre, the saved list or a search, none of which is its country. The
    /// request is made off the command loop for the reason
    /// [`Self::play_station`] connects off it — the loop has to stay free to
    /// pause and stop — and what it finds comes back as an ordinary
    /// `PlayStation`, so a seek records history like any other new
    /// destination.
    fn seek_station(&mut self, attempt: u8) {
        let Some((station, code)) = self
            .state
            .read()
            .radio
            .as_ref()
            .map(|r| (r.station.clone(), r.station.countrycode.clone()))
        else {
            return;
        };
        // Nothing to walk through, and nothing wrong with what is playing:
        // this is a control that leads nowhere, not a station that failed. The
        // row does not draw it in this case, but the `n` key reaches here
        // whether it is drawn or not.
        if code.is_empty() {
            self.state
                .write()
                .toast(format!("{} names no country to seek through", station.name));
            return;
        }
        // Silent from the press, not from the answer. The directory takes as
        // long as it takes, and going on hearing the station being seeked away
        // from is what makes the control feel like it did nothing.
        self.radio_player.hush();
        self.tune_seq += 1;
        let tune_seq = self.tune_seq;
        if let Some(r) = self.state.write().radio.as_mut() {
            r.tune_seq = tune_seq;
        }

        let api = self.radio_api.clone();
        let state = Arc::clone(&self.state);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            // Every exit below reports rather than returning quietly: the
            // stream is already silent, so a seek that gives up without saying
            // so leaves the deck claiming a station nobody can hear.
            let failed = |reason: String| {
                let _ = tx.send(AppCommand::RadioFailed {
                    station: Box::new(station.clone()),
                    reason,
                    tune_seq,
                });
            };
            let stations = match api.by_country(&code).await {
                Ok(stations) => stations,
                Err(e) => {
                    log::error!("could not seek through {code}: {e:#}");
                    failed(format!("could not reach the radio directory: {e}"));
                    return;
                }
            };
            // Whether this answer still speaks for the deck. The request takes
            // as long as the directory makes it take, and a station chosen
            // meanwhile is the one the user means.
            if !state
                .read()
                .radio
                .as_ref()
                .is_some_and(|r| r.tune_seq == tune_seq)
            {
                return;
            }
            match next_in_country(&stations, &station.uuid) {
                Some(next) => {
                    let _ = tx.send(AppCommand::PlayStation {
                        station: Box::new(next),
                        attempt,
                    });
                }
                None => failed(format!("there is nothing else to hear in {code}")),
            }
        });
    }

    /// Deal with a station that would not play.
    ///
    /// A seek walking a country is the only thing that carries on: it is a run
    /// of stations rather than a choice of one, and stopping on the first dead
    /// entry is what makes seeking through a directory of ten thousand
    /// stations useless. Every other route in — a row, a previous, a station
    /// the user has had on for an hour — takes the failure as the answer, and
    /// keeps the deck so the controls out of it are still there.
    fn radio_failed(&mut self, station: state::Station, reason: String, tune_seq: u64) {
        let ours = self
            .state
            .read()
            .radio
            .as_ref()
            .is_some_and(|r| r.tune_seq == tune_seq);
        if !ours {
            return;
        }
        let attempt = self
            .state
            .read()
            .radio
            .as_ref()
            .map_or(0, |r| r.seek_attempt);
        if seek_walks_on(attempt) {
            self.state
                .write()
                .toast(format!("{} would not play — seeking on", station.name));
            self.seek_station(attempt + 1);
            return;
        }
        // One message, not two: a walk that gave up has already said what it
        // was doing at every hop, and the last station's own error is the
        // least interesting part of it.
        let told = if attempt >= SEEK_ATTEMPTS {
            format!("gave up after {attempt} stations that would not play")
        } else {
            format!("could not play {}: {reason}", station.name)
        };
        self.fail_station(&reason, told);
    }

    /// Leave the station on the deck, marked as not playing and saying why.
    ///
    /// The controls that reach another station are on the deck, so clearing it
    /// on a failure takes away the one thing that gets you out of the station
    /// that failed.
    fn fail_station(&self, reason: &str, told: String) {
        self.radio_player.hush();
        let mut st = self.state.write();
        let Some(r) = st.radio.as_mut() else { return };
        r.is_playing = false;
        r.seek_attempt = 0;
        r.failure = Some(reason.to_string());
        st.toast(told);
    }

    /// Notice a station that connected and then stopped sending.
    ///
    /// A broadcast has no length to reach, so nothing else in the app sees one
    /// end: the sink drains to silence under a deck that goes on saying the
    /// station is on. Read off the engine on the same tick that resolves
    /// announcements, because that is the only clock the client has left.
    ///
    /// A station a seek landed on moments ago walks on; one that has been
    /// playing long enough to be the thing you chose to listen to does not.
    /// Jumping someone to a stranger after an hour is not what the control
    /// they last pressed meant.
    fn check_radio_stream(&mut self) {
        if !self.radio_player.stream_ended() {
            return;
        }
        let Some((station, tune_seq, of_a_walk)) = self
            .state
            .read()
            .radio
            .as_ref()
            .filter(|r| !r.failed())
            .map(|r| {
                (
                    r.station.clone(),
                    r.tune_seq,
                    r.seek_attempt > 0 && r.elapsed() < SEEK_CHAIN_WITHIN,
                )
            })
        else {
            return;
        };
        // A station that dropped long after the walk that found it is the one
        // you settled on, so the walk is over whatever brought you here.
        if !of_a_walk && let Some(r) = self.state.write().radio.as_mut() {
            r.seek_attempt = 0;
        }
        self.radio_failed(station, "it stopped sending".to_string(), tune_seq);
    }

    /// Silence both engines on the way out, then let the quit path know.
    ///
    /// The client is the only holder of the handles that can do this, so
    /// without it nothing stops playing when spot quits: the radio thread is
    /// detached and its stop command is only ever sent on user action, and
    /// librespot's player goes on draining its buffer. Both then survive until
    /// the process dies — which, if librespot's player thread is wedged, can
    /// be a long time after the terminal comes back.
    fn shutdown(&mut self) {
        // Radio first: it is the one that outlives the process, and it blocks
        // until the output device is actually closed.
        self.radio_player.shutdown();
        // Stop the audio, then close the connection underneath it. There is
        // neither to stop when no account was ever connected.
        if let Some(engine) = &self.engine {
            engine.player.stop();
            engine.session.shutdown();
        }
        if let Some(ack) = self.shutdown_ack.take() {
            let _ = ack.send(());
        }
    }

    /// Look up what the station just announced, once per announcement.
    ///
    /// The ICY callback runs on the decoder thread, which has neither a runtime
    /// nor an `Api`, so a new announcement is *noticed* here rather than
    /// reacted to there. [`Self::radio_probe`] is what stops the three-second
    /// tick re-asking the same question for the length of a record.
    fn resolve_radio_track(&mut self) {
        // Nothing to look a record up in. The deck then draws the station's
        // own words, exactly as it does for an announcement that parses into
        // no track at all.
        let Some(api) = self.api.clone() else { return };

        // Scoped: parking_lot's lock is not reentrant and the arms below take
        // it for writing.
        let announced = self.state.read().radio.as_ref().and_then(|r| r.now_title());

        let Some(raw) = announced else {
            // No station, or a station that has stopped saying anything.
            // Forget the probe, so the same title announced again after a gap
            // is looked up again — and drop the match with it, or the deck
            // goes on naming a record the station has stopped claiming to
            // play.
            self.radio_probe = None;
            if let Some(r) = self.state.write().radio.as_mut() {
                r.matched = state::RadioMatch::None;
            }
            return;
        };
        if !needs_lookup(&raw, self.radio_probe.as_deref()) {
            return;
        }
        self.radio_probe = Some(raw.clone());

        let station = {
            let st = self.state.read();
            let Some(r) = st.radio.as_ref() else { return };
            r.station.clone()
        };

        let Some(want) = crate::radio::track::parse(&raw, &station.name) else {
            // Not a track: a promo, a jingle, the station's own ident. The row
            // still says what the server said; there is simply nothing behind
            // it, and no request is spent finding that out.
            if let Some(r) = self.state.write().radio.as_mut() {
                r.matched = state::RadioMatch::None;
            }
            return;
        };

        if let Some(r) = self.state.write().radio.as_mut() {
            r.matched = state::RadioMatch::Searching;
        }

        let state = self.state.clone();
        let uuid = station.uuid.clone();
        // Spawned rather than awaited: rspotify is built from a bare token with
        // no HTTP timeout, so an inline await could stall the command loop on
        // one hung connection. The cost is that a lookup which errors cannot
        // reset `radio_probe` and so is not retried until the next
        // announcement — which is the right trade during a 429.
        tokio::spawn(async move {
            let found = lookup(&api, &want).await;

            let uri = {
                let mut st = state.write();
                let Some(r) = st.radio.as_mut() else { return };
                // Two ways this answer can be stale and one check for both: the
                // user changes station, or the station moves on while the
                // lookup runs. A uuid alone only catches the first.
                let current = r.now_title();
                if r.station.uuid != uuid || current.as_deref() != Some(raw.as_str()) {
                    return;
                }
                match found {
                    Some(track) => {
                        let uri = track.uri.clone();
                        r.matched = state::RadioMatch::Matched(Box::new(track));
                        Some(uri)
                    }
                    None => {
                        r.matched = state::RadioMatch::Unmatched;
                        None
                    }
                }
            };

            // The deck draws a `★` for the matched track, which is in no loaded
            // list, so its saved state has to be asked for on its own — the
            // same reason the playing track's is. Guarded on the map, so a
            // station looping a playlist asks once per record, not per play.
            if let Some(uri) = uri.filter(|u| !state.read().liked.contains_key(u)) {
                spawn_liked_check(api, state, vec![uri]);
            }
        });
    }

    /// Stop the stream if one is playing, before Spotify takes the device.
    ///
    /// Every Spotify play path calls this, which is what keeps "only one engine
    /// at a time" true rather than merely intended.
    ///
    /// The question goes to [`Self::radio_live`], which takes either sense of
    /// "a station is on". `AppState.radio` alone is not enough: it can be
    /// cleared before the audio thread has stopped streaming, and reading only
    /// it here is what once let a station and a track play over each other.
    fn yield_to_spotify(&self) {
        // Either sense of "on" counts, and a station that is only connecting
        // counts most of all: stopping it is what cancels the connect. Asking
        // the engine alone let a station the user had left behind finish
        // connecting and start playing over the track they had just chosen —
        // and `YieldToRadio`, seeing a live station, would then pause that
        // track rather than the stream.
        if self.radio_live() {
            self.stop_radio();
        }
    }

    /// `L` on a station row.
    ///
    /// Writes the file immediately rather than at exit: spot has no shutdown
    /// hook it can rely on, and a starred station that vanishes when the
    /// terminal closes is worse than no star at all.
    fn toggle_saved_station(&self, station: state::Station) {
        let stations = {
            let mut st = self.state.write();
            let existing = st
                .radio_favorites
                .iter()
                .position(|f| f.uuid == station.uuid);
            match existing {
                Some(i) => {
                    st.radio_favorites.remove(i);
                    st.toast(format!("removed {}", station.name));
                    st.radio_favorites.clone()
                }
                None => {
                    st.radio_favorites.push(station.clone());
                    st.toast(format!("saved {}", station.name));
                    st.radio_favorites.clone()
                }
            }
        };
        if let Err(e) = crate::config::save_radio(&stations) {
            log::error!("could not save the station list: {e:#}");
            self.state
                .write()
                .toast("could not write the saved-station list");
            return;
        }
        // The Saved page reads `radio_favorites` through this list, so it has
        // to be rebuilt in place — otherwise unstarring a station on that very
        // page leaves its row behind.
        let mut st = self.state.write();
        if let MainView::Radio(view) = &mut st.main
            && view.scope == state::RadioScope::Favorites
        {
            view.rows = stations.into_iter().map(state::RadioRow::Station).collect();
            let len = view.rows.len();
            if st.main_index >= len {
                st.main_index = len.saturating_sub(1);
            }
        }
    }

    /// Whether radio owns the output device, for the transport arms that must
    /// not drive the Spotify player while it does.
    ///
    /// Either source counts, because the two are true over different windows
    /// and both windows are ones where starting Spotify would put it under a
    /// station:
    ///
    /// * `AppState.radio` is set the moment a station is chosen and stays set
    ///   while it connects — seconds during which the engine is not live yet.
    /// * The engine is live from the hand-off until [`Self::stop_radio`],
    ///   which includes the turn of the command channel after the event layer
    ///   has cleared `AppState.radio` on a click. See [`Self::yield_to_spotify`].
    ///
    /// Deliberately the permissive direction: the cost of a false positive is
    /// a transport key that does nothing for one turn, and the cost of a false
    /// negative is both engines playing at once.
    fn radio_live(&self) -> bool {
        self.radio_player.is_live() || self.state.read().radio.is_some()
    }

    /// Pause Spotify if a station owns the device.
    ///
    /// The backstop under every "pause first" in this file. Those all fire
    /// *before* the thing that would make sound, and a `load` the player is
    /// still fetching can start after the pause meant to prevent it.
    /// This runs off `PlayerEvent::Playing` instead — librespot saying it has
    /// started, whatever asked — so the question is settled by what is
    /// actually making sound rather than by what we last asked for.
    fn yield_to_radio(&self) {
        // The permissive question, not the engine's own: a station change
        // silences the engine for the seconds the next one takes to connect,
        // and that is exactly the window in which a librespot load landing
        // late would start playing under a deck that says a station is on.
        if !self.radio_live() {
            return;
        }
        log::warn!("Spotify started under a live station; pausing it");
        if let Some(player) = self.player() {
            player.pause();
        }
        // The Spotify bar must not claim to be playing behind the station —
        // the `Playing` event that got us here has just set it.
        if let Some(pb) = self.state.write().playback.as_mut() {
            pb.is_playing = false;
        }
    }

    fn set_radio_volume(&self, percent: u8) {
        self.radio_player.set_volume(percent);
        if let Some(r) = self.state.write().radio.as_mut() {
            r.volume_percent = percent;
        }
    }

    /// What the soft mixer is actually applying, as a percent. Instant and
    /// local — it is the volume the audio path itself reads.
    fn local_volume_pct(&self) -> u8 {
        raw_to_pct(self.mixer.volume())
    }

    /// The volume both engines share, as a percent.
    ///
    /// Radio inherits whatever Spotify is at, so starting a station does not
    /// jump the level, and the one slider on the deck keeps meaning one thing.
    fn playback_volume(&self) -> u8 {
        let radio = self.state.read().radio.as_ref().map(|r| r.volume_percent);
        radio.unwrap_or_else(|| self.local_volume_pct())
    }

    /// `L` / the liked column / the deck's control: save or unsave one track.
    ///
    /// The map is written before the request so the mark flips on the
    /// keypress rather than a round trip later, and put back if the call
    /// fails — an optimistic mark that survives its own error is a mark
    /// that lies about your library. Errors are handled here rather than
    /// returned for that reason.
    async fn set_liked(&self, uri: String, liked: bool) {
        let Some(api) = self.api.clone() else { return };
        let previous = self.state.write().liked.insert(uri.clone(), liked);
        match api.set_track_liked(&uri, liked).await {
            Ok(()) => {
                self.state.write().toast(if liked {
                    "added to Liked Songs"
                } else {
                    "removed from Liked Songs"
                });
                // The saved-tracks page is now stale; the next open must
                // fetch it rather than serve the cached copy.
                self.cache.lock().remove(&state::liked_key());
            }
            Err(e) => {
                let mut st = self.state.write();
                match previous {
                    Some(was) => st.liked.insert(uri, was),
                    None => st.liked.remove(&uri),
                };
                st.toast(format!("error: {e}"));
            }
        }
    }

    /// `F` / the header's control: put a playlist in the library, or take it
    /// out.
    ///
    /// Optimistic and reversed on refusal, for the reason [`Self::set_liked`]
    /// gives. The library list moves with it: a saved playlist that did not
    /// appear on the Playlists page until the next refresh would read as the
    /// control having done nothing.
    async fn set_playlist_saved(&self, id: String, saved: bool) {
        let Some(api) = self.api.clone() else { return };
        let name = {
            let mut st = self.state.write();
            st.saved_playlists.insert(id.clone(), saved);
            st.playlists
                .iter()
                .find(|p| p.id == id)
                .map_or_else(|| "the playlist".to_string(), |p| p.name.clone())
        };
        match api.set_playlist_saved(&id, saved).await {
            Ok(()) => {
                let mut st = self.state.write();
                if saved {
                    // The library list is only refreshed on demand, so the row
                    // arrives with the next `R`. Nothing here can build a
                    // `Playlist` — the detail this page has is not what the
                    // list stores — and a half-filled row is worse than none.
                    st.toast(format!("saved {name}"));
                } else {
                    let mut playlists = std::mem::take(&mut st.playlists);
                    playlists.retain(|p| p.id != id);
                    st.set_playlists(playlists);
                    st.saved_playlists.insert(id, false);
                    st.toast(format!("unsaved {name}"));
                }
            }
            Err(e) => {
                let mut st = self.state.write();
                st.saved_playlists.insert(id, !saved);
                st.toast(format!("error: {e}"));
            }
        }
    }

    /// Rename a playlist and reword its blurb.
    ///
    /// Not optimistic, unlike the marks above: this is text the user typed,
    /// and showing it as accepted before Spotify has taken it would leave the
    /// page disagreeing with the account with nothing on screen saying so. The
    /// box stays up and inert until the answer lands.
    async fn edit_playlist_details(&self, id: String, name: String, description: String, seq: u64) {
        let Some(api) = self.api.clone() else { return };
        let result = api.set_playlist_details(&id, &name, &description).await;
        let mut st = self.state.write();
        match result {
            Ok(()) => {
                if st.edit.as_ref().is_some_and(|e| e.seq == seq) {
                    st.edit = None;
                }
                if let Some(p) = st.playlists.iter_mut().find(|p| p.id == id) {
                    p.name = name.clone();
                }
                st.rebuild_playlists_display();
                // The open page, when it is still this playlist. The subtitle
                // is left alone: neither the owner nor the sharing moved.
                if let MainView::Tracks(list) = &mut st.main
                    && list.cache_key.as_deref() == Some(&state::playlist_key(&id))
                {
                    list.header.name = name;
                    list.header.description = description;
                }
                st.toast("playlist updated");
            }
            Err(e) => {
                let message = e.to_string();
                match st.edit.as_mut().filter(|edit| edit.seq == seq) {
                    Some(edit) => {
                        edit.pending = false;
                        edit.error = Some(message);
                    }
                    None => st.toast(format!("error: {message}")),
                }
            }
        }
    }

    /// Put the box's record on a playlist, or take it off, and tell the box
    /// which way it went.
    ///
    /// Errors are handled here rather than returned, like [`Self::set_liked`]
    /// above: the mark is flipped before the request goes out so the row
    /// answers the click, and a refusal has to put it back. `seq` guards that
    /// — the box can be closed and opened again on another record while this
    /// is in flight, and a late answer must not touch the new one.
    async fn set_on_playlist(&self, playlist_id: String, uri: String, on: bool, seq: u64) {
        let Some(api) = self.api.clone() else { return };
        let id = state::track_id(&uri).to_string();
        set_cached_membership(&mut self.state.write(), &playlist_id, &id, on);
        let result = api.set_track_on_playlist(&playlist_id, &uri, on).await;
        let mut st = self.state.write();
        if let Some(picker) = st.picker_for(seq) {
            picker.pending.remove(&playlist_id);
        }
        match result {
            Ok(snapshot_id) => {
                // Both halves are needed. The cache keys a playlist's tracks
                // on the snapshot the fetch saw, and `start_track_fetch`
                // compares that against the copy in `playlists` — leaving the
                // old hash there would serve the list back as it stood.
                if let Some(p) = st.playlists.iter_mut().find(|p| p.id == playlist_id) {
                    p.snapshot_id = snapshot_id.clone();
                }
                self.cache.lock().remove(&state::playlist_key(&playlist_id));
                // The contents held here are still right — this change is the
                // one thing that moved them, and it is already applied — so
                // the new snapshot is stamped on rather than the set dropped.
                if let Some(contents) = st.playlist_tracks.get_mut(&playlist_id) {
                    contents.snapshot_id = snapshot_id;
                }
                save_playlist_tracks(&st);
                let name = st
                    .playlists
                    .iter()
                    .find(|p| p.id == playlist_id)
                    .map_or_else(|| "the playlist".to_string(), |p| p.name.clone());
                st.toast(match on {
                    true => format!("added to {name}"),
                    false => format!("removed from {name}"),
                });
            }
            Err(e) => {
                set_cached_membership(&mut st, &playlist_id, &id, !on);
                if let Some(picker) = st.picker_for(seq) {
                    picker.error = Some(e.to_string());
                }
            }
        }
    }

    /// Read what these playlists hold, for the box's marks.
    ///
    /// Only what is not already cached and not already being read: the whole
    /// library is offered every time the playlists load, and a walk costs one
    /// request per hundred tracks. A few at a time rather than all of them —
    /// a library of two hundred playlists let go at once is two hundred
    /// concurrent requests, which is a rate limit and a stalled UI.
    async fn cache_playlist_tracks(&mut self, playlist_ids: Vec<String>) {
        let Some(api) = self.api.clone() else { return };
        let wanted: Vec<(String, String)> = {
            let st = self.state.read();
            playlist_ids
                .into_iter()
                .filter(|id| !st.playlist_tracks.contains_key(id))
                // The snapshot is read before the walk, so a playlist changed
                // while it runs comes back with a hash the next load rejects
                // and gets walked again.
                .filter_map(|id| {
                    let snapshot = st
                        .playlists
                        .iter()
                        .find(|p| p.id == id)?
                        .snapshot_id
                        .clone();
                    Some((id, snapshot))
                })
                .filter(|(id, _)| self.membership_probe.insert(id.clone()))
                .collect()
        };
        if wanted.is_empty() {
            return;
        }
        let answers: Vec<_> = futures::stream::iter(wanted.into_iter().map(|(id, snapshot)| {
            let api = api.clone();
            async move {
                let answer = api.playlist_track_ids(&id).await;
                (id, snapshot, answer)
            }
        }))
        .buffer_unordered(PREFETCH_CONCURRENCY)
        .collect()
        .await;

        let mut st = self.state.write();
        for (id, snapshot_id, answer) in answers {
            self.membership_probe.remove(&id);
            match answer {
                Ok(track_ids) => {
                    st.playlist_tracks.insert(
                        id,
                        state::PlaylistContents {
                            snapshot_id,
                            track_ids,
                        },
                    );
                }
                // Left uncached rather than guessed at: the box draws a row it
                // cannot answer for as neither on nor off, and refuses to
                // flip it, which is the honest outcome of a failed read.
                Err(e) => log::warn!("playlist {id} contents read failed: {e}"),
            }
        }
        save_playlist_tracks(&st);
    }

    /// Load the playlist list, and start the walk that fills the box's marks.
    ///
    /// The prefetch rides this load rather than deciding for itself when to
    /// start: this already runs at sign-in and on `R`, and it is the only
    /// place that knows both which playlists exist and which snapshots they
    /// are at. A warm start finds every snapshot unchanged and asks for
    /// nothing.
    async fn load_playlists(&self) {
        let Some(api) = self.api.clone() else { return };
        // Who you are, the first time only: it cannot change within a session,
        // and `R` re-runs this whole function.
        if self.state.read().me_id.is_none()
            && let Ok(account) = api.account().await
        {
            self.state.write().me_id = Some(account.id);
        }
        let result = api.playlists().await;
        let mut st = self.state.write();
        let playlists = match result {
            Ok(playlists) => playlists,
            Err(e) => {
                st.playlists_error = Some(state::LoadError::new(
                    e.to_string(),
                    AppCommand::LoadPlaylists,
                ));
                st.toast(format!("failed to load playlists: {e}"));
                return;
            }
        };
        st.playlists_error = None;
        // A playlist whose snapshot moved was changed somewhere else, so what
        // was cached of its contents is stale. One whose id is gone entirely
        // is a playlist you no longer have.
        if drop_stale_playlist_tracks(&mut st.playlist_tracks, &playlists) {
            save_playlist_tracks(&st);
        }
        let uncached = uncached_playlists(&st.playlist_tracks, &playlists, st.me_id.as_deref());
        st.set_playlists(playlists);
        // The rows the open box holds are indices into the list just replaced.
        st.picker = None;
        drop(st);
        if !uncached.is_empty() {
            let _ = self.tx.send(AppCommand::CachePlaylistTracks {
                playlist_ids: uncached,
            });
        }
    }

    fn load_liked_view(&self, preserve_view: bool) {
        let mut list = TrackList::new("Liked Songs", "your saved tracks", None);
        list.kind = TrackListKind::LikedSongs;
        self.start_track_fetch(list, TrackSource::Liked, None, preserve_view);
    }

    fn load_playlist_view(&self, playlist_id: String, preserve_view: bool) {
        let known = self
            .state
            .read()
            .playlists
            .iter()
            .find(|p| p.id == playlist_id)
            .cloned();
        // A playlist reached from a search is not in the library, so there is
        // nothing to seed the header from. The page opens on a name that says
        // what it is and fills in when the detail fetch lands.
        let mut list = match &known {
            Some(p) => {
                let mut list = TrackList::new(
                    p.name.clone(),
                    playlist_subtitle(&p.owner, p.public, p.collaborative),
                    (p.track_count > 0).then_some(p.track_count),
                );
                list.header.cover_url = p.cover_url.clone();
                list.header.owner_id = p.owner_id.clone();
                list
            }
            None => TrackList::new("Playlist", String::new(), None),
        };
        list.kind = TrackListKind::Playlist;
        let cover_url = list.header.cover_url.clone();
        let snapshot = known.as_ref().map(|p| p.snapshot_id.clone());
        self.start_track_fetch(
            list,
            TrackSource::Playlist(playlist_id.clone()),
            snapshot.filter(|s| !s.is_empty()),
            preserve_view,
        );
        // After the view is installed, so a stale fetch cannot clear the slot
        // the new one just claimed.
        self.load_view_cover(cover_url);
        self.load_playlist_detail(playlist_id);
    }

    /// Fill in what the library list does not carry — the blurb, the sharing,
    /// and the cover and owner of a playlist you do not follow.
    ///
    /// A second fetch rather than a wait: the tracks are what the page is for,
    /// and holding them back for a line of prose would make every playlist
    /// open slower to make one of them read better.
    fn load_playlist_detail(&self, playlist_id: String) {
        let Some(api) = self.api.clone() else { return };
        let generation = self.state.read().load_generation;
        let (state, tx) = (self.state.clone(), self.tx.clone());
        tokio::spawn(async move {
            let Ok(detail) = api.playlist_detail(&playlist_id).await else {
                // Nothing to say. The page already has its tracks, and a
                // missing blurb is not news worth a toast.
                return;
            };
            let saved = api.playlist_saved(&playlist_id).await.ok();
            let cover_url = {
                let mut st = state.write();
                if st.load_generation != generation {
                    return;
                }
                if let Some(saved) = saved {
                    st.saved_playlists.insert(playlist_id.clone(), saved);
                }
                let MainView::Tracks(list) = &mut st.main else {
                    return;
                };
                if list.cache_key.as_deref() != Some(&state::playlist_key(&playlist_id)) {
                    return;
                }
                list.header.name = detail.name;
                list.header.subtitle =
                    playlist_subtitle(&detail.owner, detail.public, detail.collaborative);
                list.header.description = detail.description;
                list.header.owner_id = detail.owner_id;
                if let Some(total) = detail.total {
                    list.total = Some(total);
                }
                // Only when the page opened without one: a library playlist
                // already drew its cover, and re-pointing the slot at the same
                // URL would blink the art off and back.
                match (&list.header.cover_url, &detail.cover_url) {
                    (Some(_), _) | (None, None) => None,
                    (None, Some(url)) => {
                        list.header.cover_url = Some(url.clone());
                        Some(url.clone())
                    }
                }
            };
            // Back through the loop rather than fetched here: `LoadViewCover`
            // is already the one way the browsed slot is filled, and it owns
            // the generation guard that keeps two pages from racing for it.
            if cover_url.is_some() {
                let _ = tx.send(AppCommand::LoadViewCover { cover_url });
            }
        });
    }

    fn load_album_view(
        &self,
        id: String,
        name: String,
        artists: String,
        year: String,
        cover_url: Option<String>,
        preserve_view: bool,
    ) {
        let subtitle = if year.is_empty() {
            artists.clone()
        } else if artists.is_empty() {
            year.clone()
        } else {
            format!("{artists} · {year}")
        };
        let mut list = TrackList::new(name.clone(), subtitle, None);
        list.kind = TrackListKind::Album;
        list.header.cover_url = cover_url.clone();
        self.start_track_fetch(
            list,
            TrackSource::Album {
                id,
                name,
                artists,
                year,
                cover_url: cover_url.clone(),
            },
            None,
            preserve_view,
        );
        // After the view is installed, so a stale fetch cannot clear the slot
        // the new one just claimed.
        self.load_view_cover(cover_url);
    }

    /// Install an artist view and fill it from one concurrent overview
    /// fetch (photo + top tracks + albums). Guarded by the same generation
    /// scheme as track fetches.
    fn load_artist_view(&self, id: String, uri: String, name: String, preserve_view: bool) {
        let Some(api) = self.api.clone() else { return };
        let reopen = AppCommand::OpenArtist {
            id: id.clone(),
            uri: uri.clone(),
            name: name.clone(),
        };
        let view = ArtistView {
            id: id.clone(),
            uri,
            name: name.clone(),
            image_url: None,
            genres: Vec::new(),
            top: TrackList::new(name, "top tracks", None),
            albums: state::SortedList::new(),
            tab: ArtistTab::Albums,
            loading: true,
            error: None,
        };
        let generation = {
            let mut st = self.state.write();
            st.load_generation += 1;
            st.main = MainView::Artist(view);
            if !preserve_view {
                st.main_to_top();
            }
            st.load_generation
        };
        let state = self.state.clone();
        let http = self.http.clone();
        tokio::spawn(async move {
            let result = api.artist_overview(&id).await;
            let (uris, art) = {
                let mut st = state.write();
                if st.load_generation != generation {
                    return;
                }
                let MainView::Artist(v) = &mut st.main else {
                    return;
                };
                v.loading = false;
                let out = match result {
                    Ok(overview) => {
                        let uris: Vec<String> =
                            overview.top.iter().map(|t| t.uri.clone()).collect();
                        v.image_url = overview.image_url;
                        v.genres = overview.genres;
                        v.top.append(overview.top);
                        v.albums = overview.albums.into();
                        v.retab();
                        // The photo first: it is the one image already on
                        // screen when the page lands, and the fetches run in
                        // order. Then the group you are looking at, then the
                        // other groups — every one of them came back in the
                        // same pass, so their sleeves cost nothing but time.
                        //
                        // Except Appears On. It is not the artist's own work,
                        // it is often longer than everything else together,
                        // and most of it is never looked at; fetching it here
                        // would put a queue of other people's records in front
                        // of the ones you came for. `LoadArtistArt` asks for
                        // those sleeves if and when you open that group.
                        let open = v.albums.rows().filter_map(|a| a.cover_url.clone());
                        let rest = v
                            .albums
                            .items
                            .iter()
                            .enumerate()
                            .filter(|(i, a)| {
                                !v.albums.display.contains(i) && !ArtistTab::AppearsOn.holds(a)
                            })
                            .filter_map(|(_, a)| a.cover_url.clone());
                        let art: Vec<String> = v
                            .image_url
                            .iter()
                            .cloned()
                            .chain(open)
                            .chain(rest)
                            .collect();
                        (uris, art)
                    }
                    Err(e) => {
                        v.error = Some(state::LoadError::new(e.to_string(), reopen));
                        st.toast(format!("failed to load artist: {e}"));
                        return;
                    }
                };
                // The catalogue landing on a page that is already sorted takes
                // the page order rather than fetch order, and the selection
                // stays on the row it was on.
                st.resort_main();
                out
            };
            spawn_page_art(http, state.clone(), art, generation);
            spawn_liked_check(api, state, uris);
        });
    }

    /// Fetch the sleeves of the artist page's open album group.
    ///
    /// The page load fetches every group but Appears On, so this is a no-op
    /// for them — `spawn_page_art` drops the URLs it already holds. Switching
    /// to Appears On is what asks for its sleeves, and switching back into it
    /// later costs nothing.
    fn load_artist_art(&self) {
        let (urls, generation) = {
            let st = self.state.read();
            let MainView::Artist(v) = &st.main else {
                return;
            };
            let urls: Vec<String> = v
                .albums
                .rows()
                .filter_map(|a| a.cover_url.clone())
                .collect();
            (urls, st.load_generation)
        };
        spawn_page_art(self.http.clone(), self.state.clone(), urls, generation);
    }

    /// Install `list` as the main view, serving it from the cache when the
    /// snapshot still matches, otherwise streaming pages in from a spawned
    /// task. A newer load bumps `load_generation`, and the stale task exits
    /// at its next page boundary.
    ///
    /// The playing queue rides along: a queue started from this source while
    /// its pages were still landing (see [`Self::play`]) is extended with
    /// every page under the same lock, so the play order grows exactly as the
    /// view does.
    fn start_track_fetch(
        &self,
        mut list: TrackList,
        source: TrackSource,
        expected_snapshot: Option<String>,
        preserve_view: bool,
    ) {
        let Some(api) = self.api.clone() else { return };
        let key = source.cache_key();
        list.cache_key = Some(key.clone());

        // Cache hit: install instantly, no fetch. (Clone outside the state
        // lock; lock order is always state -> cache.)
        let cached: Option<Vec<crate::app::state::Track>> = self
            .cache
            .lock()
            .get(&key)
            .filter(|e| expected_snapshot.is_none() || e.snapshot_id == expected_snapshot)
            .map(|e| e.tracks.clone());
        if let Some(tracks) = cached {
            let mut st = self.state.write();
            list.append(tracks);
            st.main = MainView::Tracks(list);
            if !preserve_view {
                st.main_to_top();
            }
            return;
        }

        let generation = {
            let mut st = self.state.write();
            st.load_generation += 1;
            list.loading = true;
            list.generation = st.load_generation;
            st.main = MainView::Tracks(list);
            if !preserve_view {
                st.main_to_top();
            }
            st.load_generation
        };
        let state = self.state.clone();
        let cache = self.cache.clone();
        tokio::spawn(async move {
            let mut offset = 0u32;
            loop {
                let result = match &source {
                    TrackSource::Liked => api.liked_songs_page(offset).await,
                    TrackSource::Playlist(id) => api.playlist_tracks_page(id, offset).await,
                    TrackSource::Album { id, name, year, .. } => {
                        api.album_tracks_page(id, name, year, offset).await
                    }
                };
                let mut finished: Option<Vec<crate::app::state::Track>> = None;
                let page_uris: Vec<String>;
                {
                    let mut st = state.write();
                    if st.load_generation != generation {
                        return;
                    }
                    // Split borrow: the queue is extended below while the
                    // view is written.
                    let AppState { main, queue, .. } = &mut *st;
                    let MainView::Tracks(list) = main else {
                        return;
                    };
                    if list.generation != generation {
                        return;
                    }
                    match result {
                        Ok((tracks, has_more, total)) => {
                            page_uris = tracks.iter().map(|t| t.uri.clone()).collect();
                            // A queue playing this very source while it was
                            // still loading grows with it, under the same
                            // lock, so play order and view can never skew.
                            if let Some(q) = queue.as_mut()
                                && q.loading
                                && q.source_key.as_deref() == Some(key.as_str())
                            {
                                q.extend(tracks.clone());
                                if !has_more {
                                    q.loading = false;
                                }
                            }
                            list.append(tracks);
                            list.total = Some(total);
                            if !has_more {
                                list.loading = false;
                                finished = Some(list.items.clone());
                            }
                            // Keep an active sort (and the anchored
                            // selection) correct as pages arrive.
                            st.resort_main();
                        }
                        Err(e) => {
                            list.loading = false;
                            list.error =
                                Some(state::LoadError::new(e.to_string(), source.open_command()));
                            if let Some(q) = queue.as_mut()
                                && q.source_key.as_deref() == Some(key.as_str())
                            {
                                q.loading = false;
                            }
                            st.toast(format!("load failed: {e}"));
                            return;
                        }
                    }
                }
                // Heart data for the page (one 50-id request); liked songs
                // are liked by definition.
                if !page_uris.is_empty() {
                    match &source {
                        TrackSource::Liked => state
                            .write()
                            .liked
                            .extend(page_uris.iter().map(|u| (u.clone(), true))),
                        _ => match api.tracks_liked(&page_uris).await {
                            Ok(pairs) => state.write().liked.extend(pairs),
                            // Quiet skip (rate limits shouldn't toast-spam).
                            Err(e) => log::warn!("liked check failed: {e:#}"),
                        },
                    }
                }
                if let Some(tracks) = finished {
                    // Opening a playlist has just read every track in it, so
                    // the box's marks come free: a walk of a list already in
                    // memory, against a round trip the prefetch would make.
                    if let (TrackSource::Playlist(id), Some(snapshot_id)) =
                        (&source, expected_snapshot.clone())
                    {
                        let mut st = state.write();
                        st.playlist_tracks.insert(
                            id.clone(),
                            state::PlaylistContents {
                                snapshot_id,
                                track_ids: tracks
                                    .iter()
                                    .map(|t| state::track_id(&t.uri).to_string())
                                    .collect(),
                            },
                        );
                        save_playlist_tracks(&st);
                    }
                    cache.lock().insert(
                        key,
                        CachedTracks {
                            snapshot_id: expected_snapshot,
                            tracks,
                        },
                    );
                    return;
                }
                offset += PAGE_LIMIT;
            }
        });
    }

    /// Ask GitHub for the newest release and, if it beats this build, put it
    /// on Home.
    ///
    /// Logged rather than toasted on failure, for the reason
    /// [`resolve_radio_track`] gives: nobody asked for this check, and a
    /// machine that is offline would otherwise open with an error over the
    /// first screen.
    fn check_for_update(&self) {
        let http = self.http.clone();
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            match crate::update::latest(&http).await {
                Ok(Some(release)) => {
                    log::info!("update available: {}", release.tag);
                    state.write().update = Some(UpdateState::Available(release));
                }
                Ok(None) => {}
                Err(e) => log::warn!("update check failed: {e:#}"),
            }
        });
    }

    /// Download the release Home is offering and write it over this
    /// executable.
    ///
    /// Ignored unless a release is actually waiting, so a second Enter during
    /// the download does nothing and one on a finished install cannot start it
    /// again.
    fn install_update(&self) {
        let release = {
            let mut st = self.state.write();
            let Some(UpdateState::Available(release)) = st.update.clone() else {
                return;
            };
            st.update = Some(UpdateState::Installing);
            release
        };
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let outcome = crate::update::install(&release).await;
            let mut st = state.write();
            match outcome {
                Ok(()) => {
                    st.update = Some(UpdateState::Installed);
                    st.toast(format!("{} is installed — restart spot", release.tag));
                }
                Err(e) => {
                    log::error!("update to {} failed: {e:#}", release.tag);
                    st.update = Some(UpdateState::Failed);
                    st.toast(format!("update failed: {e}"));
                }
            }
        });
    }
}

impl Client {
    /// Install the playing item's cover at `url`, or clear it when there is
    /// none.
    ///
    /// This is the slot the accent and the visualizer's ramp are keyed on, so
    /// only playback may drive it.
    fn load_cover(&self, url: Option<String>) {
        self.fetch_cover(url, CoverSlot::Playing);
    }

    /// Install the *browsed* album's cover, for the album page's header band.
    ///
    /// Deliberately a different slot from [`Self::load_cover`]: browsing one
    /// album while another plays must leave the accent and the ramp on the
    /// record you are listening to, not the one you are looking at.
    fn load_view_cover(&self, url: Option<String>) {
        self.fetch_cover(url, CoverSlot::View);
    }

    /// Install the cover at `url` into `slot`, or clear that slot when there
    /// is none.
    ///
    /// Served from the cache when it is already decoded, otherwise the block
    /// falls back to its placeholder while a background task fetches. Each
    /// slot has its own generation counter, so a fast run of track changes (or
    /// of album pages) settles on the last one rather than on whichever
    /// request happens to return last — and the two never cancel each other.
    ///
    /// These GETs hit the image CDN, not `api.spotify.com`, so they do not
    /// consume the shared Web-API quota.
    fn fetch_cover(&self, url: Option<String>, slot: CoverSlot) {
        let generation = {
            let mut st = self.state.write();
            slot.bump(&mut st)
        };
        let install = move |state: &RwLock<AppState>, cover: Option<Arc<Cover>>| {
            let mut st = state.write();
            slot.install(&mut st, generation, cover);
        };

        // The URL is remote data; anything off the CDN is dropped rather than
        // fetched. See `cover::is_spotify_cdn`.
        let Some(url) = url.filter(|u| crate::cover::is_spotify_cdn(u)) else {
            return install(&self.state, None);
        };
        if let Some(hit) = self.covers.lock().get(&url) {
            return install(&self.state, Some(hit));
        }
        // Clear the slot while the fetch is in flight, so the block shows its
        // placeholder rather than the previous record's sleeve.
        install(&self.state, None);

        let (http, state, covers) = (self.http.clone(), self.state.clone(), self.covers.clone());
        tokio::spawn(async move {
            match crate::cover::load(&http, &url).await {
                Ok(cover) => {
                    let cover = Arc::new(cover);
                    covers.lock().insert(url, Arc::clone(&cover));
                    install(&state, Some(cover));
                }
                // Quiet: art is decoration, and a toast for it would be noise.
                Err(e) => log::warn!("cover load failed: {e:#}"),
            }
        });
    }
}

/// Which decoded cover a fetch is filling.
///
/// The two are separate because they are different records whenever you browse
/// one album while another plays — and because only [`CoverSlot::Playing`] may
/// repaint the UI's accent and the visualizer's ramp.
#[derive(Clone, Copy)]
enum CoverSlot {
    Playing,
    View,
}

impl CoverSlot {
    /// Claim this slot for a new fetch and return the generation that owns it.
    fn bump(self, st: &mut AppState) -> u64 {
        match self {
            CoverSlot::Playing => {
                st.cover_generation += 1;
                st.cover_generation
            }
            CoverSlot::View => {
                st.view_cover_generation += 1;
                st.view_cover_generation
            }
        }
    }

    /// Store `cover`, unless a later fetch has already claimed the slot.
    fn install(self, st: &mut AppState, generation: u64, cover: Option<Arc<Cover>>) {
        match self {
            CoverSlot::Playing if st.cover_generation == generation => {
                // The accent and the visualizer's ramp follow the art, so they
                // must move together or the UI briefly wears the previous
                // record's colours.
                crate::ui::set_cover_colors(cover.as_deref());
                st.cover = cover;
            }
            CoverSlot::View if st.view_cover_generation == generation => {
                st.view_cover = cover;
            }
            _ => {}
        }
    }
}

/// Images the artist page fetches at once. The CDN is outside the Web API's
/// quota, but a page of fifty sleeves is still fifty requests, and a handful
/// in flight fills the visible cards about as fast as all of them would while
/// leaving the connection pool to the rest of the client.
const PAGE_ART_CONCURRENCY: usize = 6;

/// Fetch and decode the artist page's images — the photo, then every album
/// sleeve — into [`AppState::page_art`].
///
/// Eager rather than on-scroll: the URLs all arrive with the album list, and
/// a card whose art appears only once you have scrolled to it reads as a bug.
/// A stale generation (another page opened) stops the run where it is.
fn spawn_page_art(
    http: reqwest::Client,
    state: Arc<RwLock<AppState>>,
    urls: Vec<String>,
    generation: u64,
) {
    use futures::StreamExt;

    let mut seen = std::collections::HashSet::new();
    let wanted: Vec<String> = {
        let st = state.read();
        urls.into_iter()
            // The URL is remote data, and only Spotify's own CDN may be
            // fetched. See `cover::is_spotify_cdn`.
            .filter(|u| crate::cover::is_spotify_cdn(u) && !st.page_art.contains(u))
            .filter(|u| seen.insert(u.clone()))
            .collect()
    };
    if wanted.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let mut fetches = futures::stream::iter(wanted)
            .map(|url| {
                let http = http.clone();
                async move {
                    let cover = crate::cover::load(&http, &url).await;
                    (url, cover)
                }
            })
            .buffer_unordered(PAGE_ART_CONCURRENCY);
        while let Some((url, cover)) = fetches.next().await {
            match cover {
                Ok(cover) => {
                    let mut st = state.write();
                    if st.load_generation != generation {
                        return;
                    }
                    st.page_art.insert(url, Arc::new(cover));
                }
                Err(e) => log::warn!("page art load failed: {e:#}"),
            }
        }
    });
}

/// Whether an announcement is one we have not already asked about.
///
/// A free function so the rule can be tested: `Client` needs a session to
/// build and cannot be constructed in a unit test.
fn needs_lookup(announced: &str, probe: Option<&str>) -> bool {
    !announced.trim().is_empty() && probe != Some(announced)
}

/// Ask Spotify for the announced record, narrowest query first.
///
/// Three queries at worst, and only because each is a different question. The
/// field-scoped form is what stops a one-word title coming back as thirty
/// unrelated records that happen to contain the word; the trimmed form covers
/// an annotation that belongs to the pressing rather than the record; the loose
/// form is for the announcements the scoping is too strict for. Each answer is
/// still put through `best_match`, so a wider query cannot buy a worse match.
async fn lookup(api: &Api, want: &crate::radio::track::Announcement) -> Option<state::Track> {
    let queries = [
        Some(want.scoped_query()),
        want.trimmed_query(),
        Some(want.loose_query()),
    ];
    for query in queries.into_iter().flatten() {
        match api.search_tracks(&query).await {
            Ok(cands) => {
                if let Some(hit) = crate::radio::track::best_match(&cands, want) {
                    return Some(hit);
                }
            }
            // Logged, not toasted. A lookup nobody asked for should not throw a
            // message over whatever the user is reading; the row simply says
            // the station's own words instead.
            Err(e) => {
                log::warn!("radio track lookup failed: {e:#}");
                return None;
            }
        }
    }
    None
}

/// Fetch saved-state for `uris` in the background and merge it in.
fn spawn_liked_check(api: Api, state: Arc<RwLock<AppState>>, uris: Vec<String>) {
    if uris.is_empty() {
        return;
    }
    tokio::spawn(async move {
        match api.tracks_liked(&uris).await {
            Ok(pairs) => state.write().liked.extend(pairs),
            Err(e) => log::warn!("liked check failed: {e:#}"),
        }
    });
}

/// The station after `current` in a country's chart, wrapping at the end.
///
/// A chart the playing station is not on still seeks: it can have come from a
/// search, a genre page or the saved list, be ranked past the directory's
/// limit, or have been dropped as broken since. Starting at the top is the
/// nearest honest reading of "the next one in this country".
///
/// HLS entries are stepped over rather than offered — spot cannot play them,
/// and a control that lands on one is a control that stops the music.
/// Whether a station that would not play is one step of a seek or the end of
/// the road.
///
/// A station you chose is the one you meant. A station a seek landed on is one
/// of a run, and a directory of ten thousand stations has plenty that no
/// longer answer — stopping on the first is what makes seeking useless.
fn seek_walks_on(attempt: u8) -> bool {
    attempt > 0 && attempt < SEEK_ATTEMPTS
}

fn next_in_country(stations: &[state::Station], current: &str) -> Option<state::Station> {
    let start = stations
        .iter()
        .position(|s| s.uuid == current)
        .map_or(0, |i| i + 1);
    stations
        .iter()
        .cycle()
        .skip(start)
        .take(stations.len())
        .find(|s| !s.hls && s.uuid != current)
        .cloned()
}

fn into_station_rows(stations: Vec<state::Station>) -> Vec<state::RadioRow> {
    stations.into_iter().map(state::RadioRow::Station).collect()
}

fn into_facet_rows(facets: Vec<crate::radio::api::Facet>) -> Vec<state::RadioRow> {
    facets
        .into_iter()
        .map(|f| state::RadioRow::Facet {
            key: f.code,
            label: f.name,
            count: f.stationcount,
        })
        .collect()
}

/// A slider percent as librespot's 16-bit volume.
pub fn pct_to_raw(percent: u8) -> u16 {
    (percent.min(100) as u32 * u16::MAX as u32 / 100) as u16
}

/// librespot's 16-bit volume back as a slider percent.
///
/// Rounded, not truncated: the mixer stores the value mapped through its
/// logarithmic curve and unmaps it on the way out, which costs a bit of
/// precision, and truncating that turns a round trip of 55 into 54. Rounding
/// makes [`pct_to_raw`] and this exact inverses across the whole range.
pub fn raw_to_pct(raw: u16) -> u8 {
    ((raw as u32 * 100 + u16::MAX as u32 / 2) / u16::MAX as u32) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_playback::mixer::{self, MixerConfig};

    fn listed(id: &str, snapshot: &str, owner_id: &str) -> state::Playlist {
        state::Playlist {
            id: id.into(),
            name: id.into(),
            track_count: 1,
            owner: owner_id.into(),
            owner_id: owner_id.into(),
            snapshot_id: snapshot.into(),
            cover_url: None,
            public: None,
            collaborative: false,
        }
    }

    fn holding(snapshot: &str, ids: &[&str]) -> state::PlaylistContents {
        state::PlaylistContents {
            snapshot_id: snapshot.into(),
            track_ids: ids.iter().map(|id| (*id).to_string()).collect(),
        }
    }

    /// A playlist changed somewhere else comes back with a new snapshot, and
    /// its cached contents go with the old one. The rest stand — a run that
    /// dropped everything would walk the whole library again.
    #[test]
    fn a_moved_snapshot_drops_that_playlist_and_no_other() {
        let mut cache = HashMap::from([
            ("p1".to_string(), holding("s1", &["a"])),
            ("p2".to_string(), holding("s2", &["b"])),
            ("gone".to_string(), holding("s3", &["c"])),
        ]);
        let playlists = vec![listed("p1", "moved", "me"), listed("p2", "s2", "me")];
        assert!(drop_stale_playlist_tracks(&mut cache, &playlists));
        assert!(!cache.contains_key("p1"), "the changed playlist stood");
        assert!(
            cache.contains_key("p2"),
            "an unchanged playlist was dropped"
        );
        assert!(!cache.contains_key("gone"), "a playlist you no longer have");
        assert!(
            !drop_stale_playlist_tracks(&mut cache, &playlists),
            "a warm cache rewrote the file for nothing"
        );
    }

    /// The prefetch asks only for what it does not hold, so a warm start asks
    /// for nothing — and never for a playlist you only follow, which the box
    /// does not offer.
    #[test]
    fn the_prefetch_asks_only_for_uncached_playlists_you_own() {
        let cache = HashMap::from([("p1".to_string(), holding("s1", &["a"]))]);
        let playlists = vec![
            listed("p1", "s1", "me"),
            listed("p2", "s2", "me"),
            listed("p3", "s3", "them"),
        ];
        assert_eq!(
            uncached_playlists(&cache, &playlists, Some("me")),
            vec!["p2".to_string()]
        );
        assert!(
            uncached_playlists(&cache, &playlists, None).is_empty(),
            "nothing is yours before the account is known"
        );
    }

    /// The client puts a refused change back, and a playlist nothing has
    /// walked is left alone — a set holding one id would read as a playlist
    /// holding one track.
    #[test]
    fn a_membership_change_flips_back_and_skips_the_unwalked() {
        let mut st = AppState::new();
        st.playlist_tracks.insert("p1".into(), holding("s1", &[]));
        set_cached_membership(&mut st, "p1", "x", true);
        assert!(st.playlist_tracks["p1"].track_ids.contains("x"));
        set_cached_membership(&mut st, "p1", "x", false);
        assert!(!st.playlist_tracks["p1"].track_ids.contains("x"));

        set_cached_membership(&mut st, "p9", "x", true);
        assert!(!st.playlist_tracks.contains_key("p9"));
    }

    /// Every percent the UI can produce has to survive a trip through the
    /// mixer unchanged, or the slider drifts a step at a time: `VolumeRel`
    /// reads the mixer back to find its base, so a percent that comes back
    /// one low turns every nudge into a nudge and a half.
    #[test]
    fn volume_percent_round_trips_through_the_mixer() {
        let mixer = mixer::find(None).expect("softvol mixer")(MixerConfig::default())
            .expect("open the mixer");
        for pct in 0..=100u8 {
            mixer.set_volume(pct_to_raw(pct));
            assert_eq!(
                raw_to_pct(mixer.volume()),
                pct,
                "volume {pct}% did not survive"
            );
        }
    }

    #[test]
    fn volume_percent_spans_the_whole_range() {
        assert_eq!(pct_to_raw(0), 0);
        assert_eq!(pct_to_raw(100), u16::MAX);
        assert_eq!(raw_to_pct(0), 0);
        assert_eq!(raw_to_pct(u16::MAX), 100);
        // Out-of-range input is clamped rather than wrapped.
        assert_eq!(pct_to_raw(200), u16::MAX);
    }

    /// The guard that keeps the three-second tick from re-asking Spotify the
    /// same question for the length of a record.
    #[test]
    fn a_lookup_runs_once_per_announcement() {
        // Nothing asked yet.
        assert!(needs_lookup("Aspen - Seasick", None));
        // Asked, and the station has not moved on.
        assert!(!needs_lookup("Aspen - Seasick", Some("Aspen - Seasick")));
        // The next record.
        assert!(needs_lookup("Moby - Porcelain", Some("Aspen - Seasick")));
        // An empty announcement is not a question.
        assert!(!needs_lookup("", None));
        assert!(!needs_lookup("   ", None));
    }

    fn chart(entries: &[(&str, bool)]) -> Vec<state::Station> {
        entries
            .iter()
            .map(|(uuid, hls)| state::Station {
                uuid: (*uuid).into(),
                name: (*uuid).into(),
                url: format!("http://stream/{uuid}"),
                homepage: String::new(),
                tags: String::new(),
                country: String::new(),
                countrycode: "DE".into(),
                language: String::new(),
                codec: "MP3".into(),
                bitrate: 128,
                votes: 1,
                hls: *hls,
            })
            .collect()
    }

    /// `seek ▸▸` walks down the chart and comes back to the top, so a country
    /// is a ring rather than a dead end.
    #[test]
    fn the_seek_walks_the_chart_and_wraps() {
        let list = chart(&[("a", false), ("b", false), ("c", false)]);
        assert_eq!(next_in_country(&list, "a").unwrap().uuid, "b");
        assert_eq!(next_in_country(&list, "c").unwrap().uuid, "a");
    }

    /// HLS entries are listed but unplayable, so the walk steps over them
    /// rather than landing on one and stopping the music.
    #[test]
    fn the_seek_steps_over_stations_spot_cannot_play() {
        let list = chart(&[("a", false), ("b", true), ("c", false)]);
        assert_eq!(next_in_country(&list, "a").unwrap().uuid, "c");
    }

    /// A station the chart does not list — reached from a search or a genre,
    /// ranked past the directory's limit, or dropped as broken — still seeks,
    /// from the top.
    #[test]
    fn a_station_the_chart_does_not_list_seeks_from_the_top() {
        let list = chart(&[("a", false), ("b", false)]);
        assert_eq!(next_in_country(&list, "elsewhere").unwrap().uuid, "a");
    }

    /// A seek that keeps hitting stations which will not play walks past them
    /// rather than stopping on the first one — a directory of ten thousand
    /// stations has plenty that answer and plenty that do not, and stopping on
    /// a dead one is what makes seeking useless.
    #[test]
    fn a_seek_walks_past_the_stations_that_would_not_play() {
        let list = chart(&[
            ("a", false),
            ("dead1", false),
            ("dead2", false),
            ("b", false),
        ]);
        let mut at = "a".to_string();
        for expected in ["dead1", "dead2", "b"] {
            at = next_in_country(&list, &at).unwrap().uuid;
            assert_eq!(at, expected);
        }
    }

    /// A station you chose is the one you meant, and a failure is the answer.
    /// A station a seek landed on is one of a run, and a failure is a reason to
    /// keep walking — up to a bound, because the cost is time rather than
    /// requests: each dead station holds the walk for the whole connect
    /// timeout.
    #[test]
    fn only_a_seek_walks_on_from_a_station_that_would_not_play() {
        assert!(!seek_walks_on(0), "a station you chose yourself");
        assert!(seek_walks_on(1), "the first station a seek landed on");
        assert!(!seek_walks_on(SEEK_ATTEMPTS), "the walk is bounded");
        assert!(!seek_walks_on(SEEK_ATTEMPTS + 1));
    }

    /// Nothing playable in the country: the caller says so rather than looping.
    #[test]
    fn a_country_of_nothing_playable_seeks_nowhere() {
        assert!(next_in_country(&chart(&[("a", true)]), "a").is_none());
        assert!(next_in_country(&chart(&[("a", false)]), "a").is_none());
        assert!(next_in_country(&[], "a").is_none());
    }
}
