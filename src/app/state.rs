use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;

use crate::app::queue::Queue;
use crate::audio_tap::AudioTap;
use crate::link::Link;
pub use crate::ui::columns::ColKey;

/// Transport state of spot's own player. Everything about *what* is playing
/// lives on the queue's current [`Track`]; this is only whether it is
/// playing, where it is, and the two toggles the deck draws.
///
/// spot drives librespot's player directly, so every field here is local
/// truth written on the keypress or on a player event — there is no remote
/// snapshot to reconcile against.
#[derive(Debug, Clone)]
pub struct Playback {
    pub is_playing: bool,
    /// Progress at `anchored_at`; the screen interpolates from there.
    pub progress_ms: u64,
    pub anchored_at: Instant,
    pub volume_percent: u8,
    pub shuffle: bool,
}

impl Playback {
    /// A playback that has just started a track from the top.
    pub fn started(volume_percent: u8, shuffle: bool) -> Self {
        Self {
            is_playing: true,
            progress_ms: 0,
            anchored_at: Instant::now(),
            volume_percent,
            shuffle,
        }
    }

    /// Re-anchor progress at `position_ms`, now.
    pub fn anchor(&mut self, position_ms: u64) {
        self.progress_ms = position_ms;
        self.anchored_at = Instant::now();
    }

    /// Progress advanced locally while playing, clamped to `duration_ms` —
    /// the playing track's length, which the queue knows and this does not.
    pub fn interpolated_progress_ms(&self, duration_ms: u64) -> u64 {
        if !self.is_playing {
            return self.progress_ms.min(duration_ms);
        }
        let elapsed = self.anchored_at.elapsed().as_millis() as u64;
        (self.progress_ms + elapsed).min(duration_ms)
    }
}

/// One credited artist: the name as printed, and the page it leads to.
///
/// `id` is `None` where the source named an artist without identifying one —
/// a radio station's announcement, most often. Such a credit still prints; it
/// simply leads nowhere, the same rule a station's country follows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credit {
    pub name: String,
    pub id: Option<String>,
}

/// The credit line as one string: what a row sorts and searches by.
///
/// The only place credits are joined, so the line and the runs drawn from
/// [`Credit`] cannot come to disagree about where a name ends.
pub fn artists_line(credits: &[Credit]) -> String {
    credits
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(CREDIT_SEP)
}

/// What separates two credits, wherever they are printed. Dim and inert: each
/// name is a target of its own and the comma is not a third one.
pub const CREDIT_SEP: &str = ", ";

#[derive(Debug, Clone)]
pub struct Track {
    pub uri: String,
    pub name: String,
    /// The credit line, joined by [`artists_line`]. Held rather than derived:
    /// it is the sort key and the search haystack of every table, and a
    /// `String` built per comparison is worse than one built per row.
    pub artists: String,
    pub album: String,
    /// Four-digit year, or empty when Spotify has no release date.
    pub release_year: String,
    pub duration_ms: u64,
    /// Position within its album disc; 0 when unknown.
    pub track_number: u32,
    pub album_id: Option<String>,
    /// Every credited artist, in the order Spotify credits them. Each name is
    /// its own link, so this is what the UI draws and hit-tests against;
    /// [`Self::artists`] is the same names joined.
    pub credits: Vec<Credit>,
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
    pub name: String,
    pub track_count: u32,
    pub owner: String,
    /// Spotify id of the owner, for telling your own playlists apart from the
    /// ones you follow. Compared against [`AppState::me_id`] rather than
    /// `owner`, which is a display name and need not be unique.
    pub owner_id: String,
    /// Spotify's content-version hash; changes whenever the playlist does.
    pub snapshot_id: String,
    /// CDN URL of the playlist's own cover, so opening it can show the art
    /// without a second round trip. `None` when Spotify gave the playlist no
    /// images, or gave it only a mosaic — see [`crate::cover::is_mosaic`].
    pub cover_url: Option<String>,
    /// Whether the playlist is listed on the owner's profile. `None` when
    /// Spotify would not say, which it does for playlists that are not yours.
    pub public: Option<bool>,
    /// Whether anyone the owner invited may add to it.
    pub collaborative: bool,
}

#[derive(Debug, Clone)]
pub struct AlbumItem {
    pub id: String,
    pub name: String,
    /// The credit line, joined by [`artists_line`], on the same terms as
    /// [`Track::artists`].
    pub artists: String,
    /// Every credited artist, on the same terms as [`Track::credits`].
    pub credits: Vec<Credit>,
    /// Four-digit year, or empty when Spotify has no release date.
    pub release_year: String,
    /// "album", "single", or "compilation"; may be empty.
    pub album_type: String,
    /// How the record relates to the artist whose page it was fetched for:
    /// "album", "single", "compilation" or "appears_on". This is what the
    /// artist page's tabs group by, and it is not the same as
    /// [`Self::album_type`] — a record the artist only guests on carries the
    /// type of whatever it is, and the group `appears_on`. Empty outside the
    /// artist page, which is the only place Spotify reports it.
    pub album_group: String,
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

/// A load that came back an error, and the command that asks for it again.
///
/// A page that failed reads exactly like a page that is genuinely empty
/// unless it keeps the reason, and the toast cannot carry it: the toast
/// expires in seconds and the bottom bar it draws on is not there at all
/// while nothing is playing.
///
/// The retry command is captured where the load starts rather than rebuilt
/// from the view, because a view that failed has nothing left to rebuild it
/// from — a failed album page holds no track to read its year off.
#[derive(Debug, Clone)]
pub struct LoadError {
    pub message: String,
    pub retry: crate::app::command::AppCommand,
}

impl LoadError {
    pub fn new(message: impl Into<String>, retry: crate::app::command::AppCommand) -> Self {
        Self {
            message: message.into(),
            retry,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub query: String,
    pub tracks: SortedList<Track>,
    pub albums: SortedList<AlbumItem>,
    pub artists: SortedList<ArtistItem>,
    pub playlists: SortedList<Playlist>,
    /// The station half of the answer. The two catalogues are two hosts and
    /// answer at their own pace, so this fills in after the four above rather
    /// than with them.
    pub stations: SortedList<Station>,
    /// True from the moment the query goes out until the directory answers or
    /// fails. Its own flag and not [`AppState::loading`], which the Spotify
    /// half owns: an empty Stations tab means "still asking" or "nothing
    /// there", and those read very differently.
    pub stations_loading: bool,
    /// Matches [`AppState::load_generation`] while a fetch owns this view, in
    /// the same spirit as [`RadioView::generation`] and
    /// [`TrackList::generation`]. The station half lands on its own, so it has
    /// to prove it belongs to the results on screen and not to a query the
    /// user has already replaced.
    pub generation: u64,
    /// Why the Spotify half of this query came back with nothing. Its four
    /// tabs report this one; [`Self::stations_error`] speaks for the fifth.
    pub error: Option<LoadError>,
    /// Why the directory half came back with nothing.
    ///
    /// Its own field for the reason [`Self::stations_loading`] is: the two
    /// halves are two hosts, and one of them being unreachable is not this
    /// query failing.
    pub stations_error: Option<LoadError>,
}

/// The five cuts of one query.
///
/// The first four are one Spotify response read four ways; [`SearchTab::Stations`]
/// is the radio directory, asked at the same moment over a different host. It
/// sits last because it is the one that fills in late, and a tab that arrives
/// after the others reads better at the end of the strip than in the middle of
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTab {
    Tracks,
    Albums,
    Artists,
    Playlists,
    Stations,
}

impl SearchTab {
    pub const ALL: [SearchTab; 5] = [
        SearchTab::Tracks,
        SearchTab::Albums,
        SearchTab::Artists,
        SearchTab::Playlists,
        SearchTab::Stations,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SearchTab::Tracks => "Tracks",
            SearchTab::Albums => "Albums",
            SearchTab::Artists => "Artists",
            SearchTab::Playlists => "Playlists",
            SearchTab::Stations => "Stations",
        }
    }
}

/// How a list is ordered: which column, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: ColKey,
    pub ascending: bool,
}

impl Default for Sort {
    /// Fetch order, which is what every list arrives in.
    fn default() -> Self {
        Self {
            key: ColKey::No,
            ascending: true,
        }
    }
}

impl Sort {
    /// Whether the rows are still in the order the source sent.
    ///
    /// What playback and the track cache test: a list read from the bottom up
    /// is as much a snapshot as one ordered by title, and later pages can only
    /// honestly extend the order they arrived in.
    pub fn is_natural(self) -> bool {
        self == Self::default()
    }
}

/// One row's value in one column, as something orderable.
///
/// `None` is what a source left empty, and it sorts last whichever way the
/// arrow points: a record with no release date belongs at neither end of a
/// list ordered by year, and floating it to the top buries the answer the
/// sort was asked for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortCell {
    Text(String),
    Num(u64),
    None,
}

impl SortCell {
    /// A text cell, folded for comparison. Empty text is [`SortCell::None`]:
    /// a blank name is a missing name.
    pub fn text(s: &str) -> Self {
        match s.trim() {
            "" => SortCell::None,
            t => SortCell::Text(t.to_lowercase()),
        }
    }
}

/// What a row is worth in each column, and what it is across a re-sort.
pub trait Sortable {
    fn cell(&self, key: ColKey) -> SortCell;
    /// What a row is, across a re-sort — a uri, an id, a uuid, a facet key.
    /// This is what re-anchors the selection to the row it was on.
    fn identity(&self) -> &str;
}

/// Re-sort a list and answer where the row that was on `index` went.
///
/// A re-sort moves every row, so an index carried across one points at
/// whatever happened to land there. The identity of the selected row is what
/// survives instead.
fn anchored<T: Sortable>(list: &mut SortedList<T>, index: usize) -> usize {
    anchored_keeping(list, index, |_| true)
}

/// [`anchored`], with the filter a filtered list is re-cut through.
fn anchored_keeping<T: Sortable>(
    list: &mut SortedList<T>,
    index: usize,
    keep: impl Fn(&T) -> bool,
) -> usize {
    let was = list.get(index).map(|t| t.identity().to_string());
    list.rebuild_keeping(keep);
    was.and_then(|id| list.position_of(&id))
        .unwrap_or_else(|| index.min(list.len().saturating_sub(1)))
}

/// The display permutation of `items` under `sort`, keeping the rows `keep`
/// holds.
///
/// The one sort in the app. Tracks, albums, playlists, stations and countries
/// all reach their order through here, so no two of them can come to disagree
/// about what ordering by a column means.
pub fn sorted_display<T: Sortable>(
    items: &[T],
    sort: Sort,
    keep: impl Fn(&T) -> bool,
) -> Vec<usize> {
    let mut rows: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, t)| keep(t))
        .map(|(i, _)| i)
        .collect();
    // The `#` column is fetch order itself, so there is nothing to compare —
    // the arrow just says which end it is read from.
    if sort.key == ColKey::No {
        if !sort.ascending {
            rows.reverse();
        }
        return rows;
    }
    let mut keyed: Vec<(SortCell, usize)> = rows
        .into_iter()
        .map(|i| (items[i].cell(sort.key), i))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    if !sort.ascending {
        keyed.reverse();
        // Reversing floats the blanks to the front; put them back on the end.
        let (blank, filled): (Vec<_>, Vec<_>) =
            keyed.into_iter().partition(|(c, _)| *c == SortCell::None);
        keyed = filled.into_iter().chain(blank).collect();
    }
    keyed.into_iter().map(|(_, i)| i).collect()
}

/// A list in fetch order with a display permutation over it.
///
/// `items` is what the source sent and is never re-ordered in place: playback
/// follows fetch order, later pages can only extend it, and a cache key means
/// nothing against a list that shuffled under it. `display` is what the screen
/// shows, and every row index the user picks indexes `display`.
#[derive(Debug, Clone)]
pub struct SortedList<T> {
    pub items: Vec<T>,
    pub display: Vec<usize>,
    pub sort: Sort,
}

/// Hand-written: the derive would demand `T: Default`, which no row type has
/// any business implementing.
impl<T> Default for SortedList<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Sortable> FromIterator<T> for SortedList<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_items(iter.into_iter().collect())
    }
}

impl<T: Sortable> From<Vec<T>> for SortedList<T> {
    fn from(items: Vec<T>) -> Self {
        Self::from_items(items)
    }
}

impl<T> SortedList<T> {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            display: Vec::new(),
            sort: Sort::default(),
        }
    }

    /// Rows on screen, which on a filtered list is not `items.len()`.
    pub fn len(&self) -> usize {
        self.display.len()
    }

    pub fn is_empty(&self) -> bool {
        self.display.is_empty()
    }

    /// The row at display position `row`.
    pub fn get(&self, row: usize) -> Option<&T> {
        self.items.get(*self.display.get(row)?)
    }

    pub fn first(&self) -> Option<&T> {
        self.get(0)
    }

    /// The rows in display order.
    pub fn rows(&self) -> impl Iterator<Item = &T> {
        self.display.iter().filter_map(|&i| self.items.get(i))
    }
}

impl<T: Sortable> SortedList<T> {
    pub fn from_items(items: Vec<T>) -> Self {
        let mut list = Self {
            items,
            display: Vec::new(),
            sort: Sort::default(),
        };
        list.rebuild();
        list
    }

    /// Append a page, putting the new rows on the end of `display`.
    ///
    /// The permutation is left to [`AppState::resort_main`], which the caller
    /// runs next: it is the one that knows which row the selection was on, and
    /// re-sorting here would move that row out from under it before anything
    /// had noted where it went.
    pub fn append(&mut self, page: Vec<T>) {
        let start = self.items.len();
        self.items.extend(page);
        self.display.extend(start..self.items.len());
    }

    pub fn rebuild(&mut self) {
        self.display = sorted_display(&self.items, self.sort, |_| true);
    }

    /// Rebuild with a filter over it. The artist page cuts its catalogue by
    /// tab, and the tab is a filter the sort then orders within.
    pub fn rebuild_keeping(&mut self, keep: impl Fn(&T) -> bool) {
        self.display = sorted_display(&self.items, self.sort, keep);
    }

    /// The display position of the row whose identity is `id`.
    pub fn position_of(&self, id: &str) -> Option<usize> {
        self.display
            .iter()
            .position(|&i| self.items[i].identity() == id)
    }
}

impl Sortable for Track {
    fn cell(&self, key: ColKey) -> SortCell {
        match key {
            ColKey::Title => SortCell::text(&self.name),
            ColKey::Artist => SortCell::text(&self.artists),
            ColKey::Album => SortCell::text(&self.album),
            ColKey::Year => SortCell::text(&self.release_year),
            ColKey::Time => SortCell::Num(self.duration_ms),
            _ => SortCell::None,
        }
    }

    fn identity(&self) -> &str {
        &self.uri
    }
}

impl Sortable for AlbumItem {
    fn cell(&self, key: ColKey) -> SortCell {
        match key {
            ColKey::Album | ColKey::Title => SortCell::text(&self.name),
            ColKey::Artist => SortCell::text(&self.artists),
            ColKey::Year => SortCell::text(&self.release_year),
            ColKey::Type => SortCell::text(&self.album_type),
            ColKey::Tracks => SortCell::Num(u64::from(self.track_count)),
            _ => SortCell::None,
        }
    }

    fn identity(&self) -> &str {
        &self.id
    }
}

impl Sortable for ArtistItem {
    fn cell(&self, key: ColKey) -> SortCell {
        match key {
            ColKey::Artist | ColKey::Name | ColKey::Title => SortCell::text(&self.name),
            _ => SortCell::None,
        }
    }

    fn identity(&self) -> &str {
        &self.id
    }
}

impl Sortable for Playlist {
    fn cell(&self, key: ColKey) -> SortCell {
        match key {
            ColKey::Title | ColKey::Name => SortCell::text(&self.name),
            ColKey::Owner => SortCell::text(&self.owner),
            ColKey::Tracks => SortCell::Num(u64::from(self.track_count)),
            _ => SortCell::None,
        }
    }

    fn identity(&self) -> &str {
        &self.id
    }
}

impl Sortable for Station {
    fn cell(&self, key: ColKey) -> SortCell {
        match key {
            ColKey::Station | ColKey::Name | ColKey::Title => SortCell::text(&self.name),
            ColKey::Tags => SortCell::text(&self.tags),
            ColKey::Where => SortCell::text(match self.countrycode.is_empty() {
                true => &self.country,
                false => &self.countrycode,
            }),
            // By what the row says, so the column sorts the way it reads:
            // "AAC+ 128k" groups the codecs and then ranks inside one.
            ColKey::Stream => SortCell::text(&self.quality()),
            _ => SortCell::None,
        }
    }

    fn identity(&self) -> &str {
        &self.uuid
    }
}

impl Sortable for RadioRow {
    fn cell(&self, key: ColKey) -> SortCell {
        match self {
            RadioRow::Facet {
                key: code,
                label,
                count,
            } => match key {
                ColKey::Name | ColKey::Title | ColKey::Station => SortCell::text(label),
                ColKey::Code => SortCell::text(code),
                ColKey::Stations => SortCell::Num(u64::from(*count)),
                _ => SortCell::None,
            },
            RadioRow::Station(s) => s.cell(key),
        }
    }

