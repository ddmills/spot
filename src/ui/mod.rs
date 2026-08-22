mod deck;
mod help;
mod main_pane;
mod now_playing;
mod player;
mod table;
mod theme;
mod top_row;

/// Installed by the client task when a cover decodes, so the accent and the
/// visualizer's ramp follow the record on screen. See
/// [`theme::set_cover_colors`].
pub use theme::set_cover_colors;

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::ListState;

use crate::app::state::{AppState, HitAreas};

const TOAST_TTL: Duration = Duration::from_secs(4);

/// Rows the top row takes: the mark and the search prompt, then a blank.
const TOP_H: u16 = 2;
/// Rows the bottom bar takes: a rule, the deck's seven rows beside the
/// sleeve, then a blank. See [`now_playing`].
const BAR_H: u16 = 1 + deck::DECK_H + 1;

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    // Expire stale toasts.
    if let Some((_, at)) = &state.toast
        && at.elapsed() > TOAST_TTL
    {
        state.toast = None;
    }

    // Rebuild mouse hit regions from scratch each frame.
    state.hit = HitAreas::default();

    // The player view draws its own play state, progress, volume and
    // transport, so the bottom bar would only repeat itself: it gets the whole
    // screen and the bar is left out.
    if state.show_player {
        // `player::draw` insets itself by a cell on each side; a margin here
        // would silently double it.
        player::draw(frame, frame.area(), state);
    } else {
        // The one-cell side margin is what `player::draw` insets itself by, so
        // the two views' content sits in the same columns and toggling `v`
        // does not shift anything sideways.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(1)
            .constraints([
                // The mark and the prompt, then a blank row. Fixed, so
                // entering and leaving search mode moves nothing below it.
                Constraint::Length(TOP_H),
                Constraint::Min(1),
                Constraint::Length(BAR_H),
            ])
            .split(frame.area());

        // One column. The left nav used to take 30 cells of it — its playlists
        // are a page of their own now, reached from Home, and the pane it was
        // crowding gets the width back. See `main_pane::draw_playlists`.
        top_row::draw(frame, rows[0], state);
        main_pane::draw(frame, rows[1], state);
        now_playing::draw(frame, rows[2], state);
    }

    if state.show_help {
        help::draw(frame);
    }
}

