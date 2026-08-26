//! Reading a Spotify link, from wherever it came in.
//!
//! Two spellings reach spot and neither is under its control. `spotify:` URIs
//! arrive from the protocol handler, which is what a chat client or the web
//! player's "Open in app" hands to Windows. `https://open.spotify.com/...`
//! URLs arrive by paste, because Windows can route a scheme to an app but
//! cannot route an https host — a link copied out of a browser has no other
//! way in.
//!
//! Everything here treats its input as hostile. A link may come off the named
//! pipe, which anything running as the user may write to, so an id is checked
//! against the shape Spotify uses before it can reach the API.

/// A Spotify link spot knows how to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Track(String),
    Album(String),
    Artist(String),
    Playlist(String),
}

impl Link {
    /// The canonical `spotify:` spelling.
    ///
    /// What spot hands to a copy of itself across the Windows Terminal bounce
    /// and across the pipe, rather than the argument it was given: this has
    /// already been read and checked, and the far end reads it with the same
    /// [`parse`] either way.
    pub fn to_uri(&self) -> String {
        let (kind, id) = self.parts();
        format!("spotify:{kind}:{id}")
    }

    /// The `https://open.spotify.com/...` spelling, for the share controls.
    ///
    /// The URL rather than the URI, because a shared link is read by whatever
    /// the far end has: a browser opens this one, and the desktop app is
    /// offered it from there. A `spotify:` URI pasted into anything but an app
    /// that already knows the scheme is an inert string.
    pub fn to_url(&self) -> String {
        let (kind, id) = self.parts();
        format!("https://open.spotify.com/{kind}/{id}")
    }

