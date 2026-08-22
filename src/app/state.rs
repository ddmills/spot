use std::sync::Arc;
use std::time::Instant;

use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;

use crate::audio_tap::AudioTap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatMode {
    Off,
    Context,
    Track,
}

/// Snapshot of playback state from the Web API, plus the moment it was
/// fetched so the UI can interpolate progress between polls.
#[derive(Debug, Clone)]
pub struct PlaybackSnapshot {
    pub is_playing: bool,
    pub progress_ms: u64,
    pub duration_ms: u64,
    /// URI of the playing item; `None` for podcast episodes.
    pub track_uri: Option<String>,
    /// URI of the playing context (playlist/album/artist), when any.
    pub context_uri: Option<String>,
    /// First credited artist's id, for click-through to the artist page.
    pub artist_id: Option<String>,
    /// Album id, for click-through to the album page.
    pub album_id: Option<String>,
    pub track_name: String,
    pub artists: String,
    pub album: String,
    /// Four-digit release year, or empty when the API did not report one.
    pub release_year: String,
    /// CDN URL of the item's cover art, ~300px. It comes back with every
    /// playback poll, so art costs no extra API call.
    pub cover_url: Option<String>,
    pub shuffle: bool,
    /// Reported by the API but not surfaced: the client pins playback to
    /// repeat-all on activation, so there is no repeat control to drive.
    /// Kept because it is what the device actually reports.
    #[allow(dead_code)]
    pub repeat: RepeatMode,
    pub volume_percent: u8,
    /// Reported by the API but no longer surfaced: the bottom bar used to
    /// print it as `▣ spot` in its corner, which said the same thing on every
    /// frame of every session. Kept because it is what the device reports and
    /// a device picker would want it.
    #[allow(dead_code)]
    pub device_name: String,
    pub fetched_at: Instant,
}

impl PlaybackSnapshot {
    /// Progress advanced locally while playing, clamped to track length.
    pub fn interpolated_progress_ms(&self) -> u64 {
        if !self.is_playing {
            return self.progress_ms;
        }
        let elapsed = self.fetched_at.elapsed().as_millis() as u64;
        (self.progress_ms + elapsed).min(self.duration_ms)
    }
}

#[derive(Debug, Clone)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    /// Four-digit year, or empty when Spotify has no release date.
    pub release_year: String,
    pub duration_ms: u64,
    /// Position within its album disc; 0 when unknown.
    pub track_number: u32,
    pub album_id: Option<String>,
    /// The first credited artist's id.
    pub artist_id: Option<String>,
    /// CDN URL of the album's sleeve, when the row's source reported one, so
    /// opening the album from this row shows its artwork straight away rather
    /// than degrading to the text-only header band.
    ///
    /// `None` for rows read off an album's own track list: that endpoint does
    /// not repeat the album object per track. Those rows never need it — the
    /// album they name is the page they are already on.
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub track_count: u32,
    pub owner: String,
    /// Spotify id of the owner, for telling your own playlists apart from the
    /// ones you follow. Compared against [`AppState::me_id`] rather than
    /// `owner`, which is a display name and need not be unique.
    pub owner_id: String,
    /// Spotify's content-version hash; changes whenever the playlist does.
    pub snapshot_id: String,
}

