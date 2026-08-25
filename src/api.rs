use std::collections::HashSet;

use anyhow::{Context, Result};
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::http::Query;
use rspotify::model::{
    AlbumId, ArtistId, FullTrack, Id, LibraryId, PlayableId, PlayableItem, PlaylistId,
    SearchResult, SearchType, SimplifiedAlbum, SimplifiedPlaylist, SubscriptionLevel, TrackId,
};
use rspotify::{AuthCodeSpotify, Token};
use serde::Deserialize;

use crate::app::state::{AlbumItem, ArtistItem, Playlist, SearchResults, Track, track_id};

pub const PAGE_LIMIT: u32 = 50;
/// Pages of an artist's catalogue to walk before giving up on the rest.
const ARTIST_ALBUM_PAGES: u32 = 4;
const MAX_PLAYLISTS: u32 = 1000;

/// The signed-in account, as much of it as spot needs.
pub struct Account {
    pub id: String,
    /// `None` when Spotify does not report the subscription level.
    pub premium: Option<bool>,
}

/// Everything the artist page is built from.
pub struct ArtistOverview {
    pub top: Vec<Track>,
    pub albums: Vec<AlbumItem>,
    pub image_url: Option<String>,
    pub genres: Vec<String>,
}

/// The fields of Spotify's album object the cards use. Only `name` is
/// required; a record missing an id cannot be opened and is dropped.
#[derive(Deserialize)]
struct RawAlbumPage {
    items: Vec<RawAlbum>,
}

#[derive(Deserialize)]
struct RawAlbum {
    id: Option<String>,
    name: String,
    album_type: Option<String>,
    /// The artist's relationship to the record, which `album_type` cannot
    /// express: only this says a record merely *features* them.
    album_group: Option<String>,
    release_date: Option<String>,
    total_tracks: Option<u32>,
    #[serde(default)]
    images: Vec<rspotify::model::Image>,
    #[serde(default)]
    artists: Vec<RawArtist>,
}

#[derive(Deserialize)]
struct RawArtist {
    name: String,
}

/// One page of a playlist cut down to the track URIs, which is all
/// [`Api::playlist_track_ids`] asks for. A local file or a podcast episode has
/// no track object, and an unavailable one has no URI, hence both `Option`s.
#[derive(Deserialize)]
struct RawTrackUriPage {
    items: Vec<RawTrackUriItem>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct RawTrackUriItem {
    track: Option<RawTrackUri>,
}

#[derive(Deserialize)]
struct RawTrackUri {
    uri: Option<String>,
}

/// A playlist's own metadata, narrowed to what the header band draws.
///
/// Read raw rather than through rspotify's `FullPlaylist` for two reasons:
/// that type drags the first hundred tracks along, which this call has no use
/// for, and a `fields`-narrowed response cannot deserialize into it — the same
/// bind [`Api::artist_albums`] is in.
#[derive(Deserialize)]
struct RawPlaylist {
    name: String,
    description: Option<String>,
    #[serde(default)]
    images: Vec<rspotify::model::Image>,
    public: Option<bool>,
    #[serde(default)]
    collaborative: bool,
    owner: Option<RawOwner>,
    /// Spotify renamed `tracks` to `items` in February 2026 and serves both
    /// for now. Whichever arrives is the total; see rspotify's own shadow
    /// structs, which straddle the same rename.
    tracks: Option<RawTotal>,
    items: Option<RawTotal>,
}

#[derive(Deserialize)]
struct RawOwner {
    id: String,
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct RawTotal {
    total: u32,
}

/// What [`Api::playlist_detail`] answers with: everything the playlist page's
/// header shows that its track pages do not carry.
#[derive(Debug, Clone)]
pub struct PlaylistDetail {
    pub name: String,
    pub description: String,
    pub cover_url: Option<String>,
    pub owner: String,
    pub owner_id: String,
    pub public: Option<bool>,
    pub collaborative: bool,
    pub total: Option<u32>,
}

#[derive(Clone)]
pub struct Api {
    client: AuthCodeSpotify,
}

impl Api {
    pub fn new(token: Token) -> Self {
        Self {
            client: AuthCodeSpotify::from_token(token),
        }
    }

