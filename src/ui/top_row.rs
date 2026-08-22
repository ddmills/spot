//! The header both views wear: the `♫ spot` mark and the path on the top row,
//! the search prompt two rows under it.
//!
//! The path used to sit on the pane's own first row and the prompt beside the
//! mark, which put the two controls that answer "where am I" on different rows
//! from each other and the one that answers "what am I looking for" next to
//! the wordmark. Swapping them puts the page's identity on the identity row —
//! mark, then path — and gives the prompt a row of its own to spread across.
//!
//! The player draws the same header over the page waiting underneath it, from
//! this same function: the only thing the two views disagree about is whether
//! the head of the path is a control (see [`super::main_pane::draw_trail`]).
//!
//! Search used to be modal: it existed only while you were typing, and
//! pressing `/` pushed the whole page down three rows to make room for a
//! bordered box. That cost a layout jump on every search, and it left the
//! feature invisible to anyone who had not read the keymap.
//!
//! The row is here whatever the mode, and is the same two rows in both
//! states, so `/` and `Esc` never move a line of the screen.
//!
//! The mark went in beside it when the left rail came out: with nothing
//! permanent on screen naming the app or leading anywhere, the row that was
//! already pinned to the top left is where a home control belongs. The player
//! draws the same mark in the same column — see [`super::table::brand`].

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::main_pane;
use super::main_pane::PageHeader;
use super::table::{brand, right_row, segment};
use super::theme;
use crate::app::state::{AppState, InputMode, MainView};

/// Shown when the box is empty, in place of the query.
const PLACEHOLDER: &str = "search artists, albums, playlists…";
/// The same box on a radio page, where it queries the station directory.
const RADIO_PLACEHOLDER: &str = "search radio stations…";

/// Cells between the mark and the path beside it. Wider than a word space, so
/// the two read as separate controls rather than as one phrase, but no wider
/// than it takes to say so: the path is what the row is *about*, and a gulf
/// between the two left it adrift in the middle of the row.
pub(super) const MARK_GAP: u16 = 3;
/// Cells between the path and a count pinned opposite it. The path is what
/// shortens when the row is tight, because a path shed from the middle still
/// reads as a path while a half-drawn count reads as a fault.
const COUNT_GAP: u16 = 3;
/// Cells between the status and whatever is to its left — the count, or the
/// path when there is none. Wider than [`COUNT_GAP`] because the two say
/// unrelated things: one is about the page, the other about the sound.
const STATUS_GAP: u16 = 4;

/// How long the tap may go quiet before a source that says it is playing is
/// read as still loading. Longer than the visualizer's own freshness window,
/// which is tuned to drop the bars' colour the instant audio stops: a word
/// that flickers between `STREAMING` and `LOADING` on a momentary underrun is
/// worse than one that waits.
const LOAD_WITHIN: std::time::Duration = std::time::Duration::from_millis(1200);

/// How dim the playing dot goes between beats, as a fraction of the accent.
/// Low enough that the swing is unmistakable, high enough that the dot never
/// reads as having gone out.
const PULSE_FLOOR: f32 = 0.22;

/// Draw the header into `area` and record its controls.
///
/// `area` is the whole header band, and what fits in it is what gets drawn:
/// the mark and the path need one row, the prompt needs three.
///
/// The player gets no prompt at any height. The mark and the path are ways
/// *out* of that view and belong on it; a search box is a way somewhere new,
/// and one drawn over a full-screen player would have to close it to show an
/// answer. The rows it would have taken go to the queue instead — which is
/// why the player asks for a band [`super::NAV_H`] tall rather than
/// [`super::HEAD_H`].
pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState, page: PageHeader) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    nav_row(frame, Rect { height: 1, ..area }, state, page);
    if !state.show_player && area.height > super::NAV_H {
        search_row(
            frame,
            Rect {
                y: area.y + super::NAV_H,
                height: 1,
                ..area
            },
            state,
        );
    }
}

