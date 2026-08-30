//! What Wikipedia says about an artist, reached from their Spotify id.
//!
//! Spotify's Web API has no biography field — `artist.genres` is the nearest
//! thing it offers, and that is both deprecated and usually empty — so the
//! prose on an artist page comes from somewhere else. Four keyless requests
//! fetch it, and each of the three services asks for something in return,
//! which is honoured here: a `User-Agent` naming the app and a way to reach
//! whoever runs it, and no more than one request a second at MusicBrainz.
//!
//! ```text
//! open.spotify.com/artist/<id>  ->  MusicBrainz  ->  an MBID
//! MBID                          ->  MusicBrainz  ->  a Wikidata Q-id
//! Q-id                          ->  Wikidata     ->  an English article title
//! title                         ->  Wikipedia    ->  the lead section
//! ```
//!
//! Keyed on the Spotify id at every step, never on the artist's name.
//! MusicBrainz would happily answer a search for "Bush", and the answer would
//! reliably put one band's history under another band's photograph. A missing
//! biography is a blank row; a wrong one is a lie, and the length of this
//! chain is the price of not telling it.
//!
//! Hand-rolled over the `reqwest::Client` the app already owns, the way
//! [`crate::radio::api`] is: there are four calls, and none of them wants a
//! crate behind it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use tokio::time::Instant;

use crate::app::state::ArtistBio;

/// Names spot to both directories, with somewhere to complain to. MusicBrainz
/// blocks a client that gives only a version and says so in its terms; the
/// shared client's plain `spot/<version>` would qualify.
const AGENT: &str = concat!(
    "spot/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/ddmills/spot )"
);

/// What MusicBrainz asks of every client, and the one hard rule in this file.
const MUSICBRAINZ_INTERVAL: Duration = Duration::from_millis(1100);

const MUSICBRAINZ: &str = "https://musicbrainz.org/ws/2";
const WIKIDATA: &str = "https://www.wikidata.org/w/api.php";
const WIKIPEDIA: &str = "https://en.wikipedia.org/w/api.php";

/// The width MediaWiki is asked to render a lead image at. Above what the
/// expanded view decodes for, and small enough to arrive inside the fetcher's
/// own size cap.
const THUMB_PX: u32 = 640;

/// Reads Wikipedia on an artist's behalf, and remembers what it read.
#[derive(Clone)]
pub struct Wiki {
    http: reqwest::Client,
    /// Serialises the MusicBrainz calls and spaces them. Held across the wait
    /// and the request both, so two pages opened in quick succession queue
    /// rather than race — nothing on screen waits on a biography, and a
    /// promise kept exactly costs a page nothing.
    gate: Arc<tokio::sync::Mutex<Option<Instant>>>,
    /// Artists already looked up, including the ones that resolved to nothing.
    /// Caching the absence matters as much as caching the article: most of the
    /// long tail has no page, and rediscovering that would cost four requests
    /// and a second of throttle every time you opened it.
    known: Arc<Mutex<HashMap<String, Option<Arc<ArtistBio>>>>>,
}

impl Wiki {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            gate: Arc::new(tokio::sync::Mutex::new(None)),
            known: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The artist's Wikipedia lead section, or nothing.
    ///
    /// `Option` rather than `Result`: every way this can fail is the same
    /// failure to the page that asked — there is nothing to show — and one
    /// answer means the caller has one thing to do about all of them. What
    /// went wrong is logged on the way past.
    pub async fn artist_bio(&self, spotify_artist_id: &str) -> Option<Arc<ArtistBio>> {
        if let Some(known) = self.known.lock().get(spotify_artist_id) {
            return known.clone();
        }
        let bio = self.resolve(spotify_artist_id).await;
        self.known
            .lock()
            .insert(spotify_artist_id.to_string(), bio.clone());
        bio
    }

    async fn resolve(&self, spotify_artist_id: &str) -> Option<Arc<ArtistBio>> {
        let mbid = self.musicbrainz_id(spotify_artist_id).await?;
        let target = self.article_target(&mbid).await?;
        // Wikidata first, and the legacy link only where it leads nowhere: an
        // article can be renamed out from under a `wikipedia` relation, and
        // the Q-id survives that.
        let title = match &target {
            Target::Wikidata(qid, legacy) => match self.english_title(qid).await {
                Some(title) => title,
                None => legacy.clone()?,
            },
            Target::Article(title) => title.clone(),
        };
        self.lead_section(&title).await.map(Arc::new)
    }