    fn identity(&self) -> &str {
        match self {
            RadioRow::Facet { key, .. } => key,
            RadioRow::Station(s) => &s.uuid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ViewHeader {
    pub name: String,
    /// e.g. "by owner" for playlists, "Artist · 2011" for albums.
    ///
    /// What the band prints where it has no [`Self::credits`] to print
    /// instead. An album has both: the string is what a narrow band falls
    /// back to, and the credits are what the full one draws as links.
    pub subtitle: String,
    /// The page's own credited artists, each a link. Empty on every page that
    /// is not about a record — a playlist is by its owner, not by an artist.
    pub credits: Vec<Credit>,
    /// CDN URL of the sleeve, for the header band to draw.
    ///
    /// A playlist sets it only when it has a cover of its own: an
    /// auto-generated mosaic is four sleeves at a sixth of the size each, and
    /// the band reads better without one. See [`crate::cover::is_mosaic`].
    pub cover_url: Option<String>,
    /// The blurb Spotify carries for a playlist, already unescaped. Empty for
    /// albums, for Liked Songs, and for the many playlists with none.
    pub description: String,
    /// Spotify id of a playlist's owner, for telling the one control Spotify
    /// would accept from the one it would refuse.
    ///
    /// On the header rather than read off [`AppState::playlists`], because a
    /// playlist opened from a search is not in that list and still has an
    /// owner. Empty until known, and on every page that is not a playlist.
    pub owner_id: String,
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
    Update,
    LikedSongs,
    DiscoverWeekly,
    Playlists,
    Radio,
    Spotify,
    /// Whether a Spotify link clicked anywhere on this machine opens in spot.
    /// A control rather than a destination, the way [`HomeItem::Update`] is.
    Links,
}

impl HomeItem {
    /// Every destination, in the order Home lists them. The two named records
    /// lead, because they are the ones you open by name; Playlists is the
    /// catch-all under them, and Radio is last because it is the one row that
    /// leaves Spotify behind. [`HomeItem::Spotify`] follows it: the three rows
    /// above only exist once it has been used, and it is not there once they
    /// are — see [`AppState::home_items`].
    ///
    /// [`HomeItem::Update`] leads because it is the one row that is about the
    /// app rather than about music, and it is absent on almost every run.
    pub const ALL: [HomeItem; 7] = [
        HomeItem::Update,
        HomeItem::LikedSongs,
        HomeItem::DiscoverWeekly,
        HomeItem::Playlists,
        HomeItem::Radio,
        HomeItem::Spotify,
        HomeItem::Links,
    ];

    pub fn title(self) -> &'static str {
        match self {
            HomeItem::Update => "Update available",
            HomeItem::LikedSongs => "Liked Songs",
            HomeItem::DiscoverWeekly => "Discover Weekly",
            HomeItem::Playlists => "Playlists",
            HomeItem::Radio => "Radio",
            HomeItem::Spotify => "Spotify",
            HomeItem::Links => "Spotify links",
        }
    }

    /// The dim line under the name, saying what the destination holds.
    ///
    /// The Spotify, Update and Links rows have lines that move, so it is
    /// [`AppState::home_blurb`] that the screen asks.
    pub fn blurb(self) -> &'static str {
        match self {
            HomeItem::Update => "press Enter to download and install it",
            HomeItem::LikedSongs => "everything you have saved",
            HomeItem::DiscoverWeekly => "thirty new tracks every Monday",
            HomeItem::Playlists => "saved and followed",
            HomeItem::Radio => "live stations from around the world",
            HomeItem::Spotify => "connect an account to play your library",
            HomeItem::Links => "where a clicked Spotify link opens",
        }
    }
}

/// What the Links row knows, read rather than assumed.
///
/// The answer lives in the registry, where another app can change it between
/// runs, so spot reads it at startup and again after the row acts. It is kept
/// here rather than read per frame: it almost never moves, and a syscall to
/// draw one dim line would be a poor trade.
#[derive(Debug, Clone, Default)]
pub struct LinksRow {
    /// The line the row shows — see `protocol::Registration::describe`.
    pub status: String,
    /// Whether a clicked Spotify link reaches spot right now.
    pub in_force: bool,
    /// The prompt the row shows while it waits for the second press that
    /// claims the scheme, naming the app that press would displace. `None`
    /// when the row is not armed, which is every state but that one.
    ///
    /// Claiming reaches outside spot and breaks another app's links, so it
    /// takes two deliberate presses. Giving it back takes one.
    pub confirming: Option<String>,
}

/// How much of Spotify spot has.
///
/// Radio needs none of it, so this starts at [`SpotifyState::Off`] and the app
/// is usable there. Everything Spotify appears or disappears from the screen
/// on this one value — see [`AppState::home_items`] and
/// [`AppState::search_tabs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotifyState {
    /// No account: no library, no lookups, radio only.
    Off,
    /// A sign-in is running.
    Connecting,
    /// The Web API answers, but nothing can be streamed. The string is the
    /// short reason the Home row shows. A radio station still gets its record
    /// named, its sleeve drawn and its Like control.
    Limited(String),
    /// Signed in with Premium: the whole application.
    Ready,
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
        }
    }

    /// The tab this scope belongs under, so drilling into a country still
    /// leaves Countries lit.
    pub fn tab(&self) -> RadioTab {
        match self {
            RadioScope::Popular => RadioTab::Popular,
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
    /// Stations, or the facets of a directory index — one list for both, since
    /// the directory never mixes the two kinds in one answer.
    pub rows: SortedList<RadioRow>,
    pub loading: bool,
    /// Matches `AppState.load_generation` while a fetch owns this view, on the
    /// same reasoning as [`TrackList::generation`].
    pub generation: u64,
    /// Why the directory came back with nothing — see [`LoadError`].
    pub error: Option<LoadError>,
}

impl RadioView {
    pub fn new(scope: RadioScope, generation: u64) -> Self {
        Self {
            scope,
            rows: SortedList::new(),
            loading: true,
            generation,
            error: None,
        }
    }
}

/// What is playing, when what is playing is a radio station.
///
/// Deliberately not a [`Playback`]: a broadcast has no duration to
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
    /// How many channels the stream decodes to, or 0 before the decoder has
    /// identified it. The directory reports a codec and a bitrate but never a
    /// channel count, so this is the only place stereo and mono are told apart.
    /// Written by the tune-in task rather than the decoder thread, but shared
    /// for the same reason [`Self::title`] is: the deck reads it every frame.
    pub channels: Arc<AtomicU8>,
    pub volume_percent: u8,
    /// What Spotify has for [`Self::title`], once the client has looked.
    ///
    /// A plain field rather than something behind the title's lock: the decoder
    /// thread owns `title` and writes nothing else, while this is written by
    /// the client task under the state lock like everything else. Keeping them
    /// apart is what lets `Client::resolve_radio_track` ask whether an answer
    /// is still about the announcement it was for.
    pub matched: RadioMatch,
    /// Why this station is not playing, when it would not play at all.
    ///
    /// A station that fails keeps the deck rather than clearing it. The
    /// controls that reach another station are *on* the deck, so dropping the
    /// deck on a failure takes away the one thing that gets you out of it.
    pub failure: Option<String>,
    /// How many stations a seek has tried to reach this one, or 0 for a
    /// station you chose yourself.
    ///
    /// A station you picked is the one you meant, and a failure is the end of
    /// it. A station a seek landed on is one of a run, and a failure is a
    /// reason to keep walking — bounded by this count, because each attempt
    /// can cost the whole connect timeout.
    pub seek_attempt: u8,
    /// Which tune-in this deck is, counted by the client.
    ///
    /// A failure comes back over the command channel, so a slow one from a
    /// tune-in already abandoned can arrive after the deck has moved on. The
    /// uuid alone does not catch it: retrying a station that failed puts the
    /// same uuid on the deck the old failure names.
    pub tune_seq: u64,
    /// A record from this station's own list is playing through Spotify, so
    /// the stream is stopped and the deck draws the record rather than the
    /// broadcast. The deck's `live` control puts the stream back.
    ///
    /// The deck is kept rather than cleared because the page is still the
    /// station's: its list is what the record is playing from, and the way
    /// back on air is a control on that page.
    pub off_air: bool,
    /// What a probe read off the station while its stream was stood down.
    ///
    /// A plain field rather than something behind the title's lock, for the
    /// same reason [`Self::matched`] is one: the decoder thread owns `title`
    /// and writes nothing else, while this is written by the client task under
    /// the state lock like everything else. Off air the decoder is stopped, so
    /// this is the only thing that knows what the station is playing — see
    /// `Client::probe_off_air_station`.
    pub probed: Option<String>,
}

impl Track {
    /// The `OpenAlbum` this record's album name leads to, when it has one.
    /// One resolution for every control that opens a record's album — the
    /// deck's link, `b`, the Album column — so they cannot drift.
    pub fn open_album(&self) -> Option<crate::app::command::AppCommand> {
        let id = self.album_id.as_ref()?;
        Some(crate::app::command::AppCommand::OpenAlbum {
            id: id.clone(),
            name: self.album.clone(),
            credits: self.credits.clone(),
            year: self.release_year.clone(),
            cover_url: self.cover_url.clone(),
        })
    }

    /// The `OpenArtist` this record's *first* credited artist leads to.
    ///
    /// What a keypress opens: `B` has no pointer to say which of several names
    /// it meant, so it takes the one the record is filed under. A click
    /// resolves the name under it instead — see `HitAreas::main_artist_links`.
    pub fn open_artist(&self) -> Option<crate::app::command::AppCommand> {
        open_artist(self.credits.first()?)
    }
}

/// The `OpenArtist` a credit leads to, when Spotify identified the artist.
///
/// One resolution for every control that opens an artist — the deck's link,
/// `B`, an Artist column, a masthead's credit line — so they cannot drift.
pub fn open_artist(credit: &Credit) -> Option<crate::app::command::AppCommand> {
    let id = credit.id.as_deref()?;
    Some(crate::app::command::AppCommand::OpenArtist {
        id: id.to_string(),
        uri: format!("spotify:artist:{id}"),
        name: credit.name.clone(),
    })
}

/// What Spotify has for the track a station just announced.
///
/// A state machine rather than an `Option<Track>` so the deck can say which of
/// four things is true, instead of drawing the same blank row for "the server
/// said nothing", "we are looking", and "we looked and it is not there". Only
/// the last of them may draw a `★`.
#[derive(Debug, Clone, Default)]
pub enum RadioMatch {
    /// No usable announcement: the server sends none, or what it sends is not
    /// a track — a station ident, a promo, a URL. A lookup that errored lands
    /// here too; see `Client::resolve_radio_track`.
    #[default]
    None,
    /// Parsed, and a search is out for it.
    Searching,
    /// Searched, and nothing on Spotify was close enough. What the station said
    /// is still drawn — it is what is playing — it just has no page behind it.
    Unmatched,
    /// Boxed: a `Track` is ten strings and `RadioPlayback` is cloned per frame.
    Matched(Box<Track>),
}

impl RadioMatch {
    /// The Spotify record behind an announcement, when the lookup found one.
    ///
    /// One resolution for the deck and for a row of a station's list, so the
    /// two cannot come to disagree about what counts as a match.
    pub fn track(&self) -> Option<&Track> {
        match self {
            RadioMatch::Matched(t) => Some(t),
            _ => None,
        }
    }
}

/// Records kept per station. A long evening on one station, past which the
/// oldest rows go: the list is a memory of what you heard, not a log.
pub const HEARD_MAX: usize = 200;

/// One record a station announced, and what Spotify made of it.
///
/// Every announcement makes one of these, whether Spotify identified it or
/// not: a row that carries only the station's own words still says what was
/// played at that moment, which is the whole point of keeping the list.
#[derive(Debug, Clone)]
pub struct Heard {
    pub announced: String,
    pub matched: RadioMatch,
    /// When the station announced this record, which is what the list's `Ago`
    /// column counts from.
    pub at: Instant,
}

impl Heard {
    pub fn new(announced: String) -> Self {
        Self {
            announced,
            matched: RadioMatch::None,
            at: Instant::now(),
        }
    }

    /// The Spotify record behind the announcement, if one was found.
    pub fn track(&self) -> Option<&Track> {
        self.matched.track()
    }
}

/// The selection and scroll of whichever list the player screen is drawing.
pub struct PlayerList<'a> {
    pub len: usize,
    /// The list's height from the last frame, which is what a half-page step
    /// and a scroll clamp are measured in.
    pub height: u16,
    pub index: &'a mut usize,
    pub list: &'a mut ListState,
}

/// The queue put aside while a record off a station's list plays.
///
/// Playing something a station played must not cost the play order you
/// already had, so the queue is parked whole — the selection and the scroll
/// with it, since the player screen is what you come back to.
#[derive(Debug, Clone)]
pub struct ParkedQueue {
    pub queue: Queue,
    pub index: usize,
    pub offset: usize,
}

/// What a station nobody is listening to is announcing.
///
/// A state machine for the same reason [`RadioMatch`] is one: "we have not
/// asked yet", "it says nothing" and "it would not answer" are three different
/// facts about a row, and one blank cell cannot tell them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NowStatus {
    /// A probe is out for this station.
    Probing,
    Title(String),
    /// Reached, and announcing nothing. Roughly four popular stations in ten
    /// interleave no metadata at all.
    Quiet,
    Unreachable,
}

/// One station's announcement and when it was read.
///
/// The stamp is what bounds the traffic: a row is re-probed only once its
/// reading is older than the client's `NOW_TTL`.
#[derive(Debug, Clone)]
pub struct StationNow {
    pub status: NowStatus,
    pub checked_at: Instant,
}

impl StationNow {
    pub fn new(status: NowStatus) -> Self {
        Self {
            status,
            checked_at: Instant::now(),
        }
    }
}

impl RadioPlayback {
    /// A station that has just started: playing, announcing nothing yet.
    pub fn new(
        station: Station,
        volume_percent: u8,
        title: Arc<parking_lot::Mutex<Option<String>>>,
        channels: Arc<AtomicU8>,
    ) -> Self {
        Self {
            station,
            is_playing: true,
            started_at: Instant::now(),
            title,
            channels,
            volume_percent,
            matched: RadioMatch::None,
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
            off_air: false,
            probed: None,
        }
    }

    /// Whether this station would not play at all.
    pub fn failed(&self) -> bool {
        self.failure.is_some()
    }

    /// The announced track, if there is one worth drawing.
    ///
    /// The decoder's reading first and a probe's second: on air the two never
    /// disagree, because the probe only runs while the stream is stood down
    /// and the stop that stands it down nulls the decoder's lock on its way
    /// past.
    pub fn now_title(&self) -> Option<String> {
        self.title.lock().clone().or_else(|| self.probed.clone())
    }

    /// How the stream is mixed, once the decoder has identified it.
    pub fn channel_label(&self) -> Option<String> {
        match self.channels.load(Ordering::Relaxed) {
            0 => None,
            1 => Some("mono".to_string()),
            2 => Some("stereo".to_string()),
            n => Some(format!("{n} ch")),
        }
    }

    /// The Spotify record behind the announcement, if one was found.
    pub fn matched_track(&self) -> Option<&Track> {
        match &self.matched {
            RadioMatch::Matched(t) => Some(t),
            _ => None,
        }
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
    /// The rows, in fetch order, with the display permutation over them.
    pub rows: SortedList<Track>,
    /// Expected total from the source's metadata; None = unknown.
    pub total: Option<u32>,
    /// More pages are still arriving for this view.
    pub loading: bool,
    /// Matches `AppState.load_generation` while a fetch owns this view.
    pub generation: u64,
    /// Key of this view in the client's track cache (`"liked"`,
    /// `"playlist:<id>"`, …); Refresh reads it to evict and re-fetch.
    pub cache_key: Option<String>,
    /// Why the pages stopped arriving — see [`LoadError`]. Set on the page
    /// that failed, so a list that got half-way through says so with the rows
    /// it did get still on screen.
    pub error: Option<LoadError>,
}

impl TrackList {
    pub fn new(name: impl Into<String>, subtitle: impl Into<String>, total: Option<u32>) -> Self {
        Self {
            kind: TrackListKind::Playlist,
            header: ViewHeader {
                name: name.into(),
                subtitle: subtitle.into(),
                credits: Vec::new(),
                cover_url: None,
                description: String::new(),
                owner_id: String::new(),
            },
            rows: SortedList::new(),
            total,
            loading: false,
            generation: 0,
            cache_key: None,
            error: None,
        }
    }

    /// An album page numbers its rows by the track's own position on the record
    /// and drops the Album column, which would name the page you are on.
    pub fn is_album(&self) -> bool {
        self.kind == TrackListKind::Album
    }
}

/// The rows read straight through, so `list.display`, `list.items` and
/// `list.sort` all still name what they always did.
impl std::ops::Deref for TrackList {
    type Target = SortedList<Track>;

    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl std::ops::DerefMut for TrackList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rows
    }
}

/// What Wikipedia says about an artist, and the picture at the head of that
/// article.
///
/// Deserialized nowhere: [`crate::wiki`] builds it out of four replies, none
/// of which has this shape. The text is CC BY-SA, which is why the article's
/// own address travels with it rather than being rebuilt for display.
#[derive(Debug, Clone)]
pub struct ArtistBio {
    /// The lead section as prose, its paragraphs one blank line apart.
    pub text: String,
    /// The article's lead image, where that image is a photograph spot can
    /// decode. See [`crate::cover::is_wikimedia_thumb`].
    pub image_url: Option<String>,
    pub source_url: String,
}

impl ArtistBio {
    /// The article's opening paragraph, which is what the header band wraps
    /// into the rows it has.
    ///
    /// One paragraph rather than the whole lead: a blank line in a band five
    /// rows deep would spend one of them on nothing, and what comes after the
    /// break is a keypress away.
    pub fn lead(&self) -> &str {
        self.text.split('\n').next().unwrap_or_default()
    }
}

/// How far the artist page has got towards saying anything about the artist.
///
/// One value rather than a flag beside an option: a page cannot be both still
/// looking and finished, and spelling it this way means nothing has to keep
/// two fields agreeing.
#[derive(Debug, Clone, Default)]
pub enum BioState {
    /// The lookup is out. The band says nothing rather than saying there is
    /// nothing yet.
    Loading,
    /// The chain reached no article, which is the ordinary answer for anyone
    /// small. The default, so a page built without a lookup behind it starts
    /// where most of them end.
    #[default]
    Missing,
    Ready(Arc<ArtistBio>),
}

/// How the catalogue is cut, as a tab strip under the "Albums" heading.
///
/// Grouped by [`AlbumItem::album_group`] rather than the type, because only
/// the group tells a record the artist made from one they play on.
///
/// Cuts of one answer, like [`SearchTab`] and unlike [`RadioTab`]: the fetch
/// asks for all four groups at once, so switching tab costs nothing and pushes
/// nothing onto the back stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistTab {
    Albums,
    Singles,
    Compilations,
    AppearsOn,
}

impl ArtistTab {
    pub const ALL: [ArtistTab; 4] = [
        ArtistTab::Albums,
        ArtistTab::Singles,
        ArtistTab::Compilations,
        ArtistTab::AppearsOn,
    ];

