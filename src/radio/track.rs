//! Turning a station's ICY announcement into a Spotify track.
//!
//! A broadcast tells you what it is playing in one free-text field, and every
//! broadcaster spells it differently. These are real `StreamTitle` values,
//! sampled from the directory's top-voted stations:
//!
//! ```text
//! Aspen - Seasick And Beer Drinking
//! Zedd, Alessia Cara - Stay
//! ERIC CLAPTON  -  Worried life blues
//! Moby - Precious Mind (feat. India Carney)
//! That Old Black Magic by The Hamburg Philharmonia Orchestra - Classic Vinyl on walmradio.com
//! BING CROSBY, Georgie Stoll and His Orchestra - SAILOR BEWARE (78 RPM) | OTR on walmradio.com
//! BBC World Service Online
//! ```
//!
//! So splitting on the first `" - "` is wrong three times over: WALM appends
//! its own branding after a dash on one channel and after a pipe on another,
//! two of its channels write `Title by Artist` while a third writes the usual
//! order, and the World Service announces no track at all. [`parse`] handles
//! those and refuses the rest — a wrong guess here would go on to like the
//! wrong record.
//!
//! [`best_match`] is the second half of the job. Spotify will answer almost
//! any query with something, so a returned track is not yet a match: it is
//! scored against what was announced and dropped unless both halves agree.

use crate::app::state::Track;

/// What a station announced, split into the two fields Spotify can be asked
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    pub artist: String,
    pub title: String,
}

impl Announcement {
    /// The scoped query: Spotify's field filters, which are far more precise
    /// than the same words thrown at the general index.
    pub fn scoped_query(&self) -> String {
        format!(
            "artist:\"{}\" track:\"{}\"",
            escape(&self.artist),
            escape(&self.title)
        )
    }

    /// The scoped query with the title's trailing annotation removed.
    ///
    /// `Precious Mind (feat. India Carney)` is how the station credits it and
    /// `Precious Mind` may be how Spotify names it; `SAILOR BEWARE (78 RPM)`
    /// carries a note about the *pressing* that no catalogue entry will have.
    /// `None` when there is no annotation to remove, so the caller does not
    /// spend a request repeating the query above.
    pub fn trimmed_query(&self) -> Option<String> {
        let bare = strip_annotation(&self.title);
        (bare != self.title).then(|| {
            format!(
                "artist:\"{}\" track:\"{}\"",
                escape(&self.artist),
                escape(&bare)
            )
        })
    }

    /// The unscoped fallback: the words, and nothing telling Spotify what they
    /// mean. Last resort, and the one most in need of [`best_match`]'s gate.
    pub fn loose_query(&self) -> String {
        format!("{} {}", self.artist, self.title)
    }
}

/// A quote inside a field filter would end the filter. Dropping them is enough
/// — Spotify has no escape syntax to use instead.
fn escape(s: &str) -> String {
    s.replace('"', " ").trim().to_string()
}

/// Announcements that are about the station rather than about a record.
const JUNK: &[&str] = &[
    "unknown",
    "unknown artist",
    "advertisement",
    "advert",
    "commercial",
    "commercials",
    "jingle",
    "station id",
    "station identification",
    "no title",
    "notitle",
    "live stream",
    "livestream",
];

/// Separators a station might put between the artist and the title. The dash
/// forms are matched with their spaces, so a hyphenated name — `Jay-Z`,
/// `Post-Punk` — is not read as a separator.
const SEPARATORS: &[&str] = &[" - ", " – ", " — ", " -- "];

/// Parse an ICY `StreamTitle` into the two fields Spotify can be asked for.
///
/// `station` is the station's own name, which is what tells an announcement
/// apart from an ident: the World Service's `BBC World Service Online` is a
/// perfectly well-formed string and simply is not a song.
///
/// `None` means there is nothing worth asking Spotify about, and the caller
/// must spend no request on it.
pub fn parse(raw: &str, station: &str) -> Option<Announcement> {
    let text = normalize(raw);
    if text.is_empty() || is_junk(&text) {
        return None;
    }

    let text = strip_station_tail(&text, station);
    if text.is_empty() {
        return None;
    }

    // `Title by Artist`, as two of WALM's three channels write it. Tested per
    // string rather than per station: their Old Time Radio channel writes the
    // usual order, so the broadcaster is no guide.
    let announced = match split_once_ci(&text, " by ") {
        Some((title, artist)) => Announcement {
            artist: artist.trim().to_string(),
            title: title.trim().to_string(),
        },
        // The usual order. Split on the *first* separator: a title with a dash
        // in it is far more common than an artist with one.
        None => {
            let (artist, title) = first_split(&text)?;
            Announcement {
                artist: artist.trim().to_string(),
                title: title.trim().to_string(),
            }
        }
    };

    if !is_plausible(&announced, station) {
        return None;
    }
    Some(announced)
}

