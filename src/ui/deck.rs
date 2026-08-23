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

use super::play_state::PlayState;
use super::table::{
    art_w, draw_art, draw_volume, fit, link, meter, right_row, segment, state_spans, width,
};
use super::theme;
use crate::app::state::{HitAreas, PlaybackSnapshot, RadioPlayback, TrackList, format_duration};
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

/// The liked control at the right end of the title row, in both states.
/// Unpadded, like every other control on the deck — the hover pill covers the
/// text and nothing else — and of a fixed width so nothing under the cursor
/// moves when the state flips.
///
/// The same solid glyph either way — the word beside it is what says which
/// state you are in, so the pair never comes down to telling one glyph from
/// a hollow twin by its shade.
/// Built from [`super::table::LIKED_MARK`] at draw time rather than spelled
/// out here: the table's column and this control wear the same mark, and a
/// second copy of the glyph is a second thing to forget. Both states are the
/// same width, so nothing under the cursor moves when one becomes the other.
fn like_label(liked: bool) -> String {
    let mark = super::table::LIKED_MARK;
    if liked {
        format!("{mark} liked")
    } else {
        format!("{mark} like ")
    }
}

/// What both views say when nothing is playing, said once.
pub fn no_playback_hint(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("nothing playing — press Enter on a track to start").style(theme::dim()),
        Rect { height: 1, ..area },
    );
}

/// Cover art filling `area`'s height, flush with its left edge.
///
/// The block is square on screen (see [`art_w`]), so its width follows from
/// the rows it is given rather than being passed in. Returns the rect it
/// painted so the caller can lay out beside it, and records it as `hit.art`
/// — a region, not a control: the sleeve is deliberately inert, and the album
/// is opened from its name on the row below.
pub fn sleeve(
    frame: &mut Frame,
    area: Rect,
    pb: &PlaybackSnapshot,
    cover: Option<&Cover>,
    hit: &mut HitAreas,
) -> Rect {
    let art = Rect {
        width: art_w(area.height).min(area.width),
        ..area
    };
    // Seeded on the album so a given record always gets the same
    // placeholder, and it does not reshuffle between tracks of one sleeve.
    let seed = pb.album_id.as_deref().unwrap_or(pb.track_name.as_str());
    draw_art(frame, art, cover, seed);
    hit.art = art;
    art
}

/// Two rows: the track title, then artists · album · year with the volume
/// slider opposite it.
///
/// The title gets the whole row, because it is what the view is about and a
/// full-width row is what lets a long one be read. The play state used to
/// share it; it now sits under the progress track, between previous and next
/// — see [`transport`].
///
/// Records `hit.volume_slider`, `hit.now_artist` and `hit.now_album`. It does
/// *not* touch `hit.now_playing`: which region the wheel adjusts volume over
/// is the caller's decision.
/// Whether the title wears the `♫` that says "this is what is playing".
///
/// The bottom bar needs it: the bar sits under a page about something else,
/// and without the note its title is one more line of text on the screen. The
/// player does not — everything on that screen is the playing track, and the
/// mark two rows above it already owns the note and the column. Two of them
/// stacked read as a rendering fault rather than as two different things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    Show,
    Hide,
}