    pub fn title(self) -> &'static str {
        match self {
            ArtistTab::Albums => "Albums",
            ArtistTab::Singles => "Singles",
            ArtistTab::Compilations => "Compilations",
            ArtistTab::AppearsOn => "Appears On",
        }
    }

    /// Whether a record belongs under this tab.
    ///
    /// Anything Spotify labelled with nothing we know falls under Albums, so
    /// no record can go missing between the tabs.
    pub fn holds(self, album: &AlbumItem) -> bool {
        match album.album_group.as_str() {
            "single" => self == ArtistTab::Singles,
            "compilation" => self == ArtistTab::Compilations,
            "appears_on" => self == ArtistTab::AppearsOn,
            _ => self == ArtistTab::Albums,
        }
    }
}

/// A browsable artist page: a header band, the artist's top tracks, and their
/// records as cards under them.
///
/// Hits and catalogue share one page: they are the same answer to the same
/// question, and a tab strip between them makes you ask it twice to see all of
/// it. The strip [`ArtistTab`] draws is a different axis — it cuts the
/// catalogue into its groups, and never hides the top tracks.
#[derive(Debug, Clone)]
pub struct ArtistView {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// CDN URL of the artist's photo, for the header band. `None` until the
    /// overview lands, and for artists Spotify has no image for.
    pub image_url: Option<String>,
    /// Spotify's genre tags. Deprecated upstream, and often absent from a
    /// response, so the band draws the line only when one arrives.
    pub genres: Vec<String>,
    /// What Wikipedia has to say, once the second fetch lands. Independent of
    /// [`Self::loading`], which is the Spotify overview's: the two are asked
    /// for separately and the page must not wait on the slower.
    pub bio: BioState,
    pub top: TrackList,
    /// The whole catalogue, every group together, with the active tab as the
    /// display permutation over it — a filter the sort then orders within.
    pub albums: SortedList<AlbumItem>,
    pub tab: ArtistTab,
    pub loading: bool,
    /// Why the overview came back with nothing — see [`LoadError`].
    pub error: Option<LoadError>,
}

impl ArtistView {
    /// The picture the page wears: Spotify's own where it has one, otherwise
    /// the one at the head of the artist's Wikipedia article.
    ///
    /// Resolved here rather than written into [`Self::image_url`] when the bio
    /// lands, because the two fetches settle in either order and a slot both
    /// of them wrote would depend on which won.
    pub fn photo_url(&self) -> Option<&str> {
        self.image_url.as_deref().or(match &self.bio {
            BioState::Ready(bio) => bio.image_url.as_deref(),
            _ => None,
        })
    }

    /// What row `index` of the page's one selectable list points at: the top
    /// tracks first, then the album cards under them.
    ///
    /// This is the only place that knows where one section ends and the other
    /// begins, so nothing else has to do the arithmetic.
    pub fn row(&self, index: usize) -> Option<ArtistRow<'_>> {
        let split = self.top.len();
        if index < split {
            return self.top.get(index).map(ArtistRow::Track);
        }
        self.albums.get(index - split).map(ArtistRow::Album)
    }

    pub fn len(&self) -> usize {
        self.top.len() + self.albums.len()
    }

    /// The tabs worth drawing: the groups this artist actually has records in.
    ///
    /// An empty tab is a dead end you can still walk into, so the strip never
    /// offers one. A catalogue with everything in one group yields one tab,
    /// and the page draws no strip at all for it.
    pub fn tabs(&self) -> Vec<ArtistTab> {
        ArtistTab::ALL
            .into_iter()
            .filter(|t| self.albums.items.iter().any(|a| t.holds(a)))
            .collect()
    }

    /// Re-cut [`Self::display`] for the active tab, moving to the first tab
    /// that has records when the active one has none.
    ///
    /// Called whenever either side of that pairing changes: when the catalogue
    /// lands, and when the tab is switched.
    pub fn retab(&mut self) {
        if !self.albums.items.is_empty()
            && !self.albums.items.iter().any(|a| self.tab.holds(a))
            && let Some(&first) = self.tabs().first()
        {
            self.tab = first;
        }
        // Filter first, then sort within it: the tab says which records are on
        // the page, and the sort only orders the ones that are.
        let tab = self.tab;
        self.albums.rebuild_keeping(|a| tab.holds(a));
    }

    pub fn set_tab(&mut self, tab: ArtistTab) {
        self.tab = tab;
        self.retab();
    }
}

/// What an artist-page row points at.
pub enum ArtistRow<'a> {
    Track(&'a Track),
    Album(&'a AlbumItem),
}

/// What the main pane is currently showing.
///
/// There is one pane, so this is the whole screen's navigation model.
/// [`MainView::Home`] is where it starts and where the back stack bottoms out.
#[derive(Debug, Clone)]
pub enum MainView {
    Home,
    /// The playlists page. Carries no data — it renders
    /// [`AppState::playlists`], so a snapshot of it on the back stack can
    /// never go stale.
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
/// This is what keeps the trail a path rather than a log. Navigating to a page
/// already on the path walks back to it, and that comparison is this type. A
/// stack that takes whatever it is handed grows by two a round trip between an
/// album and its artist, leaving `Esc` to walk the loop back out.
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
    /// Keyed by [`radio_key`]. Every scope spells to something different, so
    /// the radio pages are a path you walk into — chart, countries, one
    /// country — rather than one screen that replaces itself.
    Radio(String),
}

pub fn liked_key() -> String {
    "liked".to_string()
}

/// Head of every playlist page's cache key, so building one and reading the id
/// back out of one cannot drift apart.
pub const PLAYLIST_KEY_PREFIX: &str = "playlist:";

pub fn playlist_key(id: &str) -> String {
    format!("{PLAYLIST_KEY_PREFIX}{id}")
}

/// Head of every album page's cache key, the twin of [`PLAYLIST_KEY_PREFIX`].
pub const ALBUM_KEY_PREFIX: &str = "album:";

pub fn album_key(id: &str) -> String {
    format!("{ALBUM_KEY_PREFIX}{id}")
}

/// The bare id at the tail of a Spotify URI.
///
/// One spelling of the rule, because a cached id and the box's URI have to be
/// comparable, and a second copy of it is a second thing to get wrong.
pub fn track_id(uri: &str) -> &str {
    uri.rsplit(':').next().unwrap_or(uri)
}

/// What one playlist holds, as of the snapshot it was read at.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlaylistContents {
    pub snapshot_id: String,
    /// Bare track ids, not URIs: the same set costs about a third as much on
    /// disk, and the `spotify:track:` on the front of every one of them says
    /// nothing a playlist of tracks does not already say.
    pub track_ids: HashSet<String>,
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

/// One thing you were listening to, for the radio deck's back and forward
/// controls.
///
/// `Spotify` carries nothing because it does not have to: a station leaves
/// [`AppState::playback`] and [`AppState::queue`] where they were, so coming
/// back to Spotify is stopping the stream and letting the player go on.
#[derive(Debug, Clone)]
pub enum Listened {
    Station(Box<Station>),
    Spotify,
}

/// How far back the listening history reaches. The view stack keeps its root
/// frame when it overflows; this one has no root, so the oldest entry goes.
const LISTEN_STACK_MAX: usize = 20;

impl Listened {
    /// Whether two entries name the same thing to go back to.
    fn same(&self, other: &Listened) -> bool {
        match (self, other) {
            (Listened::Station(a), Listened::Station(b)) => a.uuid == b.uuid,
            (Listened::Spotify, Listened::Spotify) => true,
            _ => false,
        }
    }
}

/// Add an entry to one end of the listening path, capped.
///
/// Never the same thing twice running, for the reason `AppState::push_view`
/// refuses it: a station restarted, or a seek that lands back where it was,
/// would otherwise put a step in the path that goes nowhere.
fn push_listen(path: &mut Vec<Listened>, entry: Listened) {
    if path.last().is_some_and(|last| last.same(&entry)) {
        return;
    }
    if path.len() >= LISTEN_STACK_MAX {
        path.remove(0);
    }
    path.push(entry);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
}

/// Playlists the add-to-playlist box shows at once. Past this it scrolls,
/// which is what its search field is there to spare you.
///
/// Here rather than beside the drawing, because the key handler has to move
/// the selection by the same window the box shows.
pub const PICKER_ROWS: usize = 10;

/// The open "add to playlist" box: which record it is about, what has been
/// typed into it, and which of its rows are mid-change.
///
/// It is a list of checkboxes rather than a list of destinations — a row says
/// whether the record is on that playlist, and picking one flips it — so the
/// box stays up until it is clicked off. Adding to three playlists is three
/// picks, not three trips through the same control.
#[derive(Debug, Clone)]
pub struct PlaylistPicker {
    /// The record the pick applies to, fixed when the box opens. The deck can
    /// move on under an open box, and the pick has to mean what it meant when
    /// the pointer went down.
    pub uri: String,
    pub query: String,
    /// Row of [`AppState::picker_rows`], not index into
    /// [`AppState::playlists`].
    pub selected: usize,
    pub offset: usize,
    /// Playlists with a change in flight, by id. A set rather than one id,
    /// because the box outlives a pick and several rows can be waiting.
    pub pending: HashSet<String>,
    /// The last change that came back refused. The player view draws no
    /// toasts, so the box is the only surface that can report one.
    pub error: Option<String>,
    /// Identifies this opening, so a result arriving after the box was closed
    /// and opened again cannot act on the new one.
    pub seq: u64,
    /// The rows the box shows, as indices into [`AppState::playlists`], in the
    /// order it shows them — settled when the box opens.
    ///
    /// Fixed rather than derived per frame: the playlists the record is
    /// already on sort to the top, and a list that re-sorts under the pointer
    /// as rows are checked is worse than one that waits until next time.
    pub order: Vec<usize>,
}

/// Which field of the edit box the typing goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Name,
    Description,
}

impl EditField {
    /// The other field, for Tab.
    pub fn other(self) -> Self {
        match self {
            EditField::Name => EditField::Description,
            EditField::Description => EditField::Name,
        }
    }
}

/// What the edit box is about: a playlist that exists, or one it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    /// Rename a playlist that exists.
    Existing(String),
    /// Make one, and put this record on it once it does.
    New { uri: String },
    /// Make one holding what another playlist holds. The rows come off the
    /// page behind the box rather than riding along here — see
    /// `event::copyable_tracks`.
    Copy { source_id: String },
}

/// The open "edit playlist" box: which playlist it is about, and what has
/// been typed into its two fields.
///
/// An overlay rather than a page, for the same reason the add-to-playlist box
/// is one: the edit is about the playlist behind it, and walking away to type
/// would lose the thing being edited.
#[derive(Debug, Clone)]
pub struct PlaylistEdit {
    /// What sending the box does, fixed when it opens.
    pub target: EditTarget,
    pub name: String,
    pub description: String,
    pub field: EditField,
    /// A change is in flight; the box stays up and inert until it lands.
    pub pending: bool,
    /// The last change that came back refused. The box is the only surface
    /// that can report one while it covers the toast.
    pub error: Option<String>,
    /// Identifies this opening, so a result arriving after the box was closed
    /// and opened again cannot act on the new one.
    pub seq: u64,
}