#[derive(Debug, Clone)]
pub struct AlbumItem {
    pub id: String,
    pub name: String,
    pub artists: String,
    /// Four-digit year, or empty when Spotify has no release date.
    pub release_year: String,
    /// "album", "single", or "compilation"; may be empty.
    pub album_type: String,
    /// Tracks on the record, or 0 when the source did not report one.
    pub track_count: u32,
    /// CDN URL of the sleeve, so opening the album can show it without a
    /// second round trip. `None` when Spotify gave the album no images.
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ArtistItem {
    pub id: String,
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<Track>,
    pub albums: Vec<AlbumItem>,
    pub artists: Vec<ArtistItem>,
    pub playlists: Vec<Playlist>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTab {
    Tracks,
    Albums,
    Artists,
    Playlists,
}

impl SearchTab {
    pub const ALL: [SearchTab; 4] = [
        SearchTab::Tracks,
        SearchTab::Albums,
        SearchTab::Artists,
        SearchTab::Playlists,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SearchTab::Tracks => "Tracks",
            SearchTab::Albums => "Albums",
            SearchTab::Artists => "Artists",
            SearchTab::Playlists => "Playlists",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Context/fetch order (the only order playback follows).
    Position,
    Title,
    Artist,
    Album,
    Duration,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Position => "position",
            SortKey::Title => "title",
            SortKey::Artist => "artist",
            SortKey::Album => "album",
            SortKey::Duration => "duration",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackSort {
    pub key: SortKey,
    pub ascending: bool,
}

impl Default for TrackSort {
    fn default() -> Self {
        Self {
            key: SortKey::Position,
            ascending: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ViewHeader {
    pub name: String,
    /// e.g. "by owner" for playlists, "Artist · 2011" for albums.
    pub subtitle: String,
    /// CDN URL of the sleeve, for the header band to draw.
    ///
    /// Only albums set it. A playlist's mosaic is not a record cover, and the
    /// band reads better without a placeholder swatch standing where artwork
    /// would be.
    pub cover_url: Option<String>,
}

/// What kind of context a `TrackList` shows; drives the pane's type label
/// and album-only presentation (track-number column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackListKind {
    Playlist,
    Album,
    LikedSongs,
}

/// What one row of the Home view points at.
///
/// Home is the app's landing view and the bottom of the back stack: the two
/// records you reach for most, then everything else behind one door. Radio and
/// whatever comes after it append to [`HomeItem::ALL`], which — with
/// [`AppState::home_items`] — is the only place that has to learn about them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeItem {
    LikedSongs,
    DiscoverWeekly,
    Playlists,
    Radio,
}

impl HomeItem {
    /// Every destination, in the order Home lists them. The two named records
    /// lead, because they are the ones you open by name; Playlists is the
    /// catch-all under them, and Radio is last because it is the one row that
    /// leaves Spotify behind.
    pub const ALL: [HomeItem; 4] = [
        HomeItem::LikedSongs,
        HomeItem::DiscoverWeekly,
        HomeItem::Playlists,
        HomeItem::Radio,
    ];

    pub fn title(self) -> &'static str {
        match self {
            HomeItem::LikedSongs => "Liked Songs",
            HomeItem::DiscoverWeekly => "Discover Weekly",
            HomeItem::Playlists => "Playlists",
            HomeItem::Radio => "Radio",
        }
    }

    /// The dim line under the name, saying what the destination holds.
    pub fn blurb(self) -> &'static str {
        match self {
            HomeItem::LikedSongs => "everything you have saved",
            HomeItem::DiscoverWeekly => "thirty new tracks every Monday",
            HomeItem::Playlists => "saved and followed",
            HomeItem::Radio => "live stations from around the world",
        }
    }
}

/// A radio station, as the directory describes it and the deck draws it.
///
/// Flat, owned and `serde`-able because the favourites file is a list of these:
/// the directory has no accounts, so a station you keep is a station spot
/// stores. See `crate::radio::api` for the wire shape this is converted from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Station {
    pub uuid: String,
    pub name: String,
    /// The stream itself, already unwrapped from any `.pls`/`.m3u` by the
    /// directory.
    pub url: String,
    #[serde(default)]
    pub homepage: String,
    /// Comma-joined, the way the directory reports it.
    #[serde(default)]
    pub tags: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub countrycode: String,
    #[serde(default)]
    pub language: String,
    /// "MP3", "AAC+", or "UNKNOWN" — which is normal for an HLS entry.
    #[serde(default)]
    pub codec: String,
    /// Kilobits per second; 0 when the directory does not know.
    #[serde(default)]
    pub bitrate: u32,
    #[serde(default)]
    pub votes: u32,
    /// HLS stations cannot be played yet — see `crate::radio::player`. They are
    /// still listed, because hiding them would silently drop the BBC and most
    /// other national broadcasters.
    #[serde(default)]
    pub hls: bool,
}

impl Station {
    /// The right-hand column: what it sounds like, technically.
    pub fn quality(&self) -> String {
        if self.hls {
            return "HLS".to_string();
        }
        match (self.bitrate, self.codec.as_str()) {
            (0, "") | (0, "UNKNOWN") => String::new(),
            (0, codec) => codec.to_string(),
            (rate, "") | (rate, "UNKNOWN") => format!("{rate}k"),
            (rate, codec) => format!("{codec} {rate}k"),
        }
    }
}

/// What a radio page is listing.
///
/// One enum rather than one view type per page: every scope resolves to the
/// same table of rows, and the fetch that fills it is the only thing that
/// differs. It doubles as the page's identity on the back stack — see
/// [`radio_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioScope {
    /// The directory's own chart, and where the Radio row lands.
    Popular,
    /// The country list, as facets to drill into.
    Countries,
    /// The tag list, likewise.
    Genres,
    /// Stations in one country, by ISO 3166-1 alpha-2 code.
    Country(String),
    /// Stations carrying one tag.
    Genre(String),
    /// The stations you kept. Read from disk, not the network.
    Favorites,
    Search(String),
}

impl RadioScope {
    /// What the page calls itself in the trail and its heading.
    pub fn title(&self) -> String {
        match self {
            RadioScope::Popular => "radio".to_string(),
            RadioScope::Countries => "countries".to_string(),
            RadioScope::Genres => "genres".to_string(),
            RadioScope::Country(code) => code.to_uppercase(),
            RadioScope::Genre(tag) => tag.clone(),
            RadioScope::Favorites => "saved stations".to_string(),
            RadioScope::Search(q) => format!("“{q}”"),
        }
    }

    /// The tab this scope belongs under, so drilling into a country still
    /// leaves Countries lit.
    pub fn tab(&self) -> RadioTab {
        match self {
            RadioScope::Popular | RadioScope::Search(_) => RadioTab::Popular,
            RadioScope::Countries | RadioScope::Country(_) => RadioTab::Countries,
            RadioScope::Genres | RadioScope::Genre(_) => RadioTab::Genres,
            RadioScope::Favorites => RadioTab::Favorites,
        }
    }
}

/// The four ways into the directory, drawn as a tab strip above the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioTab {
    Popular,
    Countries,
    Genres,
    Favorites,
}

impl RadioTab {
    pub const ALL: [RadioTab; 4] = [
        RadioTab::Popular,
        RadioTab::Countries,
        RadioTab::Genres,
        RadioTab::Favorites,
    ];

    pub fn title(self) -> &'static str {
        match self {
            RadioTab::Popular => "Popular",
            RadioTab::Countries => "Countries",
            RadioTab::Genres => "Genres",
            RadioTab::Favorites => "Saved",
        }
    }

    /// The scope the tab opens when it is clicked.
    pub fn scope(self) -> RadioScope {
        match self {
            RadioTab::Popular => RadioScope::Popular,
            RadioTab::Countries => RadioScope::Countries,
            RadioTab::Genres => RadioScope::Genres,
            RadioTab::Favorites => RadioScope::Favorites,
        }
    }
}

/// One row of a radio page: a station to play, or a facet to drill into.
#[derive(Debug, Clone)]
pub enum RadioRow {
    /// A country or genre, with how many stations it holds. `key` is what the
    /// query needs (a country code, a tag); `label` is what the row says.
    Facet {
        key: String,
        label: String,
        count: u32,
    },
    Station(Station),
}

/// A browsable page of the radio directory.
#[derive(Debug, Clone)]
pub struct RadioView {
    pub scope: RadioScope,
    pub rows: Vec<RadioRow>,
    pub loading: bool,
    /// Matches `AppState.load_generation` while a fetch owns this view, on the
    /// same reasoning as [`TrackList::generation`].
    pub generation: u64,
}

