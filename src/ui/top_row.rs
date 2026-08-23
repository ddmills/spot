//! The header both views wear: the `♫ spot` mark and the search prompt on the
//! top row, the path on the row two under it.
//!
//! The prompt used to sit under the path rather than beside the mark, on the
//! reasoning that the top row should say where you are and a search box says
//! where you are going. That put the control you reach for most on the row
//! nothing else was on, at the margin, with no field around it — a stray dim
//! sentence between the page's name and the page — while the top row carried
//! the path, the count *and* the status, and had them fighting for its right
//! edge.
//!
//! So: the prompt takes the identity row and a fill that says it is a box you
//! can type in (see [`theme::FIELD`]), and the path takes the row under it,
//! where it has the width to itself and shares an edge with the count alone.
//! The status stays on the top row, because that is the row the player draws
//! too, and the mark and the status are the two things that must not appear to
//! move when `v` toggles between them.
//!
//! The player gets no prompt at any height. The mark and the path are ways
//! *out* of that view and belong on it; a search box is a way somewhere new,
//! and one drawn over a full-screen player would have to close it to show an
//! answer. So the player draws the path on the top row instead — see
//! [`path_row`] — and asks for a band [`super::NAV_H`] tall rather than
//! [`super::HEAD_H`]. The only other thing the two views disagree about is
//! whether the head of the path is a control (see
//! [`super::main_pane::draw_trail`]).
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
use super::play_state::{self, PlayState};
use super::table::{brand, right_row};
use super::theme;
use crate::app::state::{AppState, InputMode, MainView};

/// Shown when the box is empty, in place of the query.
///
/// It names the two catalogues rather than the kinds of thing in them, because
/// naming the sources is the new information: this box used to point at
/// whichever one the page behind it came from, and now it asks both, every
/// time, from every page. Which kinds came back is what the tab strip on the
/// results page is for.
const PLACEHOLDER: &str = "search Spotify and radio…";

/// Cells between the mark and whatever shares its row. Wider than a word
/// space, so the two read as separate controls rather than as one phrase, but
/// no wider than it takes to say so.
pub(super) const MARK_GAP: u16 = 3;
/// Cells between the path and a count pinned opposite it. The path is what
/// shortens when the row is tight, because a path shed from the middle still
/// reads as a path while a half-drawn count reads as a fault.
const COUNT_GAP: u16 = 3;
/// Cells between the status and whatever is to its left. Wider than
/// [`COUNT_GAP`] because the two say unrelated things: one is about the page,
/// the other about the sound — and on the browse screen they are not even on
/// the same row any more.
const STATUS_GAP: u16 = 4;

/// The narrowest search field worth keeping. Below this the status sheds
/// instead: a box you cannot read your own query back out of is worse than no
/// status, and the deck at the bottom is still saying what is playing.
const MIN_QUERY_W: u16 = 24;

/// The widest the field grows, however much room the row has.
///
/// It could take everything between the mark and the status, and on a wide
/// terminal that is most of the screen — a filled bar running the width of the
/// header, which reads as a band the app has painted rather than as a box you
/// can type in. Room for the longest placeholder and half again is what a
/// field looks like; past that the fill stops saying anything new.
const MAX_QUERY_W: u16 = 56;

/// How dim the playing dot goes between beats, as a fraction of the accent.
/// Low enough that the swing is unmistakable, high enough that the dot never
/// reads as having gone out.
const PULSE_FLOOR: f32 = 0.22;

/// Draw the header into `area` and record its controls.
///
/// `area` is the whole header band, and what fits in it is what gets drawn:
/// the top row needs one row, the path under it needs three.
pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState, page: PageHeader) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    // The player has no prompt at any height, and a browse band with room for
    // only one row keeps the one that says where you are: orientation beats
    // search when space is scarce, which is the trade the old layout made too.
    if state.show_player || area.height <= super::NAV_H {
        path_row(frame, Rect { height: 1, ..area }, state, page, true);
        return;
    }
    search_row(frame, Rect { height: 1, ..area }, state);
    path_row(
        frame,
        Rect {
            y: area.y + super::SEARCH_H,
            height: 1,
            ..area
        },
        state,
        page,
        false,
    );
}