    /// Swap in a refreshed token (rspotify shares it via Arc<Mutex<..>>).
    pub async fn update_token(&self, token: Token) {
        *self.client.get_token().lock().await.unwrap() = Some(token);
    }

    /// Who is signed in, and whether Spotify will let spot stream for them.
    ///
    /// The id is what the Playlists page blanks the Owner cell against — a
    /// display name need not be unique.
    ///
    /// `premium` decides whether the streaming session is worth opening:
    /// librespot's login is refused for everything below Premium, and reading
    /// it here costs one request instead of a browser window and a failure.
    /// Spotify has been withdrawing `product` from this endpoint, hence the
    /// `Option` and the allow: a level it will not report is not a level of
    /// `Free`, and the caller has to fall back to trying the login.
    #[allow(deprecated)]
    pub async fn account(&self) -> Result<Account> {
        let me = self
            .client
            .me()
            .await
            .context("failed to fetch the current user")?;
        Ok(Account {
            id: me.id.id().to_string(),
            premium: me.product.map(|p| p == SubscriptionLevel::Premium),
        })
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .current_user_playlists_manual(Some(PAGE_LIMIT), Some(offset))
                .await?;
            out.extend(page.items.iter().map(playlist_from_simplified));
            offset += page.items.len() as u32;
            if page.next.is_none() || offset >= MAX_PLAYLISTS {
                break;
            }
        }
        Ok(out)
    }

    /// One page of the user's saved tracks: (tracks, has_more, source total).
    pub async fn liked_songs_page(&self, offset: u32) -> Result<(Vec<Track>, bool, u32)> {
        let page = self
            .client
            .current_user_saved_tracks_manual(None, Some(PAGE_LIMIT), Some(offset))
            .await?;
        let tracks = page
            .items
            .iter()
            .filter_map(|s| track_from_full(&s.track))
            .collect();
        Ok((
            tracks,
            page.next.is_some() && !page.items.is_empty(),
            page.total,
        ))
    }

    /// One page of a playlist: (tracks, has_more, source total).
    pub async fn playlist_tracks_page(
        &self,
        playlist_id: &str,
        offset: u32,
    ) -> Result<(Vec<Track>, bool, u32)> {
        let id = PlaylistId::from_id(playlist_id.to_owned())?;
        let page = self
            .client
            .playlist_items_manual(id.as_ref(), None, None, Some(PAGE_LIMIT), Some(offset))
            .await?;
        let tracks = page
            .items
            .iter()
            .filter_map(|item| match item.item.as_ref()? {
                PlayableItem::Track(t) => track_from_full(t),
                _ => None,
            })
            .collect();
        Ok((
            tracks,
            page.next.is_some() && !page.items.is_empty(),
            page.total,
        ))
    }

    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        let (tracks, albums, artists, playlists) = tokio::join!(
            self.search_one(query, SearchType::Track),
            self.search_one(query, SearchType::Album),
            self.search_one(query, SearchType::Artist),
            self.search_one(query, SearchType::Playlist),
        );

        // Four requests, four answers. One that fails is dropped so the tabs
        // that did answer are still drawn, but four that fail is the search
        // failing, and reporting that as an empty result set would have every
        // tab claim Spotify holds nothing for the query.
        let refused: Vec<&anyhow::Error> = [&tracks, &albums, &artists, &playlists]
            .iter()
            .filter_map(|r| r.as_ref().err())
            .collect();
        if refused.len() == 4 {
            anyhow::bail!("{}", refused[0]);
        }
        for e in refused {
            log::warn!("part of the search for {query:?} failed: {e:#}");
        }