/// A write that takes something away, held until it is asked for a second
/// time.
///
/// An armed prompt rather than a box, because the controls that need one sit a
/// cell from `▶ play` and a box over the page would hide the thing being
/// asked about. The row's own `Enter again to replace…` (see
/// [`LinksRow::confirming`]) proved the shape; this is the same idea where any
/// screen can reach it.
#[derive(Debug, Clone)]
pub struct Confirm {
    /// What the second ask looks like, spelled for the user.
    pub message: String,
    /// Sent when the ask is repeated.
    pub command: crate::app::command::AppCommand,
    /// What repeating it means, so a different key or a click somewhere else
    /// disarms rather than fires.
    pub trigger: ConfirmTrigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmTrigger {
    Key(char),
    /// The control that armed it. Clicking any other disarms, so a pill that
    /// moves under the pointer cannot inherit the arming.
    Click(ConfirmTarget),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmTarget {
    HeaderDelete,
    HeaderUnfollow,
}

/// A table column of clickable text, and how much of each row that text fills.
///
/// `rect` is the column, so a row still comes from the pointer's y; `widths`
/// is what each row on screen actually prints in it, from the top row down.
///
/// A cell padded out to its column width is mostly empty. The pill that lights
/// under the pointer covers the text alone (see `ui::main_pane::cell_spans`),
/// so the target has to be the same run — a click landing in the padding of a
/// short name would otherwise open a page nothing on screen offered.
///
/// The band plus a width per row, rather than a rect per row: one cell of a
/// column holds one link, so the row the pointer is on is the whole of what a
/// click has to resolve, and the band is what the hover pill is measured in.
/// A column whose cell holds *several* links cannot be spelled this way — see
/// [`HitAreas::main_artist_links`], which keeps a rect per name instead.
#[derive(Debug, Default, Clone)]
pub struct TextCol {
    pub rect: Rect,
    pub widths: Vec<u16>,
}

impl TextCol {
    /// Whether `at` is on a row's printed text rather than on its padding.
    pub fn hit(&self, at: Position) -> bool {
        if !self.rect.contains(at) {
            return false;
        }
        let row = (at.y - self.rect.y) as usize;
        let width = self.widths.get(row).copied().unwrap_or(0);
        at.x - self.rect.x < width
    }
}

/// Which store the expanded view reads its picture back out of.
///
/// A click records where the art came from rather than the picture, so a cover
/// that decodes after the block is expanded still lands in it, and a block
/// expanded while its fetch is in flight fills in under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtSource {
    /// The playing record's sleeve, out of [`AppState::cover`]: the one store
    /// not keyed by URL, and the one whose subject changes under the view.
    Playing,
    /// A page's own art, by the URL it decodes from — a browsed sleeve out of
    /// [`AppState::view_cover`], or a portrait or album card out of
    /// [`AppState::page_art`]. `None` where the page named no image at all,
    /// which is the placeholder at any size.
    Page(Option<String>),
}

/// A cover-art block as drawn, and where to find its picture again.
#[derive(Debug, Clone)]
pub struct ArtHit {
    pub rect: Rect,
    pub source: ArtSource,
    /// The seed the block was painted with, so an expanded placeholder keeps
    /// the swatch of the one that was clicked. See `ui::table::draw_art`.
    pub seed: String,
}

/// The article about an artist, open over the page it is about.
///
/// Carries its own wrap because there is no resize event to rebuild one on —
/// `event::handle_event` reads keys and mouse and drops the rest. `ui::bio`
/// compares [`Self::wrapped_w`] against the column it is about to draw into
/// and re-wraps where the terminal has changed under it.
#[derive(Debug, Clone)]
pub struct BioPopup {
    /// The artist the prose belongs to. A page swapped out from under the box
    /// closes it, rather than showing one artist's article under another's
    /// name.
    pub artist_id: String,
    pub name: String,
    pub bio: Arc<ArtistBio>,
    /// The first wrapped line drawn. Lines rather than paragraphs, because
    /// every scroll in the app is a manual line offset — see `ui::clamp_offset`.
    pub offset: usize,
    pub lines: Vec<String>,
    pub wrapped_w: u16,
}

impl BioPopup {
    /// Opened unwrapped: the width belongs to the frame that draws it, and
    /// guessing one here would only be replaced.
    pub fn new(artist_id: String, name: String, bio: Arc<ArtistBio>) -> Self {
        Self {
            artist_id,
            name,
            bio,
            offset: 0,
            lines: Vec::new(),
            wrapped_w: 0,
        }
    }
}

/// A cover-art block expanded to the screen.
///
/// The whole sleeve, as large as the terminal can seat it square: the shorter
/// side decides, so it is never cropped and there is nothing to scroll. See
/// `ui::art_zoom`.
#[derive(Debug, Clone)]
pub struct ArtZoom {
    pub source: ArtSource,
    pub seed: String,
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
    /// the browse view it names on screen. Distinct from [`Self::crumbs`],
    /// which pop the view stack.
    pub close_player: Rect,
    /// Main-pane list rows.
    pub main_list: Rect,
    /// The always-on search row at the top of the browse screen.
    pub search_box: Rect,
    /// The playback status opposite the mark on the nav row — `● STREAMING`,
    /// `● RADIO`, `● LOADING`. Toggles the player view from either screen, so
    /// the mouse has a way in and back out on the row it is already reading.
    /// Empty when nothing is playing, which is when there is no player worth
    /// opening.
    pub status: Rect,
    pub search_tabs: Vec<(Rect, SearchTab)>,
    /// The radio page's tab strip, in the same spirit as [`Self::search_tabs`].
    pub radio_tabs: Vec<(Rect, RadioTab)>,
    /// The artist page's album-group strip. It scrolls with the body it sits
    /// in, so it is recorded only where the line is on screen.
    pub artist_tabs: Vec<(Rect, ArtistTab)>,
    /// The line about the artist in a header band. Clicking it opens the rest.
    /// Empty on every page that drew no such line, so a click can never reach
    /// an article there is nothing to read.
    pub artist_bio: Rect,
    /// The sortable labels of the column header the main pane last drew, each
    /// covering the cells its label occupies and nothing more. Clicking one
    /// sorts by that column. Recorded above [`Self::main_list`], so a header
    /// click and a row click can never resolve to each other.
    pub column_headers: Vec<(Rect, ColKey)>,
    /// The sortable columns of that same header, in the order they are drawn.
    /// `o` walks this, so it covers every table and skips a column the pane
    /// is too narrow to show.
    pub sort_keys: Vec<ColKey>,
    /// The main pane's flat line model: for each content line of
    /// [`Self::main_list`], the row it belongs to, or `None` for a heading, a
    /// column header, or a spacer.
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
    /// The shuffle control on each visible album card, with the row it plays.
    pub card_shuffle: Vec<(Rect, usize)>,
    /// The ▶ Play button in a view's header band; plays the whole context.
    pub header_play_btn: Rect,
    /// The shuffle button in a view's header band; plays the whole context
    /// shuffled.
    pub header_shuffle_btn: Rect,
    /// The save control in a playlist's header band, which reads `☆ save` or
    /// `✕ unfollow` for the two directions of the one write. Empty on a
    /// playlist you own, where the word is delete, and on one the library has
    /// not answered about yet.
    pub header_save_btn: Rect,
    /// The copy control in a playlist's header band. Never empty on a playlist
    /// page: copying one you cannot edit is what makes it yours.
    pub header_copy_btn: Rect,
    /// The delete control in a playlist's header band. Empty on every playlist
    /// you do not own, where the same write is unfollowing.
    pub header_delete_btn: Rect,
    /// The share control in a header band, right of the shuffle button: copies
    /// the Spotify link to the page itself. Empty where the page has no link of
    /// its own to give — see [`AppState::open_page_link`].
    pub header_share_btn: Rect,
    /// The edit control in a playlist's header band. Empty on a playlist you
    /// do not own, which Spotify refuses the change for.
    pub header_edit_btn: Rect,
    /// The credited artists on a header band's own line, on the same terms as
    /// [`Self::main_artist_links`]. Empty on every page that is not about a
    /// record.
    pub header_artist_links: Vec<(Rect, Credit)>,
    /// The Artist column of an album table, on the same terms as
    /// [`Self::main_artist_links`].
    pub album_artist_links: Vec<(Rect, Credit)>,
    /// The two fields of the edit box, and its save control.
    pub edit_name: Rect,
    pub edit_description: Rect,
    pub edit_save: Rect,
    /// The `↻ try again` control on a page whose load failed. Empty on every
    /// page that did not draw one, so a click can never reach a page that has
    /// nothing to retry.
    pub retry_btn: Rect,
    /// The trail on a page's section row, one entry per crumb, in the order
    /// they are drawn. A trail rather than a single `← <page>` pill: a pill
    /// sitting after the section label takes its column from the label's width
    /// and draws the parent to the *right* of the child it points away from.
    /// See [`crate::app::state::Crumb`]. Empty on pages that do not draw one,
    /// and on panes too narrow to hold it.
    ///
    /// Only the crumbs that lead somewhere are recorded: the head of a browse
    /// screen's trail is the page you are already on, and it gets no rect.
    pub crumbs: Vec<(Rect, CrumbTarget)>,
    /// Every artist name printed in the track table's Artist column: one entry
    /// per credit per visible row, each covering that name as printed and
    /// nothing either side of it.
    ///
    /// Per-name rather than per-column, because a record credits several
    /// artists and each leads somewhere different. A rect carries its own row
    /// in its `y`, so resolving a click needs the pointer and nothing else —
    /// the reason [`Self::album_names`] carries its row in the data does not
    /// apply.
    ///
    /// The separator between two names gets no entry, and neither does a
    /// credit Spotify identified by name only: an inert run is spelled as an
    /// absent target, the same way [`Self::station_country`] spells it.
    pub main_artist_links: Vec<(Rect, Credit)>,
    /// Album column of the track table and of the album grid; clicking a row's
    /// name opens the album. The name as printed, like [`Self::main_artist_col`].
    pub main_album_col: TextCol,
    /// Liked column of the track table, the first of the `★ ⧉ +` run that ends
    /// a row; clicking a cell likes or unlikes that row. Each mark carries a
    /// space either side and the run takes all of it, so the padding is the
    /// control here rather than a column's leftover.
    pub main_like_col: Rect,
    /// Share column of the track table, between the other two; clicking a cell
    /// copies that row's Spotify link. The row's answer to the deck's
    /// [`Self::share_btn`].
    pub main_share_col: Rect,
    /// Add column of the track table, beside [`Self::main_like_col`]; clicking
    /// a cell opens the add-to-playlist box for that row. The row's own answer
    /// to the deck's [`Self::add_btn`], which is only ever about the record
    /// that is playing.
    pub main_add_col: Rect,
    /// The credited artists on the now-playing info row, on the same terms as
    /// [`Self::main_artist_links`].
    pub now_artist_links: Vec<(Rect, Credit)>,
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
    /// The `⧉ share` control between [`Self::like_btn`] and [`Self::add_btn`],
    /// which copies the playing record's Spotify link. The first control of the
    /// three to go on a row too narrow for all of them: a record you cannot act
    /// on is worth less than one you cannot link to.
    pub share_btn: Rect,
    /// The `+ add` control beside [`Self::like_btn`], which opens the
    /// add-to-playlist box for the same record. On both screens and in the
    /// same corner, like the control it sits against: a pair that appeared
    /// only after pressing `v` would be a pair that moves.
    ///
    /// One space apart, and that space belongs to this control — a gap
    /// belonging to neither lights under the pointer and reads as a third
    /// control that does nothing.
    pub add_btn: Rect,
    /// The add-to-playlist box's search field. Clicking it keeps the box open,
    /// the way the browse screen's search row does — a click that missed the
    /// caret and closed the box would take the query with it.
    pub picker_field: Rect,
    /// The add-to-playlist box's rows. One line each, so the row is
    /// `offset + (pos.y - rect.y)` against [`AppState::picker_rows`].
    pub picker_list: Rect,
    /// The add-to-playlist box's `+ new playlist` control, which trades that
    /// box for the edit box in its create mode.
    pub picker_new: Rect,
    /// The article box. A click inside it is someone finding their place in
    /// the text; a click outside is the way out.
    pub bio_box: Rect,
    /// The prose inside that box, whose height is the page the scroll keys and
    /// the wheel move by.
    pub bio_body: Rect,
    /// The playing station's country, on the deck's station row. Opens the
    /// directory's page for that country. Empty when the directory gave us no
    /// code to ask by — the name is still printed, it just leads nowhere, the
    /// same rule an artist name without an id follows.
    pub station_country: Rect,
    /// The deck's save control, at the right of the station row: keeps or drops
    /// the *station*, where [`Self::like_btn`] two rows up is about the record
    /// it is playing. Always drawn while a station is on — unlike the liked
    /// control, the answer is in a file of spot's own and is never unknown.
    pub save_station_btn: Rect,
    /// The deck's way back to the broadcast, beside the save control. Drawn
    /// only while the stream has stood down for a record off the station's
    /// own list; empty every other frame.
    pub radio_live_btn: Rect,
    /// The `vol` / `mut` label left of the slider. Clicking it silences the
    /// player and puts the level back, so the four cells the word occupies are
    /// a control of their own rather than the slider's left end.
    pub volume_label: Rect,
    /// Volume slider track only; click position maps linearly to percent.
    pub volume_slider: Rect,
    /// The playing queue's name on the deck's context row. From the bottom
    /// bar it opens the player; from the player it folds the queue under it
    /// away and back, the name being the heading that list hangs from.
    pub queue_name: Rect,
    /// Queue list rows in the player view (inside the borders).
    pub player_queue: Rect,
    /// The queue's liked column, the twin of [`Self::main_like_col`] on the
    /// player's own list.
    pub queue_like_col: Rect,
    /// The queue's artist names, the twin of [`Self::main_artist_links`].
    pub queue_artist_links: Vec<(Rect, Credit)>,
    /// The queue's share column, the twin of [`Self::main_share_col`].
    pub queue_share_col: Rect,
    /// The queue's add column, the twin of [`Self::main_add_col`].
    pub queue_add_col: Rect,
    /// The player view's visualizer band; clicking it toggles playback. The
    /// whole band is live, not just the lit bars — it is the biggest target
    /// on the screen and nothing else is drawn there.
    pub viz: Rect,
    /// Every cover-art block the frame painted, in draw order, each clipped to
    /// what is on screen. Clicking one fills the screen with it.
    ///
    /// The seed and the source ride along rather than the picture itself:
    /// `HitAreas` is rebuilt every frame, and the expanded view has to outlive
    /// that and survive a cover decoding after the click that opened it. The
    /// strings are the same per-frame cost [`Self::main_artist_links`] already
    /// pays.
    pub art_blocks: Vec<ArtHit>,
}

impl HitAreas {
    /// The art block under `at`, if the frame drew one there.
    pub fn art_at(&self, at: Position) -> Option<&ArtHit> {
        self.art_blocks.iter().find(|a| a.rect.contains(at))
    }

    /// The first art block the frame recorded. Every view that draws a sleeve
    /// draws one, so this is the sleeve; the artist page's album cards are the
    /// only surface that records more.
    #[cfg(test)]
    pub fn art_rect(&self) -> Rect {
        self.art_blocks.first().map_or(Rect::default(), |a| a.rect)
    }

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
    /// Transport state, once something has been played this session. What is
    /// playing is [`Self::queue`]'s current track; this is only whether and
    /// where. `None` until the first play, which is when the deck appears.
    pub playback: Option<Playback>,
    pub playlists: Vec<Playlist>,
    /// Why the playlist load came back with nothing — see [`LoadError`]. Here
    /// rather than on a view because the Playlists page carries no state of
    /// its own: it draws [`Self::playlists`], and so does the left rail.
    pub playlists_error: Option<LoadError>,
    /// The Playlists page's order, and the permutation it draws through. Page
    /// state that outlives the page, like [`Self::search_tab`]: see
    /// [`Self::rebuild_playlists_display`] for why it does not live on
    /// [`MainView::Playlists`].
    pub playlists_sort: Sort,
    pub playlists_display: Vec<usize>,
    /// How many times the page on screen has been asked for by hand, counting
    /// only `↻ try again` — 0 until one is pressed, and reset by opening any
    /// page.
    ///
    /// A refusal that comes straight back, which is what a rate limit does,
    /// leaves the spinner up for less than a frame: without a count that
    /// moves, pressing the control looks exactly like pressing nothing.
    pub retries: u32,
    /// Saved ("liked") state by track URI. Absent = not checked yet, so
    /// unknown renders blank rather than as not-liked.
    pub liked: std::collections::HashMap<String, bool>,
    /// Spotify id of the signed-in user, once the playlist load has fetched
    /// it. The Playlists view leaves the Owner column blank for playlists this
    /// matches, so the column says "these are the ones you follow".
    pub me_id: Option<String>,
    /// How much of Spotify is available. Radio does not read it at all.
    pub spotify: SpotifyState,
    /// Set by the Spotify Home row and cleared by the frame loop, which is
    /// the one place that may run the sign-in: the browser flow prints to the
    /// console, so the terminal has to be given back for the length of it.
    pub connect_request: bool,

    /// The station playing, when one is.
    ///
    /// The two engines never *play* at once — `client` stops one before it
    /// starts the other — but this is not the same as the two fields being
    /// mutually exclusive, and reading it that way is a trap. While a station
    /// is on, [`Self::playback`] still holds the last Spotify track, kept on
    /// purpose so stopping the stream puts it straight back rather than after
    /// the next poll. Anything asking "what is the deck about?" must therefore
    /// go through [`Self::deck_track`] and not reach for `playback` directly.
    pub radio: Option<RadioPlayback>,
    /// Stations you kept, loaded from disk at startup. The directory has no
    /// accounts, so this list is the whole of "saved".
    pub radio_favorites: Vec<Station>,
    /// What each saved station is announcing, keyed by [`Station::uuid`].
    ///
    /// Filled only while the saved page is open — see
    /// `Client::refresh_station_now` — and bounded by the size of
    /// [`Self::radio_favorites`], so nothing evicts from it.
    pub radio_now: HashMap<String, StationNow>,
    /// What each station announced this session, oldest first, keyed by
    /// [`Station::uuid`].
    ///
    /// Here rather than on [`RadioPlayback`] because the deck is dropped every
    /// time you change station, and a station you come back to must still know
    /// what it played. Bounded per station by [`HEARD_MAX`].
    pub radio_heard: HashMap<String, Vec<Heard>>,
    /// The player screen's selection and scroll in the station's list.
    ///
    /// Apart from [`Self::queue_index`] and [`Self::queue_list`] because both
    /// lists are alive at once: while a record off a station's list plays, the
    /// queue holds that list and the screen still draws this one.
    pub heard_index: usize,
    pub heard_list: ListState,
    /// What you were listening to before the current station, oldest first.
    ///
    /// Read only while a station is live: the radio deck's `◂◂ previous` walks
    /// it, and the Spotify deck's own previous still means the queue's last
    /// track.
    pub listen_back: Vec<Listened>,
    /// What `◂◂ previous` stepped out of, nearest last.
    pub listen_forward: Vec<Listened>,

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

    /// The level to restore when the `mut` label is clicked again, and the flag
    /// that the label reads `mut` at all. `None` means not muted. Kept here
    /// rather than on [`Playback`] because a station carries its own volume and
    /// the one slider drives both engines.
    pub muted_volume: Option<u8>,

    pub input_mode: InputMode,
    pub input_buffer: String,

    /// The "add to playlist" box, while one is open. It owns the keyboard and
    /// the pointer for as long as it is up.
    pub picker: Option<PlaylistPicker>,
    /// Stamped onto the next box that opens; see [`PlaylistPicker::seq`].
    pub picker_seq: u64,
    /// The "edit playlist" box, while one is open. Owns the keyboard the same
    /// way the add-to-playlist box does.
    pub edit: Option<PlaylistEdit>,
    /// Stamped onto the next edit box that opens; see [`PlaylistEdit::seq`].
    pub edit_seq: u64,
    /// Whether a playlist is in the library, by playlist id. Absent = not
    /// checked yet, the same shape [`Self::liked`] has for tracks: unknown
    /// draws nothing rather than drawing "no".
    pub saved_playlists: HashMap<String, bool>,
    /// What each playlist holds, by playlist id — the marks in the box are
    /// read out of this.
    ///
    /// Spotify has no endpoint that answers "is this record on that playlist",
    /// so the only way to know is to read the playlist. Caching the whole
    /// contents rather than the one answer means that walk happens once per
    /// playlist instead of once per playlist per record, membership is a set
    /// lookup, and the box can sort by it the moment it opens.
    ///
    /// Absent = not walked yet, which the box draws as neither on nor off —
    /// the same rule [`Self::liked`] follows. Persisted across runs and
    /// validated by `snapshot_id`: that hash is Spotify saying the contents
    /// changed, and what was true of the old contents says nothing about the
    /// new.
    pub playlist_tracks: HashMap<String, PlaylistContents>,

    /// Player view (current track + visualizer + queue) replaces the
    /// library/main panes while set.
    pub show_player: bool,

