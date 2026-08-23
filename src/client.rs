use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use librespot_connect::Spirc;
use librespot_playback::mixer::Mixer;
use parking_lot::{Mutex, RwLock};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use crate::api::{Api, PAGE_LIMIT};
use crate::app::command::AppCommand;
use crate::app::state::{
    self, AppState, ArtistView, LocalPlayback, MainView, TrackList, TrackListKind,
};
use crate::cover::{Cover, CoverCache};
use crate::radio::api::RadioApi;
use crate::radio::player::RadioPlayer;

/// Where a streamed track fetch pulls its pages from.
enum TrackSource {
    Liked,
    Playlist(String),
    Album {
        id: String,
        name: String,
        year: String,
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
}

/// A finished fetch, kept so reopening the same view is instant.
struct CachedTracks {
    /// Playlist snapshot the tracks belong to; None for liked songs.
    snapshot_id: Option<String>,
    tracks: Vec<crate::app::state::Track>,
}

type TrackCache = HashMap<String, CachedTracks>;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const COMMAND_REPOLL_DELAY: Duration = Duration::from_millis(400);
const BACKOFF_INITIAL: Duration = Duration::from_secs(15);
const BACKOFF_MAX: Duration = Duration::from_secs(120);
const COVER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const COVER_TIMEOUT: Duration = Duration::from_secs(10);

/// Background task: consumes UI commands, drives Spirc (instant transport
/// control) and the Web API (data + starting playback), and refreshes the
/// shared playback snapshot.
pub struct Client {
    api: Api,
    spirc: Spirc,
    /// librespot's soft mixer: the volume actually being applied, readable
    /// and writable without a round trip. Spirc writes it for remote volume
    /// changes too, so it is the truth for our device either way.
    mixer: Arc<dyn Mixer>,
    /// What librespot's player events say about play/pause, so the Web API
    /// poll can leave the local answer alone.
    local: Arc<LocalPlayback>,
    state: Arc<RwLock<AppState>>,
    rx: UnboundedReceiver<AppCommand>,
    activated: bool,
    /// When to re-poll after a transport command, as a deadline rather than a
    /// sleep: the command loop has to keep draining while it waits, or the
    /// next keypress sits in the channel behind it.
    repoll_at: Option<Instant>,
    /// Rate-limit backoff: no playback polling until this instant.
    backoff_until: Option<Instant>,
    backoff: Duration,
    /// Completed track fetches by cache key; shared with fetch tasks.
    cache: Arc<Mutex<TrackCache>>,
    /// Shared with cover-fetch tasks, so the connection to Spotify's image
    /// CDN stays warm across track changes.
    http: reqwest::Client,
    covers: Arc<Mutex<CoverCache>>,
    /// The playing track we last asked the saved-check about, so a poll every
    /// three seconds does not keep asking. It is asked once per track and the
    /// answer only changes when we change it — and an answer that never comes
    /// (a 429, say) must not turn into one request per poll for as long as the
    /// record is on.
    liked_probe: Option<String>,
    /// The announcement we last ran a Spotify lookup for.
    ///
    /// The radio twin of [`Self::liked_probe`], and set *before* the search
    /// goes out rather than when it answers: a lookup that fails must cost one
    /// request per announced title, not one per poll for as long as the record
    /// is on. Cleared when the station changes or stops, so the same string
    /// announced by a different station is asked about again.
    radio_probe: Option<String>,
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
    pub fn new(
        api: Api,
        spirc: Spirc,
        mixer: Arc<dyn Mixer>,
        local: Arc<LocalPlayback>,
        state: Arc<RwLock<AppState>>,
        rx: UnboundedReceiver<AppCommand>,
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
            api,
            spirc,
            mixer,
            local,
            state,
            rx,
            activated: false,
            repoll_at: None,
            backoff_until: None,
            backoff: BACKOFF_INITIAL,
            cache: Arc::new(Mutex::new(HashMap::new())),
            radio_api: RadioApi::new(http.clone()),
            radio_player: RadioPlayer::new(audio_tap),
            http,
            covers: Arc::new(Mutex::new(CoverCache::default())),
            liked_probe: None,
            radio_probe: None,
            shutdown_ack: Some(shutdown_ack),
        };
        (client, shutdown_done)
    }