        let mut results = SearchResults {
            query: query.to_string(),
            ..Default::default()
        };
        if let Ok(SearchResult::Tracks(page)) = tracks {
            results.tracks = page.items.iter().filter_map(track_from_full).collect();
        }
        if let Ok(SearchResult::Albums(page)) = albums {
            results.albums = page
                .items
                .iter()
                .filter_map(album_from_simplified)
                .collect();
        }
        if let Ok(SearchResult::Artists(page)) = artists {
            results.artists = page
                .items
                .iter()
                .map(|a| ArtistItem {
                    id: a.id.id().to_string(),
                    uri: a.id.uri(),
                    name: a.name.clone(),
                })
                .collect();
        }
        if let Ok(SearchResult::Playlists(page)) = playlists {
            results.playlists = page.items.iter().map(playlist_from_simplified).collect();
        }
        Ok(results)
    }

    /// Track results for one query, as [`Track`]s.
    ///
    /// For the radio deck's lookup, which asks a narrow question and wants the
    /// answer in the shape the deck draws. A result that is somehow not a track
    /// page comes back empty rather than as an error: this is a best-effort
    /// lookup on the side, and there is nothing the caller could do about a
    /// shape the endpoint cannot return anyway.
    pub async fn search_tracks(&self, query: &str) -> Result<Vec<Track>> {
        match self.search_one(query, SearchType::Track).await? {
            SearchResult::Tracks(page) => {
                Ok(page.items.iter().filter_map(track_from_full).collect())
            }
            _ => Ok(Vec::new()),
        }
    }

    async fn search_one(&self, query: &str, kind: SearchType) -> Result<SearchResult> {
        Ok(self
            .client
            .search(query, kind, None, None, Some(30), None)
            .await?)
    }

    /// One page of an album's tracks: (tracks, has_more, source total).
    /// Album name/year come from the caller (the album-tracks endpoint
    /// returns `SimplifiedTrack`s with no album attached).
    pub async fn album_tracks_page(
        &self,
        album_id: &str,
        album_name: &str,
        year: &str,
        offset: u32,
    ) -> Result<(Vec<Track>, bool, u32)> {
        let id = AlbumId::from_id(album_id.to_owned())?;
        let page = self
            .client
            .album_track_manual(id.as_ref(), None, Some(PAGE_LIMIT), Some(offset))
            .await?;
        let tracks = page
            .items
            .iter()
            .filter_map(|t| track_from_simplified(t, album_id, album_name, year))
            .collect();
        Ok((
            tracks,
            page.next.is_some() && !page.items.is_empty(),
            page.total,
        ))
    }

    /// Everything the artist page draws, fetched in one pass.
    ///
    /// Three concurrent calls: the artist (for the photo the header band
    /// wears), the top tracks, and the catalogue. Only the catalogue is
    /// load-bearing — the other two are deprecated upstream and may be gone
    /// for good, so each degrades to nothing rather than failing the page.
    pub async fn artist_overview(&self, artist_id: &str) -> Result<ArtistOverview> {
        let id = ArtistId::from_id(artist_id.to_owned())?;
        #[allow(deprecated)]
        let top_fut = self.client.artist_top_tracks(id.as_ref(), None);
        let artist_fut = self.client.artist(id.as_ref());
        let albums_fut = self.artist_albums(artist_id);
        let (top, artist, albums) = tokio::join!(top_fut, artist_fut, albums_fut);
        let top = match top {
            Ok(tracks) => tracks.iter().filter_map(track_from_full).collect(),
            Err(e) => {
                log::warn!("artist top tracks unavailable: {e:#}");
                Vec::new()
            }
        };
        let (image_url, genres) = match artist {
            // `genres` is deprecated and usually absent now; an empty list is
            // the normal case rather than a failure, and the band omits the
            // line when it comes back empty.
            #[allow(deprecated)]
            Ok(a) => (crate::cover::pick_url(&a.images), a.genres),
            Err(e) => {
                log::warn!("artist details unavailable: {e:#}");
                (None, Vec::new())
            }
        };
        Ok(ArtistOverview {
            top,
            albums: albums?,
            image_url,
            genres,
        })
    }