    /// One MusicBrainz call, no sooner than [`MUSICBRAINZ_INTERVAL`] after the
    /// last one.
    async fn musicbrainz(&self, url: String) -> Option<String> {
        let mut gate = self.gate.lock().await;
        if let Some(last) = *gate {
            let waited = last.elapsed();
            if waited < MUSICBRAINZ_INTERVAL {
                tokio::time::sleep(MUSICBRAINZ_INTERVAL - waited).await;
            }
        }
        let body = self.get(&url).await;
        *gate = Some(Instant::now());
        body
    }

    async fn get(&self, url: &str) -> Option<String> {
        match self
            .http
            .get(url)
            .header(reqwest::header::USER_AGENT, AGENT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(res) => res.text().await.ok(),
            // Quiet at debug: an artist no directory has heard of is the
            // ordinary case, not a fault worth a line in the log for.
            Err(e) => {
                log::debug!("wiki lookup failed for {url}: {e}");
                None
            }
        }
    }

    /// Step one: the Spotify link, as MusicBrainz has it filed.
    async fn musicbrainz_id(&self, spotify_artist_id: &str) -> Option<String> {
        let url = format!(
            "{MUSICBRAINZ}/url?resource=https://open.spotify.com/artist/{spotify_artist_id}&inc=artist-rels&fmt=json"
        );
        artist_of(&self.musicbrainz(url).await?)
    }

    /// Step two: where that artist's relations point.
    async fn article_target(&self, mbid: &str) -> Option<Target> {
        let url = format!("{MUSICBRAINZ}/artist/{mbid}?inc=url-rels&fmt=json");
        target_of(&self.musicbrainz(url).await?)
    }

    /// Step three: the English article a Wikidata item links to. Not
    /// throttled — the one-a-second rule is MusicBrainz's alone.
    async fn english_title(&self, qid: &str) -> Option<String> {
        let url = format!(
            "{WIKIDATA}?action=wbgetentities&ids={qid}&props=sitelinks&sitefilter=enwiki&format=json&formatversion=2"
        );
        english_title_of(&self.get(&url).await?, qid)
    }

    /// Step four: the article's lead section, and the picture at the head of
    /// it.
    async fn lead_section(&self, title: &str) -> Option<ArtistBio> {
        let url = format!(
            "{WIKIPEDIA}?action=query&format=json&formatversion=2&redirects=1&prop=extracts%7Cpageimages&exintro=1&explaintext=1&piprop=thumbnail&pithumbsize={THUMB_PX}&titles={}",
            encode(title)
        );
        lead_section_of(&self.get(&url).await?)
    }
}

/// Where an artist's relations point, in the order they are trusted.
///
/// A Wikidata item carries the legacy link beside it rather than replacing it,
/// because the item may have no English article and the older relation is then
/// the better answer rather than a worse one.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    Wikidata(String, Option<String>),
    Article(String),
}

#[derive(Deserialize)]
struct UrlLookup {
    #[serde(default)]
    relations: Vec<UrlRelation>,
}

#[derive(Deserialize)]
struct UrlRelation {
    #[serde(default)]
    artist: Option<MbEntity>,
}

#[derive(Deserialize)]
struct MbEntity {
    id: String,
}

fn artist_of(body: &str) -> Option<String> {
    let lookup: UrlLookup = serde_json::from_str(body).ok()?;
    lookup
        .relations
        .into_iter()
        .find_map(|r| r.artist.map(|a| a.id))
}

#[derive(Deserialize)]
struct ArtistLookup {
    #[serde(default)]
    relations: Vec<ArtistRelation>,
}

#[derive(Deserialize)]
struct ArtistRelation {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    url: Option<MbUrl>,
}

#[derive(Deserialize)]
struct MbUrl {
    #[serde(default)]
    resource: String,
}