#[allow(clippy::too_many_arguments)]
pub fn masthead(
    frame: &mut Frame,
    area: Rect,
    pb: &PlaybackSnapshot,
    note: Note,
    liked: Option<bool>,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let dim = theme::dim();

    // Row 0: the track title, as large as a terminal allows, with the liked
    // control opposite it. The state pill used to hold that end; it moved down to the
    // transport, which left the one control the deck was missing a home.
    //
    // Drawn only once the saved state is known — an episode has no track id to
    // save, and a control that cannot say which way it would go is worse than
    // none at all. `right_row` drops it whole on a row too narrow for it, so the
    // title never has to share its cells with half a control.
    let title_row = Rect { height: 1, ..area };
    let like = liked.filter(|_| pb.track_uri.is_some());
    if let Some(liked) = like {
        let style = if liked { theme::accent() } else { dim };
        let label = like_label(liked);
        hit.like_btn = right_row(
            frame,
            title_row,
            mouse,
            vec![vec![Span::styled(label, style)]],
        )[0];
    }
    // A play in flight that named no track — the header's ▶ on a context, where
    // Spotify picks which record starts. There is nothing honest to put here
    // until the poll answers, and a bare `♫` with nothing after it reads as a
    // rendering fault rather than as a title that has not arrived. The poll is
    // the only other thing that writes the name, and it always writes one, so
    // an empty one can only mean this.
    let loading = pb.track_name.is_empty();
    let title = match (loading, note) {
        (true, _) => "loading…".to_string(),
        (false, Note::Show) => format!("♫ {}", pb.track_name),
        (false, Note::Hide) => pb.track_name.clone(),
    };
    // One cell of daylight between the title and the control, so a title that
    // runs the full width ends in an ellipsis rather than against the pill.
    let title_w = if hit.like_btn.is_empty() {
        title_row.width
    } else {
        title_row.width.saturating_sub(hit.like_btn.width + 1)
    };
    // A placeholder is not a title: dim and unbolded, so it recedes rather than
    // sitting where a record's name will be in the loudest weight on the deck.
    let title_style = if loading {
        theme::dim()
    } else {
        theme::accent().add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        Paragraph::new(Line::styled(fit(&title, title_w as usize), title_style)),
        Rect {
            width: title_w,
            ..title_row
        },
    );

    // Row 1: artists · album · year, with the volume slider right-aligned.
    // It starts flush with the title's `♫` rather than under the title's
    // first letter — the indent that used to hang it off the note made the
    // masthead read as two columns instead of one block.
    if area.height < MASTHEAD_H {
        return;
    }
    let row = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    let vol_seg = draw_volume(frame, row, pb.volume_percent, mouse, hit);
    let meta = Rect {
        width: row.width.saturating_sub(vol_seg.width + 1),
        ..row
    };
    if meta.width == 0 {
        return;
    }
    let mut spans = Vec::new();
    let mut x = meta.x;
    hit.now_artist = link(
        &mut spans,
        &mut x,
        meta,
        mouse,
        clip(&pb.artists, meta),
        theme::text(),
        pb.artist_id.is_some(),
    );
    // The album is printed even when it only repeats the artist or the track.
    // A self-titled single does read as "Abeichizoku · Abeichizoku", but the
    // two names are links to two different pages, and dropping one leaves the
    // record with no way to be opened.
    let album = clip(&pb.album, meta);
    if sep(&mut spans, &mut x, &album, meta) {
        hit.now_album = link(
            &mut spans,
            &mut x,
            meta,
            mouse,
            album,
            dim,
            pb.album_id.is_some(),
        );
    }
    if !pb.release_year.is_empty() && sep(&mut spans, &mut x, &pb.release_year, meta) {
        spans.push(Span::styled(pb.release_year.clone(), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), meta);
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
/// the row says for the rest, and [`radio_context_row`] carries the name in the
/// other two cases so it is never off the screen.
///
/// The `★` is drawn only against a matched record, and only once its saved
/// state is known — the same rule [`masthead`] follows. Keeping a *station* is
/// still done on its row in the directory: the deck's `★` has always meant
/// "save this track", and when there is one it now means it here too.
pub fn radio_masthead(
    frame: &mut Frame,
    area: Rect,
    radio: &RadioPlayback,
    note: Note,
    liked: Option<bool>,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let dim = theme::dim();
    let matched = radio.matched_track();
    let announced = radio.now_title();

    // The liked control, on the title row, exactly as `masthead` places it —
    // and only where that row names a record rather than a station.
    let title_row = Rect { height: 1, ..area };
    if let (Some(_), Some(liked)) = (matched, liked) {
        let style = if liked { theme::accent() } else { dim };
        hit.like_btn = right_row(
            frame,
            title_row,
            mouse,
            vec![vec![Span::styled(like_label(liked), style)]],
        )[0];
    }

    let name = match (matched, &announced) {
        (Some(t), _) => t.name.clone(),
        (None, Some(said)) => said.clone(),
        (None, None) => radio.station.name.clone(),
    };
    let title = match note {
        Note::Show => format!("♫ {name}"),
        Note::Hide => name,
    };
    // One cell of daylight between the title and the control, as on the
    // Spotify deck, so a long name ends in an ellipsis rather than against
    // the pill.
    let title_w = if hit.like_btn.is_empty() {
        title_row.width
    } else {
        title_row.width.saturating_sub(hit.like_btn.width + 1)
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
        hit.now_artist = link(
            &mut spans,
            &mut x,
            meta,
            mouse,
            clip(&t.artists, meta),
            theme::text(),
            t.artist_id.is_some(),
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
fn station_subtitle(radio: &RadioPlayback) -> String {
    let s = &radio.station;
    let parts: Vec<&str> = [s.tags.as_str(), s.country.as_str()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    parts.join(" · ")
}

/// The radio deck's answer to [`progress`]: `LIVE`, a filled track, and how
/// long you have been listening.
///
/// A broadcast has no length, so there is no ratio to draw and nothing to
/// seek to — the track is drawn full rather than empty, because the stream is
/// arriving, not stalled. `hit.gauge` is deliberately left unset: a bar that
/// looks like the one above a Spotify track but silently ignores clicks would
/// be worse than one that plainly is not a control.
pub fn radio_status(frame: &mut Frame, row: Rect, radio: &RadioPlayback) {
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

/// The radio deck's transport: the play/pause pill, and nothing either side.
///
/// Previous and next are not drawn rather than drawn dead. A station has no
/// track before or after it, and a greyed control that never lights is a
/// question the UI keeps asking and answering. Records `hit.play_btn`, and
/// clears the other two so a click cannot land on last frame's rects.
///
/// `play` is what the corner is saying — see [`transport`] for why a station
/// still connecting gets no pill at all.
pub fn radio_transport(
    frame: &mut Frame,
    row: Rect,
    play: PlayState,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    hit.prev_btn = Rect::default();
    hit.next_btn = Rect::default();
    hit.play_btn = Rect::default();

    let Some(pill) = state_spans(play) else {
        return;
    };
    let pill_w: u16 = pill.iter().map(|s| s.width() as u16).sum();
    if row.width < pill_w {
        return;
    }
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

/// The radio deck's bottom row: what is playing this, and how it sounds.
/// Shuffle is not here — there is one stream and no order to put it in.
///
/// The Spotify deck names the queue here, because that is what the record on
/// its masthead is coming out of. The station is exactly that for a broadcast,
/// so it is named here whenever the masthead is busy saying what is *on* the
/// station. Where the masthead has fallen back to the station's own name there
/// is nothing to add, and the row says what kind of thing it is instead — which
/// is what it always said.
pub fn radio_context_row(frame: &mut Frame, row: Rect, radio: &RadioPlayback, hit: &mut HitAreas) {
    hit.shuffle_btn = Rect::default();
    hit.queue_name = Rect::default();
    if row.width == 0 {
        return;
    }
    let quality = radio.station.quality();
    if !quality.is_empty() {
        right_row(
            frame,
            row,
            None,
            vec![vec![Span::styled(format!(" {quality} "), theme::dim())]],
        );
    }
    let left = Rect {
        width: row.width.saturating_sub(width(&quality) as u16 + 2),
        ..row
    };
    if left.width == 0 {
        return;
    }
    // The masthead is naming a record, so this row names what is playing it.
    // Where the masthead has fallen back to the station's own name, repeating
    // it here would say nothing twice.
    let names_a_track = radio.matched_track().is_some() || radio.now_title().is_some();
    let label = if names_a_track {
        radio.station.name.as_str()
    } else {
        "internet radio"
    };
    frame.render_widget(
        Paragraph::new(Line::styled(fit(label, left.width as usize), theme::dim())),
        left,
    );
}

/// One row: elapsed, the track, time remaining.
///
/// No grab handle, in either view: the track is a readout of where the record
/// is, and a `●` on a line that moves by itself reads as one more control on a
/// row whose transport is spelled out directly underneath. Clicking it still
/// seeks — the volume slider is the one that keeps its handle.
///
/// Records `hit.gauge` over the track alone — clicking anywhere on it seeks.
pub fn progress(frame: &mut Frame, row: Rect, pb: &PlaybackSnapshot, hit: &mut HitAreas) {
    if row.width <= TIME_W {
        return;
    }
    let progress = pb.interpolated_progress_ms();
    let ratio = if pb.duration_ms > 0 {
        (progress as f64 / pb.duration_ms as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let elapsed = format!("{} ", format_duration(progress));
    let remaining = format!(
        " -{}",
        format_duration(pb.duration_ms.saturating_sub(progress))
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

    let edges = (width(PREV_LABEL) + width(NEXT_LABEL)) as u16;

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
    hit.next_btn = right_row(
        frame,
        row,
        mouse,
        vec![vec![Span::styled(NEXT_LABEL, button)]],
    )[0];
}

/// One row: the playing queue's name and length on the left, the shuffle
/// state opposite. Repeat is not here because playback is always
/// repeat-all; see [`super::now_playing`].
///
/// The name is a link, and it is the mouse's way between the two views — it
/// opens the player from the bar and closes it again from the player. That
/// is why it is drawn identically in both: it is one control, not two.
///
/// Records `hit.shuffle_btn` and `hit.queue_name`.
pub fn context_row(
    frame: &mut Frame,
    row: Rect,
    pb: &PlaybackSnapshot,
    queue: Option<&TrackList>,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) {
    let dim = theme::dim();
    let accent = theme::accent();

    // The control first: the name is what gets truncated if the row is tight.
    hit.shuffle_btn = right_row(
        frame,
        row,
        mouse,
        vec![vec![Span::styled(
            format!("shuffle {}", if pb.shuffle { "on" } else { "off" }),
            if pb.shuffle { accent } else { dim },
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

    let count = format!(" · {} tracks", q.display.len());
    // The name is clipped rather than the count: the count is three or four
    // cells and says how much of the queue there is, which a name cut in
    // half no longer does.
    let name_w = text.width.saturating_sub(width(&count) as u16);
    let name = fit(&q.header.name, name_w as usize).trim_end().to_string();

    let mut spans = Vec::new();
    let mut x = text.x;
    hit.queue_name = link(
        &mut spans,
        &mut x,
        text,
        mouse,
        name,
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
    use std::time::Instant;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;
    use crate::app::state::{RepeatMode, Track, TrackListKind};

    fn snapshot() -> PlaybackSnapshot {
        PlaybackSnapshot {
            is_playing: true,
            progress_ms: 83_000,
            // Off the second boundary: progress interpolates in real time, so
            // a remaining value of exactly 142_000 ms would flip from 2:22 to
            // 2:21 within 1 ms of the snapshot.
            duration_ms: 225_500,
            track_uri: Some("spotify:track:x".into()),
            context_uri: None,
            artist_id: Some("art1".into()),
            album_id: Some("alb1".into()),
            track_name: "Song Title".into(),
            artists: "Artist Name".into(),
            album: "Album Name".into(),
            release_year: "2020".into(),
            cover_url: None,
            shuffle: false,
            repeat: RepeatMode::Context,
            volume_percent: 56,
            device_name: "MyPC".into(),
            is_local_device: true,
            fetched_at: Instant::now(),
        }
    }

    fn queue(len: usize) -> TrackList {
        let mut q = TrackList::new("My Mix", "by me", None, None);
        q.kind = TrackListKind::Playlist;
        q.tracks = (0..len)
            .map(|i| Track {
                uri: format!("spotify:track:t{i}"),
                name: format!("Track {i}"),
                artists: "Someone".into(),
                album: "Album".into(),
                release_year: "2020".into(),
                duration_ms: 60_000,
                track_number: i as u32 + 1,
                album_id: None,
                artist_id: None,
                cover_url: None,
            })
            .collect();
        q.display = (0..len).collect();
        q
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
        let pb = snapshot();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, None, None, h)
        });
        assert!(lines[0].starts_with("♫ Song Title"), "{:?}", lines[0]);
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
        assert_eq!(hit.now_artist.y, 1);
        assert_eq!(hit.now_album.x, hit.now_artist.right() + 3);
    }

    /// A play that named no track — the header's ▶ on a context, where Spotify
    /// picks the record. A bare `♫` with nothing after it would read as a
    /// rendering fault rather than as a name that has not arrived.
    #[test]
    fn a_nameless_play_says_loading_rather_than_a_stray_note() {
        let mut pb = snapshot();
        pb.track_name = String::new();
        let (lines, _, buffer) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, None, None, h)
        });
        assert!(lines[0].starts_with("loading…"), "{:?}", lines[0]);
        assert!(!lines[0].contains('♫'), "{:?}", lines[0]);
        // Dim, so a placeholder does not sit where a title will be in the
        // loudest weight on the deck.
        assert_eq!(buffer.cell(Position { x: 0, y: 0 }).unwrap().fg, theme::DIM);
    }

    /// The control holds the right end of the title row, says which way it would
    /// go, and keeps one width in both states so nothing under the cursor
    /// moves when it flips.
    #[test]
    fn the_masthead_carries_a_liked_control_for_the_playing_track() {
        let pb = snapshot();
        let (liked_lines, liked_hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, Some(true), None, h)
        });
        let (plain_lines, plain_hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, Some(false), None, h)
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
            plain_lines[0].contains(&format!("{mark} like ")),
            "{:?}",
            plain_lines[0]
        );
        assert_eq!(liked_hit.like_btn, plain_hit.like_btn);
        assert_eq!(liked_hit.like_btn.y, 0, "the control left the title row");
        assert_eq!(liked_hit.like_btn.right(), 80);
        // The title still leads the row, and stops clear of the control.
        assert!(liked_lines[0].starts_with("♫ Song Title"));
    }

    /// Nothing known about the track, so no control: one that cannot say
    /// which way it would go is worse than no control. Same for an episode,
    /// which has no track id to save.
    #[test]
    fn the_liked_control_waits_for_an_answer() {
        let pb = snapshot();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, None, None, h)
        });
        let mark = super::super::table::LIKED_MARK;
        assert!(!lines[0].contains(mark));
        assert!(hit.like_btn.is_empty());

        let mut episode = snapshot();
        episode.track_uri = None;
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &episode, Note::Show, Some(false), None, h)
        });
        assert!(!lines[0].contains(mark), "{:?}", lines[0]);
        assert!(hit.like_btn.is_empty());
    }

    /// The metadata row starts flush with the title's `♫`, not indented past
    /// it — the masthead is one block, not a note with a hanging column.
    #[test]
    fn the_metadata_row_is_not_indented() {
        let pb = snapshot();
        let (_, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, None, None, h)
        });
        assert_eq!(hit.now_artist.x, 0);
    }

    /// Names without ids are drawn but inert, so no rect is recorded.
    #[test]
    fn metadata_links_need_their_ids() {
        let mut pb = snapshot();
        pb.artist_id = None;
        pb.album_id = None;
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, None, None, h)
        });
        assert!(lines[1].contains("Artist Name · Album Name"));
        assert!(hit.now_artist.is_empty() && hit.now_album.is_empty());
    }

    /// Segments are dropped whole, and measured in cells — counting `chars`
    /// reads a CJK name as half its true width and then clips it, leaving
    /// exactly the dangling " · " the check exists to prevent.
    #[test]
    fn a_wide_album_name_is_measured_in_cells() {
        let mut pb = snapshot();
        pb.artists = "高橋洋子".into();
        pb.album = "残酷な天使のテーゼ、とても長いアルバム名".into();
        let (lines, hit, _) = render(60, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, None, None, h)
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
        let mut pb = snapshot();
        pb.album = "Artist Name".into();
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            masthead(f, a, &pb, Note::Show, None, None, h)
        });
        assert!(
            lines[1].starts_with("Artist Name · Artist Name · 2020"),
            "{:?}",
            lines[1]
        );
        assert_eq!(hit.now_album.x, hit.now_artist.right() + 3);
    }

    /// The progress track carries no grab handle in either view — the volume
    /// slider is the one that kept its knob.
    #[test]
    fn the_progress_track_has_no_knob() {
        let pb = snapshot();
        let (lines, _, _) = render(80, 1, |f, a, h| progress(f, a, &pb, h));
        assert!(!lines[0].contains('●'), "{:?}", lines[0]);
        assert!(lines[0].starts_with("1:23 ━"), "{:?}", lines[0]);
        assert!(lines[0].trim_end().ends_with("-2:22"), "{:?}", lines[0]);
    }

    /// Both buttons on one row, at opposite edges, with the state pill centred
    /// between them — and the buttons in grey, not the accent.
    #[test]
    fn the_transport_pushes_its_buttons_to_the_edges() {
        let (lines, hit, buffer) = render(60, 1, |f, a, h| {
            transport(f, a, PlayState::Playing, None, h)
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
            transport(f, a, PlayState::Loading, None, h)
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

    /// The radio deck's transport is the pill and nothing else, so a station
    /// still connecting leaves the row empty rather than offering `■ pause`
    /// over silence — which is what it used to do, under a corner already
    /// saying `LOADING`.
    #[test]
    fn the_radio_pill_goes_away_while_a_station_connects() {
        let (lines, hit, _) = render(60, 1, |f, a, h| {
            radio_transport(f, a, PlayState::Loading, None, h)
        });
        assert!(lines[0].trim().is_empty(), "{:?}", lines[0]);
        assert!(hit.play_btn.is_empty());
    }

    /// Too narrow for both, previous keeps the row — `right_row` would
    /// otherwise paint next straight over it — and the pill goes first, rather
    /// than colliding with the buttons it sits between.
    #[test]
    fn a_narrow_transport_keeps_previous_alone() {
        let (lines, hit, _) = render(18, 1, |f, a, h| {
            transport(f, a, PlayState::Playing, None, h)
        });
        assert!(lines[0].contains("◂◂ previous"), "{:?}", lines[0]);
        assert!(!lines[0].contains("next"), "{:?}", lines[0]);
        assert!(!lines[0].contains("playing"), "{:?}", lines[0]);
        assert!(hit.next_btn.is_empty() && hit.play_btn.is_empty());
    }

    #[test]
    fn the_context_row_names_the_queue_and_is_clickable() {
        let pb = snapshot();
        let q = queue(24);
        let (lines, hit, _) = render(60, 1, |f, a, h| context_row(f, a, &pb, Some(&q), None, h));
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
        let pb = snapshot();
        let (lines, hit, _) = render(60, 1, |f, a, h| context_row(f, a, &pb, None, None, h));
        assert!(lines[0].contains("shuffle off"));
        assert!(hit.queue_name.is_empty());
    }

    /// The sleeve is square on screen: an R-row block is 2R cells wide, and
    /// every cell is painted or the terminal's ground shows through the
    /// half-blocks as stripes.
    #[test]
    fn the_sleeve_is_square_and_fully_painted() {
        let pb = snapshot();
        let (_, hit, buffer) = render(40, 7, |f, a, h| {
            sleeve(f, a, &pb, None, h);
        });
        assert_eq!((hit.art.width, hit.art.height), (art_w(7), 7));
        for y in hit.art.y..hit.art.bottom() {
            for x in hit.art.x..hit.art.right() {
                let cell = buffer.cell(Position { x, y }).unwrap();
                assert!(matches!(cell.fg, Color::Rgb(..)), "no fg at {x},{y}");
                assert!(matches!(cell.bg, Color::Rgb(..)), "no bg at {x},{y}");
            }
        }
    }

    #[test]
    fn every_row_degrades_without_panicking() {
        let pb = snapshot();
        let q = queue(3);
        for width in 0..40u16 {
            render(width.max(1), 2, |f, a, h| {
                let a = Rect { width, ..a };
                masthead(f, a, &pb, Note::Show, Some(true), None, h);
                progress(f, Rect { height: 1, ..a }, &pb, h);
                transport(f, Rect { height: 1, ..a }, PlayState::Playing, None, h);
                context_row(f, Rect { height: 1, ..a }, &pb, Some(&q), None, h);
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
            artist_id: Some("art1".into()),
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
            radio_masthead(f, a, &r, Note::Show, Some(false), None, h)
        });
        // Row 0 names the record, not the station.
        assert!(lines[0].starts_with("♫ Frenesi"), "{:?}", lines[0]);
        assert!(
            lines[1].starts_with("Peter Appleyard · The Lost 1974 Sessions · 1974"),
            "{:?}",
            lines[1]
        );
        // Laid out exactly as `masthead` lays the same row out.
        assert_eq!(hit.now_artist.y, 1);
        assert_eq!(hit.now_album.x, hit.now_artist.right() + 3);
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
            radio_masthead(f, a, &r, Note::Show, None, None, h)
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
            radio_masthead(f, a, &r, Note::Show, Some(true), None, h)
        });
        assert!(
            lines[0].starts_with("♫ Some Band - A Song"),
            "{:?}",
            lines[0]
        );
        // Row 1 falls back to what the station says about itself.
        assert!(lines[1].starts_with("jazz · Germany"), "{:?}", lines[1]);
        assert!(hit.now_artist.is_empty());
        assert!(hit.now_album.is_empty());
        assert!(hit.like_btn.is_empty(), "nothing to save");
    }

    /// Six popular stations in ten announce; this is the other four.
    #[test]
    fn a_station_that_says_nothing_still_names_itself() {
        let r = radio("Adroit Jazz");
        let (lines, hit, _) = render(80, 2, |f, a, h| {
            radio_masthead(f, a, &r, Note::Show, Some(true), None, h)
        });
        assert!(lines[0].starts_with("♫ Adroit Jazz"), "{:?}", lines[0]);
        assert!(lines[1].starts_with("jazz · Germany"), "{:?}", lines[1]);
        assert!(hit.like_btn.is_empty());
    }

    /// The station moves to the context row exactly when a record takes its
    /// place on the masthead, so it is never drawn twice and never missing.
    #[test]
    fn the_context_row_names_the_station_once_the_masthead_names_a_record() {
        let quiet = radio("Adroit Jazz");
        let (lines, _, _) = render(80, 1, |f, a, h| radio_context_row(f, a, &quiet, h));
        assert!(lines[0].starts_with("internet radio"), "{:?}", lines[0]);

        let playing = radio("Adroit Jazz");
        *playing.title.lock() = Some("Peter Appleyard - Frenesi".into());
        let (lines, _, _) = render(80, 1, |f, a, h| radio_context_row(f, a, &playing, h));
        assert!(lines[0].starts_with("Adroit Jazz"), "{:?}", lines[0]);
    }

    #[test]
    fn every_radio_row_degrades_without_panicking() {
        let mut r = radio("Adroit Jazz");
        r.matched = crate::app::state::RadioMatch::Matched(Box::new(matched()));
        for width in 0..40u16 {
            render(width.max(1), 2, |f, a, h| {
                let a = Rect { width, ..a };
                radio_masthead(f, a, &r, Note::Show, Some(true), None, h);
                radio_status(f, Rect { height: 1, ..a }, &r);
                radio_transport(f, Rect { height: 1, ..a }, PlayState::Playing, None, h);
                radio_context_row(f, Rect { height: 1, ..a }, &r, h);
            });
        }
    }
}