    /// An artist's whole catalogue — albums, singles, compilations and the
    /// records they only guest on — newest first.
    ///
    /// All four groups come back in one pass because the page's tabs are cuts
    /// of one answer, not four queries: switching tab must not wait on the
    /// network.
    ///
    /// Read as raw JSON rather than through rspotify's typed call: its
    /// `SimplifiedAlbum` drops both `total_tracks` and `album_group`, and a
    /// card names how many tracks a record holds while the tabs are grouped by
    /// the latter. Everything else here is the documented shape of the same
    /// response, and anything unparseable is skipped.
    async fn artist_albums(&self, artist_id: &str) -> Result<Vec<AlbumItem>> {
        let limit = PAGE_LIMIT.to_string();
        let mut albums: Vec<AlbumItem> = Vec::new();
        // Four groups overrun one page for anyone prolific, so walk them —
        // but only so far. Past this a card list is longer than anyone
        // scrolls, and the page has already cost four round trips.
        for page_no in 0..ARTIST_ALBUM_PAGES {
            let offset = (page_no * PAGE_LIMIT).to_string();
            let params: Query = [
                ("include_groups", "album,single,compilation,appears_on"),
                ("limit", limit.as_str()),
                ("offset", offset.as_str()),
            ]
            .into_iter()
            .collect();
            let body = self
                .client
                .api_get(&format!("artists/{artist_id}/albums"), &params)
                .await
                .context("failed to fetch artist albums")?;
            let page: RawAlbumPage = serde_json::from_str(&body)?;
            let full = page.items.len() as u32 == PAGE_LIMIT;
            albums.extend(page.items.iter().filter_map(album_from_raw));
            if !full {
                break;
            }
        }
        // Newest first. A stable sort, so records sharing a year keep the
        // order Spotify returned them in.
        albums.sort_by(|a, b| b.release_year.cmp(&a.release_year));
        Ok(albums)
    }

    /// Saved-state for up to `PAGE_LIMIT` track URIs, as (uri, liked) pairs.
    pub async fn tracks_liked(&self, uris: &[String]) -> Result<Vec<(String, bool)>> {
        let pairs: Vec<(String, TrackId)> = uris
            .iter()
            .filter_map(|u| TrackId::from_uri(u).ok().map(|id| (u.clone(), id)))
            .collect();
        if pairs.is_empty() {
            return Ok(Vec::new());
        }
        let flags = self
            .client
            .library_contains(pairs.iter().map(|(_, id)| LibraryId::Track(id.clone())))
            .await?;
        Ok(pairs.into_iter().map(|(u, _)| u).zip(flags).collect())
    }

    /// Save or unsave one track in the user's library.
    ///
    /// `library_add` / `library_remove` rather than the `current_user_saved_tracks_*`
    /// pair: those are deprecated in rspotify 0.16 and are thin wrappers over these
    /// anyway. The read side (`tracks_liked`) already goes through the Library API.
    pub async fn set_track_liked(&self, uri: &str, liked: bool) -> Result<()> {
        let id = LibraryId::Track(TrackId::from_uri(uri)?);
        if liked {
            self.client.library_add([id]).await
        } else {
            self.client.library_remove([id]).await
        }
        .with_context(|| {
            if liked {
                "failed to save the track"
            } else {
                "failed to unsave the track"
            }
        })?;
        Ok(())
    }

    /// Put one track on a playlist, or take every copy of it off.
    ///
    /// Returns the playlist's new snapshot id, so the caller can retire the
    /// copy it holds — the track cache keys its entries on that hash and would
    /// otherwise serve the list back as it stood before the change.
    ///
    /// Removal takes *all* occurrences rather than a position. The box says
    /// whether the record is on the playlist, not how many times, so taking it
    /// off has to mean it is off.
    pub async fn set_track_on_playlist(
        &self,
        playlist_id: &str,
        uri: &str,
        on: bool,
    ) -> Result<String> {
        let playlist = PlaylistId::from_id(playlist_id)?;
        let track = PlayableId::Track(TrackId::from_uri(uri)?);
        let result = if on {
            self.client
                .playlist_add_items(playlist, [track], None)
                .await
        } else {
            self.client
                .playlist_remove_all_occurrences_of_items(playlist, [track], None)
                .await
        }
        .with_context(|| {
            if on {
                "failed to add the track to the playlist"
            } else {
                "failed to take the track off the playlist"
            }
        })?;
        Ok(result.snapshot_id)
    }

