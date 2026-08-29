//! The add-to-playlist box: a search field over the playlists you can write
//! to, opened by the player's `+ add`.
//!
//! An overlay rather than a page, because the pick is about the record on the
//! deck behind it, and walking away from that record to choose would lose the
//! thing being chosen for.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::table::{apply_selection, centered, draw_scrollbar, fit, segment, spinner};
use super::theme;
use crate::app::state::{AppState, PICKER_ROWS, PlaylistPicker};

const BOX_W: u16 = 44;
const PLACEHOLDER: &str = "search playlists";
const NEW_PILL: &str = "+ new playlist";

/// The mark at the head of a row, in the four states a row can be in. All one
/// cell wide, so the names beside them line up and nothing moves under the
/// cursor when a row flips.
///
/// `·` is not "no" — it is "not walked yet", which is a third thing and has to
/// look like one. See [`AppState::playlist_tracks`].
const ON: &str = "✓";
const OFF: &str = " ";
const UNKNOWN: &str = "·";
const WORKING: &str = "…";

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let Some(picker) = state.picker.clone() else {
        return;
    };
    let matches = state.picker_rows();
    let mouse = state.mouse_pos;
    let status = status_line(state, &picker);

    // The box is as tall as it has rows to show, so a handful of playlists get
    // a handful of lines rather than a box of empty ones.
    let rows = (matches.len().max(1) as u16).min(PICKER_ROWS as u16);
    let height = rows + 1 + 1 + u16::from(status.is_some()) + 2;
    let area = centered(frame.area(), BOX_W, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(" Add to playlist ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::accent()),
        area,
    );

    if area.width < 3 || area.height < 3 {
        return;
    }
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };

    let field = Rect { height: 1, ..inner };
    draw_field(frame, field, &picker, mouse);
    state.hit.picker_field = field;

    let list = Rect {
        y: field.y + 1,
        height: inner.height.saturating_sub(2 + u16::from(status.is_some())),
        ..inner
    };
    draw_rows(frame, list, state, &picker, &matches);

    let controls = Rect {
        y: list.y + list.height,
        height: 1,
        ..inner
    };
    draw_controls(frame, controls, state, mouse);

    if let Some(line) = status {
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                y: controls.y + 1,
                height: 1,
                ..inner
            },
        );
    }
}

/// The way out of the box when nothing in it is what you wanted: a control
/// that makes the playlist the query was looking for.
fn draw_controls(frame: &mut Frame, controls: Rect, state: &mut AppState, mouse: Option<Position>) {
    // Held one cell in, so the control starts under the rows above it.
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut x = controls.x + 1;
    state.hit.picker_new = segment(
        &mut spans,
        &mut x,
        controls,
        mouse,
        vec![Span::styled(NEW_PILL, theme::accent())],
    );
    spans.push(Span::styled("   ctrl+n", theme::dim()));
    frame.render_widget(Paragraph::new(Line::from(spans)), controls);
}

/// The field, in the browse screen's search-row idiom: the fill is what makes
/// it read as a box you can type in, and it covers the whole row so a click
/// that misses the caret still lands on the field rather than off the box.
fn draw_field(frame: &mut Frame, field: Rect, picker: &PlaylistPicker, mouse: Option<Position>) {
    let hover = mouse.is_some_and(|m| field.contains(m));
    frame.render_widget(Paragraph::new("").style(theme::field(hover)), field);
    let (text, style) = if picker.query.is_empty() {
        (PLACEHOLDER.to_string(), theme::dim())
    } else {
        (format!("{}▏", picker.query), theme::warn())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" /  ", theme::warn()),
            Span::styled(text, style),
        ]))
        .style(theme::field(hover)),
        field,
    );
}

/// A row's text, held one cell off the border so it starts under the field's
/// query rather than against the frame.
fn indented(text: &str, list: Rect) -> String {
    format!(" {}", fit(text, list.width.saturating_sub(1) as usize))
}

/// The mark for one row, and the style that says what it means.
fn row_mark(state: &AppState, picker: &PlaylistPicker, playlist_id: &str) -> (&'static str, Style) {
    if picker.pending.contains(playlist_id) {
        return (WORKING, theme::dim());
    }
    match state.picker_has(playlist_id) {
        Some(true) => (ON, theme::accent()),
        Some(false) => (OFF, theme::dim()),
        None => (UNKNOWN, theme::dim()),
    }
}

