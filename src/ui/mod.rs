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

/// Rows the identity row takes: the `♫ spot` mark, the search prompt beside
/// it and the playback status opposite them, then a blank. Named for what the
/// player draws, which is the same row with the path in place of the prompt.
const NAV_H: u16 = 2;
/// Rows that row takes on the browse screen before the path under it — the
/// prompt, then a blank — and so the path's own offset into the band.
const SEARCH_H: u16 = 2;
/// The header both views wear, and the line their content starts on. Neither
/// view spells these rows out for itself: the whole point is that toggling the
/// player moves nothing. Drawn by [`top_row`] either way.
const HEAD_H: u16 = NAV_H + SEARCH_H;
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
                // The mark and the prompt, a blank, the path, a blank. Fixed,
                // so entering and leaving search mode moves nothing below it.
                Constraint::Length(HEAD_H),
                Constraint::Min(1),
                Constraint::Length(BAR_H),
            ])
            .split(frame.area());

        // One column. The left nav used to take 30 cells of it — its playlists
        // are a page of their own now, reached from Home, and the pane it was
        // crowding gets the width back. See `main_pane::draw_playlists`.
        //
        // The page's own contribution to the header — its count, and whether
        // it is still loading — read off before the header is drawn, because
        // the row it goes on is not the pane's to draw.
        let page = main_pane::page_header(state);
        top_row::draw(frame, rows[0], state, page);
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
            is_local_device: true,
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
        // The prompt row itself is expected to differ; nothing else may move —
        // including the path, which is now a row of its own under it.
        for y in (0..34).filter(|&y| y != 0) {
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
        // The path is a row of its own under the prompt, and it starts at the
        // screen's margin: the head of it is the accent run in column 1.
        assert_eq!(
            buffer.cell(Position { x: 1, y: SEARCH_H }).unwrap().fg,
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

    /// Both views lead with the same two fixtures: the mark at the margin and
    /// the playback status opposite it, in the same columns, so toggling the
    /// player does not appear to move either of them.
    ///
    /// What sits *between* them is the one thing the two rows disagree about.
    /// The browse screen puts the prompt there and the path a row under it;
    /// the player has no list to search into, so the path takes the row and
    /// the rows the prompt would have cost go to the queue instead. Both are
    /// drawn from `top_row::head_row`, which is what stops the fixtures
    /// drifting apart.
    #[test]
    fn the_nav_row_is_the_same_in_both_views() {
        // Two pages deep, so the path has a step on it as well as a head.
        // Home draws no crumb, so one step down from Home is a head alone.
        let nested = || {
            use crate::app::state::{ArtistView, MainView};
            let mut st = browse_state();
            st.main = MainView::Home;
            st.push_view();
            st.main = MainView::Artist(ArtistView {
                id: "r1".into(),
                uri: "spotify:artist:r1".into(),
                name: "Muse".into(),
                image_url: None,
                genres: vec![],
                top: TrackList::new("Muse", "", None, None),
                albums: vec![],
                loading: false,
            });
            st.push_view();
            st.main = MainView::Tracks(TrackList::new("Black Holes", "", None, None));
            st
        };
        let mut browse = nested();
        let mut player = nested();
        player.show_player = true;

        let a = screen(&mut browse, 100, 34);
        let b = screen(&mut player, 100, 34);
        // Column 0 is the screen's own one-cell margin, which both views inset
        // themselves by.
        assert_eq!(browse.hit.home_btn.x, 1, "the mark sits at the margin");
        assert_eq!(browse.hit.home_btn.width, table::BRAND_W);
        assert_eq!(
            browse.hit.home_btn, player.hit.home_btn,
            "the mark moved when the player opened"
        );
        // Drawn on both screens from the same code, so this is what catches
        // the two drifting apart.
        assert_eq!(
            browse.hit.status, player.hit.status,
            "the status moved when the player opened"
        );
        assert!(a[0].trim_end().ends_with("● LOADING"), "{:?}", a[0]);
        assert!(b[0].trim_end().ends_with("● LOADING"), "{:?}", b[0]);
        assert!(a[1].trim().is_empty(), "{:?}", a[1]);

        // Between them the rows part ways: the prompt on the browse screen,
        // the path on the player, and the rows the prompt would have cost
        // spent on what the player is actually showing.
        assert!(a[0].starts_with(" ♫ spot    /  search"), "{:?}", a[0]);
        assert!(
            b[0].starts_with(" ♫ spot   MUSE  ›  BLACK HOLES"),
            "{:?}",
            b[0]
        );
        assert!(!browse.hit.search_box.is_empty());
        assert!(player.hit.search_box.is_empty(), "the player has no prompt");

        // The browse screen's path is a row under its prompt, at the margin
        // rather than indented past a mark — which is what buys it the width.
        let path = SEARCH_H as usize;
        assert!(
            a[path].starts_with(" MUSE  ›  BLACK HOLES"),
            "{:?}",
            a[path]
        );

        // The ancestors are controls on both, and there is the same one of
        // them either way. Only the head disagrees: on the browse screen it is
        // the page you are already on and leads nowhere, while in the player
        // it closes the view.
        let targets =
            |st: &AppState| -> Vec<_> { st.hit.crumbs.iter().map(|(_, t)| t.clone()).collect() };
        assert_eq!(targets(&browse), targets(&player));
        assert_eq!(targets(&browse).len(), 1);
        assert_eq!(browse.hit.crumbs[0].0.y, SEARCH_H);
        assert_eq!(player.hit.crumbs[0].0.y, 0);
        assert!(browse.hit.close_player.is_empty());
        assert!(!player.hit.close_player.is_empty());
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
        // Home draws no crumb: the mark is already the way there. The
        // playback status is opposite it, as on every other page.
        assert!(lines[0].starts_with(" ♫ spot "), "{:?}", lines[0]);
        assert!(lines[0].trim_end().ends_with("● LOADING"), "{:?}", lines[0]);
        assert!(!lines[0].contains('›'), "{:?}", lines[0]);
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

        // Radio appends, so every row above it keeps the line it was on.
        assert!(lines[13].contains("Radio"), "{:?}", lines[13]);
        assert!(
            lines[13].trim_end().ends_with("Radio"),
            "no saved stations yet, so no count: {:?}",
            lines[13]
        );
        assert!(
            lines[14].contains("live stations from around the world"),
            "{:?}",
            lines[14]
        );
    }

    /// The Radio row counts what you kept, because the directory's 57,000
    /// stations are not a number that row could honestly claim.
    #[test]
    fn the_radio_row_counts_saved_stations() {
        let mut st = browse_state();
        st.main = crate::app::state::MainView::Home;
        st.radio_favorites = vec![station("a", "Radio Paradise")];
        let lines = screen(&mut st, 100, 34);
        let row = lines.iter().find(|l| l.contains("Radio")).unwrap();
        assert!(row.contains("1 saved station"), "{row:?}");

        st.radio_favorites.push(station("b", "SomaFM"));
        let lines = screen(&mut st, 100, 34);
        let row = lines.iter().find(|l| l.contains("Radio")).unwrap();
        assert!(row.contains("2 saved stations"), "{row:?}");
    }

    pub(super) fn station(uuid: &str, name: &str) -> crate::app::state::Station {
        crate::app::state::Station {
            uuid: uuid.into(),
            name: name.into(),
            url: format!("http://stream/{uuid}"),
            homepage: String::new(),
            tags: "eclectic".into(),
            country: "The United States Of America".into(),
            countrycode: "US".into(),
            language: "english".into(),
            codec: "MP3".into(),
            bitrate: 128,
            votes: 12,
            hls: false,
        }
    }

    /// The radio page: a tab strip over a station table, with the saved mark
    /// on the ones you kept and an `HLS` marker on the ones spot cannot play.
    #[test]
    fn the_radio_page_lists_stations_under_its_tabs() {
        use crate::app::state::{RadioRow, RadioScope, RadioView};

        let mut st = browse_state();
        st.radio_favorites = vec![station("a", "Radio Paradise")];
        let mut view = RadioView::new(RadioScope::Popular, 0);
        let mut hls = station("c", "BBC Radio 6 Music");
        hls.hls = true;
        hls.codec = "UNKNOWN".into();
        hls.bitrate = 0;
        view.rows = vec![
            RadioRow::Station(station("a", "Radio Paradise")),
            RadioRow::Station(station("b", "SomaFM")),
            RadioRow::Station(hls),
        ];
        view.loading = false;
        st.main = crate::app::state::MainView::Radio(view);

        let lines = screen(&mut st, 100, 34);
        let joined = lines.join("\n");
        for tab in ["Popular", "Countries", "Genres", "Saved"] {
            assert!(joined.contains(tab), "missing tab {tab}: {joined}");
        }
        assert!(joined.contains("3 stations"), "{joined}");

        let saved = lines.iter().find(|l| l.contains("Radio Paradise")).unwrap();
        assert!(
            saved.contains(super::table::LIKED_MARK),
            "a kept station wears the mark: {saved:?}"
        );
        let unsaved = lines.iter().find(|l| l.contains("SomaFM")).unwrap();
        assert!(
            !unsaved.contains(super::table::LIKED_MARK),
            "an unkept one does not: {unsaved:?}"
        );
        assert!(unsaved.contains("MP3 128k"), "{unsaved:?}");

        // Listed, not hidden — dropping these would silently remove the BBC.
        let bbc = lines.iter().find(|l| l.contains("BBC Radio 6")).unwrap();
        assert!(bbc.contains("HLS"), "{bbc:?}");
    }

    /// An empty Saved page says how to fill it rather than showing a blank
    /// table under column headings.
    #[test]
    fn an_empty_saved_page_says_what_to_do() {
        use crate::app::state::{RadioScope, RadioView};

        let mut st = browse_state();
        let mut view = RadioView::new(RadioScope::Favorites, 0);
        view.loading = false;
        st.main = crate::app::state::MainView::Radio(view);
        let joined = screen(&mut st, 100, 34).join("\n");
        assert!(joined.contains("no saved stations yet"), "{joined}");
    }

    /// The search row is one box that asks both catalogues, so it says the
    /// same thing on every page. It used to retarget — Spotify here, the
    /// station directory there — which made the prompt something you had to
    /// read before you could trust the key.
    #[test]
    fn the_search_row_says_the_same_thing_on_every_page() {
        use crate::app::state::{RadioScope, RadioView};

        let mut st = browse_state();
        let browse = screen(&mut st, 100, 34)[0].clone();
        assert!(browse.contains("search Spotify and radio"), "{browse:?}");

        st.main = crate::app::state::MainView::Radio(RadioView::new(RadioScope::Popular, 0));
        let radio = screen(&mut st, 100, 34)[0].clone();
        assert_eq!(browse, radio, "the prompt must not retarget");
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

    /// Fetches the real chart and draws it, which is everything between the
    /// directory and the screen except the client task that wires the two.
    ///
    /// Ignored by default — it needs a network. Run it with
    /// `cargo test ui::tests::live_radio -- --ignored --nocapture`; it prints
    /// the page, so a column that no longer fits real station names shows up
    /// rather than passing an assertion about invented ones.
    #[tokio::test]
    #[ignore]
    async fn live_radio_chart_renders() {
        use crate::app::state::{RadioRow, RadioScope, RadioView};
        use crate::radio::api::RadioApi;

        let api = RadioApi::new(
            reqwest::Client::builder()
                .user_agent(concat!("spot/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap(),
        );
        let stations = api.top_voted().await.expect("the chart should load");

        let mut st = browse_state();
        let mut view = RadioView::new(RadioScope::Popular, 0);
        view.rows = stations
            .iter()
            .take(12)
            .cloned()
            .map(RadioRow::Station)
            .collect();
        view.loading = false;
        st.radio_favorites = vec![stations[0].clone()];
        st.main = crate::app::state::MainView::Radio(view);

        let lines = screen(&mut st, 100, 34);
        for (i, l) in lines.iter().enumerate() {
            println!("{i:2} |{l}|");
        }
        // The first station's name must survive the column, or the table is
        // too narrow to be worth drawing.
        let head = &stations[0].name;
        let prefix: String = head.chars().take(8).collect();
        assert!(
            lines.iter().any(|l| l.contains(&prefix)),
            "the top station {head:?} did not reach the screen"
        );
    }

    /// The radio page and the bar under it, playing a station.
    #[test]
    #[ignore]
    fn dump_radio() {
        use crate::app::state::{RadioPlayback, RadioRow, RadioScope, RadioView};

        let mut st = browse_state();
        let mut view = RadioView::new(RadioScope::Popular, 0);
        let mut bbc = station("c", "BBC Radio 6 Music");
        bbc.hls = true;
        bbc.codec = "UNKNOWN".into();
        bbc.bitrate = 0;
        bbc.countrycode = "GB".into();
        bbc.tags = "alternative,indie music".into();
        let mut soma = station("b", "SomaFM Groove Salad");
        soma.tags = "ambient,downtempo,electronic".into();
        soma.codec = "MP3".into();
        view.rows = vec![
            RadioRow::Station(station("a", "Radio Paradise (Main Mix)")),
            RadioRow::Station(soma.clone()),
            RadioRow::Station(bbc),
        ];
        view.loading = false;
        st.radio_favorites = vec![station("a", "Radio Paradise (Main Mix)")];
        st.main = crate::app::state::MainView::Radio(view);
        st.radio = Some(RadioPlayback {
            station: soma,
            is_playing: true,
            started_at: Instant::now(),
            title: std::sync::Arc::new(parking_lot::Mutex::new(Some(
                "Steve Cobby — The Unvarnished Truth".into(),
            ))),
            volume_percent: 40,
        });
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    /// The facet list, and the player view over a station.
    #[test]
    #[ignore]
    fn dump_radio_countries() {
        use crate::app::state::{RadioRow, RadioScope, RadioView};

        let mut st = browse_state();
        let mut view = RadioView::new(RadioScope::Countries, 0);
        view.rows = [
            ("US", "The United States Of America", 7051u32),
            ("DE", "Germany", 5980),
            (
                "GB",
                "The United Kingdom Of Great Britain And Northern Ireland",
                2146,
            ),
        ]
        .into_iter()
        .map(|(key, label, count)| RadioRow::Facet {
            key: key.into(),
            label: label.into(),
            count,
        })
        .collect();
        view.loading = false;
        st.main = crate::app::state::MainView::Radio(view);
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    #[test]
    #[ignore]
    fn dump_radio_player() {
        use crate::app::state::RadioPlayback;

        let mut st = browse_state();
        st.show_player = true;
        st.radio = Some(RadioPlayback {
            station: station("b", "SomaFM Groove Salad"),
            is_playing: true,
            started_at: Instant::now(),
            title: std::sync::Arc::new(parking_lot::Mutex::new(Some(
                "Steve Cobby — The Unvarnished Truth".into(),
            ))),
            volume_percent: 40,
        });
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