    /// Every track a playlist holds, as bare ids.
    ///
    /// Spotify has no endpoint that answers "does this playlist hold this
    /// track" — there is nothing here like the Library API's
    /// `library_contains` — so the only way to know is to read the playlist
    /// and look. Reading the whole thing costs no more than looking for one
    /// record and answers for every record instead, which is why the contents
    /// are what gets cached. `fields` cuts the response to the track URIs and
    /// the pages are the largest the endpoint allows, so the cost is one
    /// request per hundred tracks and almost no bytes.
    ///
    /// Read as raw JSON for the same reason [`Self::artist_albums`] is: a
    /// narrowed `fields` cannot deserialize into rspotify's full
    /// `PlaylistItem`.
    pub async fn playlist_track_ids(&self, playlist_id: &str) -> Result<HashSet<String>> {
        const LIMIT: u32 = 100;
        let limit = LIMIT.to_string();
        let mut offset = 0u32;
        let mut ids = HashSet::new();
        loop {
            let at = offset.to_string();
            let params: Query = [
                ("fields", "items(track(uri)),next"),
                ("limit", limit.as_str()),
                ("offset", at.as_str()),
            ]
            .into_iter()
            .collect();
            let body = self
                .client
                .api_get(&format!("playlists/{playlist_id}/tracks"), &params)
                .await
                .context("failed to read the playlist")?;
            let page: RawTrackUriPage = serde_json::from_str(&body)?;
            ids.extend(
                page.items
                    .iter()
                    .filter_map(|i| i.track.as_ref())
                    .filter_map(|t| t.uri.as_deref())
                    .map(|uri| track_id(uri).to_string()),
            );
            offset += page.items.len() as u32;
            if page.next.is_none() || page.items.is_empty() {
                return Ok(ids);
            }
        }
    }

    /// A playlist's own metadata: the blurb, the cover, who owns it and how it
    /// is shared.
    ///
    /// The playlist list the library page is built from carries none of the
    /// first two, and carries nothing at all about a playlist you do not
    /// follow — so a page opened from a search has only this to fill it.
    pub async fn playlist_detail(&self, playlist_id: &str) -> Result<PlaylistDetail> {
        let params: Query = [(
            "fields",
            "name,description,images,public,collaborative,\
             owner(id,display_name),tracks(total),items(total)",
        )]
        .into_iter()
        .collect();
        let body = self
            .client
            .api_get(&format!("playlists/{playlist_id}"), &params)
            .await
            .context("failed to read the playlist")?;
        let p: RawPlaylist = serde_json::from_str(&body)?;
        let owner_id = p.owner.as_ref().map(|o| o.id.clone()).unwrap_or_default();
        Ok(PlaylistDetail {
            name: p.name,
            description: unescape_html(p.description.as_deref().unwrap_or_default()),
            cover_url: playlist_cover(&p.images),
            owner: p
                .owner
                .and_then(|o| o.display_name)
                .unwrap_or_else(|| owner_id.clone()),
            owner_id,
            public: p.public,
            collaborative: p.collaborative,
            total: p.items.or(p.tracks).map(|t| t.total),
        })
    }

    /// Whether the playlist is in the user's library.
    ///
    /// "Saved" and "followed" are one thing to Spotify, and the Library API is
    /// where that lives now — `playlist_check_follow` is deprecated in
    /// rspotify 0.16, as are `playlist_follow` and `playlist_unfollow`, all
    /// three being wrappers over these calls. The track side already goes the
    /// same way; see [`Self::tracks_liked`].
    pub async fn playlist_saved(&self, playlist_id: &str) -> Result<bool> {
        let id = LibraryId::Playlist(PlaylistId::from_id(playlist_id.to_owned())?);
        Ok(self
            .client
            .library_contains([id])
            .await?
            .first()
            .copied()
            .unwrap_or(false))
    }