    pub async fn run(mut self) {
        let mut poll = tokio::time::interval(POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // A deadline the loop watches, rather than a sleep inside the
            // command arm: waiting there stalled every following command for
            // the delay plus a round trip, which is what made a second key
            // press feel like it had been swallowed. A burst of commands just
            // pushes the deadline out, so the burst still costs one poll.
            let repoll = self.repoll_at;
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    // Handled here rather than in `handle`: it is the one
                    // command that ends the loop, and it must not be racing a
                    // poll that would talk to a device we just disconnected.
                    Some(AppCommand::Shutdown) => {
                        self.shutdown();
                        break;
                    }
                    Some(cmd) => {
                        if command_touches_playback(&cmd) {
                            self.repoll_at = Some(Instant::now() + COMMAND_REPOLL_DELAY);
                        }
                        if let Err(e) = self.handle(cmd).await {
                            log::error!("command failed: {e:#}");
                            self.state.write().toast(format!("error: {e}"));
                        }
                    }
                    None => break,
                },
                _ = async { tokio::time::sleep_until(repoll.unwrap().into()).await }, if repoll.is_some() => {
                    self.repoll_at = None;
                    self.refresh_playback().await;
                }
                _ = poll.tick() => {
                    self.refresh_playback().await;
                    // Not on the `repoll_at` arm above: that one exists to
                    // catch up with a transport command, and what a station is
                    // announcing has nothing to do with one. Three seconds of
                    // lag on the metadata row is invisible anyway — ICY blocks
                    // do not arrive faster than that.
                    self.resolve_radio_track();
                }
            }
        }
    }

    /// Make our Connect device the active one before the first playback
    /// command; `/me/player` reports nothing until a device is active.
    ///
    /// Repeat is pinned to repeat-all here rather than exposed as a control:
    /// the queue is meant to keep going, and a mode the UI does not show is a
    /// mode that strands people on a silent player.
    fn ensure_active(&mut self) {
        if !self.activated {
            if let Err(e) = self.spirc.activate() {
                log::warn!("spirc activate failed: {e}");
            }
            if let Err(e) = self
                .spirc
                .repeat_track(false)
                .and_then(|_| self.spirc.repeat(true))
            {
                log::warn!("spirc repeat-all failed: {e}");
            }
            self.activated = true;
        }
    }

    async fn handle(&mut self, cmd: AppCommand) -> Result<()> {
        use AppCommand::*;
        match cmd {
            // Whichever engine owns the device owns the transport. Radio is
            // checked first everywhere below for that reason: while a station
            // is on, Spirc is paused and talking to it would start Spotify
            // playing underneath the stream.
            PlayPause if self.radio_live() => {
                let mut st = self.state.write();
                if let Some(r) = st.radio.as_mut() {
                    r.is_playing = !r.is_playing;
                    if r.is_playing {
                        self.radio_player.resume();
                    } else {
                        self.radio_player.pause();
                    }
                }
            }
            PlayPause => {
                self.spirc.play_pause()?;
                if let Some(pb) = self.state.write().playback.as_mut() {
                    pb.progress_ms = pb.interpolated_progress_ms();
                    pb.fetched_at = Instant::now();
                    pb.is_playing = !pb.is_playing;
                }
            }
            VolumeRel(delta) if self.radio_live() => {
                let current = self.playback_volume();
                self.set_radio_volume((i16::from(current) + i16::from(delta)).clamp(0, 100) as u8);
            }
            SetVolume(pct) if self.radio_live() => self.set_radio_volume(pct.min(100)),
            // A broadcast has no track either side of it and no position to
            // seek to, so these four have nothing to do while a station is on
            // — and reaching Spirc with them does not merely do nothing, it
            // starts Spotify playing underneath the stream. The key handler
            // already turns them into a toast, but it is not the only way in:
            // a mouse click, a media key, or a hit rect left over from the
            // Spotify deck all arrive here directly.
            Next | Prev | SeekRel(_) | SeekTo(_) | ToggleShuffle if self.radio_live() => {}
            Next => self.spirc.next()?,
            Prev => self.spirc.prev()?,
            SeekRel(delta_ms) => {
                let target = self.state.read().playback.as_ref().map(|pb| {
                    (pb.interpolated_progress_ms() as i64 + delta_ms)
                        .clamp(0, pb.duration_ms as i64) as u64
                });
                if let Some(target) = target {
                    self.spirc.set_position_ms(target as u32)?;
                    if let Some(pb) = self.state.write().playback.as_mut() {
                        pb.progress_ms = target;
                        pb.fetched_at = Instant::now();
                    }
                }
            }
            SeekTo(ms) => {
                let target = self
                    .state
                    .read()
                    .playback
                    .as_ref()
                    .map(|pb| ms.min(pb.duration_ms));
                if let Some(target) = target {
                    self.spirc.set_position_ms(target as u32)?;
                    if let Some(pb) = self.state.write().playback.as_mut() {
                        pb.progress_ms = target;
                        pb.fetched_at = Instant::now();
                    }
                }
            }
            VolumeRel(delta) => {
                // Stepped off the mixer, not off the snapshot: the snapshot's
                // percent is whatever `/me/player` last said, which after a
                // recent change is a second or so out of date — stepping from
                // it undoes part of the previous step.
                let current = self.local_volume_pct();
                let new_pct = (current as i16 + delta as i16).clamp(0, 100) as u8;
                self.spirc.set_volume(pct_to_raw(new_pct))?;
                if let Some(pb) = self.state.write().playback.as_mut() {
                    pb.volume_percent = new_pct;
                }
            }
            SetVolume(pct) => {
                let pct = pct.min(100);
                self.spirc.set_volume(pct_to_raw(pct))?;
                if let Some(pb) = self.state.write().playback.as_mut() {
                    pb.volume_percent = pct;
                }
            }
            ToggleShuffle => {
                let target = self
                    .state
                    .read()
                    .playback
                    .as_ref()
                    .map(|pb| !pb.shuffle)
                    .unwrap_or(true);
                self.spirc.shuffle(target)?;
                if let Some(pb) = self.state.write().playback.as_mut() {
                    pb.shuffle = target;
                }
            }
            PlayContext {
                context_uri,
                offset_uri,
            } => {
                self.begin_switch();
                let result = self
                    .api
                    .play_context(&context_uri, offset_uri.as_deref())
                    .await;
                self.settle_switch(result)?;
            }
            PlayTracks { uris, offset } => {
                self.begin_switch();
                let result = self.api.play_uris(&uris, offset).await;
                self.settle_switch(result)?;
            }
            AddToQueue(uri) => {
                self.api.add_to_queue(&uri).await?;
                self.state.write().toast("added to queue");
            }
            SetLiked { uri, liked } => self.set_liked(uri, liked).await,
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
                                    list.tracks
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
                    None => self.state.write().toast("playlists refreshed"),
                }
            }
            RefreshPlayback => self.refresh_playback().await,
            LoadQueue => self.load_queue(),
            LoadRadio { scope } => self.load_radio(scope),
            PlayStation(station) => self.play_station(*station).await,
            StopRadio => self.stop_radio(),
            ToggleSavedStation(station) => self.toggle_saved_station(*station),
            YieldToRadio => self.yield_to_radio(),
            // Intercepted by `run`, which is the only place that can end the
            // loop. Listed rather than swept into a catch-all so a new command
            // still has to be handled deliberately.
            Shutdown => {}
        }
        Ok(())
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
                Ok(stations) => results.stations = stations,
                // Logged, not toasted, unlike the Spotify half below. The
                // directory being unreachable is not *this search* failing —
                // four tabs of perfectly good results are on screen — and a
                // toast thrown over them would say that it was. The Stations
                // tab going from "searching…" to "no stations" is where that
                // news belongs, and it is only news to someone looking at it.
                Err(e) => log::error!("station search failed: {e:#}"),
            }
        });

        let result = self.api.search(&query).await;
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
                let uris: Vec<String> = results.tracks.iter().map(|t| t.uri.clone()).collect();
                drop(st);
                spawn_liked_check(self.api.clone(), self.state.clone(), uris);
            }
            Err(e) => st.toast(format!("search failed: {e}")),
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
                    view.rows = rows;
                    st.main_to_top();
                }
                Err(e) => {
                    log::error!("radio directory load failed: {e:#}");
                    st.toast(format!("could not reach the radio directory: {e}"));
                }
            }
        });
    }

    /// Start a station, having first got Spotify out of the way.
    ///
    /// The two engines share one output device, so this is the only place that
    /// may decide which of them owns it. Spirc is paused rather than
    /// deactivated: pausing is instant and local, and leaves the Connect device
    /// where the user's phone can still see it.
    async fn play_station(&mut self, station: state::Station) {
        if let Err(e) = self.spirc.pause() {
            log::warn!("could not pause Spotify before starting radio: {e}");
        }
        let volume = self.playback_volume();

        {
            let mut st = self.state.write();
            st.radio = Some(state::RadioPlayback::new(
                station.clone(),
                volume,
                self.radio_player.title(),
            ));
            // Whatever the last station was announcing is not this station's
            // business. Without this, moving to a station that happens to be
            // playing the same record would find the probe already set and
            // never look it up.
            self.radio_probe = None;
            // The Spotify bar must not keep claiming to be playing behind the
            // station; the snapshot itself is kept so stopping radio puts the
            // last track straight back rather than after the next poll.
            if let Some(pb) = st.playback.as_mut() {
                pb.is_playing = false;
            }
            // A track clicked a moment ago is not what is playing any more.
            // Left set, its wait would go on refusing polls until it timed out.
            st.pending_play = None;
        }

        // The directory's ranking runs on these, and it also hands back the
        // stream URL it believes in, which is fresher than the one in the row.
        let url = self
            .radio_api
            .click(&station.uuid)
            .await
            .unwrap_or_else(|| station.url.clone());

        // Again, right before the stream starts. A Spotify play asked for a
        // moment ago is a round trip out to the backend and back in over the
        // dealer, and it can land during the directory call above — after the
        // pause at the top of this function and before there is a station to
        // drown it out. This is the last point at which it can be caught.
        if let Err(e) = self.spirc.pause() {
            log::warn!("could not pause Spotify before starting radio: {e}");
        }
        if let Err(e) = self.radio_player.play(&url, volume).await {
            log::error!("could not play {}: {e:#}", station.name);
            let mut st = self.state.write();
            st.radio = None;
            st.toast(format!("could not play {}: {e}", station.name));
            return;
        }
        // And once more now the station is audible. `play` above spends several
        // seconds connecting and prefetching, and the pause before it can only
        // catch a Spotify play that had already arrived — one that lands during
        // the prefetch would start with nothing left to stop it. This is the
        // pause that is on the far side of that window.
        if let Err(e) = self.spirc.pause() {
            log::warn!("could not pause Spotify after starting radio: {e}");
        }
        self.state
            .write()
            .toast(format!("playing {}", station.name));
    }

    fn stop_radio(&self) {
        self.radio_player.stop();
        self.state.write().radio = None;
    }

    /// Silence both engines on the way out, then let the quit path know.
    ///
    /// The client is the only holder of the two handles that can do this, so
    /// without it nothing stopped playing when spot quit: the radio thread is
    /// detached and its stop command is only ever sent on user action, and the
    /// Connect device was left for the backend to time out. Both survived until
    /// the process died — which, if librespot's player thread was wedged, could
    /// be a long time after the terminal came back.
    fn shutdown(&mut self) {
        // Radio first: it is the one that kept playing, and it blocks until
        // the output device is actually closed.
        self.radio_player.shutdown();
        // Pauses playback, drops the Connect device off the user's device list
        // and ends the spirc future, which is what drops librespot's `Player`.
        if let Err(e) = self.spirc.shutdown() {
            log::warn!("spirc shutdown failed: {e}");
        }
        if let Some(ack) = self.shutdown_ack.take() {
            let _ = ack.send(());
        }
    }

    /// Look up what the station just announced, once per announcement.
    ///
    /// The ICY callback runs on the decoder thread, which has neither a runtime
    /// nor an `Api`, so a new announcement is *noticed* here rather than
    /// reacted to there. [`Self::radio_probe`] is what stops a three-second
    /// poll re-asking the same question for the length of a record.
    ///
    /// Deliberately outside [`Self::refresh_playback`]: that method is gated by
    /// a backoff about `/me/player`'s rate limit, which has nothing to say
    /// about search, and folding the two together would couple them silently.
    fn resolve_radio_track(&mut self) {
        // Scoped: parking_lot's lock is not reentrant and the arms below take
        // it for writing.
        let announced = self.state.read().radio.as_ref().and_then(|r| r.now_title());

        let Some(raw) = announced else {
            // No station, or a station that has stopped saying anything.
            // Forget the probe, so the same title announced again after a gap
            // is looked up again — and drop the match with it, or the deck
            // would go on naming a record the station is no longer claiming to
            // be playing.
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

        let api = self.api.clone();
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
                // user changed station, or the station moved on while we were
                // asking. A uuid alone would only catch the first.
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

    /// Silence what is playing and start the new sleeve, before asking Spotify
    /// for anything.
    ///
    /// The play below is a round trip out to `api.spotify.com` and back in to
    /// our own device over the dealer connection, and until it lands librespot
    /// happily keeps playing the old track — so a click used to be a second or
    /// two of the previous record with nothing on screen to say otherwise.
    /// Pausing here is what makes it stop on the click instead.
    /// [`crate::audio_sink::SpotSink::stop`] fades over three milliseconds and
    /// clears the queue, so this is silent at once rather than after the
    /// buffered half second.
    ///
    /// The sleeve is started here for the same reason: the event layer has
    /// already put the clicked row's cover URL on the snapshot, and
    /// [`Self::fetch_cover`] serves an already-decoded one out of its cache
    /// immediately. Waiting for the poll to hand us the same URL would cost a
    /// round trip for artwork we were holding all along.
    fn begin_switch(&mut self) {
        self.yield_to_spotify();
        // Activate before pausing: on the first play of a session the device is
        // not ours yet, and there is nothing to pause until it is.
        self.ensure_active();
        if let Err(e) = self.spirc.pause() {
            log::warn!("could not pause before switching tracks: {e}");
        }
        // Scoped: `load_cover` takes the write lock, and parking_lot's is not
        // reentrant — a read still open here would deadlock the client task.
        let url = {
            let st = self.state.read();
            st.playback.as_ref().and_then(|pb| pb.cover_url.clone())
        };
        self.load_cover(url);
    }

    /// Close out a play the API has answered.
    ///
    /// A failed play leaves the deck wearing a track that never started, and
    /// the wait would otherwise hold it there until it times out. Dropping the
    /// wait lets the next poll put back whatever is actually playing, while the
    /// error goes on up to be toasted by [`Self::run`].
    fn settle_switch(&self, result: Result<()>) -> Result<()> {
        if result.is_err() {
            self.state.write().pending_play = None;
        }
        result
    }

    /// Stop the stream if one is playing, before Spotify takes the device.
    ///
    /// Every Spotify play path calls this, which is what keeps "only one engine
    /// at a time" true rather than merely intended.
    ///
    /// The question is put to the radio player, not to `AppState.radio`. That
    /// field is what the deck draws, and the event layer clears it on the click
    /// that starts a track so the station stops being drawn at once — which
    /// leaves it saying "no station" for the turn of the command channel it
    /// takes to get here, while the audio thread is still streaming one.
    /// Reading it here is what let a station and a track play over each other.
    fn yield_to_spotify(&self) {
        if self.radio_player.is_live() {
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
    /// not reach Spirc while it does.
    ///
    /// Either source counts, because the two are true over different windows
    /// and both windows are ones where touching Spirc would start Spotify
    /// under a station:
    ///
    /// * `AppState.radio` is set the moment a station is chosen and stays set
    ///   while it connects — seconds during which the engine is not live yet.
    /// * The engine is live from the hand-off until [`Self::stop_radio`], which
    ///   includes the turn of the command channel after the event layer has
    ///   cleared `AppState.radio` on a click. See [`Self::yield_to_spotify`].
    ///
    /// Deliberately the permissive direction: the cost of a false positive is
    /// a transport key that does nothing for one turn, and the cost of a false
    /// negative is both engines playing at once.
    fn radio_live(&self) -> bool {
        self.radio_player.is_live() || self.state.read().radio.is_some()
    }

    /// Pause Spotify if a station owns the device.
    ///
    /// The backstop under every "pause Spirc first" in this file. Those all
    /// fire *before* the thing that would make sound, and a Connect device
    /// does not take orders in that order: a `load` asked for over the Web API
    /// arrives back over the dealer whenever the backend gets to it, and a
    /// phone can resume our device at any time at all. Both land after the
    /// pause that was meant to prevent them, and until this existed both were
    /// simply audible.
    ///
    /// This runs off `PlayerEvent::Playing` instead — librespot saying it has
    /// started, whoever asked — so the question is settled by what is actually
    /// making sound rather than by what we last asked for.
    fn yield_to_radio(&self) {
        if !self.radio_player.is_live() {
            return;
        }
        log::warn!("Spotify started under a live station; pausing it");
        if let Err(e) = self.spirc.pause() {
            log::warn!("could not pause Spotify under the station: {e}");
        }
        // The Spotify bar must not claim to be playing behind the station —
        // the `Playing` event that got us here has just set it. Only on our own
        // device, matching the filter the event loop applies before it writes:
        // a snapshot describing someone else's speaker is not about the pause
        // we just sent.
        if let Some(pb) = self
            .state
            .write()
            .playback
            .as_mut()
            .filter(|pb| pb.is_local_device)
        {
            pb.is_playing = false;
        }
    }

    fn set_radio_volume(&self, percent: u8) {
        self.radio_player.set_volume(percent);
        if let Some(r) = self.state.write().radio.as_mut() {
            r.volume_percent = percent;
        }
    }

    /// What the soft mixer is actually applying, as a percent.
    ///
    /// This is the truth for our own Connect device, and it is instant: Spirc
    /// writes the mixer synchronously, both for our commands and for a volume
    /// change arriving from another client. `/me/player`'s percent is the same
    /// number a second or more later, having been round-tripped through
    /// Spotify's backend.
    fn local_volume_pct(&self) -> u8 {
        raw_to_pct(self.mixer.volume())
    }

    /// The volume both engines share, as a percent.
    ///
    /// Radio inherits whatever Spotify was at, so starting a station does not
    /// jump the level, and the one slider on the deck keeps meaning one thing.
    fn playback_volume(&self) -> u8 {
        let st = self.state.read();
        st.radio
            .as_ref()
            .map(|r| r.volume_percent)
            .or_else(|| st.playback.as_ref().map(|pb| pb.volume_percent))
            .unwrap_or(50)
    }

    /// `L` / the liked column / the deck's control: save or unsave one track.
    ///
    /// The map is written before the request so the mark flips on the
    /// keypress rather than a round trip later, and put back if the call
    /// fails — an optimistic mark that survives its own error is a mark
    /// that lies about your library. Errors are handled here rather than
    /// returned for that reason.
    async fn set_liked(&self, uri: String, liked: bool) {
        let previous = self.state.write().liked.insert(uri.clone(), liked);
        match self.api.set_track_liked(&uri, liked).await {
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

    /// Fill the player view's queue from the playing context. Ad-hoc
    /// (context-less) playback is covered by the event layer snapshotting
    /// the played list, so only playlist/album contexts are fetched here;
    /// anything else clears the queue.
    fn load_queue(&self) {
        let (ctx, album_name) = {
            let st = self.state.read();
            match &st.playback {
                Some(pb) => (pb.context_uri.clone(), pb.album.clone()),
                None => (None, String::new()),
            }
        };
        let Some(ctx) = ctx else { return };
        if let Some(id) = ctx.strip_prefix("spotify:playlist:") {
            let info = self
                .state
                .read()
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| {
                    (
                        p.name.clone(),
                        p.owner.clone(),
                        p.track_count,
                        p.snapshot_id.clone(),
                    )
                });
            // Playlists outside the library (e.g. played via search) still
            // load; they just get a generic header and no snapshot check.
            let (name, owner, total, snapshot) =
                info.unwrap_or_else(|| ("Playlist".to_string(), String::new(), 0, String::new()));
            let subtitle = if owner.is_empty() {
                String::new()
            } else {
                format!("by {owner}")
            };
            let mut list = TrackList::new(
                name,
                subtitle,
                Some(ctx.clone()),
                (total > 0).then_some(total),
            );
            list.kind = TrackListKind::Playlist;
            self.start_queue_fetch(
                list,
                TrackSource::Playlist(id.to_string()),
                (!snapshot.is_empty()).then_some(snapshot),
            );
        } else if let Some(id) = ctx.strip_prefix("spotify:album:") {
            let mut list = TrackList::new(album_name.clone(), "", Some(ctx.clone()), None);
            list.kind = TrackListKind::Album;
            self.start_queue_fetch(
                list,
                TrackSource::Album {
                    id: id.to_string(),
                    name: album_name,
                    year: String::new(),
                },
                None,
            );
        } else {
            // Artist radio, collections, …: nothing browsable to show.
            self.state.write().set_queue(None);
        }
    }

    /// Queue-pane sibling of `start_track_fetch`: same cache + page loop,
    /// but writes `AppState.queue` under `queue_generation` so it never
    /// interferes with main-view loads. Skips the liked-check side fetch
    /// (the queue pane shows no liked marks).
    fn start_queue_fetch(
        &self,
        mut list: TrackList,
        source: TrackSource,
        expected_snapshot: Option<String>,
    ) {
        let key = source.cache_key();
        list.cache_key = Some(key.clone());

        let cached: Option<Vec<crate::app::state::Track>> = self
            .cache
            .lock()
            .get(&key)
            .filter(|e| expected_snapshot.is_none() || e.snapshot_id == expected_snapshot)
            .map(|e| e.tracks.clone());
        if let Some(tracks) = cached {
            list.append(tracks);
            self.state.write().set_queue(Some(list));
            return;
        }

        let generation = {
            let mut st = self.state.write();
            st.queue_generation += 1;
            list.loading = true;
            list.generation = st.queue_generation;
            st.set_queue(Some(list));
            st.queue_generation
        };
        let api = self.api.clone();
        let state = self.state.clone();
        let cache = self.cache.clone();
        tokio::spawn(async move {
            let mut offset = 0u32;
            loop {
                let result = match &source {
                    TrackSource::Liked => api.liked_songs_page(offset).await,
                    TrackSource::Playlist(id) => api.playlist_tracks_page(id, offset).await,
                    TrackSource::Album { id, name, year } => {
                        api.album_tracks_page(id, name, year, offset).await
                    }
                };
                let mut finished: Option<Vec<crate::app::state::Track>> = None;
                {
                    let mut st = state.write();
                    if st.queue_generation != generation {
                        return;
                    }
                    let Some(list) = st.queue.as_mut() else {
                        return;
                    };
                    if list.generation != generation {
                        return;
                    }
                    match result {
                        Ok((tracks, has_more, total)) => {
                            list.append(tracks);
                            list.total = Some(total);
                            if !has_more {
                                list.loading = false;
                                finished = Some(list.tracks.clone());
                            }
                        }
                        Err(e) => {
                            list.loading = false;
                            st.toast(format!("queue load failed: {e}"));
                            return;
                        }
                    }
                }
                if let Some(tracks) = finished {
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

    async fn load_playlists(&self) {
        // Who you are, the first time only: it cannot change within a session,
        // and `R` re-runs this whole function.
        if self.state.read().me_id.is_none()
            && let Ok(id) = self.api.me_id().await
        {
            self.state.write().me_id = Some(id);
        }
        let result = self.api.playlists().await;
        let mut st = self.state.write();
        match result {
            Ok(playlists) => st.playlists = playlists,
            Err(e) => st.toast(format!("failed to load playlists: {e}")),
        }
    }

    fn load_liked_view(&self, preserve_view: bool) {
        let mut list = TrackList::new("Liked Songs", "your saved tracks", None, None);
        list.kind = TrackListKind::LikedSongs;
        self.start_track_fetch(list, TrackSource::Liked, None, preserve_view);
    }

    fn load_playlist_view(&self, playlist_id: String, preserve_view: bool) {
        let info = self
            .state
            .read()
            .playlists
            .iter()
            .find(|p| p.id == playlist_id)
            .map(|p| {
                (
                    p.name.clone(),
                    p.uri.clone(),
                    p.owner.clone(),
                    p.track_count,
                    p.snapshot_id.clone(),
                )
            });
        let (title, uri, owner, total, snapshot) = info.unwrap_or_else(|| {
            (
                "Playlist".to_string(),
                String::new(),
                String::new(),
                0,
                String::new(),
            )
        });
        let subtitle = if owner.is_empty() {
            String::new()
        } else {
            format!("by {owner}")
        };
        let mut list = TrackList::new(
            title,
            subtitle,
            (!uri.is_empty()).then_some(uri),
            (total > 0).then_some(total),
        );
        list.kind = TrackListKind::Playlist;
        self.start_track_fetch(
            list,
            TrackSource::Playlist(playlist_id),
            (!snapshot.is_empty()).then_some(snapshot),
            preserve_view,
        );
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
        let mut list = TrackList::new(
            name.clone(),
            subtitle,
            Some(format!("spotify:album:{id}")),
            None,
        );
        list.kind = TrackListKind::Album;
        list.header.cover_url = cover_url.clone();
        self.start_track_fetch(
            list,
            TrackSource::Album { id, name, year },
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
        let view = ArtistView {
            id: id.clone(),
            uri,
            name: name.clone(),
            image_url: None,
            genres: Vec::new(),
            top: TrackList::new(name, "top tracks", None, None),
            albums: Vec::new(),
            loading: true,
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
        let api = self.api.clone();
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
                match result {
                    Ok(overview) => {
                        let uris: Vec<String> =
                            overview.top.iter().map(|t| t.uri.clone()).collect();
                        // The photo first: it is the one image already on
                        // screen when the page lands, and the fetches run in
                        // order.
                        let art: Vec<String> = overview
                            .image_url
                            .iter()
                            .chain(overview.albums.iter().filter_map(|a| a.cover_url.as_ref()))
                            .cloned()
                            .collect();
                        v.image_url = overview.image_url;
                        v.genres = overview.genres;
                        v.top.append(overview.top);
                        v.albums = overview.albums;
                        (uris, art)
                    }
                    Err(e) => {
                        st.toast(format!("failed to load artist: {e}"));
                        return;
                    }
                }
            };
            spawn_page_art(http, state.clone(), art, generation);
            spawn_liked_check(api, state, uris);
        });
    }

    /// Install `list` as the main view, serving it from the cache when the
    /// snapshot still matches, otherwise streaming pages in from a spawned
    /// task. A newer load bumps `load_generation`, and the stale task exits
    /// at its next page boundary.
    fn start_track_fetch(
        &self,
        mut list: TrackList,
        source: TrackSource,
        expected_snapshot: Option<String>,
        preserve_view: bool,
    ) {
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
        let api = self.api.clone();
        let state = self.state.clone();
        let cache = self.cache.clone();
        tokio::spawn(async move {
            let mut offset = 0u32;
            loop {
                let result = match &source {
                    TrackSource::Liked => api.liked_songs_page(offset).await,
                    TrackSource::Playlist(id) => api.playlist_tracks_page(id, offset).await,
                    TrackSource::Album { id, name, year } => {
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
                    let MainView::Tracks(list) = &mut st.main else {
                        return;
                    };
                    if list.generation != generation {
                        return;
                    }
                    match result {
                        Ok((tracks, has_more, total)) => {
                            page_uris = tracks.iter().map(|t| t.uri.clone()).collect();
                            list.append(tracks);
                            list.total = Some(total);
                            if !has_more {
                                list.loading = false;
                                finished = Some(list.tracks.clone());
                            }
                            // Keep an active sort (and the anchored
                            // selection) correct as pages arrive.
                            st.resort_main();
                        }
                        Err(e) => {
                            list.loading = false;
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

    async fn refresh_playback(&mut self) {
        if let Some(until) = self.backoff_until
            && Instant::now() < until
        {
            return;
        }
        match self.api.playback().await {
            Ok(mut snapshot) => {
                self.backoff_until = None;
                self.backoff = BACKOFF_INITIAL;
                prefer_local_truth(
                    snapshot.as_mut(),
                    self.local.playing(),
                    self.local_volume_pct(),
                );
                {
                    let mut st = self.state.write();
                    // A snapshot arriving mid-switch is usually Spotify's
                    // backend still describing the track we just left; taking
                    // it would put that record back on the deck for a poll or
                    // two. Both branches below sit inside the check — while a
                    // play is outstanding, the snapshot on screen is one we
                    // wrote ourselves, and clearing its `is_playing` would be
                    // flipping our own answer.
                    if st.resolve_pending(snapshot.as_ref()) {
                        // Keep the last snapshot if the API briefly reports
                        // nothing.
                        if snapshot.is_some() {
                            st.playback = snapshot;
                        } else if let Some(pb) = st.playback.as_mut() {
                            pb.is_playing = false;
                        }
                    }
                }
                // The deck draws a liked mark for the playing track, which is not
                // necessarily in any loaded list, so its saved state has to be
                // asked for on its own. Once per track — see `liked_probe`;
                // after that the map owns the answer and `set_liked` keeps it.
                let unchecked = {
                    let st = self.state.read();
                    st.playback
                        .as_ref()
                        .and_then(|pb| pb.track_uri.clone())
                        .filter(|uri| {
                            !st.liked.contains_key(uri) && self.liked_probe.as_ref() != Some(uri)
                        })
                };
                if let Some(uri) = unchecked {
                    self.liked_probe = Some(uri.clone());
                    spawn_liked_check(self.api.clone(), self.state.clone(), vec![uri]);
                }

                // Art rides along with the poll, so a change of art is just a
                // change of URL. Compare against what is installed rather
                // than against the album id: the CDN URL is what we would
                // fetch anyway, and it is content-addressed.
                let want = {
                    let st = self.state.read();
                    // A play in flight had its sleeve started by `begin_switch`
                    // from the row that was clicked, and the slot is empty
                    // while that fetch runs. Asking again from here would read
                    // the empty slot as "not fetched yet", bump the generation
                    // and strand the request already on its way.
                    if st.pending_play.is_some() {
                        None
                    } else {
                        let url = st.playback.as_ref().and_then(|p| p.cover_url.clone());
                        match (&url, st.cover.as_ref()) {
                            (Some(u), Some(c)) if c.url == *u => None,
                            (None, None) => None,
                            _ => Some(url),
                        }
                    }
                };
                if let Some(url) = want {
                    self.load_cover(url);
                }

                // Follow context switches, in either view: the bottom bar's
                // context row names the playing queue too, so it is no longer
                // only the player that has something to say about it.
                // Deliberately asymmetric: a `None` context (ad-hoc URI-list
                // playback) must not clobber the snapshot queue the event
                // layer installed for it.
                let reload = {
                    let st = self.state.read();
                    match st.playback.as_ref().and_then(|p| p.context_uri.as_deref()) {
                        Some(ctx) => st
                            .queue
                            .as_ref()
                            .is_none_or(|q| q.context_uri.as_deref() != Some(ctx)),
                        None => false,
                    }
                };
                if reload {
                    self.load_queue();
                }
            }
            Err(e) => {
                log::warn!("playback refresh failed: {e:#}");
                // The Web API quota is shared with other clients on the same
                // client ID; on 429 stop hammering it for a while.
                if format!("{e:#}").contains("429") {
                    self.backoff_until = Some(Instant::now() + self.backoff);
                    self.state.write().toast(format!(
                        "rate limited; pausing polling {}s",
                        self.backoff.as_secs()
                    ));
                    self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
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
    /// These GETs hit the image CDN, not `api.spotify.com`, so they neither
    /// consume the shared Web-API quota nor belong behind `backoff_until`.
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
/// A free function so the rule can be tested: `Client` needs a `Spirc` to
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

/// Overwrite a polled snapshot's play/pause and volume with what is known
/// locally, when the snapshot describes our own device.
///
/// `/me/player` is the authority on what is playing and a poor one on whether
/// it is playing: Spirc debounces its notify to the backend, the backend takes
/// its own time, and the poll that follows a keypress usually arrives carrying
/// the state from before it. Taking that answer is what made the pill flip
/// back a moment after being pressed and the slider snap to a stale value.
///
/// `local_playing` is `None` until librespot's player has said anything at
/// all; there is no local truth to prefer before that.
fn prefer_local_truth(
    snapshot: Option<&mut state::PlaybackSnapshot>,
    local_playing: Option<bool>,
    local_volume_pct: u8,
) {
    let Some(pb) = snapshot.filter(|pb| pb.is_local_device) else {
        return;
    };
    if let Some(playing) = local_playing {
        pb.is_playing = playing;
    }
    pb.volume_percent = local_volume_pct;
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

fn command_touches_playback(cmd: &AppCommand) -> bool {
    matches!(
        cmd,
        AppCommand::PlayPause
            | AppCommand::Next
            | AppCommand::Prev
            | AppCommand::SeekRel(_)
            | AppCommand::SeekTo(_)
            | AppCommand::ToggleShuffle
            | AppCommand::PlayContext { .. }
            | AppCommand::PlayTracks { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{PlaybackSnapshot, RepeatMode};
    use librespot_playback::mixer::{self, MixerConfig};

    fn snapshot(is_local_device: bool) -> PlaybackSnapshot {
        PlaybackSnapshot {
            is_playing: false,
            progress_ms: 0,
            duration_ms: 200_000,
            track_uri: Some("spotify:track:x".into()),
            context_uri: None,
            artist_id: None,
            album_id: None,
            track_name: "Song".into(),
            artists: "Artist".into(),
            album: "Album".into(),
            release_year: "2020".into(),
            cover_url: None,
            shuffle: false,
            repeat: RepeatMode::Context,
            volume_percent: 20,
            device_name: "somewhere".into(),
            is_local_device,
            fetched_at: Instant::now(),
        }
    }

    /// `YieldToRadio` rides on every `Playing` event, which means every track
    /// change. Arming the 400 ms re-poll on it would spend a `/me/player` call
    /// per track to learn what the event that sent it already said.
    #[test]
    fn yielding_to_radio_does_not_arm_a_repoll() {
        assert!(!command_touches_playback(&AppCommand::YieldToRadio));
        // The commands that do still do — the guard above is about this one
        // command, not a loosening of the rule.
        assert!(command_touches_playback(&AppCommand::Next));
    }

    /// The poll that lands ~400 ms after a pause still says "playing"; taking
    /// it would flip the pill back under the user's finger.
    #[test]
    fn a_stale_poll_does_not_undo_a_local_pause() {
        let mut pb = snapshot(true);
        pb.is_playing = true;
        pb.volume_percent = 20;
        prefer_local_truth(Some(&mut pb), Some(false), 75);
        assert!(!pb.is_playing);
        assert_eq!(pb.volume_percent, 75);
    }

    /// Another device's playback has no local truth to prefer — librespot is
    /// idle and its mixer describes an output nobody is listening to.
    #[test]
    fn a_remote_device_keeps_what_the_api_reported() {
        let mut pb = snapshot(false);
        pb.is_playing = true;
        prefer_local_truth(Some(&mut pb), Some(false), 75);
        assert!(pb.is_playing);
        assert_eq!(pb.volume_percent, 20);
    }

    /// Before librespot's player has said anything there is nothing to prefer,
    /// so the API's answer has to stand — otherwise the first poll of a
    /// session would report paused whatever was really happening.
    #[test]
    fn the_api_stands_until_the_player_speaks() {
        let mut pb = snapshot(true);
        pb.is_playing = true;
        prefer_local_truth(Some(&mut pb), None, 75);
        assert!(pb.is_playing);
        // Volume is not conditional: the mixer is always the truth for our
        // own device, including a change made from another Connect client.
        assert_eq!(pb.volume_percent, 75);
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

    /// The guard that keeps a three-second poll from re-asking Spotify the
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
}