/// The mark, the path beside it, and the page's count opposite them.
fn nav_row(frame: &mut Frame, row: Rect, state: &mut AppState, page: PageHeader) {
    let mouse = state.mouse_pos;
    // Drawn as its own paragraph rather than as the first span of the path's
    // line, so the path's own hit rects start where its text does.
    let mark = brand(frame, row, mouse);
    state.hit.home_btn = mark;
    // A row too narrow for the mark draws none, and then owes it no gap: the
    // path gets the whole row back rather than an indent under nothing.
    let taken = if mark.is_empty() {
        0
    } else {
        mark.width + MARK_GAP
    };
    // Measured before the path is laid out, not drawn before it: whatever is
    // pinned to the right edge has to come off the path's width first, or the
    // crumbs run underneath it.
    let mut status = status_spans(state);
    // Its cells plus the gap that separates it, or nothing at all — the two
    // travel together, so a shed status takes its gap with it.
    let mut status_slot = match status.iter().map(|s| s.width() as u16).sum::<u16>() {
        0 => 0,
        w => w + STATUS_GAP,
    };
    let count_w = page
        .count
        .as_ref()
        .map(|s| s.width() as u16 + COUNT_GAP)
        .unwrap_or(0);
    // The count is about the page and the path is the page, so they share the
    // row on the terms already set out above: the path shortens, the count
    // does not. The status is a third thing that neither of them is about, and
    // on a row too tight for all three it is what goes — a status that costs
    // you the name of the page you are on is a poor trade, and the deck at the
    // bottom is still saying the same thing. It sheds whole rather than
    // narrowing, because there is no shorter way to say `STREAMING`.
    if status_slot > 0
        && row.width.saturating_sub(taken + count_w + status_slot) < main_pane::HEAD_W as u16
    {
        status = Vec::new();
        status_slot = 0;
    }
    let reserve = count_w + status_slot;
    let field = Rect {
        x: row.x + taken,
        width: row.width.saturating_sub(taken + reserve),
        ..row
    };
    if field.width > 0 {
        // The player draws this over the page waiting underneath it, so its
        // head is the way out; on the browse screen the head is the page you
        // are already on, and a control that led there would do nothing.
        let trail = state.trail();
        let (shown, ellipsis) = main_pane::fit_trail(&trail, page.loading, field.width);
        let head = main_pane::draw_trail(
            frame,
            field,
            &shown,
            page.loading,
            ellipsis,
            state.show_player,
            mouse,
            &mut state.hit,
        );
        state.hit.close_player = head;
    }
    // The status takes the edge and the count sits inboard of it. Two calls
    // rather than one with two groups, because `right_row` applies hover to
    // every group it is given a pointer for: the status is a control and
    // should light under the cursor, the count is a readout and should not.
    state.hit.status = if status.is_empty() {
        Rect::default()
    } else {
        right_row(frame, row, mouse, vec![status])[0]
    };
    if let Some(span) = page.count {
        let count_row = Rect {
            width: row.width.saturating_sub(status_slot),
            ..row
        };
        right_row(frame, count_row, None, vec![vec![span]]);
    }
}

/// The playback status pinned opposite the mark: what is making sound, and
/// whether it is. Empty when nothing is — an idle word would be a control
/// that leads to a player with nothing in it.
///
/// Radio is checked first, as it is everywhere else: the two sources are
/// mutually exclusive by construction, and while a station is on the Spotify
/// snapshot is kept only so stopping the stream puts the last track back.
fn status_spans(state: &mut AppState) -> Vec<Span<'static>> {
    let (word, is_playing) = match (&state.radio, &state.playback) {
        (Some(r), _) => ("RADIO", r.is_playing),
        (None, Some(pb)) => ("STREAMING", pb.is_playing),
        (None, None) => return Vec::new(),
    };

    // Whether the audio is ours to judge. Playing on a phone, librespot is
    // idle and the tap will never fill — reading that as "loading" would
    // leave the word stuck yellow for the length of the record.
    let ours =
        state.radio.is_some() || state.playback.as_ref().is_some_and(|pb| pb.is_local_device);
    let fresh = state.audio_tap.is_fresh(LOAD_WITHIN);
    // Claims to be playing, but nothing has come out of it yet: a station
    // still connecting and prefetching, or a track still being fetched. The
    // radio player clears the tap before it connects, so this window is
    // exactly the buffering one.
    if is_playing && ours && !fresh {
        // The dot does not pulse here. Nothing is arriving to pulse to, and a
        // moving dot would say the opposite of the word beside it.
        return vec![
            Span::styled("● ", theme::warn()),
            Span::styled("LOADING", theme::warn()),
        ];
    }

    if !is_playing {
        // Paused is a resting state: one flat grey for the dot and the word
        // alike, so the whole control recedes rather than half of it.
        return vec![
            Span::styled("● ", theme::dim()),
            Span::styled(word, theme::dim()),
        ];
    }

    // Playing. The dot rides the loudness envelope, so it keeps time with
    // whatever is on — and falls back to the transport's own timed breath
    // when there is no local audio to ride (playback on another device),
    // which is the case that pulse was written for.
    let dot = if ours {
        let level = state
            .pulse
            .update(&state.audio_tap, fresh, std::time::Instant::now());
        // A wider travel than the transport's own breathing dot, which only
        // has to say "something is happening": this one is tracking the beat,
        // and it has to be visible from across the room to be worth doing.
        // The floor is not black — a dot that goes out between kicks reads as
        // dropping out rather than as keeping time.
        theme::accent_at(PULSE_FLOOR + (1.0 - PULSE_FLOOR) * level)
    } else {
        super::table::pulse_style(std::time::Instant::now())
    };
    vec![Span::styled("● ", dot), Span::styled(word, theme::accent())]
}