    /// Put the playlist in the user's library, or take it out.
    pub async fn set_playlist_saved(&self, playlist_id: &str, saved: bool) -> Result<()> {
        let id = LibraryId::Playlist(PlaylistId::from_id(playlist_id.to_owned())?);
        if saved {
            self.client.library_add([id]).await
        } else {
            self.client.library_remove([id]).await
        }
        .with_context(|| {
            if saved {
                "failed to save the playlist"
            } else {
                "failed to unsave the playlist"
            }
        })?;
        Ok(())
    }

    /// Rename a playlist and reword its blurb. Spotify refuses this for a
    /// playlist you do not own, so the caller checks first.
    pub async fn set_playlist_details(
        &self,
        playlist_id: &str,
        name: &str,
        description: &str,
    ) -> Result<()> {
        let id = PlaylistId::from_id(playlist_id.to_owned())?;
        self.client
            .playlist_change_detail(id, Some(name), None, Some(description), None)
            .await
            .context("failed to change the playlist")?;
        Ok(())
    }
}

/// The cover to draw for a playlist, which is none when Spotify only made it
/// a mosaic. See [`crate::cover::is_mosaic`].
fn playlist_cover(images: &[rspotify::model::Image]) -> Option<String> {
    crate::cover::pick_url(images).filter(|u| !crate::cover::is_mosaic(u))
}

