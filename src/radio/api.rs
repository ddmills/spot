//! Client for the Radio Browser directory (<https://api.radio-browser.info>).
//!
//! Radio Browser is the last free, keyless, genuinely global station directory
//! left standing: Xiph's has been empty for years, Dirble shut down in 2019,
//! and TuneIn, Shoutcast and iHeart are all contract- or key-gated. It asks two
//! things in return, and both are honoured here — a real `User-Agent` naming
//! the app, and a click report when a station is played, which is the only
//! ranking signal the community has.
//!
//! Hand-rolled over the `reqwest::Client` the app already owns rather than
//! taking the `radiobrowser` crate, which has not been published since 2023.
//! There are eight calls.

use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::Deserialize;

use crate::app::state::Station;

/// Where the list of mirrors comes from. Not itself a mirror — it is a DNS
/// round-robin over all of them, and answering `/json/servers` is all it is
/// used for here.
const DISCOVERY_URL: &str = "https://all.api.radio-browser.info/json/servers";

/// Used when discovery fails. A hard-coded mirror is a worse answer than a
/// discovered one and a much better answer than no radio at all.
const FALLBACK_HOST: &str = "https://de1.api.radio-browser.info";

/// Rows fetched for a station list. Deep enough to scroll through, shallow
/// enough to arrive in one request — the directory's own default limit is
/// 100,000, which is not a page.
pub const STATION_LIMIT: u32 = 300;

/// Rows fetched for a facet list (countries, genres).
const FACET_LIMIT: u32 = 200;

/// How the directory reports a station.
///
/// Deserialized separately from [`Station`] because the wire format is not one
/// a UI wants to hold: three of its flags are integers spelled `0`/`1` while a
/// fourth in the same object is a real bool, the tag and language lists are
/// comma-joined strings, and half the numeric fields are absent on HLS rows.
#[derive(Debug, Deserialize)]
struct RawStation {
    stationuuid: String,
    name: String,
    /// The directory has already followed `.pls`/`.m3u` wrappers into this,
    /// which is why [`Station::url`] is built from it and not from `url`.
    #[serde(default)]
    url_resolved: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    country: String,
    #[serde(default)]
    countrycode: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    codec: String,
    #[serde(default)]
    bitrate: u32,
    #[serde(default)]
    votes: u32,
    /// `0` or `1`, not a bool — unlike `has_extended_info` beside it.
    #[serde(default)]
    hls: u8,
}

impl From<RawStation> for Station {
    fn from(r: RawStation) -> Self {
        let url = if r.url_resolved.is_empty() {
            r.url
        } else {
            r.url_resolved
        };
        Station {
            uuid: r.stationuuid,
            name: r.name,
            url,
            homepage: r.homepage,
            tags: r.tags,
            country: r.country,
            countrycode: r.countrycode,
            language: r.language,
            codec: r.codec,
            bitrate: r.bitrate,
            votes: r.votes,
            hls: r.hls != 0,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawServer {
    name: String,
}

/// A country or tag, with how many stations carry it.
///
/// `code` is only ever set for countries: `/json/countries` reports the ISO
/// code beside the name, so the list can read "Germany" while the query it
/// runs is still `bycountrycodeexact/DE`. Tags have no such pairing and are
/// queried by the name itself.
#[derive(Debug, Clone)]
pub struct Facet {
    pub name: String,
    pub code: String,
    pub stationcount: u32,
}

#[derive(Debug, Deserialize)]
struct RawCountry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    iso_3166_1: String,
    #[serde(default)]
    stationcount: u32,
}

#[derive(Debug, Deserialize)]
struct RawTag {
    #[serde(default)]
    name: String,
    #[serde(default)]
    stationcount: u32,
}

/// The click report's reply. Only the resolved URL is read back.
#[derive(Debug, Deserialize)]
struct ClickReply {
    #[serde(default)]
    url: String,
}

/// The directory, pinned to one mirror for the life of the process.
#[derive(Clone)]
pub struct RadioApi {
    http: reqwest::Client,
    /// Resolved on first use and kept: every mirror carries the same data, and
    /// hopping between them mid-session would only spread this client's
    /// traffic over more of the volunteers hosting them.
    host: Arc<Mutex<Option<String>>>,
}

impl RadioApi {
    /// Shares the caller's HTTP client, which is already built with
    /// `spot/<version>` as its user agent — exactly the form the directory
    /// requires, and the reason one is not built here.
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            host: Arc::new(Mutex::new(None)),
        }
    }

