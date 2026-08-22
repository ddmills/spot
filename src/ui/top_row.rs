//! The browse screen's top row: the `♫ spot` mark, then the search prompt.
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
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::table::{brand, right_row, segment};
use super::theme;
use crate::app::state::{AppState, InputMode, MainView};

/// Shown when the box is empty, in place of the query.
const PLACEHOLDER: &str = "search artists, albums, playlists…";
/// The same box on a radio page, where it queries the station directory.
const RADIO_PLACEHOLDER: &str = "search radio stations…";

/// Cells between the mark and the search prompt. Wider than a word space so
/// the two read as separate controls rather than as one phrase.
const MARK_GAP: u16 = 6;

pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let row = Rect { height: 1, ..area };
    let typing = state.input_mode == InputMode::Search;
    let mouse = state.mouse_pos;

    // The mark leads the row and the search field starts after it. Drawn as
    // its own paragraph rather than as the first span of the field's line, so
    // the field's own hit rect starts where its text does.
    let mark = brand(frame, row, mouse);
    state.hit.home_btn = mark;
    // A row too narrow for the mark draws none, and then owes it no gap: the
    // prompt gets the whole row back rather than an indent under nothing.
    let taken = if mark.is_empty() {
        0
    } else {
        mark.width + MARK_GAP
    };
    let row = Rect {
        x: row.x + taken,
        width: row.width.saturating_sub(taken),
        ..row
    };
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
    let style = if typing {
        Style::default().fg(theme::WARN)
    } else {
        theme::dim()
    };
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

    fn render(state: &mut AppState, width: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
        terminal.draw(|f| draw(f, f.area(), state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..2)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    /// Idle it is an affordance rather than a control: the prompt is there so
    /// someone who has not read the keymap can still find search.
    #[test]
    fn idle_shows_the_prompt_and_a_placeholder() {
        let mut st = AppState::new();
        let lines = render(&mut st, 80);
        assert!(lines[0].starts_with("♫ spot"), "{:?}", lines[0]);
        assert!(lines[0].contains("/  search"), "{:?}", lines[0]);
        assert!(
            !lines[0].contains('▏'),
            "an idle row should not show a caret"
        );
        assert!(!lines[0].contains("esc"));
        assert!(!st.hit.search_box.is_empty(), "the row must be clickable");
        assert_eq!(st.hit.search_box.y, 0);
    }

    #[test]
    fn typing_shows_the_query_a_caret_and_the_keys() {
        let mut st = AppState::new();
        st.input_mode = InputMode::Search;
        st.input_buffer = "donna the buffalo".into();
        let lines = render(&mut st, 80);
        assert!(lines[0].contains("donna the buffalo▏"));
        assert!(lines[0].contains("enter · esc"));
        assert!(!lines[0].contains(PLACEHOLDER));
    }

    /// The whole point of making the row permanent: `/` and `Esc` must not
    /// move a single line of what is below it.
    #[test]
    fn the_row_is_the_same_height_in_both_states() {
        let mut idle = AppState::new();
        let idle_lines = render(&mut idle, 80);
        let mut typing = AppState::new();
        typing.input_mode = InputMode::Search;
        typing.input_buffer = "x".into();
        let typing_lines = render(&mut typing, 80);
        assert_eq!(idle_lines.len(), typing_lines.len());
        assert_eq!(idle.hit.search_box.y, typing.hit.search_box.y);
        // Both leave the second row blank for the content below to breathe.
        assert!(idle_lines[1].trim().is_empty());
        assert!(typing_lines[1].trim().is_empty());
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
        assert!(lines[0].contains("muse"));
        assert!(!lines[0].contains(PLACEHOLDER));

        // Browsing away from the results puts the prompt back.
        let mut st = AppState::new();
        let lines = render(&mut st, 80);
        assert!(lines[0].contains(PLACEHOLDER));
    }

    /// The row reads as a full-width control, so all of it must be clickable.
    /// A click past the caret used to miss `hit.search_box`, fall into the "a
    /// click elsewhere cancels the input" branch, and discard what you typed.
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
        assert!(box_rect.contains(Position { x: 40, y: 0 }));
        // It starts after the mark and the gap, not at column 0 — a click on
        // the mark means Home, not "start typing".
        assert_eq!(box_rect.x, super::super::table::BRAND_W + MARK_GAP);
        assert!(!box_rect.contains(Position { x: 2, y: 0 }));
    }

    #[test]
    fn narrow_and_empty_rows_degrade_without_panicking() {
        for width in 0..20 {
            let mut st = AppState::new();
            st.input_mode = InputMode::Search;
            st.input_buffer = "a rather long query".into();
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
                )
            })
            .unwrap();
    }
}
