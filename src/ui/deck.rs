//! The rows the bottom bar and the full player both draw.
//!
//! The two views are the same skeleton: a masthead saying what is playing,
//! a progress track, a transport row, and a row naming the queue that is
//! feeding them. The player wedges its cover and spectrum between the
//! masthead and the progress track and lists the queue underneath; the bar
//! does neither. Everything else was, for a long time, two copies of the
//! same code in [`super::now_playing`] and [`super::player`] — and every
//! layout change had to be made twice.
//!
//! This module owns the rows. It deliberately does *not* own the band
//! layout: each view decides how many rows it can spare and where its own
//! content goes between them.
//!
//! Every control records itself into the same [`HitAreas`] field whichever
//! view drew it, so `event.rs` resolves clicks without knowing which one it
//! was.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::play_state::{PlayState, RadioForward, RadioSteps};
use super::table::{
    art_w, credit_line, credit_links, credit_spans, draw_art, draw_volume, fit, hovered_credit,
    link, meter, right_row, segment, state_spans, width,
};
use super::theme;
use crate::app::queue::Queue;
use crate::app::state::{
    ArtHit, ArtSource, Credit, HitAreas, Playback, RadioPlayback, Track, format_duration,
};
use crate::cover::Cover;

/// Rows [`masthead`] occupies: the title, and the metadata under it.
pub const MASTHEAD_H: u16 = 2;

/// Rows the whole deck occupies when it is drawn as one block, as the bottom
/// bar draws it: the masthead, a spacer, the progress track, the transport,
/// another spacer, and the context row.
///
/// The player does not use it — it wedges its cover and spectrum between the
/// masthead and the progress track, and pads the bands differently to suit
/// the room a full screen gives it.
pub const DECK_H: u16 = MASTHEAD_H + 5;

/// Time labels flanking the progress track: "2:07 " and " -3:40". The
/// player's tests measure its gauge against this.
pub(super) const TIME_W: u16 = 5 + 6;

/// The transport's two buttons, in the order they are laid out.
const PREV_LABEL: &str = "◂◂ previous";
const NEXT_LABEL: &str = "next ▸▸";
/// What the radio transport's right-hand button says when there is no station
/// to step forward to. The same seven cells as [`NEXT_LABEL`], so the two
/// never move the row between them.
const SEEK_LABEL: &str = "seek ▸▸";
/// The forward control where the thing after this record is the station that
/// played it. The same seven cells again, so the row does not shift as the
/// list is walked to its end.
const LIVE_FORWARD_LABEL: &str = "live ▸▸";

/// Where the Spotify transport's forward control leads.
///
/// The row is otherwise the same wherever it is drawn, so this is the one
/// thing about it a caller decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Forward {
    /// The next record of the queue.
    #[default]
    Next,
    /// The broadcast of the station this record came off, because the record
    /// is the last of that station's list and nothing else follows it.
    Live,
}

impl Forward {
    fn label(self) -> &'static str {
        match self {
            Forward::Next => NEXT_LABEL,
            Forward::Live => LIVE_FORWARD_LABEL,
        }
    }
}

/// The liked control at the right end of the title row, in both states.
/// Unpadded, like every other control on the deck — the hover pill covers the
/// text and nothing else.
///
/// The same solid glyph either way — the word beside it is what says which
/// state you are in, so the pair never comes down to telling one glyph from
/// a hollow twin by its shade.
/// Built from [`super::table::LIKED_MARK`] at draw time rather than spelled
/// out here: the table's column and this control wear the same mark, and a
/// second copy of the glyph is a second thing to forget.
fn like_label(liked: bool) -> String {
    let mark = super::table::LIKED_MARK;
    if liked {
        format!("{mark} liked")
    } else {
        format!("{mark} like")
    }
}

/// The label of the control that copies the record's Spotify link. Built from
/// [`super::table::SHARE_MARK`] for the reason [`like_label`] is built from
/// `LIKED_MARK`: the track table's column and this control wear the same mark.
fn share_label() -> String {
    format!("{} share", super::table::SHARE_MARK)
}

/// The label of the control that opens the add-to-playlist box.
const ADD_LABEL: &str = "+ add";

/// How many of the title row's optional controls a masthead still offers, in
/// the order a narrowing row gives them up.
///
/// Share goes first: a record you cannot act on is worth less than one you
/// cannot link to. The liked control is never dropped this way — it is the
/// only one of the three that also *reports* something, and a row without it
/// cannot say whether the record is saved.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TitleRung {
    All,
    NoShare,
    LikedOnly,
}

/// Draw the title row's controls — the liked pill, `⧉ share` and `+ add` — and
/// return the cells they took.
///
/// Both mastheads carry the same three in the same corner, and `right_row`
/// gives them their order: groups are laid out left to right from the row's
/// right edge, so share lands between the `★` and the `+`.
///
/// `right_row` is all or nothing — a row too narrow for the groups draws none
/// of them — so a row that cannot hold all three is offered fewer, a rung at a
/// time, rather than losing controls that used to fit.
fn title_controls(
    frame: &mut Frame,
    row: Rect,
    liked: Option<bool>,
    add: bool,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) -> u16 {
    let groups = |rung: TitleRung| {
        let mut groups: Vec<Vec<Span<'static>>> = Vec::new();
        if let Some(liked) = liked {
            let style = if liked { theme::accent() } else { theme::dim() };
            groups.push(vec![Span::styled(like_label(liked), style)]);
        }
        if add && rung == TitleRung::All {
            groups.push(vec![Span::styled(share_label(), theme::dim())]);
        }
        if add && rung != TitleRung::LikedOnly {
            groups.push(vec![Span::styled(ADD_LABEL, theme::dim())]);
        }
        groups
    };

    let mut drawn = Vec::new();
    let mut rung = TitleRung::All;
    for next in [TitleRung::All, TitleRung::NoShare, TitleRung::LikedOnly] {
        let wanted = groups(next);
        if wanted.is_empty() {
            return 0;
        }
        rung = next;
        drawn = right_row(frame, row, mouse, wanted);
        if !drawn[0].is_empty() {
            break;
        }
    }

    // The cells the controls hold, the gaps `right_row` set between them
    // included, measured from the leftmost one to the row's right edge.
    let width = drawn
        .iter()
        .find(|r| !r.is_empty())
        .map_or(0, |r| row.right() - r.x);
    let mut rects = drawn.into_iter();
    if liked.is_some() {
        hit.like_btn = rects.next().unwrap_or_default();
    }
    hit.share_btn = match rung {
        TitleRung::All => rects.next().unwrap_or_default(),
        _ => Rect::default(),
    };
    hit.add_btn = rects.next().unwrap_or_default();
    width
}

/// What the player says when nothing is playing. The bottom bar says nothing:
/// with no subject it is dropped from the layout — see [`super::now_playing`].
///
/// It names what there is to press Enter on, so without an account it points
/// at a station rather than at a track that cannot be reached.
pub fn no_playback_hint(frame: &mut Frame, area: Rect, spotify_ready: bool) {
    let subject = if spotify_ready {
        "a track"
    } else {
        "a station"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "nothing playing — press Enter on {subject} to start"
        ))
        .style(theme::dim()),
        Rect { height: 1, ..area },
    );
}

/// Cover art filling `area`'s height, flush with its left edge.
///
/// The block is square on screen (see [`art_w`]), so its width follows from
/// the rows it is given rather than being passed in. Returns the rect it
/// painted so the caller can lay out beside it, and records it in
/// `hit.art_blocks`: clicking a sleeve fills the screen with it.
///
/// Deliberately not a link to the album — the sleeve is the biggest, most
/// inviting thing on the screen, so an album opened from it would be the one
/// control nothing labels. The album's name on the row below does that job and
/// says so; this only ever shows you the picture you clicked, larger.
pub fn sleeve(
    frame: &mut Frame,
    area: Rect,
    track: &Track,
    cover: Option<&Cover>,
    hit: &mut HitAreas,
) -> Rect {
    let art = Rect {
        width: art_w(area.height).min(area.width),
        ..area
    };
    // Seeded on the album so a given record always gets the same
    // placeholder, and it does not reshuffle between tracks of one sleeve.
    let seed = track.album_id.as_deref().unwrap_or(track.name.as_str());
    draw_art(frame, art, cover, seed);
    hit.art_blocks.push(ArtHit {
        rect: art,
        source: ArtSource::Playing,
        seed: seed.to_string(),
    });
    art
}