impl RadioView {
    pub fn new(scope: RadioScope, generation: u64) -> Self {
        Self {
            scope,
            rows: Vec::new(),
            loading: true,
            generation,
        }
    }
}

/// What is playing, when what is playing is a radio station.
///
/// Deliberately not a [`PlaybackSnapshot`]: a broadcast has no duration to
/// scrub, no album to open and nothing to like, and filling those fields with
/// zeroes would have the deck draw controls that lead nowhere.
#[derive(Debug, Clone)]
pub struct RadioPlayback {
    pub station: Station,
    pub is_playing: bool,
    /// When this station started, for the elapsed counter that stands in for a
    /// progress bar.
    pub started_at: Instant,
    /// The track the server last announced, when it announces one at all —
    /// about six popular stations in ten do. Written from the decoder thread,
    /// which is why it is behind its own lock rather than a plain field.
    pub title: Arc<parking_lot::Mutex<Option<String>>>,
    pub volume_percent: u8,
}

impl RadioPlayback {
    /// The announced track, if there is one worth drawing.
    pub fn now_title(&self) -> Option<String> {
        self.title.lock().clone()
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }
}

/// Spotify's own account id, which every playlist it generates is owned by.
const SPOTIFY_OWNER: &str = "spotify";

/// A browsable track list (playlist, Liked Songs, album, …).
#[derive(Debug, Clone)]
pub struct TrackList {
    pub kind: TrackListKind,
    pub header: ViewHeader,
    /// Canonical order == context/fetch order. Never re-sorted in place.
    pub tracks: Vec<Track>,
    /// Display row -> index into `tracks`. Rebuilt on sort change or page
    /// arrival; identity while unsorted.
    pub display: Vec<usize>,
    pub sort: TrackSort,
    /// Set when the list is a playable context (playlist/album) so Enter can
    /// play with an offset inside it.
    pub context_uri: Option<String>,
    /// Expected total from the source's metadata; None = unknown.
    pub total: Option<u32>,
    /// More pages are still arriving for this view.
    pub loading: bool,
    /// Matches `AppState.load_generation` while a fetch owns this view.
    pub generation: u64,
    /// Key of this view in the client's track cache ("liked",
    /// "playlist:<id>", …); used by Refresh to evict and re-fetch.
    pub cache_key: Option<String>,
}

impl TrackList {
    pub fn new(
        name: impl Into<String>,
        subtitle: impl Into<String>,
        context_uri: Option<String>,
        total: Option<u32>,
    ) -> Self {
        Self {
            kind: TrackListKind::Playlist,
            header: ViewHeader {
                name: name.into(),
                subtitle: subtitle.into(),
                cover_url: None,
            },
            tracks: Vec::new(),
            display: Vec::new(),
            sort: TrackSort::default(),
            context_uri,
            total,
            loading: false,
            generation: 0,
            cache_key: None,
        }
    }

    /// Album views show the track-number column.
    pub fn show_track_no(&self) -> bool {
        self.kind == TrackListKind::Album
    }

    /// Append a page of tracks, keeping `display` consistent.
    pub fn append(&mut self, page: Vec<Track>) {
        let start = self.tracks.len();
        self.tracks.extend(page);
        self.display.extend(start..self.tracks.len());
    }

    /// Recompute `display` from the current sort (stable; identity for
    /// Position order).
    pub fn rebuild_display(&mut self) {
        self.display = (0..self.tracks.len()).collect();
        let tracks = &self.tracks;
        match self.sort.key {
            SortKey::Position => {}
            SortKey::Title => self
                .display
                .sort_by_cached_key(|&i| tracks[i].name.to_lowercase()),
            SortKey::Artist => self
                .display
                .sort_by_cached_key(|&i| tracks[i].artists.to_lowercase()),
            SortKey::Album => self
                .display
                .sort_by_cached_key(|&i| tracks[i].album.to_lowercase()),
            SortKey::Duration => self.display.sort_by_key(|&i| tracks[i].duration_ms),
        }
        if !self.sort.ascending && self.sort.key != SortKey::Position {
            self.display.reverse();
        }
    }
}

/// A browsable artist page: a header band, the artist's top tracks, and their
/// records as cards under them.
///
/// The two used to be tabs. They are one page now: the hits and the catalogue
/// are the same answer to the same question, and a tab strip made you ask it
/// twice to see all of it.
#[derive(Debug, Clone)]
pub struct ArtistView {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// CDN URL of the artist's photo, for the header band. `None` until the
    /// overview lands, and for artists Spotify has no image for.
    pub image_url: Option<String>,
    /// Spotify's genre tags. Deprecated upstream — responses often omit them
    /// now — so the band draws the line only when one arrives.
    pub genres: Vec<String>,
    pub top: TrackList,
    pub albums: Vec<AlbumItem>,
    pub loading: bool,
}

impl ArtistView {
    /// What row `index` of the page's one selectable list points at: the top
    /// tracks first, then the album cards under them.
    ///
    /// This is the only place that knows where one section ends and the other
    /// begins, so nothing else has to do the arithmetic.
    pub fn row(&self, index: usize) -> Option<ArtistRow<'_>> {
        let split = self.top.display.len();
        if index < split {
            let &ti = self.top.display.get(index)?;
            return self.top.tracks.get(ti).map(ArtistRow::Track);
        }
        self.albums.get(index - split).map(ArtistRow::Album)
    }

    pub fn len(&self) -> usize {
        self.top.display.len() + self.albums.len()
    }
}