/// Trim, collapse whitespace runs, and drop a trailing `;`.
///
/// The runs are real: Jazz Radio Blues sends `ERIC CLAPTON  -  Worried life
/// blues`, whose separator is not `" - "` until this has run.
fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.trim_end_matches(';').trim().to_string()
}

fn is_junk(text: &str) -> bool {
    let lower = text.to_lowercase();
    JUNK.contains(&lower.as_str())
}

/// Cut the station's own branding off the end.
///
/// WALM appends `- Classic Vinyl on walmradio.com` on one channel and
/// `| OTR on walmradio.com` on another, so both delimiters are tried, taking
/// the *last* one — the branding comes after everything else. A segment counts
/// as branding when it names a host (`... on walmradio.com`) or repeats the
/// station's own name.
///
/// Never cuts to nothing: a station whose whole announcement is its name is an
/// ident, and [`is_plausible`] is what refuses it.
fn strip_station_tail(text: &str, station: &str) -> String {
    let mut text = text.to_string();
    // Twice, because a station could use both: `Artist - Title | Show on host`
    // still has a dash-delimited tail once the pipe is gone.
    for _ in 0..2 {
        let cut = [" | ", " - ", " – ", " — "]
            .iter()
            .filter_map(|d| text.rfind(d).map(|i| (i, d.len())))
            .max_by_key(|&(i, _)| i);
        let Some((at, len)) = cut else { break };
        let head = &text[..at];
        let tail = &text[at + len..];
        if head.trim().is_empty() || !is_branding(tail, station) {
            break;
        }
        text = head.trim().to_string();
    }
    text
}

/// Whether `segment` is the station talking about itself.
///
/// Both tests are deliberately narrow, because the cost of a false positive is
/// a record silently not looked up: a title like `Live on N.Y.C.` must not read
/// as a hostname, and a station called `Jazz` must not swallow every tail with
/// the word in it.
fn is_branding(segment: &str, station: &str) -> bool {
    let lower = segment.to_lowercase();
    // `... on walmradio.com`.
    if let Some((_, host)) = split_once_ci(&lower, " on ")
        && is_hostname(host.trim())
    {
        return true;
    }
    if lower.contains("http") || lower.contains("www.") {
        return true;
    }
    // Exactly the station's name, and nothing more. `contains` here would take
    // `Smooth Jazz Nights` off a station called `Jazz`.
    let station = station.trim().to_lowercase();
    !station.is_empty() && lower.trim() == station
}

/// Whether `s` looks like a bare hostname.
///
/// A dotted run with no spaces and a plausible TLD on the end. The TLD test is
/// what separates `walmradio.com` from `N.Y.C.`, which is otherwise the same
/// shape and is part of a song title rather than an address.
fn is_hostname(s: &str) -> bool {
    if s.contains(' ') || !s.contains('.') {
        return false;
    }
    let Some(tld) = s.rsplit('.').next() else {
        return false;
    };
    (2..=6).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// Split on the first of [`SEPARATORS`] to appear.
fn first_split(text: &str) -> Option<(&str, &str)> {
    SEPARATORS
        .iter()
        .filter_map(|sep| text.find(sep).map(|i| (i, sep.len())))
        .min_by_key(|&(i, _)| i)
        .map(|(at, len)| (&text[..at], &text[at + len..]))
}

/// Case-insensitive `split_once`, returning slices of the original.
fn split_once_ci<'a>(text: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    let lower = text.to_lowercase();
    // `to_lowercase` can change a string's length, so an index into the
    // lowered copy is not an index into `text`. Only ASCII needles are used
    // here, and the byte offsets line up as long as every character before the
    // match keeps its length — which is why this checks rather than assumes.
    let at = lower.find(&needle.to_lowercase())?;
    if lower.len() != text.len() {
        return text
            .char_indices()
            .find(|(i, _)| text[*i..].to_lowercase().starts_with(needle))
            .map(|(i, _)| (&text[..i], &text[i + needle.len()..]));
    }
    Some((&text[..at], &text[at + needle.len()..]))
}