/// Two rows: the track title, then artists · album · year with the volume
/// slider opposite it.
///
/// The title gets the whole row, because it is what the view is about and a
/// full-width row is what lets a long one be read. The play state sits under
/// the progress track instead, between previous and next — see [`transport`].
///
/// Records `hit.volume_slider`, `hit.now_artist_links` and `hit.now_album`. It does
/// *not* touch `hit.now_playing`: which region the wheel adjusts volume over
/// is the caller's decision.
#[allow(clippy::too_many_arguments)]
pub fn masthead(
    frame: &mut Frame,
    area: Rect,
    track: &Track,
    volume_percent: u8,
    liked: Option<bool>,
    add: bool,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let dim = theme::dim();

    // Row 0: the track title, as large as a terminal allows, with the record's
    // controls opposite it.
    //
    // The liked control is drawn only once the saved state is known — one that
    // cannot say which way it would go is worse than none at all. `right_row`
    // drops a control whole on a row too narrow for it, so the title never has
    // to share its cells with half of one.
    let title_row = Rect { height: 1, ..area };
    let controls_w = title_controls(frame, title_row, liked, add, mouse, hit);
    // One cell of daylight between the title and the controls, so a title that
    // runs the full width ends in an ellipsis rather than against the pill.
    let title_w = if controls_w == 0 {
        title_row.width
    } else {
        title_row.width.saturating_sub(controls_w + 1)
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            fit(&track.name, title_w as usize),
            theme::accent().add_modifier(Modifier::BOLD),
        )),
        Rect {
            width: title_w,
            ..title_row
        },
    );

    // Row 1: artists · album · year, with the volume slider right-aligned.
    // It starts flush with the title above it, one block rather than two
    // columns.
    if area.height < MASTHEAD_H {
        return;
    }
    let row = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    let vol_seg = draw_volume(frame, row, volume_percent, mouse, hit);
    let meta = Rect {
        width: row.width.saturating_sub(vol_seg.width + 1),
        ..row
    };
    if meta.width == 0 {
        return;
    }
    let mut spans = Vec::new();
    let mut x = meta.x;
    credit_segment(
        &mut spans,
        &mut x,
        meta,
        mouse,
        &track.credits,
        &mut hit.now_artist_links,
    );
    // The album is printed even when it only repeats the artist or the track.
    // A self-titled single does read as "Abeichizoku · Abeichizoku", but the
    // two names are links to two different pages, and dropping one leaves the
    // record with no way to be opened.
    let album = clip(&track.album, meta);
    if sep(&mut spans, &mut x, &album, meta) {
        hit.now_album = link(
            &mut spans,
            &mut x,
            meta,
            mouse,
            album,
            dim,
            track.album_id.is_some(),
        );
    }
    if !track.release_year.is_empty() && sep(&mut spans, &mut x, &track.release_year, meta) {
        spans.push(Span::styled(track.release_year.clone(), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), meta);
}

/// The credited artists as the row's first metadata segment, each name a link
/// of its own.
///
/// The deck's answer to a track table's Artist column: [`link`] gives a run
/// one target, and a record credited to three artists needs three. Clipped to
/// the row the way [`clip`] clips every other segment, so a long credit line
/// still stops short of the volume slider.
fn credit_segment(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    meta: Rect,
    mouse: Option<Position>,
    credits: &[Credit],
    links: &mut Vec<(Rect, Credit)>,
) {
    let (cell, runs) = credit_line(credits, meta.width as usize);
    let cell = cell.trim_end();
    let rect = Rect {
        x: *x,
        y: meta.y,
        width: width(cell) as u16,
        height: 1,
    }
    .intersection(meta);
    let hovered = hovered_credit(rect, &runs, mouse);
    spans.extend(credit_spans(cell, &runs, theme::text(), hovered));
    credit_links(rect, &runs, links);
    *x = x.saturating_add(rect.width);
}

/// Clip a metadata segment to the row.
///
/// `fit` pads to exactly the width; this trims that padding back so a link's
/// hit rect stops at the text rather than running on to the volume slider.
fn clip(s: &str, meta: Rect) -> String {
    fit(s, meta.width as usize).trim_end().to_string()
}

/// Make room for the next metadata segment, or say there is none.
///
/// A segment is dropped whole rather than clipped: the row is cut off at the
/// volume slider, and half an album name followed by a dangling " · " reads as
/// a bug rather than as a truncation. Measured in cells, so a CJK name is not
/// read as half its true width.
///
/// Shared by both mastheads. It was a closure inside [`masthead`] until the
/// radio deck grew a metadata row of its own; two copies of this rule would
/// drift the first time either row changed.
fn sep(spans: &mut Vec<Span<'static>>, x: &mut u16, next: &str, meta: Rect) -> bool {
    let run = if spans.is_empty() { 0 } else { 3 } + width(next) as u16;
    if *x + run > meta.right() {
        return false;
    }
    if !spans.is_empty() {
        spans.push(Span::styled(" · ", theme::dim()));
        *x += 3;
    }
    true
}

/// The radio deck's masthead: what the station is playing, and what is known
/// about it.
///
/// The same two rows and the same volume slider as [`masthead`], and — once
/// spot has found the announced record on Spotify — the same *content*: the
/// record's name on row 0 and its artist, album and year on row 1, both names
/// clickable. Nothing about a broadcast makes those facts different facts, so
/// the deck should not make them look different.
///
/// Row 0 falls back through what is actually known: the matched record's name,
/// else the station's own words, else the station's name. Something like six
/// popular stations in ten announce anything at all; the station's name is what
/// the row says for the rest, and [`radio_station_row`] carries the name in
/// every case, so it is never off the screen.
///
/// The `★` is drawn only against a matched record, and only once its saved
/// state is known — the same rule [`masthead`] follows. Keeping a *station* is
/// still done on its row in the directory: the deck's `★` has always meant
/// "save this track", and when there is one it now means it here too.
pub fn radio_masthead(
    frame: &mut Frame,
    area: Rect,
    radio: &RadioPlayback,
    liked: Option<bool>,
    add: bool,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let dim = theme::dim();
    let matched = radio.matched_track();
    let announced = radio.now_title();

    // The record's controls, on the title row, exactly as `masthead` places
    // them — and only where that row names a record rather than a station.
    let title_row = Rect { height: 1, ..area };
    let controls_w = match matched {
        Some(_) => title_controls(frame, title_row, liked, add, mouse, hit),
        None => 0,
    };

    let title = match (matched, &announced) {
        (Some(t), _) => t.name.clone(),
        (None, Some(said)) => said.clone(),
        (None, None) => radio.station.name.clone(),
    };
    // One cell of daylight between the title and the controls, as on the
    // Spotify deck, so a long name ends in an ellipsis rather than against
    // the pill.
    let title_w = if controls_w == 0 {
        title_row.width
    } else {
        title_row.width.saturating_sub(controls_w + 1)
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            fit(&title, title_w as usize),
            theme::accent().add_modifier(Modifier::BOLD),
        )),
        Rect {
            width: title_w,
            ..title_row
        },
    );

    if area.height < MASTHEAD_H {
        return;
    }
    let row = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    let vol_seg = draw_volume(frame, row, radio.volume_percent, mouse, hit);
    let meta = Rect {
        width: row.width.saturating_sub(vol_seg.width + 1),
        ..row
    };
    if meta.width == 0 {
        return;
    }

    // Matched: the Spotify deck's row, built by the same helpers, so the two
    // cannot drift on how a segment is clipped or dropped.
    if let Some(t) = matched {
        let mut spans = Vec::new();
        let mut x = meta.x;
        credit_segment(
            &mut spans,
            &mut x,
            meta,
            mouse,
            &t.credits,
            &mut hit.now_artist_links,
        );
        let album = clip(&t.album, meta);
        if sep(&mut spans, &mut x, &album, meta) {
            hit.now_album = link(
                &mut spans,
                &mut x,
                meta,
                mouse,
                album,
                dim,
                t.album_id.is_some(),
            );
        }
        if !t.release_year.is_empty() && sep(&mut spans, &mut x, &t.release_year, meta) {
            spans.push(Span::styled(t.release_year.clone(), dim));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), meta);
        return;
    }

    // Not matched. Row 0 already said whatever there was to say, so this row
    // is the station describing itself — a quieter fact, but a fact, and
    // better than a blank line under a name.
    //
    // No links and no `★`: `hit` is cleared at the top of every frame
    // (`super::clear_hits`), so the rects simply stay empty and unhittable
    // rather than having to be cleared here.
    frame.render_widget(
        Paragraph::new(Line::styled(
            fit(&station_subtitle(radio), meta.width as usize),
            dim,
        )),
        meta,
    );
}