fn target_of(body: &str) -> Option<Target> {
    let lookup: ArtistLookup = serde_json::from_str(body).ok()?;
    let mut qid = None;
    let mut legacy = None;
    for relation in &lookup.relations {
        let Some(url) = relation.url.as_ref().map(|u| u.resource.as_str()) else {
            continue;
        };
        match relation.kind.as_str() {
            "wikidata" => qid = qid.or_else(|| qid_of(url)),
            "wikipedia" => legacy = legacy.or_else(|| title_of(url)),
            _ => {}
        }
    }
    match qid {
        Some(qid) => Some(Target::Wikidata(qid, legacy)),
        None => legacy.map(Target::Article),
    }
}

/// The item a `wikidata.org/wiki/Q…` address names.
fn qid_of(url: &str) -> Option<String> {
    let id = url.strip_prefix("https://www.wikidata.org/wiki/")?;
    let is_qid = id.len() > 1 && id.starts_with('Q') && id[1..].bytes().all(|b| b.is_ascii_digit());
    is_qid.then(|| id.to_string())
}

/// The article an `en.wikipedia.org/wiki/…` address names.
///
/// A percent-encoded title is refused rather than decoded. One only ever
/// arrives on the legacy relation, which is already the fallback's fallback,
/// and a decoder for that path is more code than the artists it would reach
/// are worth. Wikidata's own sitelink comes back decoded, so the main path
/// never meets one.
fn title_of(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://en.wikipedia.org/wiki/")?;
    if path.is_empty() || path.contains('%') || path.contains('/') {
        return None;
    }
    Some(path.replace('_', " "))
}

#[derive(Deserialize)]
struct WikidataReply {
    #[serde(default)]
    entities: HashMap<String, WikidataEntity>,
}

#[derive(Deserialize, Default)]
struct WikidataEntity {
    /// Defaulted because MediaWiki serialises an empty map as `[]`, which a
    /// map deserialize refuses outright — and an item with no English article
    /// is exactly the shape that happens on.
    #[serde(default)]
    sitelinks: HashMap<String, Sitelink>,
}

#[derive(Deserialize)]
struct Sitelink {
    title: String,
}

fn english_title_of(body: &str, qid: &str) -> Option<String> {
    let reply: WikidataReply = serde_json::from_str(body).ok()?;
    let entity = reply.entities.get(qid)?;
    Some(entity.sitelinks.get("enwiki")?.title.clone())
}

/// `formatversion=2` is the only reason `pages` is an array here rather than a
/// map keyed by page id. It is asked for so this shape can be a list of one.
#[derive(Deserialize)]
struct WikipediaReply {
    #[serde(default)]
    query: WikipediaQuery,
}

#[derive(Deserialize, Default)]
struct WikipediaQuery {
    #[serde(default)]
    pages: Vec<WikipediaPage>,
}

#[derive(Deserialize)]
struct WikipediaPage {
    #[serde(default)]
    title: String,
    #[serde(default)]
    extract: String,
    #[serde(default)]
    missing: bool,
    #[serde(default)]
    thumbnail: Option<Thumbnail>,
}

#[derive(Deserialize)]
struct Thumbnail {
    #[serde(default)]
    source: String,
}

fn lead_section_of(body: &str) -> Option<ArtistBio> {
    let reply: WikipediaReply = serde_json::from_str(body).ok()?;
    let page = reply.query.pages.into_iter().next()?;
    // A redlink, or a disambiguation stub with no lead of its own. Both are
    // the same nothing.
    if page.missing {
        return None;
    }
    let text = tidy(&page.extract);
    if text.is_empty() {
        return None;
    }
    Some(ArtistBio {
        text,
        image_url: page
            .thumbnail
            .map(|t| t.source)
            .filter(|u| crate::cover::is_wikimedia_thumb(u)),
        // The prose is CC BY-SA, and this is where the credit for it lives.
        source_url: format!(
            "https://en.wikipedia.org/wiki/{}",
            page.title.replace(' ', "_")
        ),
    })
}

/// Trim the extract and collapse the runs of blank lines MediaWiki leaves
/// between paragraphs down to the single break the wrapper reads as one.
fn tidy(extract: &str) -> String {
    let mut out = String::with_capacity(extract.len());
    let mut blanks = 0;
    for line in extract.trim().split('\n') {
        let line = line.trim_end();
        if line.is_empty() {
            blanks += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blanks > 0 {
                out.push('\n');
            }
        }
        out.push_str(line);
        blanks = 0;
    }
    out
}