/// The mark at the left of `row`, the playback status pinned opposite it, and
/// the field left between the two.
///
/// Both views' top row is this shape — the browse screen puts the prompt in
/// the field, the player puts the path there — so it is written once and the
/// two cannot drift apart.
///
/// `min_field` is how little the caller can live with. The status sheds whole
/// rather than narrowing when the field would fall under it, because there is
/// no shorter way to say `STREAMING`, and it is the one thing on the row that
/// the row is not about.
fn head_row(frame: &mut Frame, row: Rect, state: &mut AppState, min_field: u16) -> Rect {
    let mouse = state.mouse_pos;
    // Drawn as its own paragraph rather than as the first span of whatever
    // follows it, so the field's own hit rect starts where its text does.
    let mark = brand(frame, row, mouse);
    state.hit.home_btn = mark;
    // A row too narrow for the mark draws none, and then owes it no gap: the
    // field gets the whole row back rather than an indent under nothing.
    let taken = if mark.is_empty() {
        0
    } else {
        mark.width + MARK_GAP
    };
    // Measured before the field is laid out, not drawn before it: whatever is
    // pinned to the right edge has to come off the field's width first, or the
    // two run into each other.
    let mut status = status_spans(state);
    // Its cells plus the gap that separates it, or nothing at all — the two
    // travel together, so a shed status takes its gap with it.
    let mut slot = match status.iter().map(|s| s.width() as u16).sum::<u16>() {
        0 => 0,
        w => w + STATUS_GAP,
    };
    if slot > 0 && row.width.saturating_sub(taken + slot) < min_field {
        status = Vec::new();
        slot = 0;
    }
    state.hit.status = if status.is_empty() {
        Rect::default()
    } else {
        right_row(frame, row, mouse, vec![status])[0]
    };
    Rect {
        x: row.x + taken,
        width: row.width.saturating_sub(taken + slot),
        ..row
    }
}

/// The path, and the page's count pinned opposite it.
///
/// `top` is whether this is the identity row — the player's only row, and a
/// browse band too short for two. There the path shares with the mark and the
/// status and gets what is left; on a row of its own it starts at the margin
/// and has the width to itself, which is most of the point of moving it down.
fn path_row(frame: &mut Frame, row: Rect, state: &mut AppState, page: PageHeader, top: bool) {
    let mouse = state.mouse_pos;
    let field = if top {
        head_row(frame, row, state, main_pane::HEAD_W as u16)
    } else {
        row
    };
    // The count is about the page and the path is the page, so they share the
    // row on the terms set out at COUNT_GAP: the path shortens, the count
    // does not.
    let count_w = page
        .count
        .as_ref()
        .map(|s| s.width() as u16 + COUNT_GAP)
        .unwrap_or(0);
    let trail = Rect {
        width: field.width.saturating_sub(count_w),
        ..field
    };
    if trail.width > 0 {
        // The player draws this over the page waiting underneath it, so its
        // head is the way out; on the browse screen the head is the page you
        // are already on, and a control that led there would do nothing.
        let steps = state.trail();
        let (shown, ellipsis) = main_pane::fit_trail(&steps, page.loading, trail.width);
        let head = main_pane::draw_trail(
            frame,
            trail,
            &shown,
            page.loading,
            ellipsis,
            state.show_player,
            mouse,
            &mut state.hit,
        );
        state.hit.close_player = head;
    }
    // No pointer passed: `right_row` lights every group it is given one for,
    // and the count is a readout rather than a control.
    if let Some(span) = page.count {
        right_row(frame, field, None, vec![vec![span]]);
    }
}

/// The playback status pinned opposite the mark: what is making sound, and
/// whether it is. Empty when nothing is — an idle word would be a control
/// that leads to a player with nothing in it.
///
/// Which of the three states it is comes from [`play_state::status`], so this
/// word and the transport's pill cannot drift apart. All that is decided here
/// is how to paint it.
fn status_spans(state: &mut AppState) -> Vec<Span<'static>> {
    let Some(status) = play_state::status(state) else {
        return Vec::new();
    };
    match status.state {
        // The dot does not pulse here. Nothing is arriving to pulse to, and a
        // moving dot would say the opposite of the word beside it.
        PlayState::Loading => vec![
            Span::styled("● ", theme::warn()),
            Span::styled("LOADING", theme::warn()),
        ],
        // Paused is a resting state: one flat grey for the dot and the word
        // alike, so the whole control recedes rather than half of it.
        PlayState::Paused => vec![
            Span::styled("● ", theme::dim()),
            Span::styled(status.word, theme::dim()),
        ],
        PlayState::Playing => {
            // The dot rides the loudness envelope, so it keeps time with
            // whatever is on — every sample spot makes goes through the tap,
            // so there is always a level to ride.
            let level =
                state
                    .pulse
                    .update(&state.audio_tap, status.fresh, std::time::Instant::now());
            // A wider travel than the transport's own breathing dot, which
            // only has to say "something is happening": this one is
            // tracking the beat, and it has to be visible from across the
            // room to be worth doing. The floor is not black — a dot that
            // goes out between kicks reads as dropping out rather than as
            // keeping time.
            let dot = theme::accent_at(PULSE_FLOOR + (1.0 - PULSE_FLOOR) * level);
            vec![
                Span::styled("● ", dot),
                Span::styled(status.word, theme::accent()),
            ]
        }
    }
}