/// What a station says about itself when it is not announcing a track.
///
/// Its genres, and nothing else. The country belongs to [`radio_station_row`],
/// along with the station's name and its format: a fact said twice on one deck
/// is a fact you have to read twice to find out it is the same one.
fn station_subtitle(radio: &RadioPlayback) -> String {
    radio.station.tags.clone()
}

/// The radio deck's answer to [`progress`]: `LIVE`, a filled track, and how
/// long you have been listening.
///
/// A broadcast has no length, so there is no ratio to draw and nothing to
/// seek to — the track is drawn full rather than empty, because the stream is
/// arriving, not stalled.
///
/// Clears `hit.gauge`, for the reason its neighbours clear theirs: this row is
/// where the Spotify deck draws its progress track, so a station started over a
/// track left last frame's seek rect lying under the `LIVE` bar. Clicking it
/// sent a `SeekTo` to Spirc — a transport command aimed at the engine that is
/// not playing, and the client refuses those now, but a control that is not a
/// control should not be reachable in the first place.
pub fn radio_status(frame: &mut Frame, row: Rect, radio: &RadioPlayback, hit: &mut HitAreas) {
    hit.gauge = Rect::default();
    if row.width == 0 {
        return;
    }
    // A station that would not come up gets the reason where the bar goes.
    // Neither a meter nor an elapsed count is true of a station that is not
    // sending, and drawing them anyway is the deck reporting a stream that is
    // not there.
    if let Some(reason) = &radio.failure {
        let mark = "OFF AIR ";
        let mut line = vec![Span::styled(mark, theme::warn())];
        let left = row.width as usize - width(mark).min(row.width as usize);
        line.push(Span::styled(fit(reason, left), theme::dim()));
        frame.render_widget(Paragraph::new(Line::from(line)), row);
        return;
    }
    if row.width <= TIME_W {
        return;
    }
    let live = "LIVE ";
    let elapsed = format!(" {}", format_duration(radio.elapsed().as_millis() as u64));
    let track_w = row
        .width
        .saturating_sub((width(live) + width(&elapsed)) as u16)
        .max(1);
    let mut line = vec![Span::styled(live, theme::green())];
    // Full, not empty: the whole of a live stream is "now".
    line.extend(meter(1.0, track_w, false, false));
    line.push(Span::styled(elapsed, theme::dim()));
    frame.render_widget(Paragraph::new(Line::from(line)), row);
}