/// What an artist-page row points at.
pub enum ArtistRow<'a> {
    Track(&'a Track),
    Album(&'a AlbumItem),
}

/// What the main pane is currently showing.
///
/// There is one pane now, so this is the whole screen's navigation model.
/// [`MainView::Home`] is where it starts and where the back stack bottoms out.
#[derive(Debug, Clone)]
pub enum MainView {
    Home,
    /// Everything the left rail used to hold: Liked Songs, then the
    /// playlists. Carries no data — it renders [`AppState::playlists`], so a
    /// snapshot of it on the back stack can never go stale.
    Playlists,
    Tracks(TrackList),
    Search(SearchResults),
    Artist(ArtistView),
    /// The one page that is not Spotify: the internet radio directory.
    Radio(RadioView),
}

/// What to call a view in the trail: the name of the particular thing on
/// screen, not its kind.
///
/// Lowercase for the two top-level views, which have no name of their own to
/// borrow. The trail uppercases every crumb it draws, so the casing chosen
/// here only shows through where a caller spells a title inline.
pub fn view_title(view: &MainView) -> String {
    match view {
        MainView::Home => "home".to_string(),
        MainView::Playlists => "playlists".to_string(),
        MainView::Tracks(list) => list.header.name.clone(),
        MainView::Artist(v) => v.name.clone(),
        MainView::Search(results) if !results.query.is_empty() => {
            format!("“{}”", results.query)
        }
        MainView::Search(_) => "search".to_string(),
        MainView::Radio(v) => v.scope.title(),
    }
}

/// A main-pane view frozen onto the back stack, with enough scroll/selection
/// state to restore it exactly.
#[derive(Debug, Clone)]
pub struct ViewSnapshot {
    pub view: MainView,
    pub main_index: usize,
    pub offset: usize,
    pub search_tab: SearchTab,
}

impl ViewSnapshot {
    /// What to call this view in a back control. See [`view_title`].
    pub fn title(&self) -> String {
        view_title(&self.view)
    }
}

/// Where a page's back control leads. Resolved once, by
/// [`AppState::back_target`], so the label the header draws and the action the
/// click performs can never disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackTarget {
    /// Pop the view stack; the label names the view being restored.
    History(String),
    /// An album page with nothing behind it: go up to the album's artist.
    Artist { id: String, name: String },
}

/// Where one crumb of a page's trail leads.
///
/// The trail is [`BackTarget`] unrolled: that resolves one step, this resolves
/// the whole chain, and the two agree because the trail's last *ancestor* is
/// built from the same stack the target reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrumbTarget {
    /// Pop the view stack back down to this depth. `Depth(0)` is the page at
    /// the bottom of the stack — Home, in a session that started there.
    Depth(usize),
    /// The implicit parent of an album opened with nothing behind it. Not on
    /// the stack, so it is opened rather than restored. See
    /// [`AppState::back_target`].
    Artist { id: String, name: String },
    /// The page you are on. Drawn as the trail's head and leads nowhere — on
    /// the browse screen. The player view draws the same trail over the page
    /// waiting underneath, and there this one closes the player.
    Current,
}

/// One step of the ancestor trail drawn on a page's section row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub target: CrumbTarget,
}

/// What page a view *is*, as against what is loaded into it.
///
/// This is what keeps the trail a path rather than a log. The stack used to
/// take whatever it was handed, so bouncing between an album and its artist
/// grew it by two a round trip and `Esc` walked the loop back out. Navigating
/// to a page already on the path now walks back to it, and that comparison is
/// this type.
///
/// The identity has to be known at the moment of the *click*, before the
/// client has fetched anything — and it is, because every command that opens a
/// page carries the id it opens (see `event::target_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewKey {
    Home,
    Playlists,
    /// Keyed by [`liked_key`] / [`playlist_key`] / [`album_key`], which is
    /// also the client's track-cache key — one spelling, so the two cannot
    /// drift.
    Tracks(String),
    Artist(String),
    /// Deliberately without the query: search is one slot, so a new query
    /// takes the place of the old one wherever it sat rather than stacking
    /// beside it. The row at the top of the screen already says which query
    /// is live.
    Search,
    /// Keyed by [`radio_key`]. A station search *does* carry its query, unlike
    /// [`ViewKey::Search`]: the radio pages are a path you walk into — chart,
    /// countries, one country — and a search is a stop on it rather than a
    /// second screen laid over the app.
    Radio(String),
}

pub fn liked_key() -> String {
    "liked".to_string()
}

pub fn playlist_key(id: &str) -> String {
    format!("playlist:{id}")
}

pub fn album_key(id: &str) -> String {
    format!("album:{id}")
}

/// A radio page's identity. Every scope spells to something different, so
/// Countries and one country are two pages on the path rather than one page
/// that replaces itself.
pub fn radio_key(scope: &RadioScope) -> String {
    match scope {
        RadioScope::Popular => "radio".to_string(),
        RadioScope::Countries => "radio:countries".to_string(),
        RadioScope::Genres => "radio:genres".to_string(),
        RadioScope::Country(code) => format!("radio:country:{code}"),
        RadioScope::Genre(tag) => format!("radio:genre:{tag}"),
        RadioScope::Favorites => "radio:saved".to_string(),
        RadioScope::Search(q) => format!("radio:search:{q}"),
    }
}

/// A view's identity, or `None` for a list that never reaches the main pane —
/// an ad-hoc queue, or an artist's top tracks.
///
/// `None` must never compare equal to another `None`: two unidentifiable lists
/// are not the same page. Callers compare `Option`s, so that falls out.
pub fn view_key(view: &MainView) -> Option<ViewKey> {
    match view {
        MainView::Home => Some(ViewKey::Home),
        MainView::Playlists => Some(ViewKey::Playlists),
        MainView::Search(_) => Some(ViewKey::Search),
        MainView::Artist(v) => Some(ViewKey::Artist(v.id.clone())),
        // `cache_key` rather than `context_uri`: the latter is `None` for
        // Liked Songs and empty for a playlist that was not in `playlists`
        // when it opened, so two unrelated pages would compare equal on it.
        MainView::Tracks(list) => list.cache_key.clone().map(ViewKey::Tracks),
        MainView::Radio(v) => Some(ViewKey::Radio(radio_key(&v.scope))),
    }
}

const VIEW_STACK_MAX: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
}