    /// The mirror to talk to, discovering one on first use.
    async fn host(&self) -> String {
        if let Some(host) = self.host.lock().clone() {
            return host;
        }
        let host = self
            .discover()
            .await
            .unwrap_or_else(|| FALLBACK_HOST.to_string());
        // A concurrent caller may have resolved one first. Either answer is as
        // good as the other, so whichever landed first wins and the duplicate
        // discovery is simply wasted.
        let mut slot = self.host.lock();
        slot.get_or_insert(host).clone()
    }

    async fn discover(&self) -> Option<String> {
        let servers: Vec<RawServer> = self
            .http
            .get(DISCOVERY_URL)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let name = servers
            .into_iter()
            .map(|s| s.name)
            .find(|n| !n.is_empty())?;
        Some(format!("https://{name}"))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = format!("{}/json/{path}", self.host().await);
        let res = self
            .http
            .get(&url)
            .query(query)
            .send()
            .await
            .with_context(|| format!("radio directory request failed: {path}"))?
            .error_for_status()
            .with_context(|| format!("radio directory returned an error: {path}"))?;
        res.json()
            .await
            .with_context(|| format!("could not read the radio directory's reply: {path}"))
    }

    /// Every station query goes through here, so `hidebroken` can never be
    /// forgotten: 5,498 of the directory's 57,435 entries are known-dead, and a
    /// list of stations that do not play is worse than a short one.
    async fn stations(&self, path: &str, extra: &[(&str, String)]) -> Result<Vec<Station>> {
        let mut query = vec![
            ("hidebroken", "true".to_string()),
            ("limit", STATION_LIMIT.to_string()),
            ("order", "votes".to_string()),
            ("reverse", "true".to_string()),
        ];
        query.extend(extra.iter().cloned());
        let raw: Vec<RawStation> = self.get_json(path, &query).await?;
        Ok(raw.into_iter().map(Station::from).collect())
    }

    /// The directory's own chart, which is as close to an editorial front page
    /// as a community database gets.
    pub async fn top_voted(&self) -> Result<Vec<Station>> {
        self.stations(&format!("stations/topvote/{STATION_LIMIT}"), &[])
            .await
    }

    pub async fn search(&self, name: &str) -> Result<Vec<Station>> {
        self.stations("stations/search", &[("name", name.to_string())])
            .await
    }

    /// By ISO 3166-1 alpha-2 code rather than country name: the names are free
    /// text, and include such things as "The United Kingdom Of Great Britain
    /// And Northern Ireland".
    pub async fn by_country(&self, code: &str) -> Result<Vec<Station>> {
        self.stations(
            &format!("stations/bycountrycodeexact/{}", urlish(code)),
            &[],
        )
        .await
    }

    pub async fn by_tag(&self, tag: &str) -> Result<Vec<Station>> {
        self.stations(&format!("stations/bytagexact/{}", urlish(tag)), &[])
            .await
    }

    /// Countries, most stations first.
    ///
    /// `/json/countries` rather than `/json/countrycodes` because it reports
    /// the name as well as the code, and a list of "US · DE · RU" is not a
    /// list anybody can browse. Rows without a code are dropped: several
    /// thousand stations are filed under no country at all, and there is
    /// nothing to query for them.
    pub async fn countries(&self) -> Result<Vec<Facet>> {
        let raw: Vec<RawCountry> = self
            .get_json(
                "countries",
                &[
                    ("order", "stationcount".to_string()),
                    ("reverse", "true".to_string()),
                    ("hidebroken", "true".to_string()),
                ],
            )
            .await?;
        let mut facets: Vec<Facet> = raw
            .into_iter()
            .filter(|c| !c.iso_3166_1.trim().is_empty() && !c.name.trim().is_empty())
            .map(|c| Facet {
                name: c.name,
                code: c.iso_3166_1,
                stationcount: c.stationcount,
            })
            .collect();
        facets.truncate(FACET_LIMIT as usize);
        Ok(facets)
    }

    /// The most-used tags. There are nearly 12,000 of them, most used once; the
    /// head of that distribution is the only part that reads as a genre list.
    pub async fn genres(&self) -> Result<Vec<Facet>> {
        let raw: Vec<RawTag> = self
            .get_json(
                "tags",
                &[
                    ("order", "stationcount".to_string()),
                    ("reverse", "true".to_string()),
                    ("hidebroken", "true".to_string()),
                    ("limit", FACET_LIMIT.to_string()),
                ],
            )
            .await?;
        Ok(raw
            .into_iter()
            .filter(|t| !t.name.trim().is_empty())
            .map(|t| Facet {
                code: t.name.clone(),
                name: t.name,
                stationcount: t.stationcount,
            })
            .collect())
    }