    /// What the Home row's Links entry knows about where a clicked Spotify
    /// link goes. Filled from `crate::protocol` at startup and after the row
    /// acts; empty everywhere else, because no other platform routes a scheme
    /// to an app.
    pub links: LinksRow,
    /// The play order spot owns, installed by every play. The player screen
    /// lists it, and its current track is what the deck describes.
    pub queue: Option<Queue>,
    pub queue_index: usize,
    pub queue_list: ListState,
    /// The queue set aside while a record off a station's list plays, put back
    /// when the station goes on air again or the deck is dropped.
    pub parked: Option<ParkedQueue>,
    /// Whether the player's queue is folded away, leaving the deck above it
    /// with the pane to itself. Toggled by clicking the queue's name in the
    /// player, which is the one place the fold can be seen and undone.
    pub queue_folded: bool,
    /// Bumped by [`Self::set_queue`] and stamped onto the installed queue, so
    /// a background fill can tell the queue it was started for from one that
    /// has replaced it. Independent of `load_generation`: queue fills never
    /// cancel main-view fetches (and vice versa).
    pub queue_generation: u64,
    /// Last click in the queue list, for double-click detection.
    pub last_queue_click: Option<(usize, Instant)>,
    /// PCM tap for the visualizer; replaced with the live tap at startup.
    pub audio_tap: Arc<AudioTap>,
    /// The chosen visualizer mode and every analyzer's rolling state,
    /// persisted across frames for the attack/decay animation.
    pub viz: crate::viz::Viz,
    /// Loudness envelope for the nav row's playing dot. Its own state rather
    /// than a band of [`Self::viz`]: the visualizer is only updated while the
    /// player view is drawn, and the dot is on both screens.
    pub pulse: crate::viz::Pulse,

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
    /// A write waiting to be asked for a second time. See [`Confirm`].
    pub confirm: Option<Confirm>,
    pub show_help: bool,
    /// The cover-art block filling the screen, if one is expanded. See
    /// [`ArtZoom`].
    pub art_zoom: Option<ArtZoom>,
    /// The article about the artist whose page is behind it, if one is open.
    /// See [`BioPopup`].
    pub bio: Option<BioPopup>,
    /// The expanded view's own copy of the cover it is showing, decoded at
    /// [`crate::cover::ZOOM_PX`] rather than the grid the layout's blocks are
    /// happy with.
    ///
    /// A slot of its own, not an entry in [`Self::page_art`]: it holds the same
    /// URL at a different resolution, and one screen-sized cover is worth more
    /// than the eviction it would cause. Dropped when the view closes.
    pub zoom_cover: Option<Arc<crate::cover::Cover>>,
    /// Guards [`Self::zoom_cover`] the way [`Self::cover_generation`] guards
    /// the playing sleeve: expanding a second block while the first is still
    /// in flight must settle on the second.
    pub zoom_cover_generation: u64,
    /// How far the app has got towards replacing itself. `None` on almost
    /// every run: the startup check only fills it when GitHub has something
    /// newer than this build.
    pub update: Option<UpdateState>,
    pub should_quit: bool,
    /// Set with [`Self::should_quit`] to start the new binary as this one
    /// exits. Only the frame loop may honour it, because the terminal has to
    /// be restored before another spot can take it.
    pub restart_request: bool,
}

/// The stages a self-update passes through, each of which the Home row says
/// out loud.
#[derive(Debug, Clone)]
pub enum UpdateState {
    Available(crate::update::Release),
    Installing,
    Installed,
    Failed,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            playback: None,
            radio: None,
            radio_favorites: Vec::new(),
            radio_now: HashMap::new(),
            radio_heard: HashMap::new(),
            heard_index: 0,
            heard_list: ListState::default(),
            listen_back: Vec::new(),
            listen_forward: Vec::new(),
            playlists: Vec::new(),
            playlists_error: None,
            playlists_sort: Sort::default(),
            playlists_display: Vec::new(),
            retries: 0,
            liked: std::collections::HashMap::new(),
            me_id: None,
            spotify: SpotifyState::Off,
            connect_request: false,
            main: MainView::Home,
            view_stack: Vec::new(),
            main_index: 0,
            search_tab: SearchTab::Tracks,
            main_list: ListState::default(),
            hit: HitAreas::default(),
            last_main_click: None,
            mouse_pos: None,
            muted_volume: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            picker: None,
            picker_seq: 0,
            edit: None,
            edit_seq: 0,
            saved_playlists: HashMap::new(),
            playlist_tracks: HashMap::new(),
            show_player: false,
            links: LinksRow::default(),
            queue: None,
            queue_index: 0,
            queue_list: ListState::default(),
            parked: None,
            queue_folded: false,
            queue_generation: 0,
            last_queue_click: None,
            audio_tap: Arc::new(AudioTap::new()),
            viz: crate::viz::Viz::new(),
            pulse: crate::viz::Pulse::new(),
            cover: None,
            cover_generation: 0,
            view_cover: None,
            view_cover_generation: 0,
            page_art: crate::cover::CoverCache::with_capacity(crate::cover::PAGE_ART_MAX),
            load_generation: 0,
            loading: false,
            toast: None,
            confirm: None,
            show_help: false,
            art_zoom: None,
            bio: None,
            zoom_cover: None,
            zoom_cover_generation: 0,
            update: None,
            should_quit: false,
            restart_request: false,
        }
    }

    /// The Home rows that exist right now.
    ///
    /// The picture behind an [`ArtSource`], out of whichever store holds it.
    ///
    /// [`Self::zoom_cover`] is asked first, so the expanded view sharpens the
    /// moment its own decode lands and shows the layout's own small cover until
    /// then.
    ///
    /// A page's cover is matched on the URL it decoded from rather than taken
    /// from the slot: `pop_view` restores a header without issuing a fetch, so
    /// a cover can outlive the page it belongs to, and the URL is what says the
    /// two are about the same record. A mismatch resolves to nothing, which
    /// draws the placeholder — what the block shows while any fetch is in
    /// flight anyway.
    pub fn art_cover(&self, source: &ArtSource) -> Option<Arc<crate::cover::Cover>> {
        let sharp = |url: &str| self.zoom_cover.as_ref().filter(|c| c.url == url).cloned();
        match source {
            ArtSource::Playing => {
                let playing = self.cover.as_ref()?;
                sharp(&playing.url).or_else(|| Some(Arc::clone(playing)))
            }
            ArtSource::Page(url) => {
                let url = url.as_deref()?;
                sharp(url).or_else(|| {
                    self.view_cover
                        .as_ref()
                        .filter(|c| c.url == url)
                        .cloned()
                        .or_else(|| self.page_art.get(url))
                })
            }
        }
    }

    /// The URL the expanded view should decode at [`crate::cover::ZOOM_PX`] for
    /// `source`, if there is one to ask for.
    ///
    /// `None` where the block is a placeholder, or where the playing sleeve has
    /// not decoded yet and the URL it came from is not ours to know — there is
    /// nothing for a sharper copy to be sharper than.
    pub fn art_zoom_url(&self, source: &ArtSource) -> Option<String> {
        match source {
            ArtSource::Playing => self.cover.as_ref().map(|c| c.url.clone()),
            ArtSource::Page(url) => url.clone(),
        }
    }

    /// The library rows are there only while Spotify can play; the Spotify row
    /// is there only while it cannot. Radio is always there — it is the half
    /// of the app that needs no account.
    ///
    /// Discover Weekly is Spotify's, not yours: it is only a row when you
    /// actually follow it. A dim "you don't have this" line would be a worse
    /// screen than three real destinations.
    pub fn home_items(&self) -> Vec<HomeItem> {
        let ready = self.spotify == SpotifyState::Ready;
        HomeItem::ALL
            .iter()
            .copied()
            .filter(|item| match item {
                HomeItem::Update => self.update.is_some(),
                HomeItem::DiscoverWeekly => ready && self.discover_weekly().is_some(),
                HomeItem::LikedSongs | HomeItem::Playlists => ready,
                HomeItem::Spotify => !ready,
                HomeItem::Radio => true,
                // Windows is the only platform that routes a URL scheme to an
                // app, so elsewhere the row would offer something spot cannot
                // do. It stands whether or not Spotify is connected: claiming
                // the scheme needs no account.
                HomeItem::Links => cfg!(windows),
            })
            .collect()
    }

    /// The dim line under a Home row's name. The Spotify, Update and Links
    /// rows are the ones whose lines move; the rest speak for themselves.
    pub fn home_blurb(&self, item: HomeItem) -> &str {
        if item == HomeItem::Links {
            // The armed prompt wins: while the row is waiting for a second
            // press, what it is about to do matters more than what is true.
            return match &self.links.confirming {
                Some(prompt) => prompt,
                None if self.links.status.is_empty() => item.blurb(),
                None => &self.links.status,
            };
        }
        if item == HomeItem::Update {
            return match self.update {
                Some(UpdateState::Available(_)) | None => HomeItem::Update.blurb(),
                Some(UpdateState::Installing) => "downloading…",
                Some(UpdateState::Installed) => "press Enter to restart into it",
                Some(UpdateState::Failed) => "the download failed — see the log",
            };
        }
        match (item, &self.spotify) {
            (HomeItem::Spotify, SpotifyState::Connecting) => "signing in…",
            (HomeItem::Spotify, SpotifyState::Limited(_)) => {
                "signed in, but nothing can be played — radio still can"
            }
            _ => item.blurb(),
        }
    }

    /// The search tabs that exist right now: all five with Spotify, and the
    /// directory's own tab alone without it.
    pub fn search_tabs(&self) -> Vec<SearchTab> {
        match self.spotify {
            SpotifyState::Ready => SearchTab::ALL.to_vec(),
            _ => vec![SearchTab::Stations],
        }
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

    /// The order a box opening now would show its rows in, as indices into
    /// [`Self::playlists`].
    ///
    /// Only the ones you own: Spotify refuses an add to a playlist you merely
    /// follow, and a row that can only fail is worse than no row. Indices
    /// rather than references so a caller holding `&mut self` can act on the
    /// answer.
    ///
    /// The playlists the record is already on come first, each group keeping
    /// the order [`Self::playlists`] is already in. A playlist not walked yet
    /// sorts with the "not on it" group rather than getting a third — it is
    /// about to become one or the other, and a group that exists for a moment
    /// is a group that moves.
    pub fn picker_order(&self, uri: &str) -> Vec<usize> {
        let Some(me) = self.me_id.as_deref() else {
            return Vec::new();
        };
        let owned: Vec<usize> = self
            .playlists
            .iter()
            .enumerate()
            .filter(|(_, p)| p.owner_id == me)
            .map(|(i, _)| i)
            .collect();
        let holds = |i: &usize| {
            self.playlist_tracks
                .get(&self.playlists[*i].id)
                .is_some_and(|c| c.track_ids.contains(track_id(uri)))
        };
        let (on, off): (Vec<usize>, Vec<usize>) = owned.into_iter().partition(|i| holds(i));
        on.into_iter().chain(off).collect()
    }

    /// The rows the open box offers, as indices into [`Self::playlists`].
    ///
    /// The order it opened with, cut by whatever has been typed into it —
    /// which is what makes that order hold for the life of the box. One
    /// function rather than two, so the box's rows and the clicks against
    /// them cannot disagree about what row 3 is.
    pub fn picker_rows(&self) -> Vec<usize> {
        let Some(picker) = self.picker.as_ref() else {
            return Vec::new();
        };
        let query = picker.query.trim().to_lowercase();
        picker
            .order
            .iter()
            .copied()
            // A reloaded `playlists` leaves these indices pointing at nothing.
            .filter(|i| *i < self.playlists.len())
            .filter(|i| query.is_empty() || self.playlists[*i].name.to_lowercase().contains(&query))
            .collect()
    }

    /// The rows the open box is actually showing, as indices into
    /// [`Self::playlists`].
    ///
    /// The window [`Self::picker_rows`] is scrolled to. What the `checking…`
    /// line answers for: a playlist off the bottom of the box may still be
    /// unwalked, and nothing on screen would show it.
    pub fn picker_visible(&self) -> Vec<usize> {
        let Some(picker) = self.picker.as_ref() else {
            return Vec::new();
        };
        self.picker_rows()
            .into_iter()
            .skip(picker.offset)
            .take(PICKER_ROWS)
            .collect()
    }

    /// Whether the open box's record is on `playlist_id`, or `None` while that
    /// playlist has not been walked yet.
    pub fn picker_has(&self, playlist_id: &str) -> Option<bool> {
        let uri = &self.picker.as_ref()?.uri;
        let contents = self.playlist_tracks.get(playlist_id)?;
        Some(contents.track_ids.contains(track_id(uri)))
    }

    /// The open add-to-playlist box, but only if it is the opening `seq`
    /// identifies.
    ///
    /// What an answer from the client has to go through: the box can be closed
    /// and opened again on another record while a request is out, and a late
    /// answer must not close, clear or blame the new one.
    pub fn picker_for(&mut self, seq: u64) -> Option<&mut PlaylistPicker> {
        self.picker.as_mut().filter(|p| p.seq == seq)
    }

    /// The right-aligned tail of a Home row: how much the destination holds.
    ///
    /// Liked Songs has none — its length is not known until it is opened, and
    /// a number that appears a second later reads as a glitch.
    pub fn home_count(&self, item: HomeItem) -> String {
        let plural = |n: u32, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        match item {
            // The tail carries the version, so the name can stay a constant
            // and the row still says which release it offers.
            HomeItem::Update => match &self.update {
                Some(UpdateState::Available(release)) => release.tag.clone(),
                _ => String::new(),
            },
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
            HomeItem::Spotify => match &self.spotify {
                SpotifyState::Off => "not connected".to_string(),
                SpotifyState::Connecting => "connecting…".to_string(),
                SpotifyState::Limited(reason) => reason.clone(),
                SpotifyState::Ready => String::new(),
            },
            // Two words for a state the blurb already says in a sentence, and
            // the tail is where the eye goes for on or off.
            HomeItem::Links if self.links.in_force => "on".to_string(),
            HomeItem::Links => "off".to_string(),
        }
    }

    /// Number of rows in the main pane for the current view/tab.
    pub fn main_len(&self) -> usize {
        match &self.main {
            MainView::Home => self.home_items().len(),
            MainView::Playlists => self.playlists_display.len(),
            MainView::Tracks(list) => list.len(),
            MainView::Search(results) => match self.search_tab {
                SearchTab::Tracks => results.tracks.len(),
                SearchTab::Albums => results.albums.len(),
                SearchTab::Artists => results.artists.len(),
                SearchTab::Playlists => results.playlists.len(),
                SearchTab::Stations => results.stations.len(),
            },
            MainView::Artist(v) => v.len(),
            MainView::Radio(v) => v.rows.len(),
        }
    }

    /// The playlist on row `row` of the Playlists page, through its display
    /// order.
    pub fn playlist_row(&self, row: usize) -> Option<&Playlist> {
        self.playlists.get(*self.playlists_display.get(row)?)
    }

    /// Number of rows in the player view's queue list.
    pub fn queue_len(&self) -> usize {
        self.queue.as_ref().map_or(0, |q| q.len())
    }

    /// What the station on the deck has played, oldest first.
    pub fn heard(&self) -> &[Heard] {
        self.radio
            .as_ref()
            .and_then(|r| self.radio_heard.get(&r.station.uuid))
            .map_or(&[], |rows| rows.as_slice())
    }

    /// The record on one row of the station's list.
    pub fn heard_track(&self, row: usize) -> Option<&Track> {
        self.heard().get(row)?.track()
    }

    /// Every record of the station's list Spotify identified, in list order.
    ///
    /// The play order a row's Enter installs, and the order the `▶` marker is
    /// read back through, so both come from one definition.
    pub fn heard_tracks(&self) -> Vec<Track> {
        self.heard()
            .iter()
            .filter_map(|h| h.track().cloned())
            .collect()
    }

    /// Where a row of the station's list sits among the records that can be
    /// played, or [`None`] for a row Spotify has nothing for.
    pub fn heard_play_position(&self, row: usize) -> Option<usize> {
        self.heard_track(row)?;
        Some(
            self.heard()[..row]
                .iter()
                .filter(|h| h.track().is_some())
                .count(),
        )
    }

    /// Note what the station just announced.
    ///
    /// The trim and the tail-follow are done together because both move the
    /// scroll offset, and neither is right without the other: a trim that left
    /// the offset alone would slide the rows under the reader, and a follow
    /// that ran before the trim would aim at a row about to be dropped.
    pub fn push_heard(&mut self, announced: String) {
        let Some(uuid) = self.radio.as_ref().map(|r| r.station.uuid.clone()) else {
            return;
        };
        let height = self.hit.player_queue.height as usize;
        let offset = self.heard_list.offset();
        let was_at_bottom = height == 0 || offset + height >= self.heard().len();

        let rows = self.radio_heard.entry(uuid).or_default();
        rows.push(Heard::new(announced));
        let dropped = rows.len().saturating_sub(HEARD_MAX);
        if dropped > 0 {
            rows.drain(..dropped);
        }
        let len = rows.len();

        self.heard_index = self.heard_index.saturating_sub(dropped).min(len - 1);
        let offset = offset.saturating_sub(dropped);
        *self.heard_list.offset_mut() = match was_at_bottom && height > 0 {
            true => len.saturating_sub(height),
            false => offset,
        };
    }

    /// Take an announcement the station's newest row already carries.
    ///
    /// Tuning a station in starts its deck blank and forgets what was last
    /// announced, so a station still playing the record it was playing when
    /// you left it announces that record again. Without this the list would
    /// grow a second row for it every time you went back on air, and the
    /// lookup would be spent a second time on an answer already in hand.
    pub fn adopt_newest_heard(&mut self, uuid: &str, announced: &str) -> bool {
        let Some(matched) = self
            .radio_heard
            .get(uuid)
            .and_then(|rows| rows.last())
            .filter(|row| row.announced == announced)
            .map(|row| row.matched.clone())
        else {
            return false;
        };
        if let Some(r) = self.radio.as_mut() {
            r.matched = matched;
        }
        true
    }

    /// Note that a search is out for the row an announcement made.
    ///
    /// Separate from [`Self::set_heard_match`], which only settles a row that
    /// is already searching: this is the one write that starts it.
    pub fn set_heard_searching(&mut self, uuid: &str, announced: &str) {
        if let Some(row) = self
            .radio_heard
            .get_mut(uuid)
            .and_then(|rows| rows.iter_mut().rev().find(|h| h.announced == announced))
        {
            row.matched = RadioMatch::Searching;
        }
    }

    /// Write what Spotify made of an announcement into the row it was for.
    ///
    /// Found from the back rather than by a remembered index, so a trim
    /// between the question and the answer cannot make it write the wrong row.
    /// The `Searching` guard is what stops an answer for the first `A` of an
    /// `A → B → A` run landing on the second.
    pub fn set_heard_match(&mut self, uuid: &str, announced: &str, matched: RadioMatch) {
        let Some(rows) = self.radio_heard.get_mut(uuid) else {
            return;
        };
        if let Some(row) = rows
            .iter_mut()
            .rev()
            .find(|h| h.announced == announced && matches!(h.matched, RadioMatch::Searching))
        {
            row.matched = matched;
        }
    }

    /// Put the station's list at its newest row.
    ///
    /// What a tune-in opens on: the record on air is the one the listener came
    /// for, and the tail-follow in [`Self::push_heard`] then keeps it in view.
    pub fn heard_to_newest(&mut self) {
        let len = self.heard().len();
        let height = self.hit.player_queue.height as usize;
        self.heard_index = len.saturating_sub(1);
        *self.heard_list.offset_mut() = len.saturating_sub(height.max(1));
    }

    /// Rows in the list the player screen is showing.
    pub fn player_rows(&self) -> usize {
        match self.radio.is_some() {
            true => self.heard().len(),
            false => self.queue_len(),
        }
    }

    /// The record on one row of the list the player screen is showing.
    pub fn player_row_track(&self, row: usize) -> Option<&Track> {
        match self.radio.is_some() {
            true => self.heard_track(row),
            false => self.queue.as_ref()?.rows().get(row),
        }
    }

    /// The selection and scroll of the list the player screen is showing.
    ///
    /// One accessor rather than a branch at every call site, so the movement,
    /// click and wheel handlers each have a single definition that serves both
    /// lists.
    pub fn player_list(&mut self) -> PlayerList<'_> {
        let len = self.player_rows();
        let height = self.hit.player_queue.height;
        match self.radio.is_some() {
            true => PlayerList {
                len,
                height,
                index: &mut self.heard_index,
                list: &mut self.heard_list,
            },
            false => PlayerList {
                len,
                height,
                index: &mut self.queue_index,
                list: &mut self.queue_list,
            },
        }
    }

    /// Set the queue aside, whole, while a record off a station's list plays.
    pub fn park_queue(&mut self) {
        let Some(queue) = self.queue.take() else {
            return;
        };
        self.parked = Some(ParkedQueue {
            queue,
            index: self.queue_index,
            offset: self.queue_list.offset(),
        });
    }

    /// Put a parked queue back, with the selection and scroll it had.
    ///
    /// Assigned directly rather than through [`Self::set_queue`], which resets
    /// both — the point of parking is that nothing about the queue changed.
    pub fn unpark_queue(&mut self) {
        let Some(parked) = self.parked.take() else {
            return;
        };
        self.queue = Some(parked.queue);
        self.queue_index = parked.index;
        *self.queue_list.offset_mut() = parked.offset;
    }

    /// Install a queue, resetting the player list's selection and scroll and
    /// stamping the queue with a fresh generation — see
    /// [`Self::queue_generation`].
    pub fn set_queue(&mut self, queue: Option<Queue>) {
        self.queue_generation += 1;
        self.queue = queue.map(|mut q| {
            q.generation = self.queue_generation;
            q
        });
        self.queue_index = self.queue.as_ref().map_or(0, |q| q.index());
        *self.queue_list.offset_mut() = 0;
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// What the app has to say, in the one place every surface draws it.
    ///
    /// An armed write outranks a toast. A toast reports what already happened
    /// and expires on its own; a prompt is a question, and one covered by the
    /// answer to an earlier question is one that gets missed.
    pub fn message(&self) -> Option<&str> {
        match &self.confirm {
            Some(confirm) => Some(&confirm.message),
            None => self.toast.as_ref().map(|(msg, _)| msg.as_str()),
        }
    }

    /// Hold a write until the same ask comes again. See [`Confirm`].
    pub fn arm(
        &mut self,
        trigger: ConfirmTrigger,
        message: impl Into<String>,
        command: crate::app::command::AppCommand,
    ) {
        self.confirm = Some(Confirm {
            message: message.into(),
            command,
            trigger,
        });
    }

    /// The command an armed write is waiting to send, if this ask is the one
    /// it is waiting for. Takes it either way: an ask that does not match is
    /// the user doing something else, and the prompt has to stop standing over
    /// whatever that turns out to be.
    pub fn take_armed(
        &mut self,
        trigger: ConfirmTrigger,
    ) -> Option<crate::app::command::AppCommand> {
        let armed = self.confirm.take()?;
        (armed.trigger == trigger).then_some(armed.command)
    }

    /// Whether the page on screen is still being fetched.
    ///
    /// The frame loop reads this to hold the fast tick over the pane's
    /// spinner. Deliberately not the header's `loading`, which answers the
    /// different question of whether the *trail* should say so — a page whose
    /// pane spins says it there instead, and the two must be free to disagree.
    pub fn main_loading(&self) -> bool {
        match &self.main {
            MainView::Home => false,
            MainView::Playlists => self.loading && self.playlists.is_empty(),
            MainView::Tracks(list) => self.loading || list.loading,
            MainView::Search(results) => match self.search_tab {
                SearchTab::Stations => results.stations_loading,
                _ => self.loading,
            },
            MainView::Artist(v) => v.loading,
            MainView::Radio(v) => v.loading && v.rows.is_empty(),
        }
    }

    /// Put the page on screen back in flight, clearing the refusal it is
    /// showing.
    ///
    /// Done on the press rather than left to the client, which installs a
    /// fresh view a beat later: a refusal that comes straight back — which is
    /// what a rate limit does — can land before the next frame is drawn, so
    /// waiting for the client would mean the spinner was never on screen at
    /// all and the control looked inert.
    pub fn mark_reloading(&mut self) {
        let tab = self.search_tab;
        // Split borrow: the view is written while the two page-wide flags
        // beside it are.
        let AppState {
            main,
            loading,
            playlists_error,
            ..
        } = self;
        match main {
            MainView::Playlists => {
                *playlists_error = None;
                *loading = true;
            }
            MainView::Tracks(list) => {
                list.error = None;
                list.loading = true;
            }
            MainView::Artist(v) => {
                v.error = None;
                v.loading = true;
            }
            MainView::Radio(v) => {
                v.error = None;
                v.loading = true;
            }
            MainView::Search(results) => match tab {
                SearchTab::Stations => {
                    results.stations_error = None;
                    results.stations_loading = true;
                }
                _ => {
                    results.error = None;
                    *loading = true;
                }
            },
            MainView::Home => {}
        }
    }

    /// The record the deck is currently about.
    ///
    /// Radio first, and radio *exclusively* while a station is playing. The
    /// kept Spotify queue behind it is not making any sound (see
    /// [`Self::radio`]), so a deck control that fell through to it would like,
    /// or open, a record the user last heard half an hour ago. That is why a
    /// station with no match answers `None` here rather than deferring: the
    /// honest answer is that the deck is about something Spotify has no page
    /// for, and every caller renders that better than it renders a lie.
    ///
    /// Off radio it is the queue's current track — spot owns the play order,
    /// so what the queue points at *is* what is playing.
    pub fn deck_track(&self) -> Option<&Track> {
        // Off air the deck is about a record from the station's own list, and
        // that record is the queue's current track like any other.
        if let Some(r) = self.radio.as_ref().filter(|r| !r.off_air) {
            return r.matched_track();
        }
        self.playback.as_ref()?;
        self.queue.as_ref().and_then(|q| q.current())
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
        // it does every further click pushes the *unchanged* current view, so
        // four clicks on a Home row would leave four copies of Home. The
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

    /// Drop the path back to Home.
    ///
    /// Home stays on it because it is the bottom of every path and the one
    /// page back must always reach. Its own selection and scroll come with it,
    /// so the frame restores the row you left rather than the top of the list.
    pub fn reset_to_home(&mut self) {
        if matches!(self.main, MainView::Home) {
            self.view_stack.clear();
            self.push_view();
            return;
        }
        // A path with no Home frame on it belongs to a session that opened
        // straight onto an album from the now-playing bar.
        let home = self
            .view_stack
            .iter()
            .find(|snap| matches!(snap.view, MainView::Home))
            .cloned()
            .unwrap_or_else(|| ViewSnapshot {
                view: MainView::Home,
                main_index: 0,
                offset: 0,
                search_tab: self.search_tab,
            });
        self.view_stack = vec![home];
    }

    /// What is playing now, as a history entry.
    ///
    /// A station wins over Spotify for the reason it wins everywhere else:
    /// while a station is on, the Spotify snapshot is kept but silent.
    fn now_listening(&self) -> Option<Listened> {
        match (&self.radio, &self.playback) {
            (Some(r), _) => Some(Listened::Station(Box::new(r.station.clone()))),
            (None, Some(_)) => Some(Listened::Spotify),
            (None, None) => None,
        }
    }

    /// Remember what is playing, because something else is about to.
    ///
    /// Closes the path forward, the way opening a page does on the view
    /// stack: a new destination is not the one `next ▸▸` was holding.
    pub fn record_listen(&mut self) {
        let Some(now) = self.now_listening() else {
            return;
        };
        push_listen(&mut self.listen_back, now);
        self.listen_forward.clear();
    }

    /// Step back one entry, handing what is playing to the forward path.
    pub fn step_back_listen(&mut self) -> Option<Listened> {
        let previous = self.listen_back.pop()?;
        if let Some(now) = self.now_listening() {
            push_listen(&mut self.listen_forward, now);
        }
        Some(previous)
    }

    /// Step forward one entry, handing what is playing back to the back path.
    pub fn step_forward_listen(&mut self) -> Option<Listened> {
        let next = self.listen_forward.pop()?;
        if let Some(now) = self.now_listening() {
            push_listen(&mut self.listen_back, now);
        }
        Some(next)
    }

    /// Where the current page's back control leads, or `None` when it should
    /// not be drawn.
    ///
    /// History first: back means "the page I came from", labeled with that
    /// page's name. An album page reached with nothing behind it (the very
    /// first thing opened in a session, from the now-playing bar) falls back
    /// to going *up* to the album's artist, whom the header names — so the
    /// control is there from the moment the page opens, before its first page
    /// of tracks has landed.
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
        list.header.credits.iter().find_map(|c| {
            let id = c.id.clone()?;
            (!c.name.is_empty()).then(|| BackTarget::Artist {
                id,
                name: c.name.clone(),
            })
        })
    }

    /// The page's ancestors, oldest first, then the page itself.
    ///
    /// This is what the section row draws. The stack holds the whole chain, so
    /// the row shows all of it: a single `← <page>` pill can only name one
    /// step, leaving `Home › Muse › Black Holes` to say `← Muse` and the rest
    /// of the path to be remembered.
    ///
    /// Home is not on it, at either end. The `♫ spot` mark sits at the head of
    /// the same row and *is* the way home, so a `HOME ›` in front of every
    /// path would be a second control saying what the first one says — and on
    /// Home itself a crumb naming the page the mark points at. This drops a
    /// step from what is *drawn*, not from the stack: the depths of the
    /// remaining crumbs are untouched, so they pop back to the same places.
    pub fn trail(&self) -> Vec<Crumb> {
        let mut out: Vec<Crumb> = self
            .view_stack
            .iter()
            .enumerate()
            .filter(|(_, snap)| !matches!(snap.view, MainView::Home))
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
        // Home draws no head either, so its row is the mark and nothing else.
        if !matches!(self.main, MainView::Home) {
            out.push(Crumb {
                label: view_title(&self.main),
                target: CrumbTarget::Current,
            });
        }
        out
    }

    /// Restore the snapshot at `depth`, discarding everything above it.
    ///
    /// Clicking a crumb is a jump, not a run of single steps: going from an
    /// album back to Home restores Home's own scroll and selection rather than
    /// the ones the pages in between hold. Returns false when the depth is not
    /// on the stack.
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

    /// Rebuild the current view's display order for its sort, keeping the
    /// selection anchored to the same row where possible.
    ///
    /// Anchored by [`Sortable::identity`] rather than by index: a re-sort
    /// moves every row, and an index kept across one points at whatever
    /// happens to have landed there.
    pub fn resort_main(&mut self) {
        let index = self.main_index;
        let tab = self.search_tab;
        // Ahead of the match, which borrows `main` for its whole length while
        // this arm reads two other fields of the same state.
        if matches!(self.main, MainView::Playlists) {
            let was = self
                .playlists_display
                .get(index)
                .and_then(|&i| self.playlists.get(i))
                .map(|p| p.id.clone());
            self.rebuild_playlists_display();
            self.main_index = was
                .and_then(|id| {
                    self.playlists_display
                        .iter()
                        .position(|&i| self.playlists[i].id == id)
                })
                .unwrap_or_else(|| index.min(self.playlists_display.len().saturating_sub(1)));
            return;
        }
        self.main_index = match &mut self.main {
            MainView::Tracks(list) => anchored(&mut list.rows, index),
            MainView::Radio(v) => anchored(&mut v.rows, index),
            MainView::Search(r) => match tab {
                SearchTab::Tracks => anchored(&mut r.tracks, index),
                SearchTab::Albums => anchored(&mut r.albums, index),
                SearchTab::Artists => anchored(&mut r.artists, index),
                SearchTab::Playlists => anchored(&mut r.playlists, index),
                SearchTab::Stations => anchored(&mut r.stations, index),
            },
            // The catalogue sits under the top tracks, so a row of it is
            // offset by the block above; the tab filter re-applies with the
            // sort.
            MainView::Artist(v) => {
                // Both lists are rebuilt, whichever the selection is in: the
                // page draws them one under the other, and leaving the other
                // in its old order would put a row where the flat index model
                // says something else is.
                let tab = v.tab;
                let split = v.top.len();
                if index < split {
                    let row = anchored(&mut v.top.rows, index);
                    v.albums.rebuild_keeping(|a| tab.holds(a));
                    row
                } else {
                    let row = anchored_keeping(&mut v.albums, index - split, |a| tab.holds(a));
                    v.top.rebuild();
                    // Sorting the top tracks can only reorder them, never
                    // change how many there are, so the split holds.
                    split + row
                }
            }
            // Resolved above, and Home is navigation rather than a table.
            MainView::Playlists | MainView::Home => index,
        };
    }

    /// Re-cut the Playlists page's display order.
    ///
    /// The page's sort lives here rather than on [`MainView::Playlists`],
    /// which carries no data at all — that is what stops a snapshot of it on
    /// the back stack going stale. [`Self::playlists`] itself stays canonical
    /// and unsorted: the add-to-playlist picker freezes indices into it, and a
    /// box that reshuffled under a sort applied on another page would be a
    /// different list each time it opened.
    pub fn rebuild_playlists_display(&mut self) {
        self.playlists_display = sorted_display(&self.playlists, self.playlists_sort, |_| true);
    }

    /// Install the library, re-cutting the Playlists page's order with it.
    ///
    /// The one way in, so the permutation can never be left over a list it is
    /// not about — a stale one would drop rows off the page.
    pub fn set_playlists(&mut self, playlists: Vec<Playlist>) {
        // Everything the library lists is in the library by definition, so the
        // header's save control knows its answer without a probe. Only a
        // playlist reached from a search has to ask.
        for p in &playlists {
            self.saved_playlists.insert(p.id.clone(), true);
        }
        self.playlists = playlists;
        self.rebuild_playlists_display();
    }

    /// The playlist the open page is about, when it is about one.
    ///
    /// The page carries its identity in `cache_key` rather than in the header,
    /// which is display text — see [`playlist_key`].
    pub fn open_playlist_id(&self) -> Option<&str> {
        match &self.main {
            MainView::Tracks(list) if list.kind == TrackListKind::Playlist => list
                .cache_key
                .as_deref()
                .and_then(|k| k.strip_prefix(PLAYLIST_KEY_PREFIX)),
            _ => None,
        }
    }

    /// The Spotify link to the open page itself, for the header's share
    /// control.
    ///
    /// Liked Songs is deliberately absent. Its only link is
    /// `/collection/tracks`, which resolves to whoever opens it rather than to
    /// what is on screen — a link that shows the reader their own library is
    /// worse than no control at all.
    pub fn open_page_link(&self) -> Option<Link> {
        match &self.main {
            MainView::Artist(v) => Some(Link::Artist(v.id.clone())),
            MainView::Tracks(list) => {
                let key = list.cache_key.as_deref()?;
                match list.kind {
                    TrackListKind::Playlist => Some(Link::Playlist(
                        key.strip_prefix(PLAYLIST_KEY_PREFIX)?.into(),
                    )),
                    TrackListKind::Album => {
                        Some(Link::Album(key.strip_prefix(ALBUM_KEY_PREFIX)?.into()))
                    }
                    TrackListKind::LikedSongs => None,
                }
            }
            _ => None,
        }
    }

    /// Whether the open playlist page is one the signed-in user owns.
    ///
    /// Judged on the owner id rather than the display name, which need not be
    /// unique. Unknown until both ids are in hand, and unknown means no: the
    /// control this gates would be refused by Spotify anyway.
    pub fn owns_open_playlist(&self) -> bool {
        let Some(me) = self.me_id.as_deref() else {
            return false;
        };
        match &self.main {
            MainView::Tracks(list) if list.kind == TrackListKind::Playlist => {
                !list.header.owner_id.is_empty() && list.header.owner_id == me
            }
            _ => false,
        }
    }
}