/// Turn the XML entities Spotify escapes a playlist description with back into
/// the characters they stand for.
///
/// Descriptions come back HTML-escaped — an apostrophe arrives as `&#x27;` —
/// and a header band is not a browser. Only the five predefined entities and
/// numeric references are handled; anything else is left as it was typed,
/// which is the right answer for text that never was markup.
fn unescape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[1..].find(';').map(|i| i + 1) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn artists_line(artists: &[rspotify::model::SimplifiedArtist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn track_from_full(t: &FullTrack) -> Option<Track> {
    Some(Track {
        uri: t.id.as_ref()?.uri(),
        name: t.name.clone(),
        artists: artists_line(&t.artists),
        album: t.album.name.clone(),
        release_year: release_year(t.album.release_date.as_deref()),
        duration_ms: t.duration.num_milliseconds().max(0) as u64,
        track_number: t.track_number,
        album_id: t.album.id.as_ref().map(|id| id.id().to_string()),
        artist_id: t
            .artists
            .first()
            .and_then(|a| a.id.as_ref())
            .map(|id| id.id().to_string()),
        // Comes back on the album object every full track carries, so opening
        // the album from this row costs no extra round trip.
        cover_url: crate::cover::pick_url(&t.album.images),
    })
}

fn track_from_simplified(
    t: &rspotify::model::SimplifiedTrack,
    album_id: &str,
    album_name: &str,
    year: &str,
) -> Option<Track> {
    Some(Track {
        uri: t.id.as_ref()?.uri(),
        name: t.name.clone(),
        artists: artists_line(&t.artists),
        album: album_name.to_string(),
        release_year: year.to_string(),
        duration_ms: t.duration.num_milliseconds().max(0) as u64,
        track_number: t.track_number,
        album_id: Some(album_id.to_string()),
        artist_id: t
            .artists
            .first()
            .and_then(|a| a.id.as_ref())
            .map(|id| id.id().to_string()),
        // An album's track list does not repeat the album object per track,
        // and these rows name the album whose page they are already on.
        cover_url: None,
    })
}

fn playlist_from_simplified(p: &SimplifiedPlaylist) -> Playlist {
    Playlist {
        id: p.id.id().to_string(),
        name: p.name.clone(),
        track_count: p.items.total,
        owner: p
            .owner
            .display_name
            .clone()
            .unwrap_or_else(|| p.owner.id.id().to_string()),
        owner_id: p.owner.id.id().to_string(),
        snapshot_id: p.snapshot_id.clone(),
        // Comes back with the playlist itself, so opening one can show its
        // cover without a second round trip.
        cover_url: playlist_cover(&p.images),
        public: p.public,
        collaborative: p.collaborative,
    }
}

fn release_year(date: Option<&str>) -> String {
    date.map(|d| d.chars().take(4).collect())
        .unwrap_or_default()
}

fn album_from_simplified(a: &SimplifiedAlbum) -> Option<AlbumItem> {
    let id = a.id.as_ref()?;
    Some(AlbumItem {
        id: id.id().to_string(),
        name: a.name.clone(),
        artists: artists_line(&a.artists),
        release_year: release_year(a.release_date.as_deref()),
        album_type: a.album_type.clone().unwrap_or_default(),
        // Neither is modelled by rspotify's `SimplifiedAlbum`; only the artist
        // page's raw read (see `album_from_raw`) reports them.
        album_group: String::new(),
        track_count: 0,
        // Comes back with the album itself, so opening one can show its sleeve
        // without a second round trip.
        cover_url: crate::cover::pick_url(&a.images),
    })
}

fn album_from_raw(a: &RawAlbum) -> Option<AlbumItem> {
    Some(AlbumItem {
        id: a.id.clone()?,
        name: a.name.clone(),
        artists: a
            .artists
            .iter()
            .map(|x| x.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        release_year: release_year(a.release_date.as_deref()),
        album_type: a.album_type.clone().unwrap_or_default(),
        // Spotify sends `album_group` only on this endpoint. Where it is
        // absent the type is the closest thing to it — it agrees with the
        // group for everything except the records an artist merely guests on,
        // which this response would have labelled `appears_on`.
        album_group: a
            .album_group
            .clone()
            .or_else(|| a.album_type.clone())
            .unwrap_or_default(),
        track_count: a.total_tracks.unwrap_or(0),
        cover_url: crate::cover::pick_url(&a.images),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(url: &str) -> rspotify::model::Image {
        rspotify::model::Image {
            url: url.into(),
            width: Some(640),
            height: Some(640),
        }
    }

    /// Spotify escapes a description as if it were markup. A header band is
    /// not a browser, so the characters come back before it is drawn.
    #[test]
    fn a_description_arrives_unescaped() {
        assert_eq!(
            unescape_html("Your week&#x27;s mixtape &amp; deep cuts"),
            "Your week's mixtape & deep cuts"
        );
        assert_eq!(unescape_html("&lt;b&gt;bold&lt;/b&gt;"), "<b>bold</b>");
        assert_eq!(unescape_html("&#65;&#66;"), "AB");
        assert_eq!(unescape_html("no entities here"), "no entities here");
    }

    /// An ampersand that is not an entity is an ampersand. Descriptions are
    /// prose, and prose about "rock & roll" must survive being read.
    #[test]
    fn a_bare_ampersand_is_left_alone() {
        assert_eq!(unescape_html("rock & roll"), "rock & roll");
        assert_eq!(unescape_html("half an entity &amp"), "half an entity &amp");
        assert_eq!(unescape_html("&notreal; text"), "&notreal; text");
        assert_eq!(unescape_html("&#xZZ; text"), "&#xZZ; text");
        assert_eq!(unescape_html("&"), "&");
    }

    /// A mosaic is not a cover. The page takes none rather than a quilt of
    /// four sleeves at a sixth of the size each.
    #[test]
    fn a_playlist_takes_no_cover_from_a_mosaic() {
        assert_eq!(
            playlist_cover(&[image("https://i.scdn.co/image/dw")]).as_deref(),
            Some("https://i.scdn.co/image/dw")
        );
        assert_eq!(
            playlist_cover(&[image("https://mosaic.scdn.co/640/abc")]),
            None
        );
        assert_eq!(playlist_cover(&[]), None);
    }
}
