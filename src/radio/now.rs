//! Reads what a station is announcing, without listening to it.
//!
//! [`player`](super::player) learns a station's `StreamTitle` by decoding it;
//! this learns the same thing from the same servers for the cost of one short
//! request, so a page of stations can say what each is playing before you tune
//! into any of them.
//!
//! An Icecast server asked for metadata interleaves it into the audio: every
//! `icy-metaint` bytes come one length byte and, when that byte is not zero,
//! `len * 16` bytes of `Key='value';` pairs. So the whole probe is: send the
//! header, read one metadata block, hang up. Nothing is decoded and nothing is
//! kept.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};

/// How long a whole probe may take. Deliberately short: this runs over a page
/// of stations at once and answers a glance, not a play.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// The largest `icy-metaint` this will wait through. Servers use 8k or 16k;
/// anything far above that is a station whose first announcement is minutes of
/// audio away, which is longer than [`PROBE_TIMEOUT`] anyway.
const MAX_METAINT: usize = 64 * 1024;

/// What one station is saying, or [`None`] when it says nothing.
///
/// A server that answers without `icy-metaint` announces nothing and never
/// will, and is not an error: about four popular stations in ten are like this.
pub async fn probe(http: &reqwest::Client, url: &str) -> Result<Option<String>> {
    tokio::time::timeout(PROBE_TIMEOUT, read_title(http, url))
        .await
        .map_err(|_| anyhow!("the station did not answer in time"))?
}

async fn read_title(http: &reqwest::Client, url: &str) -> Result<Option<String>> {
    // A GET rather than a HEAD, for the reason `player::open` gives: many
    // Icecast servers answer HEAD with an HTML page carrying no icy headers at
    // all, so the cheaper request learns nothing.
    let mut res = http
        .get(url)
        .header("Icy-MetaData", "1")
        .send()
        .await
        .with_context(|| format!("could not reach the station: {url}"))?
        .error_for_status()
        .with_context(|| format!("the station refused the request: {url}"))?;

    let Some(metaint) = metadata_interval(res.headers()) else {
        return Ok(None);
    };

    // The audio before the first metadata block is read only to be counted:
    // the block sits at a fixed offset and there is no way to ask for it.
    let mut skipped = 0usize;
    let mut block: Vec<u8> = Vec::new();
    let mut want = 0usize;
    while let Some(chunk) = res.chunk().await.context("the station stopped sending")? {
        let mut rest = &chunk[..];
        if skipped < metaint {
            let take = rest.len().min(metaint - skipped);
            skipped += take;
            rest = &rest[take..];
            if skipped < metaint {
                continue;
            }
        }
        if block.is_empty() && want == 0 {
            let Some((&len, tail)) = rest.split_first() else {
                continue;
            };
            if len == 0 {
                // The station carries metadata but has nothing to say at this
                // moment. Reading on would mean another `metaint` bytes for a
                // second chance at the same answer.
                return Ok(None);
            }
            want = usize::from(len) * 16;
            rest = tail;
        }
        let take = rest.len().min(want - block.len());
        block.extend_from_slice(&rest[..take]);
        if block.len() == want {
            return Ok(parse_stream_title(&block));
        }
    }
    Err(anyhow!("the station sent no metadata"))
}

/// The `icy-metaint` header, when it names a usable interval.
fn metadata_interval(headers: &reqwest::header::HeaderMap) -> Option<usize> {
    let raw = headers.get("icy-metaint")?.to_str().ok()?;
    let metaint: usize = raw.trim().parse().ok()?;
    (1..=MAX_METAINT).contains(&metaint).then_some(metaint)
}

/// Lift the `StreamTitle` out of one metadata block.
///
/// The block is `Key='value';` pairs padded to a multiple of sixteen with NULs.
/// A blank title is dropped rather than reported, on the same reasoning the
/// live callback in [`player`](super::player) applies: a station that announces
/// an empty string is a station announcing nothing.
fn parse_stream_title(block: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(block);
    let start = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[start..];
    let end = rest.find("';").unwrap_or(rest.len());
    let title = rest[..end].trim_matches('\0').trim();
    (!title.is_empty()).then(|| title.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pad to the sixteen-byte multiple a server would send.
    fn block(text: &str) -> Vec<u8> {
        let mut bytes = text.as_bytes().to_vec();
        while !bytes.len().is_multiple_of(16) {
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn a_stream_title_is_lifted_out_of_its_padding() {
        let raw = block("StreamTitle='Kruder & Dorfmeister - High Noon';StreamUrl='';");
        assert_eq!(
            parse_stream_title(&raw).as_deref(),
            Some("Kruder & Dorfmeister - High Noon")
        );
    }

    #[test]
    fn a_title_is_read_when_it_is_the_only_field() {
        let raw = block("StreamTitle='The Cure - Pictures of You';");
        assert_eq!(
            parse_stream_title(&raw).as_deref(),
            Some("The Cure - Pictures of You")
        );
    }

    #[test]
    fn a_block_without_a_title_reads_as_nothing() {
        let raw = block("StreamUrl='https://example.org';");
        assert_eq!(parse_stream_title(&raw), None);
    }

    #[test]
    fn an_empty_title_reads_as_nothing() {
        let raw = block("StreamTitle='';StreamUrl='';");
        assert_eq!(parse_stream_title(&raw), None);
    }

    #[test]
    fn an_unterminated_title_still_reads() {
        let raw = block("StreamTitle='Half a record");
        assert_eq!(parse_stream_title(&raw).as_deref(), Some("Half a record"));
    }

    #[test]
    fn an_interval_is_taken_only_when_it_is_usable() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(metadata_interval(&headers), None);
        headers.insert("icy-metaint", "16000".parse().unwrap());
        assert_eq!(metadata_interval(&headers), Some(16000));
        headers.insert("icy-metaint", "0".parse().unwrap());
        assert_eq!(metadata_interval(&headers), None);
        headers.insert("icy-metaint", "1048576".parse().unwrap());
        assert_eq!(metadata_interval(&headers), None);
    }

    /// Reads a real station, which is the only check that covers the header,
    /// the interval and the block together. SomaFM Groove Salad: plain MP3
    /// over ICY, and the same station the player's live test uses.
    ///
    /// Run it with `cargo test radio::now::tests::live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_probe_reads_an_announcement() {
        let http = reqwest::Client::builder()
            .user_agent(concat!("spot/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap();
        let title = probe(&http, "https://ice2.somafm.com/groovesalad-128-mp3")
            .await
            .expect("the station should answer");
        println!("groove salad: {title:?}");
        assert!(title.is_some(), "groove salad announces its records");
    }
}