pub fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(id: &str, name: &str, owner_id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: name.into(),
            track_count: 1,
            owner: owner_id.into(),
            owner_id: owner_id.into(),
            snapshot_id: "s".into(),
            cover_url: None,
            public: None,
            collaborative: false,
        }
    }

    fn picking(query: &str) -> AppState {
        let mut st = AppState::new();
        st.me_id = Some("me".into());
        st.set_playlists(vec![
            owned("a", "Late Night", "me"),
            owned("b", "Someone Else's", "them"),
            owned("c", "late lunch", "me"),
        ]);
        let uri = "spotify:track:x".to_string();
        st.picker = Some(PlaylistPicker {
            order: st.picker_order(&uri),
            uri,
            query: query.into(),
            selected: 0,
            offset: 0,
            pending: Default::default(),
            error: None,
            seq: 1,
        });
        st
    }

    fn holding(snapshot: &str, ids: &[&str]) -> PlaylistContents {
        PlaylistContents {
            snapshot_id: snapshot.into(),
            track_ids: ids.iter().map(|id| (*id).to_string()).collect(),
        }
    }

    /// Spotify refuses an add to a playlist you only follow, so the box never
    /// offers one: a row that can only fail is worse than no row.
    #[test]
    fn the_picker_offers_only_playlists_you_own() {
        let st = picking("");
        assert_eq!(st.picker_rows(), vec![0, 2]);
    }

    /// The query cuts the rows without regard to case, the way any other
    /// search in the app does.
    #[test]
    fn the_pickers_query_ignores_case() {
        assert_eq!(picking("LATE").picker_rows(), vec![0, 2]);
        assert_eq!(picking("lunch").picker_rows(), vec![2]);
        assert!(picking("nothing here").picker_rows().is_empty());
    }

    /// A box that was closed and opened again on another record is a different
    /// box, and an answer to the first one has nothing to say to it.
    #[test]
    fn a_late_answer_cannot_act_on_a_reopened_box() {
        let mut st = picking("");
        assert!(st.picker_for(1).is_some());
        st.picker = None;
        assert!(st.picker_for(1).is_none());
        st.picker_seq = 2;
        st.picker = Some(PlaylistPicker {
            seq: 2,
            ..picking("").picker.unwrap()
        });
        assert!(st.picker_for(1).is_none());
        assert!(st.picker_for(2).is_some());
    }

    /// Who you are is not known until the playlist load has been round the
    /// account endpoint, and until then nothing can be said to be yours.
    #[test]
    fn the_picker_offers_nothing_before_the_account_is_known() {
        let mut st = picking("");
        st.me_id = None;
        assert!(st.picker_order("spotify:track:x").is_empty());
    }

    /// The marks come out of the cached contents, so one walk of a playlist
    /// answers for every record rather than for the one it was opened on.
    #[test]
    fn a_cached_playlist_answers_for_the_record_in_the_box() {
        let mut st = picking("");
        st.playlist_tracks.insert("a".into(), holding("s", &["x"]));
        st.playlist_tracks.insert("c".into(), holding("s", &["y"]));
        assert_eq!(st.picker_has("a"), Some(true));
        assert_eq!(st.picker_has("c"), Some(false));
    }

    /// A playlist nothing has walked yet is neither on nor off, which is the
    /// third state the box draws as `·`.
    #[test]
    fn an_unwalked_playlist_stays_unknown() {
        let st = picking("");
        assert_eq!(st.picker_has("a"), None);
    }

    /// The URI in the box and the ids on disk have to be comparable, and this
    /// is the one place that rule is written.
    #[test]
    fn a_track_id_is_the_tail_of_its_uri() {
        assert_eq!(track_id("spotify:track:abc"), "abc");
        assert_eq!(track_id("abc"), "abc");
    }

    /// The playlists the record is already on open at the top, and the ones
    /// nothing has walked sit with the rest rather than in a group of their
    /// own.
    #[test]
    fn the_box_opens_with_the_playlists_youre_on_first() {
        let mut st = picking("");
        st.playlist_tracks.insert("c".into(), holding("s", &["x"]));
        assert_eq!(st.picker_order("spotify:track:x"), vec![2, 0]);
    }

    /// The order is settled when the box opens: checking a row must not move
    /// it out from under the pointer.
    #[test]
    fn the_order_holds_while_the_query_cuts_it() {
        let mut st = picking("");
        st.playlist_tracks.insert("c".into(), holding("s", &["x"]));
        assert_eq!(st.picker_rows(), vec![0, 2], "the order it opened with");
        st.picker.as_mut().unwrap().query = "late".into();
        assert_eq!(st.picker_rows(), vec![0, 2]);
    }

    /// `R` replaces the list the box's indices point into, so any that now
    /// point past the end are dropped rather than panicking a draw.
    #[test]
    fn rows_past_a_replaced_playlist_list_are_dropped() {
        let mut st = picking("");
        st.playlists.truncate(1);
        assert_eq!(st.picker_rows(), vec![0]);
    }

    /// Without an account spot is a radio player, and it says so: one Home row
    /// to listen with and one to sign in with, and the directory's own search
    /// tab alone. Nothing on the screen promises what cannot be done.
    #[test]
    fn home_and_search_offer_only_radio_until_spotify_is_ready() {
        let mut st = AppState::new();
        st.set_playlists(vec![Playlist {
            id: "dw".into(),
            name: "Discover Weekly".into(),
            track_count: 30,
            owner: "Spotify".into(),
            owner_id: SPOTIFY_OWNER.into(),
            snapshot_id: "s".into(),
            cover_url: None,
            public: None,
            collaborative: false,
        }]);

        for state in [
            SpotifyState::Off,
            SpotifyState::Connecting,
            SpotifyState::Limited("no Premium".into()),
        ] {
            st.spotify = state.clone();
            assert_eq!(
                destinations(&st),
                vec![HomeItem::Radio, HomeItem::Spotify],
                "{state:?}"
            );
            assert_eq!(st.search_tabs(), vec![SearchTab::Stations], "{state:?}");
        }

        st.spotify = SpotifyState::Ready;
        assert_eq!(
            destinations(&st),
            vec![
                HomeItem::LikedSongs,
                HomeItem::DiscoverWeekly,
                HomeItem::Playlists,
                HomeItem::Radio
            ]
        );
        assert_eq!(st.search_tabs(), SearchTab::ALL.to_vec());
    }

    /// Home's rows without the Links entry, which turns on the platform rather
    /// than on the account and is a control rather than a destination.
    fn destinations(st: &AppState) -> Vec<HomeItem> {
        st.home_items()
            .into_iter()
            .filter(|item| *item != HomeItem::Links)
            .collect()
    }

    /// The Links row stands whatever Spotify is doing — claiming the scheme
    /// needs no account — and it reads as off until something says otherwise,
    /// which is what keeps a fresh copy of spot from implying a claim it has
    /// not made.
    #[test]
    fn the_links_row_reports_what_was_read_rather_than_assuming() {
        let mut st = AppState::new();
        assert_eq!(st.home_items().contains(&HomeItem::Links), cfg!(windows));
        st.spotify = SpotifyState::Ready;
        assert_eq!(st.home_items().contains(&HomeItem::Links), cfg!(windows));

        assert_eq!(st.home_count(HomeItem::Links), "off");
        assert_eq!(st.home_blurb(HomeItem::Links), HomeItem::Links.blurb());

        st.links.in_force = true;
        st.links.status = "Spotify links open in spot".to_string();
        assert_eq!(st.home_count(HomeItem::Links), "on");
        assert_eq!(st.home_blurb(HomeItem::Links), "Spotify links open in spot");

        // While the row waits for the second press, what it is about to do
        // wins over what is true.
        st.links.confirming = Some("Enter again to replace Spotify".to_string());
        assert!(st.home_blurb(HomeItem::Links).starts_with("Enter again"));
    }

    /// The Spotify row's tail and line report how far the connection got, so
    /// an account that cannot stream says why rather than leaving Home short.
    #[test]
    fn the_spotify_row_reports_the_connection() {
        let mut st = AppState::new();
        assert_eq!(st.home_count(HomeItem::Spotify), "not connected");

        st.spotify = SpotifyState::Connecting;
        assert_eq!(st.home_count(HomeItem::Spotify), "connecting…");
        assert_eq!(st.home_blurb(HomeItem::Spotify), "signing in…");

        st.spotify = SpotifyState::Limited("no Premium".into());
        assert_eq!(st.home_count(HomeItem::Spotify), "no Premium");
        assert!(st.home_blurb(HomeItem::Spotify).contains("radio still can"));

        // Radio's own line never moves with it.
        assert_eq!(st.home_blurb(HomeItem::Radio), HomeItem::Radio.blurb());
    }

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
            credits: vec![Credit {
                name: "artist".into(),
                id: None,
            }],
            cover_url: None,
        }
    }

    fn record(name: &str, group: &str) -> AlbumItem {
        AlbumItem {
            id: format!("id-{name}"),
            name: name.into(),
            artists: "artist".into(),
            credits: vec![Credit {
                name: "artist".into(),
                id: None,
            }],
            release_year: "2020".into(),
            album_type: "album".into(),
            album_group: group.into(),
            track_count: 10,
            cover_url: None,
        }
    }

    fn artist_view(albums: Vec<AlbumItem>) -> ArtistView {
        let mut v = ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "artist".into(),
            image_url: None,
            genres: Vec::new(),
            bio: BioState::default(),
            top: TrackList::new("artist", "top tracks", None),
            albums: albums.into(),
            tab: ArtistTab::Albums,
            loading: false,
            error: None,
        };
        v.retab();
        v
    }

    /// The strip offers the groups this artist has records in, in one fixed
    /// order, and nothing else.
    #[test]
    fn a_catalogue_names_the_groups_it_holds() {
        let v = artist_view(vec![
            record("A Record", "album"),
            record("A Cut", "single"),
            record("A Guest Spot", "appears_on"),
            record("Another Cut", "single"),
        ]);
        assert_eq!(
            v.tabs(),
            vec![ArtistTab::Albums, ArtistTab::Singles, ArtistTab::AppearsOn]
        );
        assert!(artist_view(Vec::new()).tabs().is_empty());
    }

    /// A record Spotify grouped as nothing we know still has to be reachable,
    /// so it falls under Albums rather than out of the page.
    #[test]
    fn an_unlabelled_record_falls_under_albums() {
        let v = artist_view(vec![record("A Record", "")]);
        assert_eq!(v.tabs(), vec![ArtistTab::Albums]);
        assert_eq!(v.albums.display, vec![0]);
    }

    /// An artist with only guest spots opens on the tab that has them: the
    /// default tab must not leave the page looking empty.
    #[test]
    fn the_page_opens_on_a_group_that_has_records() {
        let v = artist_view(vec![record("A Guest Spot", "appears_on")]);
        assert_eq!(v.tab, ArtistTab::AppearsOn);
        assert_eq!(v.albums.display, vec![0]);
        assert_eq!(v.len(), 1);
    }

    /// `row` reads through the open group, so the same index means a different
    /// record on a different tab.
    #[test]
    fn a_row_points_into_the_open_group() {
        let mut v = artist_view(vec![
            record("A Record", "album"),
            record("A Cut", "single"),
            record("A Compilation", "compilation"),
        ]);
        let name = |v: &ArtistView| match v.row(0) {
            Some(ArtistRow::Album(a)) => a.name.clone(),
            _ => panic!("row 0 is not a card"),
        };
        assert_eq!(name(&v), "A Record");
        v.set_tab(ArtistTab::Compilations);
        assert_eq!(name(&v), "A Compilation");
        assert_eq!(v.len(), 1);
    }

    /// A minimal directory row, for the deck-subject tests.
    fn a_station() -> Station {
        Station {
            uuid: "s1".into(),
            name: "A Station".into(),
            url: "http://stream/s1".into(),
            homepage: String::new(),
            tags: String::new(),
            country: String::new(),
            countrycode: String::new(),
            language: String::new(),
            codec: "MP3".into(),
            bitrate: 128,
            votes: 0,
            hls: false,
        }
    }

    /// Nothing is claimed until the decoder has identified the stream, because
    /// the directory record it would otherwise be guessed from carries no
    /// channel count at all.
    #[test]
    fn a_station_says_how_it_is_mixed_only_once_the_decoder_knows() {
        let radio = RadioPlayback::new(a_station(), 40, Default::default(), Default::default());
        assert_eq!(radio.channel_label(), None);

        for (channels, expected) in [(1, "mono"), (2, "stereo"), (6, "6 ch")] {
            radio.channels.store(channels, Ordering::Relaxed);
            assert_eq!(radio.channel_label().as_deref(), Some(expected));
        }
    }

    fn list_of(names: &[&str]) -> TrackList {
        let mut list = TrackList::new("L", "", None);
        list.append(names.iter().map(|n| track(n, 1000)).collect());
        list
    }

    #[test]
    fn a_rebuild_sorts_case_insensitively_and_reverses() {
        let mut list = list_of(&["banana", "Apple", "cherry"]);
        list.sort = Sort {
            key: ColKey::Title,
            ascending: true,
        };
        list.rebuild();
        assert_eq!(list.display, vec![1, 0, 2]);
        list.sort.ascending = false;
        list.rebuild();
        assert_eq!(list.display, vec![2, 0, 1]);
        list.sort = Sort::default();
        list.rebuild();
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
    /// The header names it, so the control is there from the moment the page
    /// opens rather than once its first page of tracks lands.
    #[test]
    fn an_empty_stack_on_an_album_page_falls_back_to_its_artist() {
        let mut st = AppState::new();
        let mut list = list_of(&["one"]);
        list.kind = TrackListKind::Album;
        st.main = MainView::Tracks(list);
        // Nobody credited on the header yet: nowhere to go.
        assert_eq!(st.back_target(), None);

        if let MainView::Tracks(list) = &mut st.main {
            list.header.credits = vec![
                Credit {
                    name: "Donna The Buffalo".into(),
                    id: Some("r1".into()),
                },
                Credit {
                    name: "Guest".into(),
                    id: Some("r2".into()),
                },
            ];
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
        list.sort = Sort {
            key: ColKey::Title,
            ascending: true,
        };
        st.main = MainView::Tracks(list);
        // "banana", in fetch order.
        st.main_index = 0;
        st.resort_main();
        // banana lands on display row 1 after the sort.
        assert_eq!(st.main_index, 1);
    }

    #[test]
    fn resort_main_reanchors_after_page_append() {
        let mut st = AppState::new();
        let mut list = list_of(&["banana", "cherry"]);
        list.sort = Sort {
            key: ColKey::Title,
            ascending: true,
        };
        st.main = MainView::Tracks(list);
        st.resort_main();
        // "cherry".
        st.main_index = 1;
        if let MainView::Tracks(list) = &mut st.main {
            list.append(vec![track("Apple", 1000)]);
        }
        st.resort_main();
        // Apple sorts first; cherry moves from row 1 to row 2.
        assert_eq!(st.main_index, 2);
    }

    /// Every view kind re-anchors the same way: the row you were on is the row
    /// you are on, wherever the sort has moved it to.
    #[test]
    fn a_resort_keeps_the_selection_on_the_same_row_in_every_view() {
        let by_title = Sort {
            key: ColKey::Title,
            ascending: true,
        };

        // The Playlists page, whose sort lives on the state rather than the
        // view.
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        st.set_playlists(vec![
            owned("p1", "banana", "me"),
            owned("p2", "Apple", "me"),
            owned("p3", "cherry", "me"),
        ]);
        st.playlists_sort = by_title;
        st.main_index = 0;
        st.resort_main();
        assert_eq!(st.playlist_row(st.main_index).unwrap().name, "banana");

        // A search tab, which sorts the tab you are looking at and no other.
        let mut st = AppState::new();
        st.search_tab = SearchTab::Albums;
        st.main = MainView::Search(SearchResults {
            albums: vec![
                record("banana", "album"),
                record("Apple", "album"),
                record("cherry", "album"),
            ]
            .into(),
            ..Default::default()
        });
        st.main_index = 2;
        if let MainView::Search(r) = &mut st.main {
            r.albums.sort = by_title;
        }
        st.resort_main();
        let MainView::Search(r) = &st.main else {
            unreachable!()
        };
        assert_eq!(r.albums.get(st.main_index).unwrap().name, "cherry");

        // A radio page, whose rows are stations or facets through one list.
        let mut st = AppState::new();
        let mut view = RadioView::new(RadioScope::Popular, 0);
        view.rows = vec![
            RadioRow::Facet {
                key: "us".into(),
                label: "banana".into(),
                count: 3,
            },
            RadioRow::Facet {
                key: "de".into(),
                label: "Apple".into(),
                count: 9,
            },
        ]
        .into();
        view.rows.sort = by_title;
        st.main = MainView::Radio(view);
        st.main_index = 0;
        st.resort_main();
        assert_eq!(st.main_index, 1, "banana sorts behind Apple");

        // The artist page, where a catalogue row sits under the top tracks and
        // the tab filter re-applies with the sort.
        let mut st = AppState::new();
        let mut v = artist_view(vec![
            record("banana", "album"),
            record("Apple", "album"),
            record("A Cut", "single"),
        ]);
        v.albums.sort = by_title;
        st.main = MainView::Artist(v);
        st.main_index = 0;
        st.resort_main();
        // Still the record it was on, and the single is still off this tab.
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(st.main_index, 1);
        assert_eq!(v.albums.len(), 2);
        match v.row(st.main_index) {
            Some(ArtistRow::Album(a)) => assert_eq!(a.name, "banana"),
            _ => panic!("the selection left the record it was on"),
        }

        // The page's two lists are drawn one under the other through one flat
        // index, so a re-sort has to rebuild both — whichever the selection
        // happens to be in.
        let MainView::Artist(v) = &mut st.main else {
            unreachable!()
        };
        v.top
            .append(vec![track("banana", 1000), track("Apple", 1000)]);
        v.top.sort = by_title;
        st.main_index = 2 + 1;
        st.resort_main();
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.top.get(0).unwrap().name, "Apple");
        match v.row(st.main_index) {
            Some(ArtistRow::Album(a)) => assert_eq!(a.name, "banana"),
            _ => panic!("the card selection moved when the top tracks sorted"),
        }
    }

    /// A source that reported nothing for a column says nothing about where
    /// its row belongs, so it goes last whichever way the arrow points.
    #[test]
    fn a_blank_cell_sorts_last_in_both_directions() {
        let mut list = SortedList::from_items(vec![
            record("has none", "album"),
            record("has one", "album"),
        ]);
        list.items[0].release_year = String::new();
        list.sort = Sort {
            key: ColKey::Year,
            ascending: true,
        };
        list.rebuild();
        assert_eq!(list.display, vec![1, 0]);
        list.sort.ascending = false;
        list.rebuild();
        assert_eq!(list.display, vec![1, 0]);
    }

    /// A playing queue of one named track, for the deck-subject tests.
    fn playing(uri: &str) -> (Option<Playback>, Option<Queue>) {
        let q = Queue::new(vec![track(uri, 1000)], 0, "Q");
        (Some(Playback::started(50, false)), Some(q))
    }

    /// The trap this whole accessor exists for.
    ///
    /// While a station plays, the Spotify queue is still installed — kept on
    /// purpose, so stopping the stream puts it straight back. A deck control
    /// that reads it directly makes `★` on the radio deck like whatever the
    /// user last heard on Spotify.
    #[test]
    fn the_deck_ignores_the_kept_queue_while_a_station_plays() {
        let mut st = AppState::new();
        let (pb, q) = playing("kept");
        st.playback = pb;
        st.queue = q;
        assert_eq!(
            st.deck_track().map(|t| t.uri.clone()),
            Some("spotify:track:kept".to_string())
        );

        st.radio = Some(RadioPlayback::new(
            a_station(),
            40,
            Default::default(),
            Default::default(),
        ));
        // Announcing nothing: the honest answer is that the deck is about no
        // record at all, *not* the one behind the stream.
        assert!(
            st.deck_track().is_none(),
            "the kept Spotify track leaked through a live station"
        );

        let mut found = track("announced", 1000);
        found.credits = vec![Credit {
            name: "artist".into(),
            id: Some("art1".into()),
        }];
        found.album_id = Some("alb1".into());
        if let Some(r) = st.radio.as_mut() {
            r.matched = RadioMatch::Matched(Box::new(found));
        }
        let deck = st.deck_track().expect("a matched station has a record");
        assert_eq!(deck.uri, "spotify:track:announced");
        assert!(matches!(
            deck.open_album(),
            Some(crate::app::command::AppCommand::OpenAlbum { ref id, .. }) if id == "alb1"
        ));
        assert!(matches!(
            deck.open_artist(),
            Some(crate::app::command::AppCommand::OpenArtist { ref id, .. }) if id == "art1"
        ));

        // Stopping the station hands the deck back to the record that was
        // waiting behind it.
        st.radio = None;
        assert_eq!(
            st.deck_track().map(|t| t.uri.clone()),
            Some("spotify:track:kept".to_string())
        );
    }

    /// A station that announced something spot could not place is not the same
    /// as one announcing nothing, but the deck acts on neither.
    #[test]
    fn an_unmatched_announcement_gives_the_deck_nothing_to_act_on() {
        let mut st = AppState::new();
        let (pb, q) = playing("kept");
        st.playback = pb;
        st.queue = q;
        let mut radio = RadioPlayback::new(a_station(), 40, Default::default(), Default::default());
        radio.matched = RadioMatch::Unmatched;
        st.radio = Some(radio);
        assert!(st.deck_track().is_none());
    }

    /// Before anything has played there is no deck, however a queue got there.
    #[test]
    fn the_deck_is_empty_until_something_has_played() {
        let mut st = AppState::new();
        assert!(st.deck_track().is_none());
        let (_, q) = playing("staged");
        st.queue = q;
        assert!(st.deck_track().is_none(), "a queue alone is not playback");
    }

    /// Progress runs off the anchor while playing, stands still while paused,
    /// and never runs past the track.
    #[test]
    fn progress_interpolates_only_while_playing() {
        let mut pb = Playback::started(50, false);
        pb.is_playing = false;
        pb.anchor(5_000);
        pb.anchored_at = Instant::now() - std::time::Duration::from_secs(10);
        assert_eq!(pb.interpolated_progress_ms(60_000), 5_000);

        pb.is_playing = true;
        let got = pb.interpolated_progress_ms(60_000);
        assert!((14_900..=15_500).contains(&got), "{got}");
        assert_eq!(pb.interpolated_progress_ms(7_000), 7_000, "clamped");
    }

    fn test_station(uuid: &str) -> Station {
        Station {
            uuid: uuid.into(),
            name: uuid.into(),
            url: format!("http://stream/{uuid}"),
            homepage: String::new(),
            tags: String::new(),
            country: String::new(),
            countrycode: "US".into(),
            language: String::new(),
            codec: "MP3".into(),
            bitrate: 128,
            votes: 1,
            hls: false,
        }
    }

    /// Tune a station the way the client does: record what is playing, then
    /// install the new one.
    fn tune(st: &mut AppState, uuid: &str) {
        st.record_listen();
        st.radio = Some(RadioPlayback::new(
            test_station(uuid),
            50,
            Arc::new(parking_lot::Mutex::new(None)),
            Default::default(),
        ));
    }

    fn name_of(entry: &Listened) -> String {
        match entry {
            Listened::Station(s) => s.uuid.clone(),
            Listened::Spotify => "spotify".into(),
        }
    }

    /// Back and forward are one path walked in two directions: a step out and
    /// back in must land where it started, and the Spotify queue a station
    /// interrupted is the bottom of it.
    #[test]
    fn stepping_back_and_forward_walks_the_same_path() {
        let mut st = AppState::new();
        st.playback = Some(Playback::started(50, false));
        for uuid in ["a", "b", "c"] {
            tune(&mut st, uuid);
        }

        for expected in ["b", "a", "spotify"] {
            let entry = st.step_back_listen().expect("a path back");
            assert_eq!(name_of(&entry), expected);
            if let Listened::Station(s) = &entry {
                st.radio = Some(RadioPlayback::new(
                    (**s).clone(),
                    50,
                    Arc::new(parking_lot::Mutex::new(None)),
                    Default::default(),
                ));
            } else {
                st.radio = None;
            }
        }
        assert!(st.step_back_listen().is_none(), "nothing before the queue");

        for expected in ["a", "b", "c"] {
            let entry = st.step_forward_listen().expect("a path forward");
            assert_eq!(name_of(&entry), expected);
            let Listened::Station(s) = &entry else {
                unreachable!()
            };
            st.radio = Some(RadioPlayback::new(
                (**s).clone(),
                50,
                Arc::new(parking_lot::Mutex::new(None)),
                Default::default(),
            ));
        }
        assert!(st.step_forward_listen().is_none());
    }

    /// A new destination is not the one `next ▸▸` was holding, so choosing one
    /// closes the path forward — the same rule `push_view` follows.
    #[test]
    fn a_new_station_closes_the_path_forward() {
        let mut st = AppState::new();
        st.playback = Some(Playback::started(50, false));
        tune(&mut st, "a");
        tune(&mut st, "b");
        st.step_back_listen();
        assert_eq!(st.listen_forward.len(), 1);

        tune(&mut st, "c");
        assert!(st.listen_forward.is_empty());
    }

    /// A station restarted, or a seek that lands back where it was, must not
    /// put a step in the path that goes nowhere.
    #[test]
    fn the_path_never_records_the_same_thing_twice_running() {
        let mut st = AppState::new();
        tune(&mut st, "a");
        tune(&mut st, "b");
        st.record_listen();
        st.record_listen();
        assert_eq!(st.listen_back.len(), 2);
    }

    /// The view stack keeps its root frame when it overflows because `Esc`
    /// needs somewhere to bottom out. This path has no root, so the oldest
    /// entry is the one that goes.
    #[test]
    fn the_path_drops_its_oldest_step_rather_than_growing() {
        let mut st = AppState::new();
        for i in 0..LISTEN_STACK_MAX + 5 {
            tune(&mut st, &format!("s{i}"));
        }
        assert_eq!(st.listen_back.len(), LISTEN_STACK_MAX);
        assert_eq!(name_of(&st.listen_back[0]), "s4");
    }

    /// Walking back and forth must not grow the path past its cap either.
    #[test]
    fn walking_forward_cannot_outgrow_the_cap() {
        let mut st = AppState::new();
        for i in 0..LISTEN_STACK_MAX {
            tune(&mut st, &format!("s{i}"));
        }
        for _ in 0..10 {
            st.step_back_listen();
        }
        for _ in 0..10 {
            st.step_forward_listen();
        }
        assert!(st.listen_back.len() <= LISTEN_STACK_MAX);
    }

    /// A tune-in that never came up clears the station but leaves the path
    /// alone, so previous still reaches the one you were listening to.
    #[test]
    fn a_failed_tune_in_leaves_the_station_you_came_from_reachable() {
        let mut st = AppState::new();
        tune(&mut st, "a");
        tune(&mut st, "b");
        st.radio = None;
        assert_eq!(name_of(&st.step_back_listen().expect("a path back")), "a");
    }

    fn page(kind: TrackListKind, cache_key: Option<&str>) -> AppState {
        let mut st = AppState::new();
        let mut list = TrackList::new("Page", "", None);
        list.kind = kind;
        list.cache_key = cache_key.map(Into::into);
        st.main = MainView::Tracks(list);
        st
    }

    /// The page's own link, read off the identity it already carries rather
    /// than off the header, which is display text.
    #[test]
    fn a_page_names_its_own_link() {
        let cases = [
            (
                page(TrackListKind::Playlist, Some(&playlist_key("p1"))),
                Link::Playlist("p1".into()),
            ),
            (
                page(TrackListKind::Album, Some(&album_key("a1"))),
                Link::Album("a1".into()),
            ),
        ];
        for (st, want) in cases {
            assert_eq!(st.open_page_link(), Some(want));
        }
    }

    /// Every page with no link of its own, so the header knows to draw no
    /// control rather than one that shares the wrong thing.
    #[test]
    fn a_page_without_a_link_names_none() {
        let mut liked = page(TrackListKind::LikedSongs, Some(&liked_key()));
        assert_eq!(liked.open_page_link(), None, "liked songs");
        liked.main = MainView::Home;
        assert_eq!(liked.open_page_link(), None, "home");
        liked.main = MainView::Playlists;
        assert_eq!(liked.open_page_link(), None, "playlists");
        // A page still loading has no key stamped on it yet.
        assert_eq!(
            page(TrackListKind::Album, None).open_page_link(),
            None,
            "unstamped"
        );
    }

    /// A station on the deck, tuned in.
    fn tuned(uuid: &str) -> AppState {
        let mut st = AppState::new();
        let mut station = a_station();
        station.uuid = uuid.into();
        st.radio = Some(RadioPlayback::new(
            station,
            40,
            Default::default(),
            Default::default(),
        ));
        st
    }

    /// The rows a station's list holds, by what they announced.
    fn announced(st: &AppState) -> Vec<&str> {
        st.heard().iter().map(|h| h.announced.as_str()).collect()
    }

    /// Every announcement makes a row, whether Spotify can place it or not:
    /// what the station said is what was played, and that is the list.
    #[test]
    fn every_announcement_makes_a_row() {
        let mut st = tuned("s1");
        st.push_heard("Aspen - Seasick And Beer Drinking".into());
        st.push_heard("Big R Radio - We'll Be Right Back".into());
        assert_eq!(
            announced(&st),
            [
                "Aspen - Seasick And Beer Drinking",
                "Big R Radio - We'll Be Right Back"
            ]
        );
        assert!(st.heard_track(1).is_none(), "nothing has been looked up");
    }

    /// The list belongs to the station, not to the deck: change station and
    /// come back, and what it played is still there.
    #[test]
    fn a_station_you_come_back_to_keeps_its_records() {
        let mut st = tuned("s1");
        st.push_heard("first".into());

        let mut second = a_station();
        second.uuid = "s2".into();
        st.radio = Some(RadioPlayback::new(
            second,
            40,
            Default::default(),
            Default::default(),
        ));
        st.push_heard("elsewhere".into());
        assert_eq!(announced(&st), ["elsewhere"]);

        let mut first = a_station();
        first.uuid = "s1".into();
        st.radio = Some(RadioPlayback::new(
            first,
            40,
            Default::default(),
            Default::default(),
        ));
        assert_eq!(announced(&st), ["first"]);
    }

    /// An answer that lands after the record ended still settles its own row.
    /// A late one settles nothing else, and a second helping of the same
    /// announcement is a row of its own.
    #[test]
    fn a_late_answer_settles_the_row_it_was_for() {
        let mut st = tuned("s1");
        st.push_heard("a".into());
        st.set_heard_searching("s1", "a");
        st.push_heard("b".into());

        st.set_heard_match("s1", "a", RadioMatch::Matched(Box::new(track("A", 1000))));
        assert_eq!(st.heard_track(0).map(|t| t.name.as_str()), Some("A"));
        assert!(st.heard_track(1).is_none(), "b was never searched for");

        // The row is settled, so a second answer for the same words has no
        // searching row to land on.
        st.set_heard_match("s1", "a", RadioMatch::Unmatched);
        assert_eq!(st.heard_track(0).map(|t| t.name.as_str()), Some("A"));
    }

    /// The list stops growing, and the rows it drops take the selection and
    /// the scroll down with them rather than sliding under the reader.
    #[test]
    fn the_list_trims_its_oldest_and_carries_the_selection() {
        let mut st = tuned("s1");
        st.hit.player_queue = Rect::new(0, 0, 80, 10);
        for i in 0..HEARD_MAX {
            st.push_heard(format!("row {i}"));
        }
        st.heard_index = 5;
        *st.heard_list.offset_mut() = 3;

        st.push_heard("one over".into());
        assert_eq!(st.heard().len(), HEARD_MAX);
        assert_eq!(st.heard()[0].announced, "row 1", "the oldest row went");
        assert_eq!(st.heard_index, 4, "the selection followed its row");
        assert_eq!(st.heard_list.offset(), 2, "and so did the scroll");
    }

    /// The view follows the newest row while it is already at the bottom, and
    /// holds still the moment you scroll up to read.
    #[test]
    fn the_view_follows_the_newest_row_only_from_the_bottom() {
        let mut st = tuned("s1");
        st.hit.player_queue = Rect::new(0, 0, 80, 3);
        for i in 0..5 {
            st.push_heard(format!("row {i}"));
        }
        assert_eq!(st.heard_list.offset(), 2, "the newest row is in view");

        *st.heard_list.offset_mut() = 0;
        st.push_heard("row 5".into());
        assert_eq!(st.heard_list.offset(), 0, "a reader is not chased down");
    }

    /// The player screen has two lists and draws one at a time. Which one it
    /// is under a station must not depend on the queue that is kept behind it.
    #[test]
    fn the_players_list_is_the_station_while_one_is_on() {
        let mut st = tuned("s1");
        st.set_queue(Some(crate::app::queue::Queue::new(
            vec![track("kept", 1000)],
            0,
            "Kept",
        )));
        st.push_heard("said".into());
        st.set_heard_searching("s1", "said");
        st.set_heard_match(
            "s1",
            "said",
            RadioMatch::Matched(Box::new(track("S", 2000))),
        );

        assert_eq!(st.player_rows(), 1);
        assert_eq!(
            st.player_row_track(0).map(|t| t.name.as_str()),
            Some("S"),
            "the station's row, not the kept queue's"
        );

        st.radio = None;
        assert_eq!(st.player_rows(), 1);
        assert_eq!(
            st.player_row_track(0).map(|t| t.name.as_str()),
            Some("kept")
        );
    }

    /// Two things can know what a station is playing and only ever one of them
    /// at a time: the decoder while the stream is on, and a probe while it is
    /// stood down. The deck reads whichever there is.
    #[test]
    fn the_deck_reads_the_decoder_first_and_a_probe_second() {
        let mut st = tuned("s1");
        assert_eq!(st.radio.as_ref().unwrap().now_title(), None);

        st.radio.as_mut().unwrap().probed = Some("off the wire".into());
        assert_eq!(
            st.radio.as_ref().unwrap().now_title().as_deref(),
            Some("off the wire")
        );

        *st.radio.as_ref().unwrap().title.lock() = Some("off the decoder".into());
        assert_eq!(
            st.radio.as_ref().unwrap().now_title().as_deref(),
            Some("off the decoder"),
            "the stream itself wins while there is one"
        );
    }

    /// Going back on air re-announces whatever the station is still playing,
    /// and that is the row the list already has. A second row for it would
    /// grow the list by one every time you left and came back.
    #[test]
    fn going_back_on_air_does_not_log_the_same_record_twice() {
        let mut st = tuned("s1");
        st.push_heard("a".into());
        st.set_heard_searching("s1", "a");
        st.set_heard_match("s1", "a", RadioMatch::Matched(Box::new(track("A", 1000))));

        // A fresh deck for the same station, as a tune-in installs.
        let station = st.radio.as_ref().unwrap().station.clone();
        st.radio = Some(RadioPlayback::new(
            station,
            40,
            Default::default(),
            Default::default(),
        ));
        let stamped = st.heard()[0].at;
        assert!(st.adopt_newest_heard("s1", "a"), "the row is already there");
        assert_eq!(st.heard().len(), 1);
        // The record started when it was first announced, not when you came
        // back to it, so the `Ago` column goes on counting from where it was.
        assert_eq!(st.heard()[0].at, stamped);
        // And the answer comes back with it, so the lookup is not spent twice.
        assert_eq!(
            st.radio
                .as_ref()
                .and_then(|r| r.matched_track())
                .map(|t| t.name.as_str()),
            Some("A")
        );

        assert!(!st.adopt_newest_heard("s1", "b"), "a new record is new");
    }

    /// Playing a record off a station's list must not cost the play order you
    /// already had, nor where you were in it.
    #[test]
    fn parking_gives_the_queue_back_whole() {
        let mut st = AppState::new();
        st.set_queue(Some(crate::app::queue::Queue::new(
            vec![track("one", 1000), track("two", 1000)],
            1,
            "Kept",
        )));
        st.queue_index = 1;
        *st.queue_list.offset_mut() = 4;

        st.park_queue();
        assert!(st.queue.is_none(), "the queue stood down");
        st.set_queue(Some(crate::app::queue::Queue::new(
            vec![track("other", 1000)],
            0,
            "From a station",
        )));

        st.unpark_queue();
        assert_eq!(st.queue.as_ref().map(|q| q.name()), Some("Kept"));
        assert_eq!(st.queue.as_ref().map(|q| q.index()), Some(1));
        assert_eq!(st.queue_index, 1);
        assert_eq!(st.queue_list.offset(), 4);
    }

    /// The play order a row installs and the position it starts at are read
    /// off the same rows, so a row the station only named cannot shift what
    /// Enter plays.
    #[test]
    fn a_rows_play_position_skips_what_could_not_be_placed() {
        let mut st = tuned("s1");
        for (words, matched) in [
            ("a", RadioMatch::Matched(Box::new(track("A", 1000)))),
            ("said", RadioMatch::Unmatched),
            ("b", RadioMatch::Matched(Box::new(track("B", 1000)))),
        ] {
            st.push_heard(words.into());
            st.set_heard_searching("s1", words);
            st.set_heard_match("s1", words, matched);
        }

        let names: Vec<String> = st.heard_tracks().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["A", "B"]);
        assert_eq!(st.heard_play_position(0), Some(0));
        assert_eq!(st.heard_play_position(1), None, "nothing to play");
        assert_eq!(st.heard_play_position(2), Some(1));
    }

    /// Off air the deck is about a record from the station's list, which comes
    /// down the Spotify path like any other track.
    #[test]
    fn the_deck_names_the_record_while_the_stream_stands_down() {
        let mut st = tuned("s1");
        st.radio.as_mut().unwrap().matched =
            RadioMatch::Matched(Box::new(track("announced", 1000)));
        st.playback = Some(Playback::started(50, false));
        st.set_queue(Some(crate::app::queue::Queue::new(
            vec![track("chosen", 1000)],
            0,
            "earlier on A Station",
        )));

        assert_eq!(st.deck_track().map(|t| t.name.as_str()), Some("announced"));
        st.radio.as_mut().unwrap().off_air = true;
        assert_eq!(st.deck_track().map(|t| t.name.as_str()), Some("chosen"));
    }
}