/// Whether an announcement could be a record at all.
fn is_plausible(a: &Announcement, station: &str) -> bool {
    if a.artist.is_empty() || a.title.is_empty() {
        return false;
    }
    for field in [&a.artist, &a.title] {
        let lower = field.to_lowercase();
        if lower.contains("http") || lower.contains("://") || lower.contains("www.") {
            return false;
        }
        if JUNK.contains(&lower.as_str()) {
            return false;
        }
        let station = station.trim().to_lowercase();
        if !station.is_empty() && lower == station {
            return false;
        }
    }
    true
}

/// Drop a trailing `(...)` or `[...]` annotation.
fn strip_annotation(title: &str) -> String {
    let trimmed = title.trim_end();
    let closer = trimmed.chars().last();
    let opener = match closer {
        Some(')') => '(',
        Some(']') => '[',
        _ => return title.to_string(),
    };
    match trimmed.rfind(opener) {
        // A title that is *entirely* a parenthesis is left alone; there would
        // be nothing left to search for.
        Some(at) if !trimmed[..at].trim().is_empty() => trimmed[..at].trim().to_string(),
        _ => title.to_string(),
    }
}

/// Accept a candidate only when both halves agree. Spotify answers nearly any
/// query with something, so a returned track is a suggestion, not a match.
const TITLE_FLOOR: f32 = 0.8;
const ARTIST_FLOOR: f32 = 0.6;

/// How well `cand` answers `want`, from 0.0 to 1.0.
///
/// This ranks; it does not decide. Whether a candidate is a match at all is
/// [`best_match`]'s two independent floors, which is the part that matters —
/// a right title by the wrong artist is not a near miss but a different record
/// with the same name, and there are a great many of those. Blending only
/// orders the candidates that already cleared both bars, and the title is
/// weighted higher because that is what separates a record from its own
/// remaster.
pub fn score(cand: &Track, want: &Announcement) -> f32 {
    0.6 * title_score(cand, want) + 0.4 * artist_score(cand, want)
}

/// Records that answer a search for someone else's song.
///
/// Spotify's index is full of them, they rank well on title, and their artist
/// is frequently the original's name with a word bolted on — so the floors
/// alone do not keep them out. A station announcing an actual karaoke record
/// says so, which is why this only fires when the candidate claims it and the
/// announcement does not.
const IMPOSTORS: &[&str] = &[
    "karaoke",
    "tribute",
    "made famous by",
    "in the style of",
    "cover version",
    "as made popular by",
];

fn is_impostor(cand: &Track, want: &Announcement) -> bool {
    let theirs = format!("{} {}", cand.name, cand.artists).to_lowercase();
    let ours = format!("{} {}", want.title, want.artist).to_lowercase();
    IMPOSTORS
        .iter()
        .any(|m| theirs.contains(m) && !ours.contains(m))
}

/// The announcement with its two halves swapped.
///
/// A minority of stations write `Title - Artist`. Trying both orders costs one
/// more comparison and rescues that whole class; the floors still have to be
/// cleared either way, so a genuinely ambiguous pair cannot sneak through by
/// being read backwards.
fn swapped(want: &Announcement) -> Announcement {
    Announcement {
        artist: want.title.clone(),
        title: want.artist.clone(),
    }
}

fn title_score(cand: &Track, want: &Announcement) -> f32 {
    similarity(&cand.name, &want.title).max(similarity(
        &strip_annotation(&cand.name),
        &strip_annotation(&want.title),
    ))
}

/// The best pairwise agreement between anyone Spotify credits and anyone the
/// station credits.
///
/// Both sides list several: Spotify joins its artists with commas, and a
/// station writes `Zedd, Alessia Cara` or `J.M. Rhythm Four & Peter Appleyard`.
/// Requiring the whole strings to agree would fail on a credit order, so the
/// question asked is whether *any* named artist is common to both.
fn artist_score(cand: &Track, want: &Announcement) -> f32 {
    let theirs = credits(&cand.artists);
    let ours = credits(&want.artist);
    let pairwise = theirs
        .iter()
        .flat_map(|t| ours.iter().map(move |o| similarity(t, o)))
        .fold(0.0f32, f32::max);
    // Also compare the two lists whole: a duo Spotify credits as one artist
    // ("Simon & Garfunkel") would otherwise be split apart and matched against
    // half of itself.
    pairwise.max(similarity(&cand.artists, &want.artist))
}