    fn parts(&self) -> (&'static str, &str) {
        match self {
            Link::Track(id) => ("track", id),
            Link::Album(id) => ("album", id),
            Link::Artist(id) => ("artist", id),
            Link::Playlist(id) => ("playlist", id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A Spotify link to something spot does not play. Named, so the caller
    /// can say why rather than failing silently.
    Unsupported(&'static str),
    /// Not a Spotify link at all. The search prompt reads this as "the user
    /// typed a query", so it must not be returned for a link that is merely
    /// malformed.
    NotALink,
}

/// Length of the base62 id at the tail of every Spotify link.
const ID_LEN: usize = 22;

/// Read a link, in either spelling.
///
/// A bare id is deliberately refused: 22 letters is a plausible search, and a
/// query silently becoming a track load is worse than a search that finds
/// nothing.
pub fn parse(input: &str) -> Result<Link, ParseError> {
    let input = input.trim();
    let (kind, id) = match split_uri(input) {
        Some(pair) => pair,
        None => split_url(input).ok_or(ParseError::NotALink)?,
    };
    if !is_id(id) {
        return Err(ParseError::NotALink);
    }
    match kind {
        "track" => Ok(Link::Track(id.to_string())),
        "album" => Ok(Link::Album(id.to_string())),
        "artist" => Ok(Link::Artist(id.to_string())),
        "playlist" => Ok(Link::Playlist(id.to_string())),
        "show" | "episode" => Err(ParseError::Unsupported("podcasts")),
        _ => Err(ParseError::NotALink),
    }
}

/// The kind and id of a `spotify:` URI.
///
/// The last two segments, rather than the second and third, because the
/// playlist URI Spotify wrote for years names the owner first:
/// `spotify:user:<name>:playlist:<id>`. Those links are still in circulation.
fn split_uri(input: &str) -> Option<(&str, &str)> {
    let rest = input.strip_prefix("spotify:")?;
    let mut segments = rest.rsplit(':');
    let id = segments.next()?;
    let kind = segments.next()?;
    Some((kind, id))
}

/// The kind and id of an `open.spotify.com` URL.
fn split_url(input: &str) -> Option<(&str, &str)> {
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest.strip_prefix("open.spotify.com/")?;
    // A shared link carries `?si=` and may carry a fragment. Neither says
    // anything about what the link points at.
    let path = path.split(['?', '#']).next()?;

    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let first = segments.next()?;
    // Spotify localizes a share link by prefixing the path with `intl-de` and
    // the like. The kind follows it.
    let kind = if first.starts_with("intl-") {
        segments.next()?
    } else {
        first
    };
    let id = segments.next()?;
    Some((kind, id))
}

fn is_id(candidate: &str) -> bool {
    candidate.len() == ID_LEN && candidate.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";
    const OTHER: &str = "1301WleyT98MSxVHPZCA6M";

    #[test]
    fn reads_every_uri_kind() {
        assert_eq!(
            parse(&format!("spotify:track:{ID}")),
            Ok(Link::Track(ID.into()))
        );
        assert_eq!(
            parse(&format!("spotify:album:{ID}")),
            Ok(Link::Album(ID.into()))
        );
        assert_eq!(
            parse(&format!("spotify:artist:{ID}")),
            Ok(Link::Artist(ID.into()))
        );
        assert_eq!(
            parse(&format!("spotify:playlist:{ID}")),
            Ok(Link::Playlist(ID.into()))
        );
    }

    #[test]
    fn reads_the_legacy_playlist_uri() {
        assert_eq!(
            parse(&format!("spotify:user:someone:playlist:{ID}")),
            Ok(Link::Playlist(ID.into()))
        );
    }

    #[test]
    fn reads_a_url() {
        assert_eq!(
            parse(&format!("https://open.spotify.com/album/{ID}")),
            Ok(Link::Album(ID.into()))
        );
    }

    #[test]
    fn drops_the_share_query_and_fragment() {
        assert_eq!(
            parse(&format!(
                "https://open.spotify.com/track/{ID}?si=abc123&pt=x"
            )),
            Ok(Link::Track(ID.into()))
        );
        assert_eq!(
            parse(&format!("https://open.spotify.com/track/{ID}#play")),
            Ok(Link::Track(ID.into()))
        );
    }

    #[test]
    fn drops_the_locale_segment() {
        assert_eq!(
            parse(&format!("https://open.spotify.com/intl-de/artist/{ID}")),
            Ok(Link::Artist(ID.into()))
        );
    }

    #[test]
    fn accepts_a_url_without_its_scheme() {
        assert_eq!(
            parse(&format!("open.spotify.com/track/{ID}")),
            Ok(Link::Track(ID.into()))
        );
        assert_eq!(
            parse(&format!("https://www.open.spotify.com/track/{ID}")),
            Ok(Link::Track(ID.into()))
        );
    }

    #[test]
    fn trims_surrounding_space() {
        assert_eq!(
            parse(&format!("  spotify:track:{ID}\t")),
            Ok(Link::Track(ID.into()))
        );
    }

    #[test]
    fn names_podcasts_rather_than_refusing_them_blankly() {
        assert_eq!(
            parse(&format!("spotify:episode:{ID}")),
            Err(ParseError::Unsupported("podcasts"))
        );
        assert_eq!(
            parse(&format!("https://open.spotify.com/show/{OTHER}")),
            Err(ParseError::Unsupported("podcasts"))
        );
    }

    #[test]
    fn refuses_what_is_not_a_link() {
        assert_eq!(parse("hello world"), Err(ParseError::NotALink));
        assert_eq!(parse(""), Err(ParseError::NotALink));
        assert_eq!(parse("spotify:track"), Err(ParseError::NotALink));
        assert_eq!(
            parse("https://example.com/track/abc"),
            Err(ParseError::NotALink)
        );
        assert_eq!(
            parse(&format!("spotify:search:{ID}")),
            Err(ParseError::NotALink)
        );
    }

    #[test]
    fn a_uri_survives_a_round_trip() {
        for url in [
            format!("https://open.spotify.com/intl-de/track/{ID}?si=abc"),
            format!("spotify:user:someone:playlist:{ID}"),
            format!("open.spotify.com/artist/{ID}"),
        ] {
            let link = parse(&url).expect("reads");
            assert_eq!(parse(&link.to_uri()), Ok(link));
        }
    }

    /// What the share controls put on the clipboard: the canonical share
    /// spelling, and one this module reads back.
    #[test]
    fn a_url_survives_a_round_trip() {
        for link in [
            Link::Track(ID.into()),
            Link::Album(ID.into()),
            Link::Artist(ID.into()),
            Link::Playlist(ID.into()),
        ] {
            let url = link.to_url();
            assert!(url.starts_with("https://open.spotify.com/"), "{url}");
            assert!(url.ends_with(ID), "{url}");
            assert_eq!(parse(&url), Ok(link));
        }
    }

    #[test]
    fn refuses_a_bare_id() {
        assert_eq!(parse(ID), Err(ParseError::NotALink));
    }

    #[test]
    fn refuses_a_misshapen_id() {
        assert_eq!(parse("spotify:track:short"), Err(ParseError::NotALink));
        assert_eq!(
            parse("spotify:track:4uLU6hMCjMI75M1A2tKUQC5"),
            Err(ParseError::NotALink)
        );
        assert_eq!(
            parse("spotify:track:4uLU6hMCjMI75M1A2tKUQ/"),
            Err(ParseError::NotALink)
        );
    }
}