fn draw_rows(
    frame: &mut Frame,
    list: Rect,
    state: &mut AppState,
    picker: &PlaylistPicker,
    matches: &[usize],
) {
    if list.height == 0 || list.width == 0 {
        return;
    }
    if matches.is_empty() {
        // Two different nothings: one says the query is too narrow, the other
        // says there is nothing for it to be narrow about.
        let said = if picker.query.trim().is_empty() {
            "no playlists of your own to add to"
        } else {
            "no match"
        };
        frame.render_widget(
            Paragraph::new(Line::styled(indented(said, list), theme::dim())),
            Rect { height: 1, ..list },
        );
        return;
    }

    // Two cells for the mark and its gap, so the names line up whatever each
    // row's mark is.
    let name_w = list.width.saturating_sub(3) as usize;
    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(picker.offset)
        .take(list.height as usize)
        .map(|(row, index)| {
            let playlist = &state.playlists[*index];
            let (mark, mark_style) = row_mark(state, picker, &playlist.id);
            let mut line = Line::from(vec![
                Span::styled(format!(" {mark} "), mark_style),
                Span::styled(fit(&playlist.name, name_w), theme::text()),
            ]);
            if row == picker.selected {
                apply_selection(&mut line);
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
    state.hit.picker_list = list;

    draw_scrollbar(
        frame,
        Rect {
            x: list.right(),
            width: 1,
            ..list
        },
        matches.len(),
        picker.offset,
    );
}

/// What the box says under its rows, when it has anything to say.
///
/// A refusal first: it is the one thing here you have to act on, and it is
/// gone the moment the next pick goes out. Otherwise the line explains the
/// marks that cannot answer yet — without it, a row's `·` is a glyph with
/// nothing to say what it is waiting for. The player view draws no toasts, so
/// the box is the only surface either can be reported on.
fn status_line(state: &AppState, picker: &PlaylistPicker) -> Option<Line<'static>> {
    if let Some(e) = &picker.error {
        return Some(Line::styled(format!(" {e}"), theme::warn()));
    }
    let unanswered = state
        .picker_visible()
        .into_iter()
        .any(|i| state.picker_has(&state.playlists[i].id).is_none());
    unanswered.then(|| {
        Line::styled(
            format!(" {} checking…", spinner()),
            theme::dim().add_modifier(Modifier::ITALIC),
        )
    })
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::state::Playlist;

    fn playlist(id: &str, name: &str, owner_id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: name.into(),
            track_count: 1,
            owner: owner_id.into(),
            owner_id: owner_id.into(),
            snapshot_id: "s".into(),
            cover_url: None,
            public: None,
            collaborative: false,
        }
    }

    fn open(query: &str) -> AppState {
        let mut st = AppState::new();
        st.me_id = Some("me".into());
        st.set_playlists(vec![
            playlist("a", "Late Night", "me"),
            playlist("b", "Someone Else's", "them"),
            playlist("c", "Lunch", "me"),
        ]);
        let uri = "spotify:track:x".to_string();
        st.picker = Some(PlaylistPicker {
            order: st.picker_order(&uri),
            uri,
            query: query.into(),
            selected: 0,
            offset: 0,
            pending: Default::default(),
            error: None,
            seq: 1,
        });
        st
    }

    /// Record what `holding` holds, and that the rest were walked and hold
    /// nothing.
    fn walked(st: &mut AppState, holding: &[&str]) {
        let ids: Vec<String> = st.playlists.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            let on = holding.contains(&id.as_str());
            st.playlist_tracks.insert(
                id,
                crate::app::state::PlaylistContents {
                    snapshot_id: "s".into(),
                    track_ids: on.then(|| "x".to_string()).into_iter().collect(),
                },
            );
        }
    }

    fn screen(state: &mut AppState) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..24)
            .map(|y| {
                (0..80)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    fn body(lines: &[String]) -> String {
        lines.join("\n")
    }

    /// An open box whose visible rows have all been answered for, which is
    /// what it settles into a moment after it opens.
    fn answered() -> AppState {
        let mut st = open("");
        walked(&mut st, &[]);
        st
    }

    /// An empty box says what to type into it, and lists the playlists that
    /// can be written to and no others.
    #[test]
    fn an_empty_box_prompts_and_lists_what_you_own() {
        let mut st = open("");
        let lines = screen(&mut st);
        let text = body(&lines);
        assert!(text.contains("Add to playlist"), "{text}");
        assert!(text.contains(PLACEHOLDER), "{text}");
        assert!(
            text.contains("Late Night") && text.contains("Lunch"),
            "{text}"
        );
        assert!(
            !text.contains("Someone Else"),
            "a followed playlist is listed"
        );
        assert!(!st.hit.picker_field.is_empty());
        assert!(!st.hit.picker_list.is_empty());
    }

    /// Typing cuts the rows and puts what was typed in the field, caret and
    /// all, in place of the prompt.
    #[test]
    fn a_query_cuts_the_rows() {
        let mut st = open("lun");
        let text = body(&screen(&mut st));
        assert!(text.contains("lun▏"), "{text}");
        assert!(!text.contains(PLACEHOLDER), "{text}");
        assert!(
            text.contains("Lunch") && !text.contains("Late Night"),
            "{text}"
        );
    }

    /// A query that matches nothing still leaves a box you can type in, and
    /// says which nothing it is.
    #[test]
    fn a_query_that_matches_nothing_says_so() {
        let mut st = open("zzz");
        let text = body(&screen(&mut st));
        assert!(text.contains("no match"), "{text}");
        assert!(!st.hit.picker_field.is_empty());
        assert!(st.hit.picker_list.is_empty(), "an empty list took clicks");
    }

    /// Nothing of your own to add to reads differently from a query that cut
    /// everything away.
    #[test]
    fn no_playlists_of_your_own_reads_as_itself() {
        let mut st = AppState::new();
        st.me_id = Some("me".into());
        st.set_playlists(vec![playlist("b", "Someone Else's", "them")]);
        let uri = "spotify:track:x".to_string();
        st.picker = Some(PlaylistPicker {
            order: st.picker_order(&uri),
            uri,
            query: String::new(),
            selected: 0,
            offset: 0,
            pending: Default::default(),
            error: None,
            seq: 1,
        });
        let text = body(&screen(&mut st));
        assert!(text.contains("no playlists of your own"), "{text}");
    }

    /// The selection is the mark every other list in the app wears, and it
    /// moves to whichever row the box is pointing at.
    #[test]
    fn the_selected_row_is_marked() {
        let mut st = open("");
        st.picker.as_mut().unwrap().selected = 1;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let list = st.hit.picker_list;
        let bold = |y: u16| {
            buffer
                .cell(Position { x: list.x, y })
                .is_some_and(|c| c.style().add_modifier.contains(Modifier::BOLD))
        };
        assert!(!bold(list.y), "the first row is still marked");
        assert!(bold(list.y + 1), "the second row is not marked");
    }

    /// A row the record is on wears the check, and one it is not wears
    /// nothing — not the mark that means "not asked yet".
    #[test]
    fn a_checked_row_says_the_record_is_on_that_playlist() {
        let mut st = answered();
        walked(&mut st, &["a"]);
        let lines = screen(&mut st);
        let row = |name: &str| lines.iter().find(|l| l.contains(name)).unwrap().clone();
        assert!(row("Late Night").contains(&format!("{ON} Late Night")));
        assert!(row("Lunch").contains(&format!("{OFF} Lunch")));
        assert!(!row("Lunch").contains(UNKNOWN));
    }

    /// A mark that has not been answered for is neither on nor off, and the
    /// line under the rows says what it is waiting for — a bare `·` with
    /// nothing to explain it is a glyph, not a state.
    #[test]
    fn an_unanswered_row_says_it_is_still_checking() {
        let mut st = open("");
        let lines = screen(&mut st);
        let text = body(&lines);
        assert!(text.contains("checking…"), "{text}");
        let row = lines.iter().find(|l| l.contains("Late Night")).unwrap();
        assert!(row.contains(&format!("{UNKNOWN} Late Night")), "{row}");
    }

    /// A row mid-change says so, and stops saying it once the answer lands.
    #[test]
    fn a_row_in_flight_is_marked_as_working() {
        let mut st = answered();
        st.picker.as_mut().unwrap().pending.insert("a".into());
        let lines = screen(&mut st);
        let row = lines.iter().find(|l| l.contains("Late Night")).unwrap();
        assert!(row.contains(&format!("{WORKING} Late Night")), "{row}");
    }

    /// Every mark answered, so nothing is being waited on and the box says
    /// nothing under its rows.
    #[test]
    fn an_answered_box_says_nothing_under_its_rows() {
        let mut st = answered();
        let text = body(&screen(&mut st));
        assert!(!text.contains("checking"), "{text}");
    }

    /// The way out of the box is drawn and can be clicked.
    #[test]
    fn the_box_offers_a_new_playlist() {
        let mut st = answered();
        let text = body(&screen(&mut st));
        assert!(text.contains(NEW_PILL), "{text}");
        assert!(!st.hit.picker_new.is_empty());
    }

    /// And most of all on the box that has nothing to offer, which is where
    /// the control is the only thing left to press.
    #[test]
    fn a_box_that_matches_nothing_still_offers_a_new_playlist() {
        let mut st = open("zzz");
        let text = body(&screen(&mut st));
        assert!(text.contains("no match"), "{text}");
        assert!(text.contains(NEW_PILL), "{text}");
        assert!(!st.hit.picker_new.is_empty());
    }

    /// Prints the box with row numbers, for eyeballing the layout.
    /// `cargo test dump_picker -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_picker() {
        let mut st = answered();
        for (i, l) in screen(&mut st).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    /// And the one that came back refused, which is the only place an error
    /// from the add can be read.
    #[test]
    fn a_refusal_stays_in_the_box() {
        let mut st = open("");
        st.picker.as_mut().unwrap().error = Some("403 Forbidden".into());
        let text = body(&screen(&mut st));
        assert!(text.contains("403 Forbidden"), "{text}");
    }
}