/// The search prompt, in the field beside the mark.
fn search_row(frame: &mut Frame, row: Rect, state: &mut AppState) {
    let typing = state.input_mode == InputMode::Search;
    let mouse = state.mouse_pos;
    let field = head_row(frame, row, state, MIN_QUERY_W);
    let field = Rect {
        width: field.width.min(MAX_QUERY_W),
        ..field
    };
    if field.width == 0 {
        state.hit.search_box = Rect::default();
        return;
    }

    // The fill is what makes this read as a box you can type in rather than
    // as a line of text at the margin, and it is drawn across the whole field
    // rather than around the run inside it. The *clickable* area has always
    // been the whole field — a click past the caret that missed would fall
    // into the "a click elsewhere cancels the input" branch in `event.rs` and
    // silently discard what you had typed — and now you can see it.
    let hover = mouse.is_some_and(|m| field.contains(m));
    frame.render_widget(Paragraph::new("").style(theme::field(hover)), field);
    state.hit.search_box = field;

    // While typing, the hint goes first: the query is what gets truncated if
    // the field is tight, and a half-drawn hint under a full query reads
    // worse than a clipped query under none. It sits *inside* the field, so
    // it can never collide with the status pinned outside it.
    let mut hint_w = 0;
    if typing {
        let hint = Rect {
            width: field.width.saturating_sub(1),
            ..field
        };
        let rects = right_row(
            frame,
            hint,
            None,
            vec![vec![Span::styled("enter · esc", theme::dim())]],
        );
        // Its cells, the one it was inset from the field's edge by, and one
        // more so the query never runs flush into it.
        hint_w = match rects[0].width {
            0 => 0,
            w => w + 2,
        };
    }

    // Idle, the row says what the list below it is answering — read off the
    // view rather than off `input_buffer`, which is cleared on submit. Only a
    // search view has a query; browsing a playlist puts the prompt back.
    let text = match (typing, &state.main) {
        (true, _) => format!("{}▏", state.input_buffer),
        (false, MainView::Search(r)) if !r.query.is_empty() => r.query.clone(),
        // Every other page, radio included, gets the same prompt. The box used
        // to retarget — Spotify here, the station directory there — which meant
        // the same keystroke did two different things depending on where you
        // were standing, and you had to read the row to find out which. It
        // queries both now, so it can say one thing everywhere.
        (false, _) => PLACEHOLDER.to_string(),
    };

    // The prompt keeps [`theme::WARN`] whatever the mode, so the glyph reads
    // as a control rather than as punctuation; only the text beside it goes
    // dim when there is nothing of yours in the box. A leading space because
    // the fill has an edge now, and a glyph flush against it reads as clipped.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" /  ", theme::warn()),
            Span::styled(text, if typing { theme::warn() } else { theme::dim() }),
        ]))
        .style(theme::field(hover)),
        Rect {
            width: field.width.saturating_sub(hint_w),
            ..field
        },
    );
}
#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    use super::*;

    /// The whole header — the mark and the prompt, a blank, the path, a blank.
    const H: u16 = super::super::HEAD_H;
    /// The row the prompt lands on: the first one, beside the mark.
    const PROMPT_ROW: usize = 0;
    /// And the row the path drops to, under it.
    const PATH_ROW: usize = super::super::SEARCH_H as usize;
    /// Where the field beside the mark starts.
    const FIELD_X: u16 = super::super::table::BRAND_W + MARK_GAP;

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

    /// The mark leads the top row and the prompt follows it — the control you
    /// reach for most, on the row that is always there.
    #[test]
    fn the_mark_leads_the_prompt_on_the_top_row() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        let lines = render(&mut st, 80);
        assert!(lines[PROMPT_ROW].starts_with("♫ spot"), "{:?}", lines[0]);
        assert!(lines[PROMPT_ROW].contains("/  search"), "{:?}", lines[0]);
        assert_eq!(st.hit.home_btn.x, 0);
        // And the path is a row down, at the margin, with the width to itself.
        assert!(lines[PATH_ROW].starts_with("PLAYLISTS"), "{:?}", lines[2]);
        assert!(
            !lines[PROMPT_ROW].contains("PLAYLISTS"),
            "the path is not on the top row"
        );
    }

    /// Home draws no crumb: the mark is already the way there, and a `HOME`
    /// under it would be the same control said twice.
    #[test]
    fn home_draws_no_path_at_all() {
        let mut st = AppState::new();
        let lines = render(&mut st, 80);
        assert_eq!(
            lines[PROMPT_ROW].trim_end(),
            "♫ spot    /  search Spotify and radio…"
        );
        assert!(lines[PATH_ROW].trim().is_empty(), "{:?}", lines[PATH_ROW]);
        assert!(st.hit.crumbs.is_empty());
    }

    /// And it is gone from the *front* of a path too, not just from its own
    /// page — the mark leads every screen, so a `HOME ›` under it says nothing.
    #[test]
    fn home_is_not_the_root_of_a_deeper_path() {
        let mut st = AppState::new();
        st.push_view();
        st.main = MainView::Playlists;
        let lines = render(&mut st, 80);
        assert!(!lines[PATH_ROW].contains("HOME"), "{:?}", lines[PATH_ROW]);
        assert!(!lines[PATH_ROW].contains('›'), "{:?}", lines[PATH_ROW]);
        assert!(
            lines[PATH_ROW].contains("PLAYLISTS"),
            "{:?}",
            lines[PATH_ROW]
        );
        // The page behind it is still on the stack — this drops a step from
        // what is drawn, not from where Backspace goes.
        assert_eq!(st.view_stack.len(), 1);
    }

    /// A page's total is pinned opposite the path, on the row the path is on.
    #[test]
    fn a_count_sits_opposite_the_path() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        let lines = with_count(&mut st, 80, "8 playlists");
        assert!(
            lines[PATH_ROW].trim_end().ends_with("8 playlists"),
            "{:?}",
            lines[PATH_ROW]
        );
        assert!(
            lines[PATH_ROW].starts_with("PLAYLISTS"),
            "{:?}",
            lines[PATH_ROW]
        );
    }

    /// Render with a page count on the header.
    fn with_count(state: &mut AppState, width: u16, count: &str) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, H)).unwrap();
        let page = PageHeader {
            loading: false,
            count: Some(Span::styled(count.to_string(), theme::dim())),
        };
        terminal.draw(|f| draw(f, f.area(), state, page)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..H)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
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
        assert!(!st.hit.search_box.is_empty(), "the field must be clickable");
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

    /// The whole point of keeping the prompt on screen in both modes: `/` and
    /// `Esc` must not move a single line of what is around it.
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
        // The blanks under the prompt and the path stay blank, so the content
        // under the header does not shift.
        for y in [1, PATH_ROW + 1] {
            assert!(idle_lines[y].trim().is_empty(), "{:?}", idle_lines[y]);
            assert!(typing_lines[y].trim().is_empty(), "{:?}", typing_lines[y]);
        }
    }

    /// A submitted query stays on screen while its results are, so the field
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

    /// The field reads as a box, so all of it must be clickable — and it now
    /// *looks* the size it is, because the fill spans the same rect. A click
    /// past the caret used to miss `hit.search_box`, fall into the "a click
    /// elsewhere cancels the input" branch, and discard what you typed.
    #[test]
    fn the_whole_field_is_clickable_not_just_the_text() {
        let mut st = AppState::new();
        st.input_mode = InputMode::Search;
        st.input_buffer = "mu".into();
        render(&mut st, 80);
        let box_rect = st.hit.search_box;
        assert_eq!(box_rect.x, FIELD_X, "the field starts past the mark");
        assert!(
            box_rect.width > 40,
            "hit rect is only {} wide",
            box_rect.width
        );
        assert!(box_rect.contains(Position {
            x: 40,
            y: PROMPT_ROW as u16
        }));
        // And it does not reach back to the mark: a click there means Home,
        // not "start typing".
        assert!(!box_rect.contains(Position { x: 2, y: 0 }));
    }

    /// The fill is what says "box you can type in" with no border to say it,
    /// and it covers the rect the click does — including the cells past the
    /// end of the text, which is the half that used to look inert.
    #[test]
    fn the_field_is_filled_at_rest_and_lights_under_the_pointer() {
        let mut st = AppState::new();
        render(&mut st, 80);
        let field = st.hit.search_box;
        assert_eq!(bg(&mut st, 80, field.x), theme::FIELD);
        assert_eq!(
            bg(&mut st, 80, field.right() - 1),
            theme::FIELD,
            "the fill stops short of the field's own edge"
        );
        // The gap between the mark and the field is not painted: the fill is
        // the control, not the row.
        assert_eq!(bg(&mut st, 80, field.x - 1), ratatui::style::Color::Reset);

        st.mouse_pos = Some(Position {
            x: field.x + 20,
            y: 0,
        });
        assert_eq!(bg(&mut st, 80, field.x), theme::FIELD_HOVER);
    }

    /// The field is a box, not a band: given a very wide terminal it stops
    /// growing rather than painting a filled bar the width of the header.
    #[test]
    fn the_field_stops_growing_on_a_wide_terminal() {
        let mut st = AppState::new();
        render(&mut st, 200);
        assert_eq!(st.hit.search_box.width, MAX_QUERY_W);
        // And the cells past it are the terminal's own ground again.
        let past = st.hit.search_box.right();
        assert_eq!(bg(&mut st, 200, past), ratatui::style::Color::Reset);
        // A row with less than that to give hands over what it has.
        let mut st = AppState::new();
        render(&mut st, 60);
        assert_eq!(st.hit.search_box.width, 60 - FIELD_X);
    }

    /// The `/` keeps [`theme::WARN`] whether or not you are typing, so it
    /// reads as a control; the placeholder beside it stays dim, so an empty
    /// box does not read as one with something in it.
    #[test]
    fn the_slash_stays_lit_over_a_dim_placeholder() {
        let mut st = AppState::new();
        render(&mut st, 80);
        let field = st.hit.search_box;
        assert_eq!(fg(&mut st, 80, field.x + 1), theme::WARN, "the /");
        assert_eq!(fg(&mut st, 80, field.x + 4), theme::DIM, "the placeholder");

        st.input_mode = InputMode::Search;
        st.input_buffer = "muse".into();
        render(&mut st, 80);
        assert_eq!(fg(&mut st, 80, field.x + 1), theme::WARN, "the /");
        assert_eq!(fg(&mut st, 80, field.x + 4), theme::WARN, "the query");
    }

    /// A band with room for one row draws the path, not the prompt. The mark
    /// and the path are the ways *out*; a search box is a way somewhere new,
    /// and it is what gives — which is also what the player asks for.
    #[test]
    fn a_short_band_keeps_the_path_and_sheds_the_prompt() {
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

    /// A playing Spotify transport, with PCM already in the tap.
    fn streaming() -> AppState {
        let mut st = AppState::new();
        st.playback = Some(crate::app::state::Playback::started(70, false));
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
            matched: Default::default(),
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

    /// The style of the cell at `x` on the top row.
    fn style_at(state: &mut AppState, width: u16, x: u16) -> ratatui::style::Style {
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
    }

    fn fg(state: &mut AppState, width: u16, x: u16) -> ratatui::style::Color {
        style_at(state, width, x).fg.unwrap()
    }

    fn bg(state: &mut AppState, width: u16, x: u16) -> ratatui::style::Color {
        style_at(state, width, x).bg.unwrap()
    }

    /// What is making sound, opposite the mark, on the row the mark is on —
    /// which is the row the player draws too, so neither appears to move when
    /// `v` toggles between them. The two sources are named rather than both
    /// called "playing": a station and a track behave differently enough that
    /// which one is on is worth a word.
    #[test]
    fn the_status_names_the_source_it_is_playing_from() {
        let mut st = streaming();
        let lines = render(&mut st, 80);
        assert!(
            lines[PROMPT_ROW].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[PROMPT_ROW]
        );
        assert!(!st.hit.status.is_empty(), "the status must be clickable");
        assert_eq!(st.hit.status.y, 0);
        assert_eq!(st.hit.status.right(), 80);

        let mut st = radio_state();
        let lines = render(&mut st, 80);
        assert!(
            lines[PROMPT_ROW].trim_end().ends_with("● RADIO"),
            "{:?}",
            lines[PROMPT_ROW]
        );
    }

    /// Nothing playing draws nothing: an idle word would be a control leading
    /// to a player with nothing in it.
    #[test]
    fn nothing_playing_draws_no_status() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        let lines = render(&mut st, 80);
        assert!(!lines[PROMPT_ROW].contains('●'), "{:?}", lines[PROMPT_ROW]);
        assert_eq!(
            lines[PATH_ROW].trim_end(),
            "PLAYLISTS",
            "{:?}",
            lines[PATH_ROW]
        );
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
            lines[PROMPT_ROW].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[PROMPT_ROW]
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
        assert!(
            lines[PROMPT_ROW].trim_end().ends_with("● LOADING"),
            "{:?}",
            lines[PROMPT_ROW]
        );
        let dot = st.hit.status.x;
        assert_eq!(fg(&mut st, 80, dot), theme::WARN);
    }

    /// A track that has been asked for but has not made sound yet: the client
    /// clears the tap on the load, so the gap reads as `LOADING` rather than
    /// as a dim `STREAMING`.
    #[test]
    fn a_track_asked_for_but_not_started_reads_as_loading() {
        let mut st = streaming();
        st.audio_tap.clear();
        let lines = render(&mut st, 80);
        assert!(
            lines[PROMPT_ROW].trim_end().ends_with("● LOADING"),
            "{:?}",
            lines[PROMPT_ROW]
        );
        let dot = st.hit.status.x;
        assert_eq!(fg(&mut st, 80, dot), theme::WARN);

        // And the moment audio arrives it is streaming, whatever the poll has
        // or has not confirmed by then.
        st.audio_tap.push(&[0.5, 0.5], 1.0);
        let lines = render(&mut st, 80);
        assert!(
            lines[PROMPT_ROW].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[PROMPT_ROW]
        );
    }

    /// The count is about the page and the status is about the sound, so they
    /// are on different rows now and never have to negotiate an edge between
    /// them — which is half of what moving the path down bought.
    #[test]
    fn the_count_and_the_status_no_longer_share_a_row() {
        let mut st = streaming();
        st.main = MainView::Playlists;
        let lines = with_count(&mut st, 80, "8 playlists");
        assert!(
            lines[PROMPT_ROW].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[PROMPT_ROW]
        );
        assert!(!lines[PROMPT_ROW].contains("8 playlists"));
        assert!(
            lines[PATH_ROW].trim_end().ends_with("8 playlists"),
            "{:?}",
            lines[PATH_ROW]
        );
        assert!(!lines[PATH_ROW].contains("STREAMING"));
        // And the path gets the whole row's width, not what is left after a
        // mark, a count and a status.
        assert!(
            lines[PATH_ROW].starts_with("PLAYLISTS"),
            "{:?}",
            lines[PATH_ROW]
        );
    }

    /// On a top row too tight for both, the status is what goes: a box you
    /// cannot read your own query back out of is worse than no status, and
    /// the deck at the bottom is still saying the same thing.
    #[test]
    fn a_tight_row_sheds_the_status_before_the_query() {
        let mut st = streaming();
        st.push_view();
        st.main = MainView::Tracks(crate::app::state::TrackList::new("Black Holes", "", None));
        let lines = render(&mut st, 40);
        assert!(!lines[PROMPT_ROW].contains("STREAMING"), "{:?}", lines[0]);
        assert!(st.hit.status.is_empty());
        assert!(
            st.hit.search_box.width >= MIN_QUERY_W,
            "the field kept only {} cells",
            st.hit.search_box.width
        );
        // The path is on its own row and never had to pay for either of them.
        assert!(
            lines[PATH_ROW].contains("BLACK HOLES"),
            "{:?}",
            lines[PATH_ROW]
        );

        // Given the room, the status comes back.
        let lines = render(&mut st, 80);
        assert!(
            lines[PROMPT_ROW].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[PROMPT_ROW]
        );
        assert!(
            lines[PATH_ROW].contains("BLACK HOLES"),
            "{:?}",
            lines[PATH_ROW]
        );
    }
}
