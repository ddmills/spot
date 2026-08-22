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
    /// Open a browsable artist view (top tracks + albums).
    OpenArtist {
        id: String,
        uri: String,
        name: String,
    },
    /// `R`: evict the current view from the track cache, re-fetch it, and
    /// reload your playlists.
    Refresh,
}