/// Screen regions recorded during draw, used to resolve mouse events.
/// Reset at the start of every frame; a region not drawn that frame stays
/// zero-sized and can never be hit.
#[derive(Debug, Default)]
pub struct HitAreas {
    /// The `♫ spot` mark at the top left of both screens: the Home button.
    pub home_btn: Rect,
    /// Each Home entry's two-line block, with the row it opens.
    ///
    /// Home is navigation, so a row opens on one click the way the left rail's
    /// did — it is not a track list where a single click has to mean "select"
    /// so a double one can mean "play".
    pub home_rows: Vec<(Rect, usize)>,
    /// The player view's `← <page>` pill, which closes the player and leaves
    /// the browse view it names on screen. Distinct from [`Self::back_btn`],
    /// which pops the view stack.
    pub close_player: Rect,
    /// Main-pane list rows.
    pub main_list: Rect,
    /// The always-on search row at the top of the browse screen.
    pub search_box: Rect,
    pub search_tabs: Vec<(Rect, SearchTab)>,
    /// The radio page's tab strip, in the same spirit as [`Self::search_tabs`].
    pub radio_tabs: Vec<(Rect, RadioTab)>,
    /// The main pane's flat line model, in the same spirit as
    /// [`Self::library_lines`]: for each content line of [`Self::main_list`],
    /// the row it belongs to, or `None` for a heading, a column header, or a
    /// spacer.
    ///
    /// Empty on every view whose rows are one line each — there a row *is* a
    /// line, and its index is the scroll offset plus the line. Only the artist
    /// page, whose album cards are four lines apiece, fills it.
    pub main_lines: Vec<Option<usize>>,
    /// The name on each visible album card, with the row it opens. The name
    /// is the link — the rest of the card selects, like any other row — so
    /// these cover the printed name and nothing else.
    pub album_names: Vec<(Rect, usize)>,
    /// The ▶ play control on each visible album card, with the row it plays.
    pub card_play: Vec<(Rect, usize)>,
    /// The ▶ Play button in a view's header band; plays the whole context.
    pub header_play_btn: Rect,
    /// The trail on a page's section row, one entry per crumb, in the order
    /// they are drawn. Replaces the single `← <page>` pill this used to be:
    /// that pill sat after the section label, so its column moved with the
    /// label's width and it drew the parent to the *right* of the child it
    /// pointed away from. See [`crate::app::state::Crumb`]. Empty on
    /// pages that do not draw one, and on panes too narrow to hold it.
    ///
    /// Only the crumbs that lead somewhere are recorded: the head of a browse
    /// screen's trail is the page you are already on, and it gets no rect.
    pub crumbs: Vec<(Rect, CrumbTarget)>,
    /// Artist column of the track table (row-height rect); clicking a cell
    /// opens that row's artist page.
    pub main_artist_col: Rect,
    /// Album column of the track table; clicking a cell opens the album.
    pub main_album_col: Rect,
    /// Liked column of the track table; clicking a cell likes or unlikes that
    /// row. The whole two-cell column, not the glyph: an unliked row draws
    /// nothing at all until the pointer is over it, so the cell is the only
    /// target there is.
    pub main_like_col: Rect,
    /// Artist name in the now-playing info row.
    pub now_artist: Rect,
    /// Album name in the now-playing info row.
    pub now_album: Rect,
    /// Whole now-playing bar (scroll target for volume).
    pub now_playing: Rect,
    pub gauge: Rect,
    pub prev_btn: Rect,
    pub play_btn: Rect,
    pub next_btn: Rect,
    pub shuffle_btn: Rect,
    /// The deck's liked control, on the title row of both views: likes or
    /// unlikes the
    /// playing track. Empty while its saved state is still unknown — a control
    /// that cannot say which way it would go is worse than no control.
    pub like_btn: Rect,
    /// Volume slider track only; click position maps linearly to percent.
    pub volume_slider: Rect,
    /// The playing queue's name on the deck's context row. Clicking it
    /// toggles the player view: it opens from the bottom bar and closes
    /// again from the player, so the two views share one way in and out.
    pub queue_name: Rect,
    /// Queue list rows in the player view (inside the borders).
    pub player_queue: Rect,
    /// The player view's visualizer band; clicking it toggles playback. The
    /// whole band is live, not just the lit bars — it is the biggest target
    /// on the screen and nothing else is drawn there.
    pub viz: Rect,
    /// The cover-art block, in whichever view drew it. Deliberately *not* a
    /// click target: the sleeve is the biggest, most inviting thing on the
    /// screen and it used to open the album, which made it the one control
    /// nothing labelled. The album's name on the row below does that job, and
    /// says so. Recorded because the layout is worth being able to find.
    pub art: Rect,
}

impl HitAreas {
    /// The main-pane row on content line `line`, if any.
    ///
    /// Without a line model a row is a line, so the answer is the line itself;
    /// the caller bounds it against the view's row count either way.
    pub fn main_item_at(&self, line: usize) -> Option<usize> {
        if self.main_lines.is_empty() {
            return Some(line);
        }
        self.main_lines.get(line).copied().flatten()
    }

    /// How many lines the main pane's content scrolls through. `rows` is the
    /// view's row count, which is the same number for every view that has no
    /// line model.
    pub fn main_scroll_len(&self, rows: usize) -> usize {
        if self.main_lines.is_empty() {
            rows
        } else {
            self.main_lines.len()
        }
    }

    /// The content lines to keep on screen for main-pane row `item`: its own
    /// lines, plus any heading directly above it — so landing on the first
    /// album card brings the "Albums" label into view with it.
    pub fn main_span(&self, item: usize) -> Option<(usize, usize)> {
        if self.main_lines.is_empty() {
            return Some((item, item + 1));
        }
        let first = self.main_lines.iter().position(|&o| o == Some(item))?;
        let end = first
            + self.main_lines[first..]
                .iter()
                .take_while(|&&o| o == Some(item))
                .count();
        let start = self.main_lines[..first]
            .iter()
            .rposition(|o| o.is_some())
            .map_or(0, |prev| prev + 1);
        Some((start, end))
    }
}