/// Percent-encode an article title for a query string.
///
/// Only the characters a title can hold that a URL cannot; a space becomes the
/// underscore Wikipedia spells its own addresses with. Everything else passes
/// through, which keeps a readable address readable in the log.
fn encode(title: &str) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(title.len());
    for b in title.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('_'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL_LOOKUP: &str = r#"{"relations":[{"type":"free streaming","target-type":"artist",
        "artist":{"id":"9c9f1380-2516-4fc9-a3e6-f9f61941d090","name":"Muse"}}],
        "id":"29d05957-4215-4265-a30c-6d0b60da0751"}"#;

    #[test]
    fn a_url_lookup_names_the_artist() {
        assert_eq!(
            artist_of(URL_LOOKUP).as_deref(),
            Some("9c9f1380-2516-4fc9-a3e6-f9f61941d090")
        );
    }

    /// A link MusicBrainz has filed against no artist, and a body that is not
    /// the shape at all. Both are the same nothing.
    #[test]
    fn a_url_lookup_without_an_artist_names_nobody() {
        assert_eq!(artist_of(r#"{"relations":[]}"#), None);
        assert_eq!(artist_of(r#"{"error":"Not Found"}"#), None);
        assert_eq!(artist_of("<html>"), None);
    }

    fn relations(body: &str) -> Option<Target> {
        target_of(body)
    }

    #[test]
    fn wikidata_is_preferred_over_the_legacy_link() {
        let both = r#"{"relations":[
            {"type":"wikipedia","url":{"resource":"https://en.wikipedia.org/wiki/Muse_(band)"}},
            {"type":"wikidata","url":{"resource":"https://www.wikidata.org/wiki/Q22151"}}]}"#;
        assert_eq!(
            relations(both),
            Some(Target::Wikidata(
                "Q22151".into(),
                Some("Muse (band)".into())
            ))
        );
    }

    #[test]
    fn the_legacy_link_is_used_where_nothing_newer_is() {
        let old = r#"{"relations":[
            {"type":"wikipedia","url":{"resource":"https://en.wikipedia.org/wiki/Muse_(band)"}}]}"#;
        assert_eq!(relations(old), Some(Target::Article("Muse (band)".into())));
    }

    #[test]
    fn relations_pointing_nowhere_resolve_to_nothing() {
        let other = r#"{"relations":[
            {"type":"official homepage","url":{"resource":"https://muse.mu"}}]}"#;
        assert_eq!(relations(other), None);
        assert_eq!(relations(r#"{"relations":[]}"#), None);
    }

    #[test]
    fn a_wikidata_address_yields_its_item() {
        assert_eq!(
            qid_of("https://www.wikidata.org/wiki/Q22151").as_deref(),
            Some("Q22151")
        );
        // A foreign host, a property rather than an item, and a bare Q.
        assert_eq!(qid_of("https://evil.com/wiki/Q22151"), None);
        assert_eq!(qid_of("https://www.wikidata.org/wiki/P434"), None);
        assert_eq!(qid_of("https://www.wikidata.org/wiki/Q"), None);
    }

    #[test]
    fn a_wikipedia_address_yields_its_title() {
        assert_eq!(
            title_of("https://en.wikipedia.org/wiki/Muse_(band)").as_deref(),
            Some("Muse (band)")
        );
        // Another language, and a title spelt in escapes this refuses to
        // decode rather than carry a decoder for.
        assert_eq!(title_of("https://de.wikipedia.org/wiki/Muse"), None);
        assert_eq!(
            title_of("https://en.wikipedia.org/wiki/Sigur_R%C3%B3s"),
            None
        );
    }

    #[test]
    fn a_wikidata_entity_names_its_english_article() {
        let body = r#"{"entities":{"Q22151":{"type":"item","id":"Q22151",
            "sitelinks":{"enwiki":{"site":"enwiki","title":"Muse (band)","badges":[]}}}},
            "success":1}"#;
        assert_eq!(
            english_title_of(body, "Q22151").as_deref(),
            Some("Muse (band)")
        );
    }

    /// An item with no English article. MediaWiki spells the empty map as an
    /// empty list, which is the whole reason `sitelinks` is defaulted.
    #[test]
    fn an_entity_without_an_english_article_resolves_to_nothing() {
        let body = r#"{"entities":{"Q1":{"type":"item","id":"Q1","sitelinks":[]}},"success":1}"#;
        assert_eq!(english_title_of(body, "Q1"), None);
        assert_eq!(english_title_of(body, "Q2"), None);
    }

    const PAGE: &str = r#"{"batchcomplete":true,"query":{"pages":[{"pageid":178244,"ns":0,
        "title":"Muse (band)",
        "extract":"Muse are an English rock band.\n\n\nThey formed in 1994.",
        "thumbnail":{"source":
        "https://upload.wikimedia.org/wikipedia/commons/thumb/3/33/M.jpg/960px-M.jpg",
        "width":640,"height":360}}]}}"#;

    #[test]
    fn a_page_yields_its_lead_and_its_picture() {
        let bio = lead_section_of(PAGE).expect("a page with an extract");
        assert_eq!(
            bio.text,
            "Muse are an English rock band.\n\nThey formed in 1994."
        );
        assert!(bio.image_url.is_some());
        assert_eq!(bio.source_url, "https://en.wikipedia.org/wiki/Muse_(band)");
    }

    /// The decoder reads JPEG and nothing else, so a wordmark rendered to PNG
    /// contributes no picture and the text arrives on its own.
    #[test]
    fn a_picture_the_decoder_cannot_read_is_left_behind() {
        let png = PAGE.replace(".jpg/960px-M.jpg", ".png/960px-M.png");
        let bio = lead_section_of(&png).expect("a page with an extract");
        assert!(bio.image_url.is_none());
        assert!(!bio.text.is_empty());
    }

    #[test]
    fn a_missing_page_yields_no_bio() {
        let missing = r#"{"query":{"pages":[{"ns":0,"title":"Nobody","missing":true}]}}"#;
        assert!(lead_section_of(missing).is_none());
        let empty = r#"{"query":{"pages":[{"ns":0,"title":"Stub","extract":"  "}]}}"#;
        assert!(lead_section_of(empty).is_none());
        assert!(lead_section_of(r#"{"query":{"pages":[]}}"#).is_none());
    }

    #[test]
    fn paragraphs_keep_one_break_between_them() {
        assert_eq!(tidy("  one\n\n\n\ntwo\n  "), "one\n\ntwo");
        assert_eq!(tidy("one\ntwo"), "one\ntwo");
        assert_eq!(tidy("\n\n"), "");
    }

    #[test]
    fn a_title_survives_the_query_string() {
        assert_eq!(encode("Muse (band)"), "Muse_%28band%29");
        assert_eq!(encode("Sigur Ros"), "Sigur_Ros");
        assert_eq!(encode("Beyonce"), "Beyonce");
    }

    /// Talks to MusicBrainz, Wikidata and Wikipedia. Ignored by default: the
    /// suite must not need a network, and MusicBrainz is a volunteer service
    /// that asks for one request a second and would get one from every
    /// `cargo test` otherwise. Run
    /// `cargo test wiki::tests::live_ -- --ignored --nocapture` after touching
    /// anything in this file — the wire format is the part of this module no
    /// unit test can vouch for.
    #[tokio::test]
    #[ignore]
    async fn live_chain_reaches_an_article() {
        let wiki = Wiki::new(reqwest::Client::new());
        // Radiohead.
        let bio = wiki
            .artist_bio("4Z8W4fKeB5YxbusRsdQVPb")
            .await
            .expect("Radiohead have an article");
        assert!(bio.text.len() > 200, "a lead section is more than a line");
        assert!(bio.source_url.starts_with("https://en.wikipedia.org/wiki/"));
        if let Some(url) = &bio.image_url {
            assert!(crate::cover::is_wikimedia_thumb(url));
        }
        // The second ask is the cache, and costs neither a request nor the
        // throttle.
        let again = wiki.artist_bio("4Z8W4fKeB5YxbusRsdQVPb").await;
        assert!(again.is_some());
    }
}
