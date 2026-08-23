use anyhow::{Context, Result};
use rspotify::clients::{BaseClient, OAuthClient};
use rspotify::http::Query;
use rspotify::model::{
    AlbumId, ArtistId, FullTrack, Id, LibraryId, PlayableItem, PlaylistId, SearchResult,
    SearchType, SimplifiedAlbum, SimplifiedPlaylist, TrackId,
};
use rspotify::{AuthCodeSpotify, Token};
use serde::Deserialize;

use crate::app::state::{AlbumItem, ArtistItem, Playlist, SearchResults, Track};

pub const PAGE_LIMIT: u32 = 50;
/// Pages of an artist's catalogue to walk before giving up on the rest.
const ARTIST_ALBUM_PAGES: u32 = 4;
const MAX_PLAYLISTS: u32 = 1000;

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

    /// The signed-in user's Spotify id.
    ///
    /// Fetched alongside the playlists, which is the only thing that wants it:
    /// the Playlists page blanks the Owner cell for the ones you own, and it
    /// needs an id rather than a display name to tell which those are.
    pub async fn me_id(&self) -> Result<String> {
        let me = self
            .client
            .me()
            .await
            .context("failed to fetch the current user")?;
        Ok(me.id.id().to_string())
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