pub struct AppState {
    pub playback: Option<PlaybackSnapshot>,
    pub playlists: Vec<Playlist>,
    /// Saved ("liked") state by track URI. Absent = not checked yet, so
    /// unknown renders blank rather than as not-liked.
    pub liked: std::collections::HashMap<String, bool>,
    /// Spotify id of the signed-in user, once the playlist load has fetched
    /// it. The Playlists view leaves the Owner column blank for playlists this
    /// matches, so the column says "these are the ones you follow".
    pub me_id: Option<String>,

    /// The station playing, when one is. Mutually exclusive with
    /// [`Self::playback`] by construction: `client` stops one engine before it
    /// starts the other, so the deck never has two things to draw.
    pub radio: Option<RadioPlayback>,
    /// Stations you kept, loaded from disk at startup. The directory has no
    /// accounts, so this list is the whole of "saved".
    pub radio_favorites: Vec<Station>,

    pub main: MainView,
    /// Back-navigation history (Backspace pops), bottoming out at Home.
    pub view_stack: Vec<ViewSnapshot>,
    pub main_index: usize,
    pub search_tab: SearchTab,

    /// Persisted list widget state; its scroll offset is needed to map a
    /// clicked row back to an item index.
    pub main_list: ListState,
    pub hit: HitAreas,
    /// Last click in the main list, for double-click detection.
    pub last_main_click: Option<(usize, Instant)>,
    /// Last known mouse position; hit is rebuilt each frame, so hover state
    /// has to live here and be re-tested against fresh rects during draw.
    pub mouse_pos: Option<Position>,

    pub input_mode: InputMode,
    pub input_buffer: String,

    /// Player view (current track + visualizer + queue) replaces the
    /// library/main panes while set.
    pub show_player: bool,
    /// Tracks of the playing context, shown in the player view. Loaded by
    /// the client for playlist/album contexts; snapshotted by the event
    /// layer when playback starts from an ad-hoc URI list.
    pub queue: Option<TrackList>,
    pub queue_index: usize,
    pub queue_list: ListState,
    /// Generation guard for queue fetches, independent of `load_generation`
    /// so queue loads never cancel main-view fetches (and vice versa).
    pub queue_generation: u64,
    /// Last click in the queue list, for double-click detection.
    pub last_queue_click: Option<(usize, Instant)>,
    /// PCM tap for the visualizer; replaced with the live tap at startup.
    pub audio_tap: Arc<AudioTap>,
    /// Spectrum analysis and bar/glow/peak envelopes, persisted across frames
    /// for the visualizer's attack/decay animation.
    pub viz: crate::viz::VizState,

    /// Decoded art for the playing item, at a fixed pixel grid; the player
    /// resamples it to whatever the pane can spare. `None` until the first
    /// fetch lands, or when the item has none — the player draws a
    /// placeholder either way, so the block never changes size.
    pub cover: Option<Arc<crate::cover::Cover>>,
    /// Generation guard for cover fetches, independent of `load_generation`
    /// so browsing the library never cancels one (and vice versa) — the same
    /// reasoning as `queue_generation`.
    pub cover_generation: u64,

    /// Decoded art for the album currently being *browsed*, for the album
    /// page's header band.
    ///
    /// Deliberately separate from [`Self::cover`], which follows playback.
    /// They are different records whenever you browse one album while another
    /// plays, and only the playing one may drive the accent and the
    /// visualizer's ramp — see `Client::load_cover`.
    pub view_cover: Option<Arc<crate::cover::Cover>>,
    /// Generation guard for browsed-album cover fetches, on the same reasoning
    /// as `cover_generation` and independent of it.
    pub view_cover_generation: u64,

    /// Decoded art for the artist page: the photo in its header band and the
    /// sleeve on every album card, keyed by CDN URL.
    ///
    /// A cache rather than a slot, because the page draws many images at once
    /// and scrolls through more. Its own store, and not the client's, so
    /// filling it with a catalogue of sleeves cannot evict the playing
    /// record's cover out from under the visualizer.
    pub page_art: crate::cover::CoverCache,