/// The search prompt, spread across a row of its own.
fn search_row(frame: &mut Frame, row: Rect, state: &mut AppState) {
    let typing = state.input_mode == InputMode::Search;
    let mouse = state.mouse_pos;
    if row.width == 0 {
        state.hit.search_box = Rect::default();
        return;
    }

    // While typing, the hint goes first: the query is what gets truncated if
    // the row is tight, and a half-drawn hint under a full query reads worse
    // than a clipped query under none.
    let mut hint_w = 0;
    if typing {
        let rects = right_row(
            frame,
            row,
            None,
            vec![vec![Span::styled("enter · esc", theme::dim())]],
        );
        hint_w = rects[0].width;
    }

    // The prompt and the text share a colour, so the row reads as one control
    // rather than as a glyph next to a word.
    let style = if typing { theme::warn() } else { theme::dim() };
    // Idle, the row says what the list below it is answering — read off the
    // view rather than off `input_buffer`, which is cleared on submit. Only a
    // search view has a query; browsing a playlist puts the prompt back.
    let text = match (typing, &state.main) {
        (true, _) => format!("{}▏", state.input_buffer),
        (false, MainView::Search(r)) if !r.query.is_empty() => r.query.clone(),
        // On a radio page the prompt searches the station directory, not
        // Spotify — see `event::handle_search_input`. The row says so, because
        // one box that quietly meant two different catalogues depending on the
        // page behind it would be a trap.
        (false, MainView::Radio(v)) => match &v.scope {
            crate::app::state::RadioScope::Search(q) if !q.is_empty() => q.clone(),
            _ => RADIO_PLACEHOLDER.to_string(),
        },
        (false, _) => PLACEHOLDER.to_string(),
    };

    let mut spans = Vec::new();
    let mut x = row.x;
    let field = Rect {
        width: row.width.saturating_sub(hint_w + 1),
        ..row
    };
    // The pill is drawn around the text alone, so hovering highlights
    // something the size of what is written.
    segment(
        &mut spans,
        &mut x,
        field,
        mouse,
        vec![Span::styled(format!("/  {text}"), style)],
    );
    frame.render_widget(Paragraph::new(Line::from(spans)), field);
    // The *clickable* area is the whole field, not just the drawn run. The row
    // reads as a full-width control, and a click past the caret would
    // otherwise fall through to the "a click elsewhere cancels the input"
    // branch in `event.rs` and silently discard what you had typed.
    state.hit.search_box = field;
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    use super::*;

    /// The whole header — the mark and the path, a blank, the prompt, a blank.
    const H: u16 = super::super::HEAD_H;
    /// The row the prompt lands on.
    const PROMPT_ROW: usize = super::super::NAV_H as usize;

    fn render(state: &mut AppState, width: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, H)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), state, PageHeader::default()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..H)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    /// The mark leads the top row and the path follows it — the two things
    /// that say where you are, on the row that says it.
    #[test]
    fn the_mark_leads_the_path_on_the_top_row() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        let lines = render(&mut st, 80);
        assert!(lines[0].starts_with("♫ spot"), "{:?}", lines[0]);
        assert!(lines[0].contains("PLAYLISTS"), "{:?}", lines[0]);
        assert_eq!(st.hit.home_btn.x, 0);
        assert!(!lines[0].contains('/'), "the prompt is not on this row");
    }

    /// Home draws no crumb: the mark is already the way there, and a `HOME`
    /// beside it would be the same control said twice.
    #[test]
    fn home_draws_the_mark_and_nothing_else() {
        let mut st = AppState::new();
        let lines = render(&mut st, 80);
        assert_eq!(lines[0].trim_end(), "♫ spot", "{:?}", lines[0]);
        assert!(st.hit.crumbs.is_empty());
    }

    /// And it is gone from the *front* of a path too, not just from its own
    /// page — the mark leads every row, so a `HOME ›` after it says nothing.
    #[test]
    fn home_is_not_the_root_of_a_deeper_path() {
        let mut st = AppState::new();
        st.push_view();
        st.main = MainView::Playlists;
        let lines = render(&mut st, 80);
        assert!(!lines[0].contains("HOME"), "{:?}", lines[0]);
        assert!(!lines[0].contains('›'), "{:?}", lines[0]);
        assert!(lines[0].contains("PLAYLISTS"), "{:?}", lines[0]);
        // The page behind it is still on the stack — this drops a step from
        // what is drawn, not from where Backspace goes.
        assert_eq!(st.view_stack.len(), 1);
    }

    /// A page's total is pinned opposite the path, on the row the path is on.
    #[test]
    fn a_count_sits_opposite_the_path() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        let mut terminal = Terminal::new(TestBackend::new(80, H)).unwrap();
        let page = PageHeader {
            loading: false,
            count: Some(Span::styled("8 playlists", theme::dim())),
        };
        terminal.draw(|f| draw(f, f.area(), &mut st, page)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row: String = (0..80)
            .filter_map(|x| buffer.cell(Position { x, y: 0 }).map(|c| c.symbol()))
            .collect();
        assert!(row.trim_end().ends_with("8 playlists"), "{row:?}");
        assert!(row.contains("PLAYLISTS  "), "{row:?}");
    }

    /// Idle the prompt is an affordance rather than a control: it is there so
    /// someone who has not read the keymap can still find search.
    #[test]
    fn idle_shows_the_prompt_and_a_placeholder() {
        let mut st = AppState::new();
        let lines = render(&mut st, 80);
        let row = &lines[PROMPT_ROW];
        assert!(row.contains("/  search"), "{row:?}");
        assert!(!row.contains('▏'), "an idle row should not show a caret");
        assert!(!row.contains("esc"));
        assert!(!st.hit.search_box.is_empty(), "the row must be clickable");
        assert_eq!(st.hit.search_box.y, PROMPT_ROW as u16);
    }

    #[test]
    fn typing_shows_the_query_a_caret_and_the_keys() {
        let mut st = AppState::new();
        st.input_mode = InputMode::Search;
        st.input_buffer = "donna the buffalo".into();
        let lines = render(&mut st, 80);
        let row = &lines[PROMPT_ROW];
        assert!(row.contains("donna the buffalo▏"), "{row:?}");
        assert!(row.contains("enter · esc"), "{row:?}");
        assert!(!row.contains(PLACEHOLDER));
    }

    /// The whole point of making the row permanent: `/` and `Esc` must not
    /// move a single line of what is around it.
    #[test]
    fn the_header_is_the_same_height_in_both_states() {
        let mut idle = AppState::new();
        let idle_lines = render(&mut idle, 80);
        let mut typing = AppState::new();
        typing.input_mode = InputMode::Search;
        typing.input_buffer = "x".into();
        let typing_lines = render(&mut typing, 80);
        assert_eq!(idle_lines.len(), typing_lines.len());
        assert_eq!(idle.hit.search_box.y, typing.hit.search_box.y);
        // The blanks above and below the prompt stay blank, so the content
        // under the header does not shift.
        for y in [1, PROMPT_ROW + 1] {
            assert!(idle_lines[y].trim().is_empty(), "{:?}", idle_lines[y]);
            assert!(typing_lines[y].trim().is_empty(), "{:?}", typing_lines[y]);
        }
    }

    /// A submitted query stays on screen while its results are, so the row
    /// says what the list below it is answering. `input_buffer` is cleared on
    /// submit, so this has to come off the view.
    #[test]
    fn a_submitted_query_stays_visible_after_the_mode_ends() {
        let mut st = AppState::new();
        st.main = MainView::Search(crate::app::state::SearchResults {
            query: "muse".into(),
            ..Default::default()
        });
        assert!(
            st.input_buffer.is_empty(),
            "the buffer is cleared on submit"
        );
        let lines = render(&mut st, 80);
        assert!(lines[PROMPT_ROW].contains("muse"));
        assert!(!lines[PROMPT_ROW].contains(PLACEHOLDER));

        // Browsing away from the results puts the prompt back.
        let mut st = AppState::new();
        let lines = render(&mut st, 80);
        assert!(lines[PROMPT_ROW].contains(PLACEHOLDER));
    }

    /// The row reads as a full-width control, so all of it must be clickable.
    /// A click past the caret used to miss `hit.search_box`, fall into the "a
    /// click elsewhere cancels the input" branch, and discard what you typed.
    ///
    /// It has the row to itself now, so it starts at the margin — the mark is
    /// a row above rather than beside it, and no longer indents it.
    #[test]
    fn the_whole_row_is_clickable_not_just_the_text() {
        let mut st = AppState::new();
        st.input_mode = InputMode::Search;
        st.input_buffer = "mu".into();
        render(&mut st, 80);
        let box_rect = st.hit.search_box;
        // Well past the end of "/  mu▏", which is 6 cells.
        assert!(
            box_rect.width > 40,
            "hit rect is only {} wide",
            box_rect.width
        );
        assert!(box_rect.contains(Position {
            x: 40,
            y: PROMPT_ROW as u16
        }));
        assert_eq!(box_rect.x, 0);
        // And it does not reach up to the mark: a click there means Home, not
        // "start typing".
        assert!(!box_rect.contains(Position { x: 2, y: 0 }));
    }

    /// A band with room for the top row but not the prompt draws the top row.
    /// The mark and the path are the ways *out*; a search box is a way
    /// somewhere new, and it is what gives.
    #[test]
    fn a_short_band_keeps_the_mark_and_sheds_the_prompt() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        let mut terminal = Terminal::new(TestBackend::new(80, super::super::NAV_H)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), &mut st, PageHeader::default()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row: String = (0..80)
            .filter_map(|x| buffer.cell(Position { x, y: 0 }).map(|c| c.symbol()))
            .collect();
        assert!(row.starts_with("♫ spot"), "{row:?}");
        assert!(row.contains("PLAYLISTS"), "{row:?}");
        assert!(st.hit.search_box.is_empty(), "the prompt should be shed");
    }

    #[test]
    fn narrow_and_empty_rows_degrade_without_panicking() {
        for width in 0..20 {
            let mut st = AppState::new();
            st.input_mode = InputMode::Search;
            st.input_buffer = "a rather long query".into();
            st.push_view();
            st.main = MainView::Playlists;
            render(&mut st, width);
        }
        let mut st = AppState::new();
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    Rect {
                        x: 0,
                        y: 0,
                        width: 40,
                        height: 0,
                    },
                    &mut st,
                    PageHeader::default(),
                )
            })
            .unwrap();
    }

    /// A Spotify snapshot on our own device, with PCM already in the tap.
    fn streaming() -> AppState {
        let mut st = AppState::new();
        st.playback = Some(crate::app::state::PlaybackSnapshot {
            is_playing: true,
            progress_ms: 0,
            duration_ms: 1000,
            track_uri: None,
            context_uri: None,
            artist_id: None,
            album_id: None,
            track_name: "Envejecer".into(),
            artists: "Erameld".into(),
            album: "Días Despejados".into(),
            release_year: "2020".into(),
            cover_url: None,
            shuffle: false,
            repeat: crate::app::state::RepeatMode::Off,
            volume_percent: 70,
            device_name: "spot".into(),
            is_local_device: true,
            fetched_at: std::time::Instant::now(),
        });
        audible(&st);
        st
    }

    fn radio_state() -> AppState {
        let mut st = AppState::new();
        st.radio = Some(crate::app::state::RadioPlayback {
            station: super::super::tests::station("s1", "KEXP"),
            is_playing: true,
            started_at: std::time::Instant::now(),
            title: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            volume_percent: 50,
        });
        audible(&st);
        st
    }

    /// Put samples in the tap, so the source reads as playing rather than as
    /// still loading. A silent buffer is enough: what the word turns on is
    /// whether audio is *arriving*, not how loud it is.
    fn audible(st: &AppState) {
        st.audio_tap.push(&[0.0; 2048], 1.0);
    }

    /// The fg of the cell at `x` on the nav row.
    fn fg(state: &mut AppState, width: u16, x: u16) -> ratatui::style::Color {
        let mut terminal = Terminal::new(TestBackend::new(width, H)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), state, PageHeader::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .cell(Position { x, y: 0 })
            .unwrap()
            .style()
            .fg
            .unwrap()
    }

    /// What is making sound, opposite the mark, on the row that says where
    /// you are. The two sources are named rather than both called "playing":
    /// a station and a track behave differently enough that which one is on
    /// is worth a word.
    #[test]
    fn the_status_names_the_source_it_is_playing_from() {
        let mut st = streaming();
        let lines = render(&mut st, 80);
        assert!(
            lines[0].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[0]
        );
        assert!(!st.hit.status.is_empty(), "the status must be clickable");
        assert_eq!(st.hit.status.y, 0);
        assert_eq!(st.hit.status.right(), 80);

        let mut st = radio_state();
        let lines = render(&mut st, 80);
        assert!(lines[0].trim_end().ends_with("● RADIO"), "{:?}", lines[0]);
    }

    /// Nothing playing draws nothing: an idle word would be a control leading
    /// to a player with nothing in it.
    #[test]
    fn nothing_playing_draws_no_status() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        let lines = render(&mut st, 80);
        assert_eq!(lines[0].trim_end(), "♫ spot   PLAYLISTS", "{:?}", lines[0]);
        assert!(st.hit.status.is_empty(), "and nothing to click");
    }

    /// Paused is a resting state, so the whole control recedes — the dot and
    /// the word together, rather than a lit dot beside a grey word.
    #[test]
    fn paused_greys_the_dot_and_the_word_alike() {
        let mut st = streaming();
        st.playback.as_mut().unwrap().is_playing = false;
        let lines = render(&mut st, 80);
        assert!(
            lines[0].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[0]
        );
        let dot = st.hit.status.x;
        assert_eq!(fg(&mut st, 80, dot), theme::DIM, "the dot is not at rest");
        assert_eq!(fg(&mut st, 80, dot + 2), theme::DIM, "nor is the word");
    }

    /// Claims to be playing, but no audio has arrived: a station still
    /// connecting and prefetching, or a track still being fetched. This is
    /// the several-second window a radio station spends buffering, which the
    /// row used to spend saying it was already playing.
    #[test]
    fn a_source_with_no_audio_yet_reads_as_loading() {
        let mut st = radio_state();
        st.audio_tap.clear();
        let lines = render(&mut st, 80);
        assert!(lines[0].trim_end().ends_with("● LOADING"), "{:?}", lines[0]);
        let dot = st.hit.status.x;
        assert_eq!(fg(&mut st, 80, dot), theme::WARN);

        // But only when the audio is ours to judge. Playing on a phone,
        // librespot is idle and the tap will never fill.
        let mut st = streaming();
        st.audio_tap.clear();
        st.playback.as_mut().unwrap().is_local_device = false;
        let lines = render(&mut st, 80);
        assert!(
            lines[0].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[0]
        );
    }

    /// The count is about the page and the status is about the sound. Both
    /// fit, in that order out from the edge, and neither lands on the path.
    #[test]
    fn a_count_and_a_status_share_the_right_edge() {
        let mut st = streaming();
        st.main = MainView::Playlists;
        let mut terminal = Terminal::new(TestBackend::new(80, H)).unwrap();
        let page = PageHeader {
            loading: false,
            count: Some(Span::styled("8 playlists", theme::dim())),
        };
        terminal.draw(|f| draw(f, f.area(), &mut st, page)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row: String = (0..80)
            .filter_map(|x| buffer.cell(Position { x, y: 0 }).map(|c| c.symbol()))
            .collect();
        assert!(
            row.trim_end().ends_with("8 playlists    ● STREAMING"),
            "{row:?}"
        );
        assert!(row.starts_with("♫ spot   PLAYLISTS  "), "{row:?}");
        // The path stops before the count, which stops before the status.
        let count_at = row.find("8 playlists").unwrap() as u16;
        assert!(count_at > 18, "the count overlaps the path: {row:?}");
        assert!(st.hit.status.x > count_at, "{row:?}");
    }

    /// On a row too tight for all three, the status is what goes. It is the
    /// one thing the row is not about, the deck is still saying it, and a
    /// status bought with the name of the page you are on is a poor trade.
    #[test]
    fn a_tight_row_sheds_the_status_before_the_path() {
        let mut st = streaming();
        st.push_view();
        st.main = MainView::Tracks(crate::app::state::TrackList::new(
            "Black Holes",
            "",
            None,
            None,
        ));
        let lines = render(&mut st, 40);
        assert!(lines[0].contains("BLACK HOLES"), "{:?}", lines[0]);
        assert!(!lines[0].contains("STREAMING"), "{:?}", lines[0]);
        assert!(st.hit.status.is_empty());
        // Given the room, it comes back.
        let lines = render(&mut st, 80);
        assert!(lines[0].contains("BLACK HOLES"), "{:?}", lines[0]);
        assert!(
            lines[0].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[0]
        );
    }
}