/// Split a credit list into the individual names in it.
fn credits(list: &str) -> Vec<String> {
    let mut names = Vec::new();
    for part in list.split([',', '&', '/']) {
        // `feat.` and `ft.` introduce a further credit rather than ending one.
        for piece in split_credit_words(part) {
            let piece = piece.trim();
            if !piece.is_empty() {
                names.push(piece.to_string());
            }
        }
    }
    names
}

fn split_credit_words(part: &str) -> Vec<&str> {
    const MARKERS: &[&str] = &[
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
        " with ",
    ];
    let lower = part.to_lowercase();
    for marker in MARKERS {
        if let Some(at) = lower.find(marker) {
            let (head, tail) = part.split_at(at);
            return vec![head, &tail[marker.len()..]];
        }
    }
    vec![part]
}

/// How alike two names are once spelling is set aside, from 0.0 to 1.0.
fn similarity(a: &str, b: &str) -> f32 {
    let (a, b) = (fold(a), fold(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    // One inside the other: a remaster suffix, a `(Live)`, an edition note.
    if a.contains(&b) || b.contains(&a) {
        return 0.85;
    }
    let (aw, bw): (Vec<&str>, Vec<&str>) = (a.split(' ').collect(), b.split(' ').collect());
    let shared = aw.iter().filter(|w| bw.contains(w)).count();
    let union = aw.len() + bw.len() - shared;
    if union == 0 {
        return 0.0;
    }
    shared as f32 / union as f32
}

/// Lowercase, drop punctuation, collapse spaces.
///
/// Non-ASCII letters are kept: `Übermorgen` folds to `übermorgen`, which still
/// matches itself. Stripping them would fold it to `bermorgen` and match
/// nothing.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

/// The best candidate that clears both floors, or `None`.
///
/// Ties go to the earlier candidate: Spotify already returns search results in
/// its own relevance order, and that is a better tiebreak than anything this
/// module knows.
pub fn best_match(cands: &[Track], want: &Announcement) -> Option<Track> {
    let flipped = swapped(want);
    let mut best: Option<(f32, &Track)> = None;
    for cand in cands {
        if is_impostor(cand, want) {
            continue;
        }
        // Read the announcement both ways round and keep whichever reading the
        // candidate answers better. A record has to clear both floors under
        // one single reading — scoring the title against one order and the
        // artist against the other would match almost anything.
        let Some(s) = [want, &flipped]
            .into_iter()
            .filter(|w| {
                title_score(cand, w) >= TITLE_FLOOR && artist_score(cand, w) >= ARTIST_FLOOR
            })
            .map(|w| score(cand, w))
            .fold(None, |acc: Option<f32>, s| {
                Some(acc.map_or(s, |a| a.max(s)))
            })
        else {
            continue;
        };
        if best.is_none_or(|(bs, _)| s > bs) {
            best = Some((s, cand));
        }
    }
    best.map(|(_, t)| t.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(artist: &str, title: &str) -> Option<Announcement> {
        Some(Announcement {
            artist: artist.to_string(),
            title: title.to_string(),
        })
    }

    fn track(name: &str, artists: &str) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.to_string(),
            artists: artists.to_string(),
            album: "An Album".to_string(),
            release_year: "1999".to_string(),
            duration_ms: 0,
            track_number: 0,
            album_id: None,
            artist_id: None,
            cover_url: None,
        }
    }

    /// Every one of these is a real `StreamTitle`, sampled from the
    /// directory's top-voted stations. They are the whole reason this module
    /// is not a call to `split_once(" - ")`.
    #[test]
    fn parses_what_real_stations_actually_send() {
        let cases: &[(&str, &str, Option<Announcement>)] = &[
            (
                "Aspen - Seasick And Beer Drinking",
                "Groove Salad",
                ann("Aspen", "Seasick And Beer Drinking"),
            ),
            (
                "Human League - Do Or Die",
                "Underground 80s",
                ann("Human League", "Do Or Die"),
            ),
            (
                "Zedd, Alessia Cara - Stay",
                "MANGORADIO",
                ann("Zedd, Alessia Cara", "Stay"),
            ),
            // Doubled spaces around the dash: not a separator until the string
            // has been normalized.
            (
                "ERIC CLAPTON  -  Worried life blues",
                "Jazz Radio Blues",
                ann("ERIC CLAPTON", "Worried life blues"),
            ),
            (
                "Steve Cole - With You All The Way ",
                "101 SMOOTH JAZZ",
                ann("Steve Cole", "With You All The Way"),
            ),
            // The credit is kept: Spotify names it the same way.
            (
                "Moby - Precious Mind (feat. India Carney)",
                "Radio Paradise",
                ann("Moby", "Precious Mind (feat. India Carney)"),
            ),
            // `Title by Artist`, with the branding after a dash.
            (
                "That Old Black Magic by The Hamburg Philharmonia Orchestra \
                 - Classic Vinyl on walmradio.com",
                "Classic Vinyl HD",
                ann("The Hamburg Philharmonia Orchestra", "That Old Black Magic"),
            ),
            (
                "GBT by Harald Haerter - Adroit Jazz Underground on walmradio.com",
                "Adroit Jazz Underground",
                ann("Harald Haerter", "GBT"),
            ),
            // Same broadcaster, usual order, branding after a pipe.
            (
                "BING CROSBY, Georgie Stoll and His Orchestra - SAILOR BEWARE (78 RPM) \
                 | OTR on walmradio.com",
                "WALM - Old Time Radio",
                ann(
                    "BING CROSBY, Georgie Stoll and His Orchestra",
                    "SAILOR BEWARE (78 RPM)",
                ),
            ),
            // An ident, not a song. No separator anywhere in it.
            ("BBC World Service Online", "BBC World Service", None),
            ("", "Dance Wave!", None),
            ("   ", "Dance Wave!", None),
        ];
        for (raw, station, want) in cases {
            assert_eq!(parse(raw, station).as_ref(), want.as_ref(), "{raw:?}");
        }
    }

    #[test]
    fn refuses_what_is_not_a_record() {
        for (raw, station) in [
            ("Advertisement", "Any FM"),
            ("unknown", "Any FM"),
            ("Listen at www.example.com - Any FM", "Any FM"),
            ("http://example.com/stream - now", "Any FM"),
            // Nothing before the separator to be an artist.
            (" - Just A Title", "Any FM"),
            ("Just An Artist - ", "Any FM"),
            // Every part of it is the station's own name.
            ("Any FM - Any FM", "Any FM"),
        ] {
            assert_eq!(parse(raw, station), None, "{raw:?}");
        }
    }

    /// A hyphen inside a name is not a separator — the separators carry their
    /// spaces for exactly this reason.
    #[test]
    fn a_hyphenated_name_is_not_a_separator() {
        assert_eq!(
            parse("Jay-Z - Big Pimpin'", "Hot 97"),
            ann("Jay-Z", "Big Pimpin'")
        );
    }

    /// The first separator wins, so a title containing a dash survives whole.
    #[test]
    fn the_first_separator_splits_it() {
        assert_eq!(
            parse("Emerson, Lake & Palmer - Fanfare - Reprise", "Prog FM"),
            ann("Emerson, Lake & Palmer", "Fanfare - Reprise")
        );
    }

    #[test]
    fn an_en_dash_separates_too() {
        assert_eq!(parse("Björk – Jóga", "Rás 2"), ann("Björk", "Jóga"));
    }

    #[test]
    fn queries_are_field_scoped() {
        let a = Announcement {
            artist: "Moby".into(),
            title: "Precious Mind (feat. India Carney)".into(),
        };
        assert_eq!(
            a.scoped_query(),
            "artist:\"Moby\" track:\"Precious Mind (feat. India Carney)\""
        );
        assert_eq!(
            a.trimmed_query().as_deref(),
            Some("artist:\"Moby\" track:\"Precious Mind\"")
        );
        assert_eq!(a.loose_query(), "Moby Precious Mind (feat. India Carney)");
    }

    /// No annotation means no second query to spend a request on.
    #[test]
    fn a_plain_title_has_no_trimmed_query() {
        let a = Announcement {
            artist: "Aspen".into(),
            title: "Seasick And Beer Drinking".into(),
        };
        assert_eq!(a.trimmed_query(), None);
    }

    /// A quote would close the field filter early.
    #[test]
    fn quotes_are_stripped_from_queries() {
        let a = Announcement {
            artist: "The \"Real\" Band".into(),
            title: "Song".into(),
        };
        assert!(!a.scoped_query()[8..].contains('"') || a.scoped_query().matches('"').count() == 4);
        assert_eq!(
            a.scoped_query(),
            "artist:\"The  Real  Band\" track:\"Song\""
        );
    }

    #[test]
    fn an_exact_pair_matches() {
        let want = Announcement {
            artist: "Aspen".into(),
            title: "Seasick And Beer Drinking".into(),
        };
        let cands = [track("Seasick And Beer Drinking", "Aspen")];
        assert_eq!(best_match(&cands, &want).unwrap().name, cands[0].name);
    }

    /// The station credits two artists in one order and Spotify in the other.
    #[test]
    fn a_credit_order_still_matches() {
        let want = Announcement {
            artist: "Zedd, Alessia Cara".into(),
            title: "Stay".into(),
        };
        let cands = [track("Stay", "Alessia Cara, Zedd")];
        assert!(best_match(&cands, &want).is_some());
    }

    /// `&` is a credit separator on the station's side and often not on
    /// Spotify's.
    #[test]
    fn an_ampersand_credit_matches_one_of_its_halves() {
        let want = Announcement {
            artist: "J.M. Rhythm Four & Peter Appleyard".into(),
            title: "Frenesi".into(),
        };
        let cands = [track("Frenesi", "Peter Appleyard")];
        assert!(best_match(&cands, &want).is_some());
    }

    /// A duo Spotify credits as a single artist must not be split apart and
    /// then matched against half of itself.
    #[test]
    fn a_duo_credited_whole_still_matches() {
        let want = Announcement {
            artist: "Simon & Garfunkel".into(),
            title: "America".into(),
        };
        let cands = [track("America", "Simon & Garfunkel")];
        assert!(best_match(&cands, &want).is_some());
    }

    /// A remaster suffix is the same record.
    #[test]
    fn a_remaster_suffix_still_matches() {
        let want = Announcement {
            artist: "Human League".into(),
            title: "Do Or Die".into(),
        };
        let cands = [track("Do Or Die - 2003 Remaster", "The Human League")];
        assert!(best_match(&cands, &want).is_some());
    }

    /// The whole point of the gate: the same title by somebody else is a
    /// different record, and Spotify will happily return one.
    #[test]
    fn the_same_title_by_another_artist_is_refused() {
        let want = Announcement {
            artist: "Aspen".into(),
            title: "Stay".into(),
        };
        let cands = [track("Stay", "Rihanna")];
        assert!(best_match(&cands, &want).is_none());
    }

    /// Spotify's index is thick with these and they rank well on title alone.
    #[test]
    fn a_karaoke_version_is_refused() {
        let want = Announcement {
            artist: "Human League".into(),
            title: "Do Or Die".into(),
        };
        let cands = [
            track("Do Or Die (Karaoke Version)", "The Karaoke Channel"),
            track("Do Or Die", "Human League Tribute Band"),
        ];
        assert!(best_match(&cands, &want).is_none());
    }

    /// Unless that is genuinely what the station announced.
    #[test]
    fn a_karaoke_record_the_station_announced_is_kept() {
        let want = Announcement {
            artist: "The Karaoke Channel".into(),
            title: "Do Or Die (Karaoke Version)".into(),
        };
        let cands = [track("Do Or Die (Karaoke Version)", "The Karaoke Channel")];
        assert!(best_match(&cands, &want).is_some());
    }

    /// A minority of stations write the title first.
    #[test]
    fn a_station_that_announces_the_title_first_still_matches() {
        let want = Announcement {
            artist: "Do Or Die".into(),
            title: "Human League".into(),
        };
        let cands = [track("Do Or Die", "Human League")];
        assert!(best_match(&cands, &want).is_some());
    }

    /// Reading it backwards must not become a way past the floors: the title
    /// and the artist have to agree under one and the same reading.
    #[test]
    fn a_swapped_reading_cannot_mix_the_two_orders() {
        let want = Announcement {
            artist: "Rihanna".into(),
            title: "Do Or Die".into(),
        };
        // Title agrees with `want`, artist agrees only with the flip. Neither
        // reading clears both floors.
        let cands = [track("Do Or Die", "Do Or Die")];
        assert!(best_match(&cands, &want).is_none());
    }

    #[test]
    fn an_unrelated_title_is_refused() {
        let want = Announcement {
            artist: "Moby".into(),
            title: "Precious Mind".into(),
        };
        let cands = [track("Porcelain", "Moby")];
        assert!(best_match(&cands, &want).is_none());
    }

    /// Diacritics must survive folding, or a name matches nothing.
    #[test]
    fn diacritics_are_kept() {
        assert_eq!(fold("Übermorgen"), "übermorgen");
        let want = Announcement {
            artist: "Mark Forster".into(),
            title: "Übermorgen".into(),
        };
        let cands = [track("Übermorgen", "Mark Forster")];
        assert!(best_match(&cands, &want).is_some());
    }

    /// Higher-scoring candidates win, and an empty list is not a match.
    #[test]
    fn the_best_candidate_wins() {
        let want = Announcement {
            artist: "Human League".into(),
            title: "Do Or Die".into(),
        };
        let cands = [
            track("Do Or Die - 2003 Remaster", "The Human League"),
            track("Do Or Die", "The Human League"),
        ];
        assert_eq!(best_match(&cands, &want).unwrap().name, "Do Or Die");
        assert!(best_match(&[], &want).is_none());
    }

    /// Connects to two real stations and parses whatever they are announcing.
    ///
    /// Ignored by default, following `radio::api` and `radio::player`: the
    /// suite must not need a network. Run it with
    /// `cargo test radio::track::tests::live_ -- --ignored --nocapture` after
    /// touching the parser — the wire format is the part no unit test can
    /// vouch for, and these are the two stations whose shapes differ most.
    #[tokio::test]
    #[ignore]
    async fn live_announcements_still_parse() {
        use std::sync::Arc;
        use std::time::Duration;

        use crate::audio_tap::AudioTap;
        use crate::radio::player::RadioPlayer;

        for (url, station) in [
            (
                "https://ice1.somafm.com/groovesalad-128-mp3",
                "Groove Salad",
            ),
            (
                "https://icecast.walmradio.com:8443/classic",
                "Classic Vinyl HD",
            ),
        ] {
            let player = RadioPlayer::new(Arc::new(AudioTap::new()));
            player.play(url, 0).await.expect("the station should play");
            tokio::time::sleep(Duration::from_secs(3)).await;
            let raw = player.title().lock().clone();
            player.stop();

            let raw = raw.unwrap_or_else(|| panic!("{station} announced nothing"));
            let parsed = parse(&raw, station);
            println!("{station}: {raw:?} -> {parsed:?}");
            assert!(parsed.is_some(), "{station} announced {raw:?}, unparsed");
        }
    }

    /// The tail stripper must not eat part of a song. Both of these look like
    /// branding at a glance and are not.
    #[test]
    fn a_title_is_not_mistaken_for_the_stations_branding() {
        assert_eq!(
            parse("Duke Ellington - Live on N.Y.C.", "Adroit Jazz"),
            ann("Duke Ellington", "Live on N.Y.C.")
        );
        // A short station name must not swallow every tail containing it.
        assert_eq!(
            parse("Norah Jones - Smooth Jazz Nights", "Jazz"),
            ann("Norah Jones", "Smooth Jazz Nights")
        );
    }

    /// But a tail that really is only the station's name still goes.
    #[test]
    fn a_tail_that_is_only_the_station_name_is_stripped() {
        assert_eq!(
            parse(
                "Norah Jones - Come Away With Me - Adroit Jazz",
                "Adroit Jazz"
            ),
            ann("Norah Jones", "Come Away With Me")
        );
    }

    #[test]
    fn a_hostname_needs_a_plausible_tld() {
        assert!(is_hostname("walmradio.com"));
        assert!(is_hostname("somafm.co.uk"));
        assert!(!is_hostname("N.Y.C."));
        assert!(!is_hostname("nodots"));
        assert!(!is_hostname("has space.com"));
        assert!(!is_hostname("trailing.1234567"));
    }
}