    /// Bumped on every load command; in-flight fetch tasks compare against it
    /// (and their view's `generation`) and quietly exit when stale.
    pub load_generation: u64,
    /// Global busy flag; only search still blocks on it.
    pub loading: bool,
    pub toast: Option<(String, Instant)>,
    pub show_help: bool,
    pub should_quit: bool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            playback: None,
            radio: None,
            radio_favorites: Vec::new(),
            playlists: Vec::new(),
            liked: std::collections::HashMap::new(),
            me_id: None,
            main: MainView::Home,
            view_stack: Vec::new(),
            main_index: 0,
            search_tab: SearchTab::Tracks,
            main_list: ListState::default(),
            hit: HitAreas::default(),
            last_main_click: None,
            mouse_pos: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            show_player: false,
            queue: None,
            queue_index: 0,
            queue_list: ListState::default(),
            queue_generation: 0,
            last_queue_click: None,
            audio_tap: Arc::new(AudioTap::new()),
            viz: crate::viz::VizState::new(),
            cover: None,
            cover_generation: 0,
            view_cover: None,
            view_cover_generation: 0,
            page_art: crate::cover::CoverCache::with_capacity(crate::cover::PAGE_ART_MAX),
            load_generation: 0,
            loading: false,
            toast: None,
            show_help: false,
            should_quit: false,
        }
    }

    /// The Home rows that exist right now.
    ///
    /// Discover Weekly is Spotify's, not yours: it is only a row when you
    /// actually follow it. A dim "you don't have this" line would be a worse
    /// screen than three real destinations.
    pub fn home_items(&self) -> Vec<HomeItem> {
        HomeItem::ALL
            .iter()
            .copied()
            .filter(|item| match item {
                HomeItem::DiscoverWeekly => self.discover_weekly().is_some(),
                _ => true,
            })
            .collect()
    }

    /// The followed copy of Discover Weekly, if there is one.
    ///
    /// Matched on Spotify's own ownership *and* the name, because the name
    /// alone is something anyone can call a playlist of their own.
    pub fn discover_weekly(&self) -> Option<&Playlist> {
        self.playlists
            .iter()
            .find(|p| p.owner_id == SPOTIFY_OWNER && p.name.eq_ignore_ascii_case("Discover Weekly"))
    }

    /// The right-aligned tail of a Home row: how much the destination holds.
    ///
    /// Liked Songs has none — its length is not known until it is opened, and
    /// a number that appears a second later reads as a glitch.
    pub fn home_count(&self, item: HomeItem) -> String {
        let plural = |n: u32, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        match item {
            HomeItem::LikedSongs => String::new(),
            HomeItem::DiscoverWeekly => self
                .discover_weekly()
                .map(|p| plural(p.track_count, "track"))
                .unwrap_or_default(),
            HomeItem::Playlists => plural(self.playlists.len() as u32, "playlist"),
            // Only what you kept. The directory's 57,000 stations are not a
            // number this row could honestly claim, and the count is here to
            // say how much of yours is behind the door.
            HomeItem::Radio if self.radio_favorites.is_empty() => String::new(),
            HomeItem::Radio => plural(self.radio_favorites.len() as u32, "saved station"),
        }
    }

    /// Number of rows in the main pane for the current view/tab.
    pub fn main_len(&self) -> usize {
        match &self.main {
            MainView::Home => self.home_items().len(),
            MainView::Playlists => self.playlists.len(),
            MainView::Tracks(list) => list.display.len(),
            MainView::Search(results) => match self.search_tab {
                SearchTab::Tracks => results.tracks.len(),
                SearchTab::Albums => results.albums.len(),
                SearchTab::Artists => results.artists.len(),
                SearchTab::Playlists => results.playlists.len(),
            },
            MainView::Artist(v) => v.len(),
            MainView::Radio(v) => v.rows.len(),
        }
    }

    /// Number of rows in the player view's queue list.
    pub fn queue_len(&self) -> usize {
        self.queue.as_ref().map_or(0, |q| q.display.len())
    }

    /// Install a queue list, resetting its selection and scroll.
    pub fn set_queue(&mut self, list: Option<TrackList>) {
        self.queue = list;
        self.queue_index = 0;
        *self.queue_list.offset_mut() = 0;
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Reset main-pane selection and scroll to the top (new content).
    pub fn main_to_top(&mut self) {
        self.main_index = 0;
        *self.main_list.offset_mut() = 0;
    }

    /// Freeze the current main view onto the back stack (drill-in about to
    /// replace it).
    ///
    /// Home is pushed like anything else: it is a page you can come back to,
    /// and it is what the bottom of the stack restores.
    pub fn push_view(&mut self) {
        // Never the same page twice in a row. A page is pushed on the click
        // and its replacement only arrives when the client answers, so until
        // it does every further click pushes the *unchanged* current view —
        // four clicks on a Home row used to leave four copies of Home. The
        // key-based check in `event::make_way` cannot catch that, because the
        // page it is looking for is not on screen yet.
        let key = view_key(&self.main);
        if key.is_some() && self.view_stack.last().map(|s| view_key(&s.view)) == Some(key) {
            return;
        }
        if self.view_stack.len() >= VIEW_STACK_MAX {
            // The frame above the root, not the root itself. Home is the
            // bottom of this stack and the thing back exists to reach; losing
            // it left an album page with nothing behind it, which sent `Esc`
            // oscillating between that album and its artist forever.
            self.view_stack.remove(1);
        }
        self.view_stack.push(ViewSnapshot {
            view: self.main.clone(),
            main_index: self.main_index,
            offset: self.main_list.offset(),
            search_tab: self.search_tab,
        });
    }

    /// Where the current page's back control leads, or `None` when it should
    /// not be drawn.
    ///
    /// History first: back means "the page I came from", labeled with that
    /// page's name. An album page reached with nothing behind it (the very
    /// first thing opened in a session, from the now-playing bar) falls back
    /// to going *up* to the album's artist. The artist id comes from the
    /// album's own tracks — `ViewHeader` carries no ids — so the control
    /// appears once the first page of tracks has landed.
    pub fn back_target(&self) -> Option<BackTarget> {
        if let Some(snap) = self.view_stack.last() {
            return Some(BackTarget::History(snap.title()));
        }
        let MainView::Tracks(list) = &self.main else {
            return None;
        };
        if list.kind != TrackListKind::Album {
            return None;
        }
        list.tracks.iter().find_map(|t| {
            let id = t.artist_id.clone()?;
            let name = t.artists.split(',').next().unwrap_or_default().trim();
            (!name.is_empty()).then(|| BackTarget::Artist {
                id,
                name: name.to_string(),
            })
        })
    }

    /// The page's ancestors, oldest first, then the page itself.
    ///
    /// This is what the section row draws. It exists because the old single
    /// `← <page>` pill could only ever name one step: on `Home › Muse › Black
    /// Holes` it said `← Muse` and left the rest of the path to be
    /// remembered. The stack already held the whole chain — it was simply
    /// never shown.
    ///
    /// Home is one crumb and no ancestors, which draws exactly as the old
    /// `HOME` label did.
    pub fn trail(&self) -> Vec<Crumb> {
        let mut out: Vec<Crumb> = self
            .view_stack
            .iter()
            .enumerate()
            .map(|(depth, snap)| Crumb {
                label: snap.title(),
                target: CrumbTarget::Depth(depth),
            })
            .collect();
        // An empty stack can still have a parent: an album opened first thing
        // in a session belongs to an artist. `back_target` is the one place
        // that rule is written down, and with the stack empty it is the only
        // branch it can take.
        if out.is_empty()
            && let Some(BackTarget::Artist { id, name }) = self.back_target()
        {
            out.push(Crumb {
                label: name.clone(),
                target: CrumbTarget::Artist { id, name },
            });
        }
        out.push(Crumb {
            label: view_title(&self.main),
            target: CrumbTarget::Current,
        });
        out
    }

    /// Restore the snapshot at `depth`, discarding everything above it.
    ///
    /// Clicking a crumb is a jump, not a run of single steps: going from an
    /// album back to Home restores Home's own scroll and selection rather
    /// than the ones the pages in between were left at. Returns false when
    /// the depth is not on the stack.
    pub fn pop_to(&mut self, depth: usize) -> bool {
        if depth >= self.view_stack.len() {
            return false;
        }
        self.view_stack.truncate(depth + 1);
        self.pop_view()
    }

    /// Backspace: restore the most recent snapshot. Returns false when the
    /// stack is empty.
    pub fn pop_view(&mut self) -> bool {
        let Some(snap) = self.view_stack.pop() else {
            return false;
        };
        self.main = snap.view;
        self.main_index = snap.main_index;
        *self.main_list.offset_mut() = snap.offset;
        self.search_tab = snap.search_tab;
        true
    }

    /// Rebuild the track view's display order for its current sort, keeping
    /// the selection anchored to the same track where possible.
    pub fn resort_main(&mut self) {
        let MainView::Tracks(list) = &mut self.main else {
            return;
        };
        let keep = list
            .display
            .get(self.main_index)
            .map(|&i| list.tracks[i].uri.clone());
        list.rebuild_display();
        self.main_index = match keep
            .and_then(|uri| list.display.iter().position(|&i| list.tracks[i].uri == uri))
        {
            Some(pos) => pos,
            None => self.main_index.min(list.display.len().saturating_sub(1)),
        };
    }
}