#[cfg(test)]
mod bio_tests {
    use std::sync::Arc;

    use super::*;

    fn bio(text: &str) -> ArtistBio {
        ArtistBio {
            text: text.into(),
            image_url: None,
            source_url: "https://en.wikipedia.org/wiki/X".into(),
        }
    }

    /// The band wraps the opening paragraph into the rows it has. A blank line
    /// in five rows would spend one of them on nothing, so the break and what
    /// follows it stay in the box.
    #[test]
    fn the_lead_is_the_first_paragraph_only() {
        let b = bio("A short one\n\nThe paragraph after it, which is longer.");
        assert_eq!(b.lead(), "A short one");

        let b = bio(
            "Muse are an English rock band from Teignmouth, Devon, formed in 1994. They released Showbiz in 1999.",
        );
        assert_eq!(
            b.lead(),
            "Muse are an English rock band from Teignmouth, Devon, formed in 1994. They released Showbiz in 1999."
        );

        assert_eq!(bio("").lead(), "");
    }

    /// Spotify's photograph where there is one, the article's where there is
    /// not, and nothing where neither has one.
    #[test]
    fn the_page_wears_whichever_photo_it_has() {
        let mut v = artist_view(Vec::new());
        assert_eq!(v.photo_url(), None);

        let mut from_wiki = bio("text");
        from_wiki.image_url = Some("https://upload.wikimedia.org/x.jpg".into());
        v.bio = BioState::Ready(Arc::new(from_wiki));
        assert_eq!(v.photo_url(), Some("https://upload.wikimedia.org/x.jpg"));

        v.image_url = Some("https://i.scdn.co/image/artist".into());
        assert_eq!(v.photo_url(), Some("https://i.scdn.co/image/artist"));
    }

    fn artist_view(albums: Vec<AlbumItem>) -> ArtistView {
        ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "artist".into(),
            image_url: None,
            genres: Vec::new(),
            bio: BioState::default(),
            top: TrackList::new("artist", "top tracks", None),
            albums: albums.into(),
            tab: ArtistTab::Albums,
            loading: false,
            error: None,
        }
    }
}
