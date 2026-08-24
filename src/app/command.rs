use crate::app::state::{RadioScope, Station, Track};
use crate::client::Spotify;

/// A playable source whose rows are not in hand yet — a playlist played from
/// its row, an album played from its card. The client fetches the pages and
/// fills the queue as they land; see `Client::play_fetched`.
#[derive(Debug, Clone)]
pub enum FetchSource {
    Playlist {
        id: String,
    },
    /// Name and year ride along because the album-tracks endpoint does not
    /// return them per track.
    Album {
        id: String,
        year: String,
    },
}

/// Commands sent from the UI/event layer to the client task.
#[derive(Debug, Clone)]
pub enum AppCommand {
    /// A sign-in finished: take what it got. Sent by the frame loop, which
    /// owns the sign-in because the browser flow prints to the console.
    SpotifyConnected(Spotify),
    PlayPause,
    Next,
    Prev,
    /// Seek relative to current position, in milliseconds (may be negative).
    SeekRel(i64),
    /// Seek to an absolute position in milliseconds (progress-bar click).
    SeekTo(u64),
    /// Volume delta in percent (may be negative).
    VolumeRel(i8),
    /// Set absolute volume in percent (volume-slider click).
    SetVolume(u8),
    ToggleShuffle,

    /// Install a new queue and start playing `tracks[start]`. The tracks are
    /// the caller's display order — what you see is the play order.
    ///
    /// `key` is the source's track-cache key when the list is a re-fetchable
    /// context in its natural order; with `loading` true, pages still landing
    /// in the view extend the queue as they arrive.
    Play {
        tracks: Vec<Track>,
        start: usize,
        name: String,
        key: Option<String>,
        loading: bool,
        /// Start the queue shuffled and turn the shuffle mode on; false
        /// inherits whatever mode is in force.
        shuffle: bool,
    },
    /// Play a source whose rows are not loaded yet, from the top: fetch its
    /// pages and grow the queue as they land.
    PlayFetched {
        source: FetchSource,
        name: String,
        /// Start the queue shuffled and turn the shuffle mode on; false
        /// inherits whatever mode is in force.
        shuffle: bool,
    },
    /// Enter on a queue row: play that row of the queue as it stands.
    JumpTo(usize),
    /// `a`: put a track directly after the playing one.
    QueueInsertNext(Track),
    /// librespot reached the end of the playing track: advance and load.
    TrackEnded,
    /// librespot wants the next track fetched ahead of the gap.
    PreloadNext,

    /// `L`, the liked column, and the deck's control: save or unsave one track.
    SetLiked {
        uri: String,
        liked: bool,
    },
    /// A pick in the player's add-to-playlist box: put one track on a
    /// playlist, or take it off.
    ///
    /// `seq` is the opening of the box this answers, so a result that arrives
    /// after it was closed and opened again cannot act on the new one.
    SetOnPlaylist {
        playlist_id: String,
        uri: String,
        on: bool,
        seq: u64,
    },
    /// Read what these playlists hold, for the box's marks. Carries no track:
    /// the whole playlist is cached, so one walk answers every record.
    ///
    /// The client drops the ids it already holds or is already walking, so the
    /// caller may send its whole list without counting the cost.
    CachePlaylistTracks {
        playlist_ids: Vec<String>,
    },
    Search(String),
    LoadPlaylists,
    LoadLikedSongs,
    LoadPlaylistTracks {
        playlist_id: String,
    },
    /// Open a browsable album view. Metadata rides along because the
    /// album-tracks endpoint doesn't return it.
    OpenAlbum {
        id: String,
        name: String,
        artists: String,
        year: String,
        /// The sleeve, when the row that opened this already had it. `None`
        /// from a track row, which knows the album's id but not its art — the
        /// header band then draws its placeholder.
        cover_url: Option<String>,
    },
    /// Install the sleeve for the album page currently on screen, or clear it.
    ///
    /// Sent when a view is *restored* rather than opened: `pop_view` brings a
    /// header back without a fetch, so the decoded cover has to be re-pointed
    /// at it. See `AppState::view_cover_url`.
    LoadViewCover {
        cover_url: Option<String>,
    },
    /// Install the sleeve for the track that has just started playing.
    ///
    /// Sent from the librespot event loop on `TrackChanged`, which carries
    /// the artwork with the metadata — it is what fills the sleeve for rows
    /// that arrived without a cover URL of their own (an album's track list
    /// does not repeat the album object per row).
    LoadPlayingCover {
        cover_url: Option<String>,
    },
    /// Open a browsable artist view (top tracks + albums).
    OpenArtist {
        id: String,
        uri: String,
        name: String,
    },
    /// Fetch the sleeves of the artist page's open album group.
    ///
    /// Sent when the group is switched. Opening the page fetches the art of
    /// every group except **Appears On**, which is the one group that is not
    /// the artist's own work, is often the longest, and is usually never
    /// looked at — so its sleeves are asked for only once you open it.
    ///
    /// Carries nothing: the client reads the open group off the view it is
    /// about to draw, so a page swapped out from under the command cannot be
    /// fetched for.
    LoadArtistArt,

    /// `R`: evict the current view from the track cache, re-fetch it, and
    /// reload your playlists.
    Refresh,

    /// Open a page of the radio directory. The scope says which one, and is
    /// also the page's identity on the back stack — see
    /// `crate::app::state::radio_key`.
    LoadRadio {
        scope: RadioScope,
    },
    /// Start a station. Stops Spotify first: the two engines share one output
    /// device and only one of them may own it.
    ///
    /// `attempt` counts the stations a seek has walked to reach this one, and
    /// is 0 for a station chosen directly. A station you picked is the one you
    /// meant and a failure is the end of it; a station a seek landed on is one
    /// of a run, and a failure is a reason to keep walking.
    PlayStation {
        station: Box<Station>,
        attempt: u8,
    },
    /// Stop the stream and release the device.
    StopRadio,
    /// A station that would not play, or one that stopped sending.
    ///
    /// Reported rather than acted on: the tune-in runs on a task of its own,
    /// off the command loop, and whether a dead station is the end of the road
    /// or one step of a seek is the loop's question. `tune_seq` says which
    /// tune-in this is about — a failure can arrive after the deck has moved
    /// on, and a retry puts the same station's uuid back on the deck.
    RadioFailed {
        station: Box<Station>,
        reason: String,
        tune_seq: u64,
    },
    /// `L` on a station row: keep it, or stop keeping it.
    ToggleSavedStation(Box<Station>),

    /// librespot has started making sound. Pause it again if a station owns
    /// the device.
    ///
    /// Sent from the player event loop on every `Playing`, because that loop
    /// hears librespot start for *any* reason — including a load that landed
    /// after a station took the device. The client answers it, because only
    /// the client knows whether the radio engine is streaming.
    YieldToRadio,

    /// Ask GitHub whether a newer spot has been released. Sent once at
    /// startup; the answer, if there is one, becomes the Home row.
    CheckForUpdate,
    /// Enter on that row: download the release and write it over this
    /// executable.
    InstallUpdate,

    /// Quit: silence both engines and end the client loop.
    ///
    /// The only command whose completion the sender waits for — see
    /// `Client::new`'s `shutdown_ack`. Everything else about quitting is
    /// cosmetic, but audio that outlives the UI is not, and only the client
    /// holds the handles that can stop it.
    Shutdown,
}