pub fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str, dur: u64) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            artists: "artist".into(),
            album: "album".into(),
            release_year: "2020".into(),
            duration_ms: dur,
            track_number: 1,
            album_id: None,
            artist_id: None,
            cover_url: None,
        }
    }

    fn list_of(names: &[&str]) -> TrackList {
        let mut list = TrackList::new("L", "", None, None);
        list.append(names.iter().map(|n| track(n, 1000)).collect());
        list
    }

    #[test]
    fn rebuild_display_sorts_case_insensitively_and_reverses() {
        let mut list = list_of(&["banana", "Apple", "cherry"]);
        list.sort = TrackSort {
            key: SortKey::Title,
            ascending: true,
        };
        list.rebuild_display();
        assert_eq!(list.display, vec![1, 0, 2]);
        list.sort.ascending = false;
        list.rebuild_display();
        assert_eq!(list.display, vec![2, 0, 1]);
        list.sort = TrackSort::default();
        list.rebuild_display();
        assert_eq!(list.display, vec![0, 1, 2]);
    }

    /// Back means "the page I came from", named by that page, whatever kind
    /// of page it was.
    #[test]
    fn back_target_names_the_page_it_restores() {
        let mut st = AppState::new();
        let mut list = list_of(&["one"]);
        list.header.name = "Liked Songs".into();
        st.main = MainView::Tracks(list);
        assert_eq!(st.back_target(), None, "nothing behind a playlist page");

        st.push_view();
        st.main = MainView::Search(SearchResults {
            query: "muse".into(),
            ..Default::default()
        });
        assert_eq!(
            st.back_target(),
            Some(BackTarget::History("Liked Songs".into()))
        );

        st.push_view();
        assert_eq!(st.back_target(), Some(BackTarget::History("“muse”".into())));
    }

    /// An album page reached with nothing behind it — the first thing opened
    /// in a session, from the now-playing bar — goes *up* to its artist.
    /// The id comes from the tracks, so it appears once page one lands.
    #[test]
    fn an_empty_stack_on_an_album_page_falls_back_to_its_artist() {
        let mut st = AppState::new();
        let mut list = list_of(&["one"]);
        list.kind = TrackListKind::Album;
        st.main = MainView::Tracks(list);
        // No artist id on the tracks yet: nowhere to go.
        assert_eq!(st.back_target(), None);

        if let MainView::Tracks(list) = &mut st.main {
            list.tracks[0].artist_id = Some("r1".into());
            list.tracks[0].artists = "Donna The Buffalo, Guest".into();
        }
        assert_eq!(
            st.back_target(),
            Some(BackTarget::Artist {
                id: "r1".into(),
                name: "Donna The Buffalo".into()
            }),
            "the first credited artist, not the whole credit line"
        );

        // History still wins over the fallback.
        st.view_stack.push(ViewSnapshot {
            view: MainView::Tracks(list_of(&["x"])),
            main_index: 0,
            offset: 0,
            search_tab: SearchTab::Tracks,
        });
        assert_eq!(st.back_target(), Some(BackTarget::History("L".into())));
    }

    #[test]
    fn resort_main_keeps_selection_anchored_to_track() {
        let mut st = AppState::new();
        let mut list = list_of(&["banana", "Apple", "cherry"]);
        list.sort = TrackSort {
            key: SortKey::Title,
            ascending: true,
        };
        st.main = MainView::Tracks(list);
        st.main_index = 0; // "banana" in fetch order
        st.resort_main();
        // banana lands on display row 1 after the sort.
        assert_eq!(st.main_index, 1);
    }

    #[test]
    fn resort_main_reanchors_after_page_append() {
        let mut st = AppState::new();
        let mut list = list_of(&["banana", "cherry"]);
        list.sort = TrackSort {
            key: SortKey::Title,
            ascending: true,
        };
        st.main = MainView::Tracks(list);
        st.resort_main();
        st.main_index = 1; // "cherry"
        if let MainView::Tracks(list) = &mut st.main {
            list.append(vec![track("Apple", 1000)]);
        }
        st.resort_main();
        // Apple sorts first; cherry moves from row 1 to row 2.
        assert_eq!(st.main_index, 2);
    }
}