/// Keep a manually managed scroll offset in bounds when content shrinks.
/// Selection is painted by the row builders, never via `ListState.selected`
/// (ratatui would snap the view to the selected row, fighting wheel scroll).
fn clamp_offset(list_state: &mut ListState, len: usize, height: usize) {
    let max_offset = len.saturating_sub(height);
    if list_state.offset() > max_offset {
        *list_state.offset_mut() = max_offset;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    use super::*;
    use crate::app::state::{PlaybackSnapshot, Playlist, RepeatMode, TrackList};

    fn browse_state() -> AppState {
        let mut st = AppState::new();
        st.playlists = (0..8)
            .map(|i| Playlist {
                id: format!("p{i}"),
                uri: format!("spotify:playlist:p{i}"),
                name: format!("Playlist {i}"),
                track_count: 10 + i,
                owner: "me".into(),
                owner_id: "me".into(),
                snapshot_id: "s".into(),
            })
            .collect();
        let mut list = TrackList::new("Hey Arnold!", "by me", None, None);
        list.tracks = (0..12)
            .map(|i| crate::app::state::Track {
                uri: format!("spotify:track:t{i}"),
                name: format!("Track Number {i}"),
                artists: format!("Artist {i}"),
                album: "Album Name".into(),
                release_year: "2020".into(),
                duration_ms: 83_000,
                track_number: i as u32 + 1,
                album_id: Some("alb".into()),
                artist_id: Some("art".into()),
                cover_url: None,
            })
            .collect();
        list.display = (0..list.tracks.len()).collect();
        // The same list is what is playing, so the bar's context row names it.
        st.queue = Some(list.clone());
        st.main = crate::app::state::MainView::Tracks(list);
        st.playback = Some(PlaybackSnapshot {
            is_playing: true,
            progress_ms: 49_000,
            duration_ms: 67_500,
            track_uri: Some("spotify:track:t3".into()),
            context_uri: Some("spotify:playlist:p2".into()),
            artist_id: Some("art".into()),
            album_id: Some("alb".into()),
            track_name: "Envejecer".into(),
            artists: "Erameld, Hipnos".into(),
            album: "Días Despejados".into(),
            release_year: "2020".into(),
            cover_url: None,
            shuffle: false,
            repeat: RepeatMode::Context,
            volume_percent: 70,
            device_name: "spot".into(),
            fetched_at: Instant::now(),
        });
        st
    }

    fn screen(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    /// The whole point of the redesign: the browse screen wears no frames.
    ///
    /// Two marks are allowed to survive. The `─` rule above the bottom bar is
    /// what stops the bar floating into the list above it, and the scrollbar
    /// track is a control rather than chrome.
    #[test]
    fn the_browse_screen_draws_no_pane_frames() {
        let mut st = browse_state();
        let lines = screen(&mut st, 100, 34);
        for (y, line) in lines.iter().enumerate() {
            for c in "╭╮╰╯┌┐└┘".chars() {
                assert!(!line.contains(c), "corner {c:?} on row {y}: {line:?}");
            }
        }
        // Exactly one horizontal rule, and it is the bar's. Matched as a row
        // of nothing but `─` and margin — the progress and volume tracks are
        // drawn with the same glyph, but always alongside other marks.
        let rules: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains('─') && l.chars().all(|c| c == '─' || c == ' '))
            .map(|(y, _)| y)
            .collect();
        assert_eq!(rules.len(), 1, "expected one rule, got rows {rules:?}");
        assert_eq!(rules[0], 34 - BAR_H as usize);
    }

    /// The layout bands are fixed, so entering and leaving search mode must
    /// not move a single row of what is below the prompt.
    #[test]
    fn entering_search_mode_shifts_nothing() {
        let mut normal = browse_state();
        let before = screen(&mut normal, 100, 34);
        let mut searching = browse_state();
        searching.input_mode = crate::app::state::InputMode::Search;
        let after = screen(&mut searching, 100, 34);
        // Row 0 is the prompt itself and is expected to differ; nothing else
        // may move.
        for y in 1..34 {
            assert_eq!(before[y], after[y], "row {y} moved when search opened");
        }
    }

    /// The pane starts at the screen's own margin now that the rail is gone,
    /// and its label is simply the page's name — there is no second pane for
    /// focus to be anywhere else.
    #[test]
    fn the_pane_spans_the_screen_under_its_label() {
        use crate::ui::theme;

        let mut st = browse_state();
        let mut terminal = Terminal::new(TestBackend::new(100, 34)).unwrap();
        terminal.draw(|f| draw(f, &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        // Row 2 is the label row, one cell in from the screen margin.
        assert_eq!(
            buffer.cell(Position { x: 1, y: 2 }).unwrap().fg,
            theme::accent_color()
        );
        // The track table reaches the far side: with the rail gone, the header
        // row.s Time column ends in the scrollbar gutter rather than 33 cells
        // short of it. Row 7 is that heading: label, blank, the name row,
        // ▶ play, blank, then the columns.
        let row: String = (0..100)
            .filter_map(|x| buffer.cell(Position { x, y: 7 }).map(|c| c.symbol()))
            .collect();
        assert!(row.contains("Title"), "not the column header: {row:?}");
        assert!(
            row.trim_end().len() > 90,
            "the table stops short of the screen: {row:?}"
        );
    }

    /// The mark leads the top row in both views, in the same column, so
    /// toggling the player never appears to move it.
    #[test]
    fn the_mark_leads_the_top_row_in_both_views() {
        // Column 0 is the screen's own one-cell margin, which both views inset
        // themselves by.
        let mut st = browse_state();
        assert!(
            screen(&mut st, 100, 34)[0].starts_with(" ♫ spot"),
            "{:?}",
            screen(&mut st, 100, 34)[0]
        );
        assert_eq!(st.hit.home_btn.x, 1, "the mark sits at the screen margin");
        assert_eq!(st.hit.home_btn.width, table::BRAND_W);

        let browse_mark = st.hit.home_btn;
        let mut st = browse_state();
        st.show_player = true;
        assert!(screen(&mut st, 100, 34)[0].starts_with(" ♫ spot"));
        assert_eq!(
            st.hit.home_btn, browse_mark,
            "the mark moved when the player opened"
        );
    }

    /// Home is what the app opens onto, and the mark is what gets back to it.
    /// Two named records, then everything else behind one door.
    #[test]
    fn home_lists_its_destinations() {
        let mut st = browse_state();
        st.playlists.push(Playlist {
            id: "dw".into(),
            uri: "spotify:playlist:dw".into(),
            name: "Discover Weekly".into(),
            track_count: 30,
            owner: "Spotify".into(),
            owner_id: "spotify".into(),
            snapshot_id: "s".into(),
        });
        st.main = crate::app::state::MainView::Home;
        let lines = screen(&mut st, 100, 34);
        assert!(lines[2].contains("HOME"));
        assert!(lines[4].contains("Liked Songs"), "{:?}", lines[4]);
        // No count: its length is not known until it is opened.
        assert!(
            lines[4].trim_end().ends_with("Liked Songs"),
            "{:?}",
            lines[4]
        );
        assert!(
            lines[5].contains("everything you have saved"),
            "{:?}",
            lines[5]
        );

        assert!(lines[7].contains("Discover Weekly"), "{:?}", lines[7]);
        assert!(lines[7].contains("30 tracks"), "{:?}", lines[7]);

        assert!(lines[10].contains("Playlists"), "{:?}", lines[10]);
        assert!(lines[10].contains("9 playlists"), "{:?}", lines[10]);
        assert!(lines[11].contains("saved and followed"), "{:?}", lines[11]);
    }

    #[test]
    fn a_small_terminal_degrades_without_panicking() {
        for height in 0..12 {
            for width in [0u16, 1, 20, 60, 100] {
                let mut st = browse_state();
                screen(&mut st, width, height);
                let mut st = browse_state();
                st.show_player = true;
                screen(&mut st, width, height);
                let mut st = browse_state();
                st.show_help = true;
                screen(&mut st, width, height);
            }
        }
    }

    /// Arrive at whatever the state is showing from Home, by way of `name`,
    /// so the dump shows a trail with real steps in it rather than a snapshot
    /// of the page it is already on.
    fn arrive_via(st: &mut AppState, name: &str) {
        let page = std::mem::replace(&mut st.main, crate::app::state::MainView::Home);
        st.push_view();
        st.main = crate::app::state::MainView::Tracks(TrackList::new(name, "", None, None));
        st.push_view();
        st.main = page;
    }

    #[test]
    #[ignore]
    fn dump_album() {
        let mut st = browse_state();
        if let crate::app::state::MainView::Tracks(l) = &mut st.main {
            l.kind = crate::app::state::TrackListKind::Album;
            l.header.name = "Dance In The Street".into();
            l.header.subtitle = "Donna The Buffalo · 2018".into();
            l.header.cover_url = Some("https://i.scdn.co/image/abc".into());
        }
        arrive_via(&mut st, "Donna The Buffalo");
        for (i, l) in screen(&mut st, 100, 26).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    #[test]
    #[ignore]
    fn dump_artist() {
        let mut st = browse_state();
        let crate::app::state::MainView::Tracks(list) = st.main.clone() else {
            unreachable!()
        };
        let mut top = TrackList::new("Roy Hargrove", "top tracks", None, None);
        top.append(list.tracks);
        st.main = crate::app::state::MainView::Artist(crate::app::state::ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Roy Hargrove".into(),
            image_url: Some("https://i.scdn.co/image/artist".into()),
            genres: vec!["jazz".into(), "hard bop".into()],
            top,
            albums: (0..12)
                .map(|i| crate::app::state::AlbumItem {
                    id: format!("a{i}"),
                    name: format!("Record Number {i}"),
                    artists: "Roy Hargrove".into(),
                    release_year: (2010 - i).to_string(),
                    album_type: if i % 3 == 0 { "single" } else { "album" }.into(),
                    track_count: 4 + i as u32,
                    cover_url: Some(format!("https://i.scdn.co/image/a{i}")),
                })
                .collect(),
            loading: false,
        });
        arrive_via(&mut st, "Jazz");
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    #[test]
    #[ignore]
    fn dump_home() {
        let mut st = browse_state();
        st.main = crate::app::state::MainView::Home;
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    #[test]
    #[ignore]
    fn dump_playlists() {
        let mut st = browse_state();
        st.me_id = Some("me".into());
        st.main = crate::app::state::MainView::Playlists;
        st.main = crate::app::state::MainView::Home;
        st.push_view();
        st.main = crate::app::state::MainView::Playlists;
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    #[test]
    #[ignore]
    fn dump_player() {
        let mut st = browse_state();
        st.show_player = true;
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    /// Prints the whole browse screen with row numbers, for eyeballing the
    /// layout. `cargo test dump_browse -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_browse() {
        let mut st = browse_state();
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }
}