/// The radio deck's transport, laid out like [`transport`]: previous flush
/// left, the play state centred, and the forward control flush right.
///
/// A broadcast has no track either side of it, but a station does have
/// something either side: what you were listening to before it, and — with
/// nothing stepped out of — the rest of its own country. `steps` says which of
/// those exist, and a control that leads nowhere is not drawn rather than
/// drawn dead: a greyed control that never lights is a question the UI keeps
/// asking and answering. Records `hit.prev_btn`, `hit.play_btn` and
/// `hit.next_btn`, clearing each first so a click cannot land on last frame's
/// rects.
///
/// `play` is what the corner is saying — see [`transport`] for why a station
/// still connecting gets no pill at all. The two buttons stay where they are
/// through that window, so the row does not move around what is missing.
pub fn radio_transport(
    frame: &mut Frame,
    row: Rect,
    play: PlayState,
    steps: RadioSteps,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    hit.prev_btn = Rect::default();
    hit.next_btn = Rect::default();
    hit.play_btn = Rect::default();

    let button = theme::text();
    let forward = match steps.forward {
        RadioForward::None => None,
        RadioForward::Next => Some(NEXT_LABEL),
        RadioForward::Seek => Some(SEEK_LABEL),
    };
    // Only the controls actually offered, unlike the Spotify row's fixed pair:
    // a station with nothing behind it gives the pill the left edge's cells.
    let edges = (if steps.back { width(PREV_LABEL) } else { 0 } + forward.map_or(0, width)) as u16;

    if steps.back {
        let mut spans = Vec::new();
        let mut x = row.x;
        hit.prev_btn = segment(
            &mut spans,
            &mut x,
            row,
            mouse,
            vec![Span::styled(PREV_LABEL, button)],
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
    }

    if let Some(pill) = state_spans(play) {
        let pill_w: u16 = pill.iter().map(|s| s.width() as u16).sum();
        if row.width >= edges + pill_w + 2 {
            let seg = Rect {
                x: row.x + (row.width - pill_w) / 2,
                width: pill_w,
                ..row
            };
            let mut spans = Vec::new();
            let mut x = seg.x;
            hit.play_btn = segment(&mut spans, &mut x, row, mouse, pill);
            frame.render_widget(Paragraph::new(Line::from(spans)), seg);
        }
    }

    if let Some(label) = forward
        && row.width > edges
    {
        hit.next_btn = right_row(frame, row, mouse, vec![vec![Span::styled(label, button)]])[0];
    }
}

/// The station control at the right end of [`radio_station_row`], in both
/// states.
///
/// Built from [`super::table::LIKED_MARK`] and padded to one width for the same
/// reasons [`like_label`] is: the directory's own table marks a kept station
/// with the same `★`, and a control that changes width when it flips moves out
/// from under the cursor that just pressed it.
///
/// "saved", not "liked" — the deck's other `★` is about the record the station
/// is playing, and Spotify is where that one is kept. This one is a station,
/// kept in a file of spot's own because the directory has no account to keep it
/// in. The two sit on different rows and say different words.
/// The control back to the broadcast, drawn while a record off the station's
/// own list is playing instead of it.
const LIVE_LABEL: &str = "◂ live";

fn save_label(saved: bool) -> String {
    let mark = super::table::LIKED_MARK;
    if saved {
        format!("{mark} saved")
    } else {
        format!("{mark} save ")
    }
}

/// The radio deck's bottom row: the station itself — what it is called, where
/// it broadcasts from, how it sounds, and whether you have kept it.
///
/// Shuffle is not here: there is one stream and no order to put it in. The row
/// carries controls instead, rather than spending its width restating what the
/// masthead already says.
///
/// The station is what its list is called, so the name heads that list the way
/// a queue name heads the queue — marker and all, and clicking it folds the
/// list away and back.
///
/// The name is white and the rest grey, so the row reads as one fact with its
/// footnotes rather than as three of equal weight. The country is a link into
/// the directory's page for it — clickable only where the directory gave us a
/// code to ask by; without one the name is still printed, just inert, exactly as
/// an artist without an id is on the masthead. The genres are deliberately not
/// here: they are what the masthead falls back to (see [`station_subtitle`]),
/// and this row is about the station, not its programming.
///
/// Records `hit.save_station_btn`, `hit.station_country` and `hit.queue_name`,
/// and clears the one Spotify-deck control that shares the row so a click
/// cannot land on last frame's rect.
pub fn radio_station_row(
    frame: &mut Frame,
    row: Rect,
    radio: &RadioPlayback,
    saved: bool,
    // Whether the list under this row is folded away, or [`None`] where the
    // row is drawn with no list under it at all — the bottom bar, where the
    // same click opens the player instead.
    folded: Option<bool>,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    hit.shuffle_btn = Rect::default();
    if row.width == 0 {
        return;
    }
    let s = &radio.station;

    // The controls first, so the text is what gives way on a tight row rather
    // than the things here you can press. `right_row` drops each whole below
    // its own width, which leaves the row a plain readout rather than half a
    // button.
    //
    // `live` only where the stream has stood down for a record off the
    // station's list: on air it would name the state you are already in.
    let mut groups = Vec::new();
    if radio.off_air {
        groups.push(vec![Span::styled(LIVE_LABEL, theme::accent())]);
    }
    groups.push(vec![Span::styled(
        save_label(saved),
        if saved { theme::accent() } else { theme::dim() },
    )]);
    let rects = right_row(frame, row, mouse, groups);
    let (live_btn, save_btn) = match radio.off_air {
        true => (rects[0], rects[1]),
        false => (Rect::default(), rects[0]),
    };
    hit.radio_live_btn = live_btn;
    hit.save_station_btn = save_btn;

    // One cell of daylight between the text and the controls, as on the
    // masthead.
    let controls_w = [live_btn, save_btn]
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| r.width + 1)
        .sum::<u16>();
    let left = Rect {
        width: row.width.saturating_sub(controls_w),
        ..row
    };
    if left.width == 0 {
        return;
    }

    let dim = theme::dim();
    let mut spans = Vec::new();
    let mut x = left.x;
    // The station names its own list, so the marker rides with the name and
    // the pair is the fold control — the same click, in the same place, as the
    // queue name in the Spotify player. In the bottom bar the same rect is
    // what opens the player, which is where that list is drawn.
    let mark = match folded {
        Some(true) => "▸ ",
        Some(false) => "▾ ",
        None => "",
    };
    let name = clip(&format!("{mark}{}", s.name), left);
    if sep(&mut spans, &mut x, &name, left) {
        hit.queue_name = link(
            &mut spans,
            &mut x,
            left,
            mouse,
            name,
            theme::bright().add_modifier(Modifier::BOLD),
            true,
        );
    }
    let country = clip(&s.country, left);
    if !country.is_empty() && sep(&mut spans, &mut x, &country, left) {
        hit.station_country = link(
            &mut spans,
            &mut x,
            left,
            mouse,
            country,
            dim,
            !s.countrycode.is_empty(),
        );
    }
    let quality = s.quality();
    if !quality.is_empty() && sep(&mut spans, &mut x, &quality, left) {
        spans.push(Span::styled(quality, dim));
    }
    // Last, so it is the first thing a tight row gives up: the directory says
    // nothing about channels, so this is the one segment that reads the live
    // decoder rather than the station record.
    if let Some(mode) = radio.channel_label()
        && sep(&mut spans, &mut x, &mode, left)
    {
        spans.push(Span::styled(mode, dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), left);
}

/// One row: elapsed, the track, time remaining.
///
/// No grab handle, in either view: the track is a readout of where the record
/// is, and a `●` on a line that moves by itself reads as one more control on a
/// row whose transport is spelled out directly underneath. Clicking it still
/// seeks — the volume slider is the one that keeps its handle.
///
/// Records `hit.gauge` over the track alone — clicking anywhere on it seeks.
pub fn progress(frame: &mut Frame, row: Rect, pb: &Playback, duration_ms: u64, hit: &mut HitAreas) {
    if row.width <= TIME_W {
        return;
    }
    let progress = pb.interpolated_progress_ms(duration_ms);
    let ratio = if duration_ms > 0 {
        (progress as f64 / duration_ms as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let elapsed = format!("{} ", format_duration(progress));
    let remaining = format!(
        " -{}",
        format_duration(duration_ms.saturating_sub(progress))
    );
    let track_w = row
        .width
        .saturating_sub((elapsed.chars().count() + remaining.chars().count()) as u16)
        .max(1);
    hit.gauge = Rect {
        x: row.x + elapsed.chars().count() as u16,
        width: track_w,
        ..row
    }
    .intersection(row);
    // Elapsed brighter than remaining: it is the number that is actually
    // moving, and the pair reads as a direction rather than as two readouts.
    let mut line = vec![Span::styled(elapsed, theme::bright())];
    line.extend(meter(ratio, track_w, false, false));
    line.push(Span::styled(remaining, theme::dim()));
    frame.render_widget(Paragraph::new(Line::from(line)), row);
}

/// One row: previous flush left, the play state centred, next flush right.
///
/// The two buttons are pushed to opposite edges rather than sat side by side
/// because the row directly under the progress track reads as belonging to it,
/// and a pair spanning the same width says "back" and "forward" about the
/// thing above them without a label. The pill sits between them, centred under
/// the track it is reporting on — the three together are the transport a deck
/// has always had, in the order it has always had them.
///
/// Previous and next are grey rather than accent. They are the plainest
/// controls on the deck, and painting them in the cover's colour put the two
/// least interesting marks on the row in its loudest one; the pill is its only
/// colour now, which is also the only thing on it that reports rather than
/// offers.
///
/// A row too narrow for both buttons keeps previous alone — `right_row` would
/// otherwise paint next over it — and one too narrow to clear them both drops
/// the pill rather than colliding with it. Records `hit.prev_btn`,
/// `hit.play_btn` and `hit.next_btn`.
///
/// `play` is the same answer the corner of the header is drawing, so the two
/// always agree — see [`super::play_state`]. On [`PlayState::Loading`] the
/// pill is left out entirely and `hit.play_btn` stays empty: the sound has
/// been asked for and has not arrived, so `▶ play` would offer to start what
/// is already starting and `■ pause` would claim audio nobody can hear. The
/// corner says `LOADING` for exactly that window, and previous and next stay
/// where they are, so the row does not move around what is missing.
pub fn transport(
    frame: &mut Frame,
    row: Rect,
    play: PlayState,
    forward: Forward,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    let button = theme::text();
    let mut spans = Vec::new();
    let mut x = row.x;
    hit.play_btn = Rect::default();
    hit.prev_btn = segment(
        &mut spans,
        &mut x,
        row,
        mouse,
        vec![Span::styled(PREV_LABEL, button)],
    );
    frame.render_widget(Paragraph::new(Line::from(spans)), row);

    let ahead = forward.label();
    let edges = (width(PREV_LABEL) + width(ahead)) as u16;

    // Centred on the row rather than between the two buttons: they are of
    // different widths, and a pill centred between them would not line up with
    // the middle of the progress track above it.
    if let Some(pill) = state_spans(play) {
        let pill_w: u16 = pill.iter().map(|s| s.width() as u16).sum();
        if row.width >= edges + pill_w + 2 {
            let seg = Rect {
                x: row.x + (row.width - pill_w) / 2,
                width: pill_w,
                ..row
            };
            let mut spans = Vec::new();
            let mut x = seg.x;
            hit.play_btn = segment(&mut spans, &mut x, row, mouse, pill);
            frame.render_widget(Paragraph::new(Line::from(spans)), seg);
        }
    }

    if row.width < edges + 1 {
        return;
    }
    hit.next_btn = right_row(frame, row, mouse, vec![vec![Span::styled(ahead, button)]])[0];
}

/// One row: the playing queue's name and length on the left, the shuffle
/// state opposite. Repeat is not here because playback is always
/// repeat-all; see [`super::now_playing`].
///
/// The name is a link. From the bottom bar it opens the player; in the player
/// it is the heading the queue hangs from, and clicking it folds that list
/// away and back.
///
/// `fold` is `Some(folded)` only in the player, where there is a list under
/// the name to fold. It puts a `▾`/`▸` on the front of the name, inside the
/// link so the marker is part of the target rather than something beside it —
/// a control that can be in two states has to say which one it is in.
///
/// Records `hit.shuffle_btn` and `hit.queue_name`.
pub fn context_row(
    frame: &mut Frame,
    row: Rect,
    shuffle: bool,
    queue: Option<&Queue>,
    fold: Option<bool>,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    let dim = theme::dim();
    let accent = theme::accent();

    // The station row's own controls share this row in the other deck, so a
    // click cannot be allowed to land on last frame's rects — the mirror of
    // what `radio_station_row` clears on its way in.
    hit.save_station_btn = Rect::default();
    hit.radio_live_btn = Rect::default();

    // The control first: the name is what gets truncated if the row is tight.
    hit.shuffle_btn = right_row(
        frame,
        row,
        mouse,
        vec![vec![Span::styled(
            format!("shuffle {}", if shuffle { "on" } else { "off" }),
            if shuffle { accent } else { dim },
        )]],
    )[0];

    let Some(q) = queue else { return };
    let text = Rect {
        width: row.width.saturating_sub(hit.shuffle_btn.width + 2),
        ..row
    };
    if text.width == 0 {
        return;
    }

    // A true count of the play order: the rows on the player screen are the
    // rows this number describes.
    let count = format!(" · {} tracks", q.len());
    let mark = match fold {
        Some(true) => "▸ ",
        Some(false) => "▾ ",
        None => "",
    };
    // The name is clipped rather than the count or the marker: the count is
    // three or four cells and still says how much of the queue there is, and
    // the marker says which way the list is, neither of which a name cut in
    // half does.
    let name_w = text
        .width
        .saturating_sub(width(&count) as u16 + width(mark) as u16);
    let name = fit(q.name(), name_w as usize).trim_end().to_string();

    let mut spans = Vec::new();
    let mut x = text.x;
    hit.queue_name = link(
        &mut spans,
        &mut x,
        text,
        mouse,
        format!("{mark}{name}"),
        theme::bright().add_modifier(Modifier::BOLD),
        true,
    );
    spans.push(Span::styled(count, dim));
    if q.loading {
        spans.push(Span::styled(" (loading…)", dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), text);
}

#[cfg(test)]
mod tests {

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    /// The playing track's length in the tests. Off the second boundary:
    /// progress interpolates in real time, so a remaining value of exactly
    /// 142_000 ms would flip from 2:22 to 2:21 within 1 ms of the anchor.
    const DURATION: u64 = 225_500;

    fn song() -> Track {
        Track {
            uri: "spotify:track:x".into(),
            name: "Song Title".into(),
            artists: "Artist Name".into(),
            album: "Album Name".into(),
            release_year: "2020".into(),
            duration_ms: DURATION,
            track_number: 1,
            album_id: Some("alb1".into()),
            credits: vec![Credit {
                name: "Artist Name".into(),
                id: Some("art1".into()),
            }],
            cover_url: None,
        }
    }

    fn transport_state() -> Playback {
        let mut pb = Playback::started(56, false);
        pb.anchor(83_000);
        pb
    }

    fn queue(len: usize) -> Queue {
        Queue::new(
            (0..len)
                .map(|i| Track {
                    uri: format!("spotify:track:t{i}"),
                    name: format!("Track {i}"),
                    artists: "Someone".into(),
                    album: "Album".into(),
                    release_year: "2020".into(),
                    duration_ms: 60_000,
                    track_number: i as u32 + 1,
                    album_id: None,
                    credits: vec![Credit {
                        name: "Someone".into(),
                        id: None,
                    }],
                    cover_url: None,
                })
                .collect(),
            0,
            "My Mix",
        )
    }

    /// The whole credit run the metadata row recorded, from the first name to
    /// the last. The fixtures credit one artist, so this is that name's rect;
    /// the tests below use it to place the album segment beside it.
    fn artist_rect(hit: &HitAreas) -> Rect {
        hit.now_artist_links
            .iter()
            .map(|(rect, _)| *rect)
            .reduce(|a, b| a.union(b))
            .unwrap_or_default()
    }

    /// Render one deck row set into a bare rect and hand back the text plus
    /// the hit areas it recorded.
    fn render(
        width: u16,
        height: u16,
        draw: impl FnOnce(&mut Frame, Rect, &mut HitAreas),
    ) -> (Vec<String>, HitAreas, ratatui::buffer::Buffer) {
        let mut hit = HitAreas::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw(f, area, &mut hit);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect();
        (lines, hit, buffer)
    }

    #[test]
    fn the_masthead_writes_two_rows_and_their_controls() {
        let t = song();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, None, false, None, h)
        });
        assert!(lines[0].starts_with("Song Title"), "{:?}", lines[0]);
        // The state pill left this row for the transport; the title has the
        // whole width to itself now.
        assert!(!lines[0].contains("playing"), "{:?}", lines[0]);
        assert!(hit.play_btn.is_empty(), "{:?}", hit.play_btn);
        assert!(
            lines[1].starts_with("Artist Name · Album Name · 2020"),
            "{:?}",
            lines[1]
        );
        assert!(lines[1].contains("vol ") && lines[1].contains(" 56%"));
        assert_eq!(hit.volume_slider.y, 1);
        assert_eq!(artist_rect(&hit).y, 1);
        assert_eq!(hit.now_album.x, artist_rect(&hit).right() + 3);
    }

    /// The control holds the right end of the title row, says which way it would
    /// go, and keeps one width in both states so nothing under the cursor
    /// moves when it flips.
    #[test]
    fn the_masthead_carries_a_liked_control_for_the_playing_track() {
        let t = song();
        let (liked_lines, liked_hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, Some(true), false, None, h)
        });
        let (plain_lines, plain_hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, Some(false), false, None, h)
        });

        // The same mark either way; the word is what changes, so the two
        // states never come down to telling one glyph's shade from another's.
        let mark = super::super::table::LIKED_MARK;
        assert!(
            liked_lines[0].contains(&format!("{mark} liked")),
            "{:?}",
            liked_lines[0]
        );
        assert!(
            plain_lines[0].contains(&format!("{mark} like")),
            "{:?}",
            plain_lines[0]
        );
        assert_eq!(liked_hit.like_btn.right(), plain_hit.like_btn.right());
        assert_eq!(liked_hit.like_btn.y, 0, "the control left the title row");
        assert_eq!(liked_hit.like_btn.right(), 80);
        // The title still leads the row, and stops clear of the control.
        assert!(liked_lines[0].starts_with("Song Title"));
    }

    /// `+ add` takes the right end and pushes the other two left of it,
    /// so the run reads in the order the mouse hints name them.
    #[test]
    fn the_add_control_sits_right_of_the_liked_one() {
        let t = song();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, Some(true), true, None, h)
        });
        // One space between the two, owned by neither, so it never lights
        // under the pointer as a third control that does nothing.
        let mark = super::super::table::LIKED_MARK;
        assert!(
            lines[0].contains(&format!("{mark} liked ⧉ share + add")),
            "{:?}",
            lines[0]
        );
        assert_eq!(hit.add_btn.y, 0);
        assert_eq!(hit.add_btn.right(), 80);
        assert_eq!(
            hit.share_btn.x,
            hit.like_btn.right() + 1,
            "a gap belonging to neither control"
        );
        assert_eq!(
            hit.add_btn.x,
            hit.share_btn.right() + 1,
            "a gap belonging to neither control"
        );
        assert!(lines[0].starts_with("Song Title"));
    }

    /// The control does not depend on the saved state the `★` waits for, so
    /// it is drawn while that answer is still out.
    #[test]
    fn the_add_control_does_not_wait_for_the_liked_answer() {
        let t = song();
        let (lines, hit, _) = render(80, 2, |f, a, h| masthead(f, a, &t, 56, None, true, None, h));
        assert!(lines[0].contains("+ add"), "{:?}", lines[0]);
        assert_eq!(hit.add_btn.right(), 80);
        assert!(hit.like_btn.is_empty());
    }

    /// The first rung down: share goes before add, because a record you cannot
    /// act on is worth less than one you cannot link to.
    #[test]
    fn a_narrow_title_row_drops_the_share_control_first() {
        let t = song();
        let (lines, hit, _) = render(20, 2, |f, a, h| {
            masthead(f, a, &t, 56, Some(true), true, None, h)
        });
        assert!(!lines[0].contains("share"), "{:?}", lines[0]);
        assert!(hit.share_btn.is_empty());
        assert!(lines[0].contains("+ add"), "{:?}", lines[0]);
        assert_eq!(hit.add_btn.x, hit.like_btn.right() + 1);
    }

    /// The last rung: a row that cannot hold even two keeps the one that was
    /// there first, rather than losing the set to `right_row`'s all-or-nothing
    /// rule.
    #[test]
    fn a_narrow_title_row_drops_the_add_control_first() {
        let t = song();
        let (lines, hit, _) = render(12, 2, |f, a, h| {
            masthead(f, a, &t, 56, Some(true), true, None, h)
        });
        assert!(!lines[0].contains("+ add"), "{:?}", lines[0]);
        assert!(hit.add_btn.is_empty());
        assert!(hit.share_btn.is_empty());
        assert!(!hit.like_btn.is_empty(), "the ★ went with it");
    }

    /// The bar passes `false`, so its title row is unchanged by any of this.
    #[test]
    fn the_add_control_is_opt_in() {
        let t = song();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, Some(true), false, None, h)
        });
        assert!(!lines[0].contains("+ add"));
        assert!(hit.add_btn.is_empty());
        assert_eq!(hit.like_btn.right(), 80);
    }

    /// Nothing known about the track, so no control: one that cannot say
    /// which way it would go is worse than no control.
    #[test]
    fn the_liked_control_waits_for_an_answer() {
        let t = song();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, None, false, None, h)
        });
        let mark = super::super::table::LIKED_MARK;
        assert!(!lines[0].contains(mark));
        assert!(hit.like_btn.is_empty());
    }

    /// The metadata row starts flush with the title, not indented past it —
    /// the masthead is one block, not a note with a hanging column.
    #[test]
    fn the_metadata_row_is_not_indented() {
        let t = song();
        let (_, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, None, false, None, h)
        });
        assert_eq!(artist_rect(&hit).x, 0);
    }

    /// Names without ids are drawn but inert, so no rect is recorded.
    #[test]
    fn metadata_links_need_their_ids() {
        let mut t = song();
        t.credits = vec![Credit {
            name: "Artist Name".into(),
            id: None,
        }];
        t.album_id = None;
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, None, false, None, h)
        });
        assert!(lines[1].contains("Artist Name · Album Name"));
        assert!(hit.now_artist_links.is_empty() && hit.now_album.is_empty());
    }

    /// Segments are dropped whole, and measured in cells — counting `chars`
    /// reads a CJK name as half its true width and then clips it, leaving
    /// exactly the dangling " · " the check exists to prevent.
    #[test]
    fn a_wide_album_name_is_measured_in_cells() {
        let mut t = song();
        t.credits = vec![Credit {
            name: "高橋洋子".into(),
            id: Some("art1".into()),
        }];
        t.artists = "高橋洋子".into();
        t.album = "残酷な天使のテーゼ、とても長いアルバム名".into();
        let (lines, hit, _) = render(60, 2, |f, a, h| {
            masthead(f, a, &t, 56, None, false, None, h)
        });
        let row = &lines[1];
        assert!(
            !row.trim_end().ends_with('·'),
            "a dangling separator survived: {row:?}"
        );
        // The album did not fit and was dropped whole — not clipped to a
        // stump. The year is a segment of its own and may still follow the
        // artist.
        assert!(hit.now_album.is_empty(), "the album should not have fit");
        assert!(!row.contains('残'), "a clipped album survived: {row:?}");
        assert!(row.contains("· 2020"), "the year segment went too: {row:?}");
    }

    /// A self-titled record still prints both names: they are two links to two
    /// different pages, and dropping one leaves the album with no way in.
    #[test]
    fn a_self_titled_album_is_still_printed() {
        let mut t = song();
        t.album = "Artist Name".into();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &t, 56, None, false, None, h)
        });
        assert!(
            lines[1].starts_with("Artist Name · Artist Name · 2020"),
            "{:?}",
            lines[1]
        );
        assert_eq!(hit.now_album.x, artist_rect(&hit).right() + 3);
    }

    /// The progress track carries no grab handle in either view — the volume
    /// slider is the one that kept its knob.
    #[test]
    fn the_progress_track_has_no_knob() {
        let pb = transport_state();
        let (lines, _, _) = render(80, 1, |f, a, h| progress(f, a, &pb, DURATION, h));
        assert!(!lines[0].contains('●'), "{:?}", lines[0]);
        assert!(lines[0].starts_with("1:23 ━"), "{:?}", lines[0]);
        assert!(lines[0].trim_end().ends_with("-2:22"), "{:?}", lines[0]);
    }

    /// Both buttons on one row, at opposite edges, with the state pill centred
    /// between them — and the buttons in grey, not the accent.
    #[test]
    fn the_transport_pushes_its_buttons_to_the_edges() {
        let (lines, hit, buffer) = render(60, 1, |f, a, h| {
            transport(f, a, PlayState::Playing, Forward::Next, None, h)
        });
        assert!(lines[0].contains("◂◂ previous") && lines[0].contains("next ▸▸"));
        assert_eq!(hit.prev_btn.x, 0);
        assert_eq!(hit.next_btn.right(), 60);
        assert_eq!(hit.prev_btn.y, hit.next_btn.y);
        assert!(lines[0].contains("■ pause"), "{:?}", lines[0]);
        // Centred on the row: the same gap either side, to within the odd
        // cell an even-width pill cannot split.
        let (left, right) = (hit.play_btn.x, 60 - hit.play_btn.right());
        assert!(left.abs_diff(right) <= 1, "{left} vs {right}");
        assert_eq!(hit.play_btn.y, hit.prev_btn.y);
        // Grey buttons, so the pill is the only colour left on the row.
        let fg = |x: u16| buffer.cell(Position { x, y: 0 }).unwrap().fg;
        assert_eq!(fg(hit.prev_btn.x + 1), theme::TEXT);
        assert_eq!(fg(hit.next_btn.x + 1), theme::TEXT);
        assert_ne!(fg(hit.play_btn.x + 1), theme::TEXT);
    }

    /// A play asked for and not started. The pill names what a click does, so
    /// left alone it would offer `▶ play` on a track that is already starting,
    /// or `■ pause` on audio nobody can hear yet. It is left out instead: the
    /// corner of the header says `LOADING` for exactly this window, and the
    /// row has no second opinion to offer.
    #[test]
    fn the_pill_goes_away_while_a_play_is_in_flight() {
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            transport(f, a, PlayState::Loading, Forward::Next, None, h)
        });
        assert!(!lines[0].contains("play"), "{:?}", lines[0]);
        assert!(!lines[0].contains("pause"), "{:?}", lines[0]);
        assert!(
            hit.play_btn.is_empty(),
            "a click must not land on a play already happening"
        );
        // The buttons either side are untouched — only the middle is waiting,
        // so the row does not shuffle around the gap.
        assert_eq!(hit.prev_btn.x, 0);
        assert_eq!(hit.next_btn.right(), 60);
    }

    /// A station still connecting is offered no pill, for the reason a track
    /// still loading is not: neither word on it would be true, under a corner
    /// already saying `LOADING`. With nothing behind the station and no
    /// country to walk, that leaves the row empty.
    #[test]
    fn the_radio_pill_goes_away_while_a_station_connects() {
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            radio_transport(f, a, PlayState::Loading, RadioSteps::default(), None, h)
        });
        assert!(lines[0].trim().is_empty(), "{:?}", lines[0]);
        assert!(hit.play_btn.is_empty());
    }

    /// Connecting is exactly when you want out of a station that will not come
    /// up, so the two step controls stay while the pill is away.
    #[test]
    fn the_step_controls_stay_while_a_station_connects() {
        let steps = RadioSteps {
            back: true,
            forward: RadioForward::Seek,
        };
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            radio_transport(f, a, PlayState::Loading, steps, None, h)
        });
        assert!(hit.play_btn.is_empty());
        assert_eq!(hit.prev_btn.x, 0);
        assert_eq!(hit.next_btn.right(), 60);
        assert!(lines[0].contains(PREV_LABEL) && lines[0].contains(SEEK_LABEL));
    }

    /// The radio twin of [`the_transport_pushes_its_buttons_to_the_edges`]: the
    /// same three controls in the same places, so the row does not move under
    /// the eye when the source changes.
    #[test]
    fn the_radio_transport_pushes_its_steps_to_the_edges() {
        let steps = RadioSteps {
            back: true,
            forward: RadioForward::Next,
        };
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            radio_transport(f, a, PlayState::Playing, steps, None, h)
        });
        assert!(lines[0].contains(PREV_LABEL) && lines[0].contains(NEXT_LABEL));
        assert_eq!(hit.prev_btn.x, 0);
        assert_eq!(hit.next_btn.right(), 60);
        assert_eq!(hit.prev_btn.y, hit.next_btn.y);
        assert!(lines[0].contains("■ pause"));
        let (left, right) = (hit.play_btn.x, 60 - hit.play_btn.right());
        assert!(
            left.abs_diff(right) <= 1,
            "pill off centre: {left} vs {right}"
        );
    }

    /// A station reached with nothing behind it and no country to walk offers
    /// neither control, and records neither rect — so a click cannot land on
    /// what the Spotify deck left in the same place.
    #[test]
    fn a_station_with_no_path_either_side_draws_no_step_controls() {
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            radio_transport(f, a, PlayState::Playing, RadioSteps::default(), None, h)
        });
        assert!(!lines[0].contains('◂') && !lines[0].contains('▸'));
        assert!(hit.prev_btn.is_empty() && hit.next_btn.is_empty());
    }

    /// The two readings of the right-hand control are one width, so the row
    /// does not jump under the cursor that just pressed it.
    #[test]
    fn seek_and_next_are_the_same_width() {
        assert_eq!(width(SEEK_LABEL), width(NEXT_LABEL));
    }

    /// The `LIVE` bar is a readout, and the row it sits on is where the Spotify
    /// deck draws a track you can seek by clicking. A station started over a
    /// track must not leave the Spotify gauge's rect lying under it: a click on
    /// `LIVE` would then send `SeekTo` to Spirc — a transport command aimed at
    /// the engine that is not playing, which is one of the ways Spotify makes
    /// sound underneath a station.
    #[test]
    fn the_live_bar_does_not_inherit_the_seek_rect() {
        let pb = transport_state();
        let r = radio("Adroit Jazz");
        let mut hit = HitAreas::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        // Frame one: a Spotify track, which arms the gauge.
        terminal
            .draw(|f| progress(f, f.area(), &pb, DURATION, &mut hit))
            .unwrap();
        assert!(!hit.gauge.is_empty(), "the Spotify deck arms the gauge");
        // Frame two: a station on the same row.
        terminal
            .draw(|f| radio_status(f, f.area(), &r, &mut hit))
            .unwrap();
        assert!(hit.gauge.is_empty(), "{:?}", hit.gauge);
    }

    /// A station that would not come up says so where the bar goes. Neither a
    /// full meter nor an elapsed count is true of a station that is not
    /// sending, and drawing them anyway has the deck report a stream that is
    /// not there.
    #[test]
    fn a_station_that_would_not_play_says_why_instead_of_live() {
        let mut r = radio("Adroit Jazz");
        r.failure = Some("could not reach the station".into());
        r.is_playing = false;
        let (lines, hit, _) = render(60, 1, |f, a, h| radio_status(f, a, &r, h));

        assert!(lines[0].contains("OFF AIR"), "{:?}", lines[0]);
        assert!(lines[0].contains("could not reach the station"));
        assert!(!lines[0].contains("LIVE"));
        assert!(hit.gauge.is_empty(), "there is no stream to seek through");
    }

    /// Too narrow for both, previous keeps the row — `right_row` would
    /// otherwise paint next straight over it — and the pill goes first, rather
    /// than colliding with the buttons it sits between.
    #[test]
    fn a_narrow_transport_keeps_previous_alone() {
        let (lines, hit, _) = render(18, 1, |f, a, h| {
            transport(f, a, PlayState::Playing, Forward::Next, None, h)
        });
        assert!(lines[0].contains("◂◂ previous"), "{:?}", lines[0]);
        assert!(!lines[0].contains("next"), "{:?}", lines[0]);
        assert!(!lines[0].contains("playing"), "{:?}", lines[0]);
        assert!(hit.next_btn.is_empty() && hit.play_btn.is_empty());
    }

    #[test]
    fn the_context_row_names_the_queue_and_is_clickable() {
        let q = queue(24);
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            context_row(f, a, false, Some(&q), None, None, h)
        });
        assert!(lines[0].starts_with("My Mix · 24 tracks"), "{:?}", lines[0]);
        assert!(lines[0].contains("shuffle off"));
        assert_eq!(hit.queue_name.x, 0);
        assert_eq!(hit.queue_name.width as usize, "My Mix".len());
        assert_eq!(hit.shuffle_btn.right(), 60);
    }

    /// No queue loaded yet: the shuffle control still draws, and nothing
    /// claims a click on the empty half of the row.
    #[test]
    fn the_context_row_survives_an_empty_queue() {
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            context_row(f, a, false, None, None, None, h)
        });
        assert!(lines[0].contains("shuffle off"));
        assert!(hit.queue_name.is_empty());
    }

    /// The sleeve is square on screen: an R-row block is 2R cells wide, and
    /// every cell is painted or the terminal's ground shows through the
    /// half-blocks as stripes.
    #[test]
    fn the_sleeve_is_square_and_fully_painted() {
        let t = song();
        let (_, hit, buffer) = render(40, 7, |f, a, h| {
            sleeve(f, a, &t, None, h);
        });
        assert_eq!((hit.art_rect().width, hit.art_rect().height), (art_w(7), 7));
        for y in hit.art_rect().y..hit.art_rect().bottom() {
            for x in hit.art_rect().x..hit.art_rect().right() {
                let cell = buffer.cell(Position { x, y }).unwrap();
                assert!(matches!(cell.fg, Color::Rgb(..)), "no fg at {x},{y}");
                assert!(matches!(cell.bg, Color::Rgb(..)), "no bg at {x},{y}");
            }
        }
    }

    #[test]
    fn every_row_degrades_without_panicking() {
        let t = song();
        let pb = transport_state();
        let q = queue(3);
        for width in 0..40u16 {
            render(width.max(1), 2, |f, a, h| {
                let a = Rect { width, ..a };
                masthead(f, a, &t, 56, Some(true), false, None, h);
                progress(f, Rect { height: 1, ..a }, &pb, DURATION, h);
                transport(
                    f,
                    Rect { height: 1, ..a },
                    PlayState::Playing,
                    Forward::Next,
                    None,
                    h,
                );
                context_row(f, Rect { height: 1, ..a }, false, Some(&q), None, None, h);
            });
        }
    }

    /// A station announcing nothing, matched to nothing — the state most of
    /// the directory is in.
    fn radio(name: &str) -> RadioPlayback {
        let mut r = RadioPlayback::new(
            crate::app::state::Station {
                uuid: "s1".into(),
                name: name.into(),
                url: "http://stream/s1".into(),
                homepage: String::new(),
                tags: "jazz".into(),
                country: "Germany".into(),
                countrycode: "DE".into(),
                language: String::new(),
                codec: "MP3".into(),
                bitrate: 128,
                votes: 0,
                hls: false,
            },
            56,
            Default::default(),
            Default::default(),
        );
        r.is_playing = true;
        r
    }

    /// The record spot found for what the station said.
    fn matched() -> Track {
        Track {
            uri: "spotify:track:m1".into(),
            name: "Frenesi".into(),
            artists: "Peter Appleyard".into(),
            album: "The Lost 1974 Sessions".into(),
            release_year: "1974".into(),
            duration_ms: 180_000,
            track_number: 3,
            album_id: Some("alb1".into()),
            credits: vec![Credit {
                name: "Peter Appleyard".into(),
                id: Some("art1".into()),
            }],
            cover_url: None,
        }
    }

    /// The whole point of the feature: once the announced record is found, the
    /// radio deck says what the Spotify deck says, in the same places.
    #[test]
    fn a_matched_record_gives_the_radio_deck_the_spotify_decks_rows() {
        let mut r = radio("Adroit Jazz");
        *r.title.lock() = Some("Peter Appleyard - Frenesi".into());
        r.matched = crate::app::state::RadioMatch::Matched(Box::new(matched()));

        let (lines, hit, _) = render(80, 2, |f, a, h| {
            radio_masthead(f, a, &r, Some(false), false, None, h)
        });
        // Row 0 names the record, not the station.
        assert!(lines[0].starts_with("Frenesi"), "{:?}", lines[0]);
        assert!(
            lines[1].starts_with("Peter Appleyard · The Lost 1974 Sessions · 1974"),
            "{:?}",
            lines[1]
        );
        // Laid out exactly as `masthead` lays the same row out.
        assert_eq!(artist_rect(&hit).y, 1);
        assert_eq!(hit.now_album.x, artist_rect(&hit).right() + 3);
        assert!(
            !hit.like_btn.is_empty(),
            "a matched record must be likeable"
        );
        assert_eq!(hit.like_btn.y, 0);
        assert!(lines[1].contains("vol ") && lines[1].contains(" 56%"));
    }

    /// The saved state is not in yet, so there is no honest way to draw a
    /// control that has to say which way it would go. Same rule as `masthead`.
    #[test]
    fn the_liked_control_waits_until_the_saved_state_is_known() {
        let mut r = radio("Adroit Jazz");
        r.matched = crate::app::state::RadioMatch::Matched(Box::new(matched()));
        let (_, hit, _) = render(80, 2, |f, a, h| {
            radio_masthead(f, a, &r, None, false, None, h)
        });
        assert!(hit.like_btn.is_empty(), "{:?}", hit.like_btn);
    }

    /// Announced, but spot could not place it. The station's own words stand,
    /// and nothing on the row pretends to lead anywhere.
    #[test]
    fn an_unmatched_announcement_is_drawn_with_no_links_and_no_star() {
        let mut r = radio("Adroit Jazz");
        *r.title.lock() = Some("Some Band - A Song".into());
        r.matched = crate::app::state::RadioMatch::Unmatched;

        let (lines, hit, _) = render(80, 2, |f, a, h| {
            radio_masthead(f, a, &r, Some(true), false, None, h)
        });
        assert!(lines[0].starts_with("Some Band - A Song"), "{:?}", lines[0]);
        // Row 1 falls back to what the station says about itself.
        assert!(lines[1].starts_with("jazz"), "{:?}", lines[1]);
        // The country moved to the station row; saying it twice on one deck
        // means reading it twice to find out it was the same fact.
        assert!(!lines[1].contains("Germany"), "{:?}", lines[1]);
        assert!(hit.now_artist_links.is_empty());
        assert!(hit.now_album.is_empty());
        assert!(hit.like_btn.is_empty(), "nothing to save");
    }

    /// Six popular stations in ten announce; this is the other four.
    #[test]
    fn a_station_that_says_nothing_still_names_itself() {
        let r = radio("Adroit Jazz");
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            radio_masthead(f, a, &r, Some(true), false, None, h)
        });
        assert!(lines[0].starts_with("Adroit Jazz"), "{:?}", lines[0]);
        assert!(lines[1].starts_with("jazz"), "{:?}", lines[1]);
        // The country moved to the station row; saying it twice on one deck
        // means reading it twice to find out it was the same fact.
        assert!(!lines[1].contains("Germany"), "{:?}", lines[1]);
        assert!(hit.like_btn.is_empty());
    }

    /// The station's own row: what it is called, where from, how it sounds —
    /// and the control that keeps it. It says the same thing whether or not the
    /// masthead above has found a record, because it is about the station and
    /// not about what is on it.
    #[test]
    fn the_station_row_names_the_station_its_country_and_its_format() {
        let r = radio("Adroit Jazz");
        let (lines, hit, _) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        assert!(
            lines[0].starts_with("▾ Adroit Jazz · Germany · MP3 128k"),
            "{:?}",
            lines[0]
        );
        // The row it replaced said this, about a station it could not name.
        assert!(!lines[0].contains("internet radio"), "{:?}", lines[0]);
        // The genres stay on the masthead's fallback row; this one is about the
        // station, not its programming.
        assert!(!lines[0].contains("jazz ·"), "{:?}", lines[0]);

        // The country is the link, and it covers the name and nothing else.
        let at = |rect: Rect| -> String {
            lines[0]
                .chars()
                .skip(rect.x as usize)
                .take(rect.width as usize)
                .collect()
        };
        assert_eq!(at(hit.station_country), "Germany");
        assert!(hit.save_station_btn.right() == 80);
        // The station names its own list, so the name is the fold control the
        // queue name is in the other deck — marker included.
        assert_eq!(at(hit.queue_name), "▾ Adroit Jazz");
        // Shuffle does not belong on a radio row, and a rect left over from
        // the Spotify deck would still be hittable.
        assert!(hit.shuffle_btn.is_empty());

        // A record on the masthead changes nothing here.
        let mut playing = radio("Adroit Jazz");
        *playing.title.lock() = Some("Peter Appleyard - Frenesi".into());
        playing.matched = crate::app::state::RadioMatch::Matched(Box::new(matched()));
        let (named, _, _) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &playing, false, Some(false), None, h)
        });
        assert_eq!(named[0], lines[0]);
    }

    /// The directory reports a bitrate but never a channel count, and 128k
    /// mono and 128k stereo do not sound alike. Only the live decoder knows,
    /// so the row says nothing until it has decided.
    #[test]
    fn the_station_row_says_how_the_live_stream_is_mixed() {
        let r = radio("Adroit Jazz");
        let (quiet, _, _) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        assert!(
            !quiet[0].contains("stereo") && !quiet[0].contains("mono"),
            "{:?}",
            quiet[0]
        );

        r.channels.store(2, std::sync::atomic::Ordering::Relaxed);
        let (lines, _, _) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        assert!(
            lines[0].starts_with("▾ Adroit Jazz · Germany · MP3 128k · stereo"),
            "{:?}",
            lines[0]
        );

        r.channels.store(1, std::sync::atomic::Ordering::Relaxed);
        let (mono, _, _) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        assert!(mono[0].contains("MP3 128k · mono"), "{:?}", mono[0]);

        // The narrow row drops it before it drops the name, the country or the
        // format, which is what `sep` is for.
        let (tight, _, _) = render(34, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        assert!(tight[0].contains("Adroit Jazz"), "{:?}", tight[0]);
        assert!(!tight[0].contains("mono"), "{:?}", tight[0]);
    }

    /// Both states are the same width and land on the same cells, so the
    /// control does not move out from under the cursor that just pressed it —
    /// the rule the liked control on the masthead follows.
    #[test]
    fn the_save_control_says_which_way_it_would_go_without_moving() {
        let r = radio("Adroit Jazz");
        let (saved_lines, saved_hit, saved_buf) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, true, Some(false), None, h)
        });
        let (plain_lines, plain_hit, plain_buf) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        let mark = super::super::table::LIKED_MARK;
        assert!(
            saved_lines[0].contains(&format!("{mark} saved")),
            "{:?}",
            saved_lines[0]
        );
        assert!(
            plain_lines[0].contains(&format!("{mark} save ")),
            "{:?}",
            plain_lines[0]
        );
        assert_eq!(saved_hit.save_station_btn, plain_hit.save_station_btn);

        // Accent when kept, grey when not — the only colour on the row either
        // way is the one that reports a state.
        let fg = |buf: &ratatui::buffer::Buffer, x: u16| buf.cell(Position { x, y: 0 }).unwrap().fg;
        let x = saved_hit.save_station_btn.x;
        assert_eq!(fg(&saved_buf, x), theme::accent_color());
        assert_eq!(fg(&plain_buf, x), theme::DIM);
    }

    /// No code to ask the directory by, so the country is printed and inert —
    /// the same rule an artist name without an id follows on the masthead.
    #[test]
    fn a_country_with_no_code_is_drawn_but_leads_nowhere() {
        let mut r = radio("Adroit Jazz");
        r.station.countrycode = String::new();
        let (lines, hit, _) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        assert!(lines[0].contains("Germany"), "{:?}", lines[0]);
        assert!(hit.station_country.is_empty());
    }

    /// A station the directory knows nothing technical about: the format
    /// segment goes whole rather than leaving a dangling separator.
    #[test]
    fn a_station_with_no_format_drops_the_segment() {
        let mut r = radio("Adroit Jazz");
        r.station.codec = "UNKNOWN".into();
        r.station.bitrate = 0;
        let (lines, _, _) = render(80, 1, |f, a, h| {
            radio_station_row(f, a, &r, false, Some(false), None, h)
        });
        let text = lines[0].split('★').next().unwrap().trim_end();
        assert_eq!(text, "▾ Adroit Jazz · Germany", "{:?}", lines[0]);
    }

    #[test]
    fn every_radio_row_degrades_without_panicking() {
        let mut r = radio("Adroit Jazz");
        r.matched = crate::app::state::RadioMatch::Matched(Box::new(matched()));
        let paths = [
            RadioSteps::default(),
            RadioSteps {
                back: true,
                forward: RadioForward::Next,
            },
            RadioSteps {
                back: false,
                forward: RadioForward::Seek,
            },
            RadioSteps {
                back: true,
                forward: RadioForward::Seek,
            },
        ];
        let mut dead = r.clone();
        dead.failure = Some("the station stopped sending, which is a long way to say so".into());
        for width in 0..40u16 {
            for saved in [false, true] {
                for steps in paths {
                    let r = if saved { &r } else { &dead };
                    render(width.max(1), 2, |f, a, h| {
                        let a = Rect { width, ..a };
                        radio_masthead(f, a, r, Some(true), false, None, h);
                        radio_status(f, Rect { height: 1, ..a }, r, h);
                        radio_transport(
                            f,
                            Rect { height: 1, ..a },
                            PlayState::Playing,
                            steps,
                            None,
                            h,
                        );
                        radio_station_row(
                            f,
                            Rect { height: 1, ..a },
                            r,
                            saved,
                            Some(false),
                            None,
                            h,
                        );
                    });
                }
            }
        }
    }
}
