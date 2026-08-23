use crate::app::state::{RadioScope, Station};

/// Commands sent from the UI/event layer to the client task.
#[derive(Debug, Clone)]
pub enum AppCommand {
    PlayPause,
    Next,
    Prev,
    /// Seek relative to current position, in milliseconds (may be negative).
    SeekRel(i64),
    /// Seek to an absolute position in milliseconds (progress-bar click).
    SeekTo(u64),
    /// Volume delta in Web-API percent (may be negative).
    VolumeRel(i8),
    /// Set absolute volume in Web-API percent (volume-slider click).
    SetVolume(u8),
    ToggleShuffle,

    /// Start playback of a context (playlist/album/artist), optionally at a
    /// specific track offset within it.
    PlayContext {
        context_uri: String,
        offset_uri: Option<String>,
    },
    /// Start playback of a flat list of track URIs, starting at `offset`.
    PlayTracks {
        uris: Vec<String>,
        offset: usize,
    },
    AddToQueue(String),
    /// `L`, the liked column, and the deck's control: save or unsave one track.
    SetLiked {
        uri: String,
        liked: bool,
    },
    Search(String),
    LoadPlaylists,
    LoadLikedSongs,
    LoadPlaylistTracks {
        playlist_id: String,
    },
    RefreshPlayback,
    /// (Re)load the player view's queue from the playing context.
    LoadQueue,
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
    /// The playing slot's twin of [`Self::LoadViewCover`]. Sent from the
    /// librespot event loop, which learns of a track change the moment it
    /// happens and carries the artwork with it — where the `/me/player` poll
    /// that would otherwise drive this is up to three seconds behind, and does
    /// not run at all between two tracks of one album.
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
    PlayStation(Box<Station>),
    /// Stop the stream and release the device.
    StopRadio,
    /// `L` on a station row: keep it, or stop keeping it.
    ToggleSavedStation(Box<Station>),

    /// librespot has started making sound. Pause it again if a station owns
    /// the device.
    ///
    /// Sent from the player event loop on every `Playing`, because that loop
    /// hears librespot start *whoever* asked it to — our own play, a `load`
    /// arriving late over the dealer, or a phone resuming our Connect device —
    /// and it is the only signal that covers all three. The client answers it,
    /// because only the client knows whether the radio engine is streaming.
    ///
    /// A no-op in the ordinary case, which is why it is not listed in
    /// `command_touches_playback`: nothing here changes what is playing unless
    /// two things already are.
    YieldToRadio,

    /// Quit: silence both engines and end the client loop.
    ///
    /// The only command whose completion the sender waits for — see
    /// `Client::new`'s `shutdown_ack`. Everything else about quitting is
    /// cosmetic, but audio that outlives the UI is not, and only the client
    /// holds the handles that can stop it.
    Shutdown,
}