    /// Report a play, and take the directory's word for the stream URL.
    ///
    /// This is the click that ranks the charts every other call here reads.
    /// spot takes from the directory on every screen of this feature, so it
    /// reports back. A click counts once per station per address per day, so
    /// calling it on every play costs the servers nothing.
    ///
    /// Failure is not something the user needs told: the caller falls back to
    /// the URL it already has.
    pub async fn click(&self, uuid: &str) -> Option<String> {
        let reply: ClickReply = self
            .get_json(&format!("url/{}", urlish(uuid)), &[])
            .await
            .ok()?;
        (!reply.url.is_empty()).then_some(reply.url)
    }
}

/// Percent-encode a path segment. Tags and country codes are the only things
/// interpolated into a path here, and they are short and space-separated at
/// worst — but a tag like `drum&bass` would otherwise end the path.
fn urlish(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_station_prefers_the_resolved_url() {
        let raw: RawStation = serde_json::from_str(
            r#"{"stationuuid":"u","name":"n","url":"http://wrapper.pls",
                "url_resolved":"http://stream/mount","hls":0}"#,
        )
        .unwrap();
        let station = Station::from(raw);
        assert_eq!(station.url, "http://stream/mount");
        assert!(!station.hls);
    }

    #[test]
    fn raw_station_falls_back_to_url_when_unresolved() {
        let raw: RawStation = serde_json::from_str(
            r#"{"stationuuid":"u","name":"n","url":"http://stream/mount","url_resolved":""}"#,
        )
        .unwrap();
        assert_eq!(Station::from(raw).url, "http://stream/mount");
    }

    /// The flags are integers and `codec`/`bitrate` are routinely missing or
    /// meaningless on HLS rows; such a station must still parse.
    #[test]
    fn raw_station_tolerates_the_sparse_hls_shape() {
        let raw: RawStation = serde_json::from_str(
            r#"{"stationuuid":"u","name":"BBC Radio 6 Music",
                "url_resolved":"http://x/y.m3u8","hls":1,"codec":"UNKNOWN","bitrate":0,
                "geo_lat":null,"serveruuid":null,"has_extended_info":false}"#,
        )
        .unwrap();
        let station = Station::from(raw);
        assert!(station.hls);
        assert_eq!(station.bitrate, 0);
        assert_eq!(station.votes, 0);
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(urlish("classic rock"), "classic%20rock");
        assert_eq!(urlish("GB"), "GB");
        assert_eq!(urlish("drum&bass"), "drum%26bass");
    }

    fn live() -> RadioApi {
        RadioApi::new(
            reqwest::Client::builder()
                .user_agent(concat!("spot/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap(),
        )
    }

    /// Talks to the real directory. Ignored by default — the suite must not
    /// need a network, and the volunteers hosting these mirrors should not be
    /// hit by every `cargo test`. Run it with
    /// `cargo test radio::api::tests::live_ -- --ignored --nocapture` after
    /// touching anything in this file: the wire format is the part of this
    /// module that no unit test can vouch for.
    #[tokio::test]
    #[ignore]
    async fn live_directory_answers_every_call() {
        let api = live();

        let top = api.top_voted().await.expect("topvote");
        assert!(top.len() > 50, "a thin chart means the query is wrong");
        assert!(
            top.iter().all(|s| !s.url.is_empty()),
            "every row must carry a stream URL"
        );

        let hits = api.search("jazz").await.expect("search");
        assert!(!hits.is_empty());

        let countries = api.countries().await.expect("countries");
        let gb = countries
            .iter()
            .find(|f| f.code == "GB")
            .expect("GB is in the directory");
        // The reason `/json/countries` is used over `/json/countrycodes`.
        assert!(gb.name.len() > 2, "a country needs a name, not a code");

        let genres = api.genres().await.expect("genres");
        assert!(genres.iter().any(|f| f.name == "pop"));

        let uk = api.by_country("GB").await.expect("by_country");
        assert!(uk.iter().all(|s| s.countrycode == "GB"));

        let jazz = api.by_tag("jazz").await.expect("by_tag");
        assert!(!jazz.is_empty());
    }
}
