pub mod columns;
mod deck;
mod help;
mod main_pane;
mod now_playing;
mod play_state;
mod player;
mod playlist_edit;
mod playlist_picker;
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
/// Rows the bottom bar takes when it has a subject: a rule, the deck's seven
/// rows beside the sleeve, then a blank. With nothing playing the bar takes no
/// rows at all. See [`now_playing`].
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
        let bar_h = match now_playing::has_subject(state) {
            true => BAR_H,
            false => 0,
        };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(1)
            .constraints([
                // The mark and the prompt, a blank, the path, a blank. Fixed,
                // so entering and leaving search mode moves nothing below it.
                Constraint::Length(HEAD_H),
                Constraint::Min(1),
                Constraint::Length(bar_h),
            ])
            .split(frame.area());

        // One column: the playlists are a page of their own, reached from Home,
        // so no left rail takes width from the pane. See
        // `main_pane::draw_playlists`.
        //
        // The page's own contribution to the header — its count, and whether
        // it is still loading — read off before the header is drawn, because
        // the row it goes on is not the pane's to draw.
        let page = main_pane::page_header(state);
        top_row::draw(frame, rows[0], state, page);
        main_pane::draw(frame, rows[1], state);
        now_playing::draw(frame, rows[2], state);
    }

    // Under the help box: help is the one overlay that answers "what does any
    // of this do", so nothing may cover it.
    if state.picker.is_some() {
        playlist_picker::draw(frame, state);
    }

    if state.edit.is_some() {
        playlist_edit::draw(frame, state);
    }

    if state.show_help {
        help::draw(frame, state);
    }
}

/// Whether anything on screen is mid-animation and needs the next frame.
///
/// A spinner only turns if something keeps drawing it, and at the idle tick it
/// would step about four times a second rather than spin. This is what the
/// frame loop reads to hold the fast tick over a load — see `run_tui`.
///
/// The visualizer is not here: the player view asks for the fast tick
/// outright, whatever is on it.
pub fn is_animating(state: &AppState) -> bool {
    // The nav row's `LOADING`, on either screen.
    if play_state::status(state).is_some_and(|s| s.state == play_state::PlayState::Loading) {
        return true;
    }
    // The main pane's own spinner, while the page it is drawing is still
    // being fetched.
    if state.main_loading() {
        return true;
    }
    // The add-to-playlist box's `checking…`, while a row's mark is still out.
    state
        .picker_visible()
        .into_iter()
        .any(|i| state.picker_has(&state.playlists[i].id).is_none())
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
    use crate::app::state::{Playback, Playlist, TrackList};

    fn browse_state() -> AppState {
        let mut st = AppState::new();
        // A signed-in Premium account, which is what the library screens are
        // about; the app itself starts on radio alone.
        st.spotify = crate::app::state::SpotifyState::Ready;
        st.set_playlists(
            (0..8)
                .map(|i| Playlist {
                    id: format!("p{i}"),
                    name: format!("Playlist {i}"),
                    track_count: 10 + i,
                    owner: "me".into(),
                    owner_id: "me".into(),
                    snapshot_id: "s".into(),
                    cover_url: None,
                    public: None,
                    collaborative: false,
                })
                .collect(),
        );
        let mut list = TrackList::new("Hey Arnold!", "by me", None);
        list.rows = ((0..12)
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
            .collect::<Vec<_>>())
        .into();
        // The same list is what is playing, so the bar's context row names it
        // — the queue is the list's rows, at the row that is on.
        let mut q = crate::app::queue::Queue::new(list.items.clone(), 3, "My Mix");
        q.source_key = Some(crate::app::state::playlist_key("p2"));
        st.queue = Some(q);
        st.main = crate::app::state::MainView::Tracks(list);
        let mut pb = Playback::started(70, false);
        pb.anchor(49_000);
        // Freeze the readout: with the anchor in the future, `elapsed()`
        // saturates to zero, so two renders a millisecond apart cannot
        // disagree about the remaining time.
        pb.anchored_at = Instant::now() + Duration::from_secs(60);
        st.playback = Some(pb);
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
    /// One mark is allowed to survive — the scrollbar track, which is a control
    /// rather than chrome. The bottom bar is separated from the list above it
    /// by a blank row, not a rule.
    #[test]
    fn the_browse_screen_draws_no_pane_frames() {
        let mut st = browse_state();
        let lines = screen(&mut st, 100, 34);
        for (y, line) in lines.iter().enumerate() {
            for c in "╭╮╰╯┌┐└┘".chars() {
                assert!(!line.contains(c), "corner {c:?} on row {y}: {line:?}");
            }
        }
        // No horizontal rules at all. Matched as a row of nothing but `─` and
        // margin — the progress and volume tracks are drawn with the same
        // glyph, but always alongside other marks.
        let rules: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains('─') && l.chars().all(|c| c == '─' || c == ' '))
            .map(|(y, _)| y)
            .collect();
        assert!(rules.is_empty(), "expected no rules, got rows {rules:?}");
        // And the bar's own top row is blank.
        assert!(
            lines[34 - BAR_H as usize].trim().is_empty(),
            "bar's top row is not blank: {:?}",
            lines[34 - BAR_H as usize]
        );
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
        // The prompt row itself is expected to differ; nothing else may move,
        // the path on the row under it included.
        for y in (0..34).filter(|&y| y != 0) {
            assert_eq!(before[y], after[y], "row {y} moved when search opened");
        }
    }

    /// With no rail beside it the pane starts at the screen's own margin, and
    /// its label is simply the page's name — there is no second pane for focus
    /// to be anywhere else.
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
        // short of it. Row 10 is that heading: label, blank, the band's four
        // lines and the blank under its control row, then the columns.
        let row: String = (0..100)
            .filter_map(|x| buffer.cell(Position { x, y: 10 }).map(|c| c.symbol()))
            .collect();
        assert!(row.contains("Title"), "not the column header: {row:?}");
        assert!(
            row.find("Year").is_some_and(|at| at > 70),
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
                top: TrackList::new("Muse", "", None),
                albums: vec![].into(),
                tab: crate::app::state::ArtistTab::Albums,
                loading: false,
                error: None,
            });
            st.push_view();
            st.main = MainView::Tracks(TrackList::new("Black Holes", "", None));
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
        assert!(table::ends_with_loading(&a[0]), "{:?}", a[0]);
        assert!(table::ends_with_loading(&b[0]), "{:?}", b[0]);
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
            name: "Discover Weekly".into(),
            track_count: 30,
            owner: "Spotify".into(),
            owner_id: "spotify".into(),
            snapshot_id: "s".into(),
            cover_url: None,
            public: None,
            collaborative: false,
        });
        st.main = crate::app::state::MainView::Home;
        let lines = screen(&mut st, 100, 34);
        // Home draws no crumb: the mark is already the way there. The
        // playback status is opposite it, as on every other page.
        assert!(lines[0].starts_with(" ♫ spot "), "{:?}", lines[0]);
        assert!(table::ends_with_loading(&lines[0]), "{:?}", lines[0]);
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

    /// The update row is on Home only while there is an update, and it names
    /// the release it offers.
    #[test]
    fn the_update_row_appears_only_with_a_release_waiting() {
        use crate::app::state::UpdateState;

        let mut st = browse_state();
        st.main = crate::app::state::MainView::Home;
        let quiet = screen(&mut st, 100, 34).join("\n");
        assert!(!quiet.contains("Update available"), "{quiet}");

        st.update = Some(UpdateState::Available(crate::update::Release {
            tag: "v9.9.9".into(),
            url: "https://example.test/spot.exe".into(),
        }));
        let lines = screen(&mut st, 100, 34);
        let row = lines
            .iter()
            .find(|l| l.contains("Update available"))
            .expect("the update row should be on Home");
        assert!(row.contains("v9.9.9"), "{row:?}");

        st.update = Some(UpdateState::Installed);
        let done = screen(&mut st, 100, 34).join("\n");
        assert!(done.contains("press Enter to restart into it"), "{done}");
    }

    /// The help overlay is the only place the running version is written down.
    #[test]
    fn the_help_overlay_names_the_version() {
        use crate::app::state::UpdateState;

        let mut st = browse_state();
        st.show_help = true;
        let running = format!("spot v{}", env!("CARGO_PKG_VERSION"));
        let quiet = screen(&mut st, 100, 44).join("\n");
        assert!(quiet.contains(&running), "{quiet}");

        st.update = Some(UpdateState::Available(crate::update::Release {
            tag: "v9.9.9".into(),
            url: "https://example.test/spot.exe".into(),
        }));
        let offered = screen(&mut st, 100, 44).join("\n");
        assert!(offered.contains("v9.9.9 available"), "{offered}");
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
        view.rows = (vec![
            RadioRow::Station(station("a", "Radio Paradise")),
            RadioRow::Station(station("b", "SomaFM")),
            RadioRow::Station(hls),
        ])
        .into();
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
    /// same thing on every page. A prompt that retargets — Spotify here, the
    /// station directory there — is something you have to read before you can
    /// trust the key.
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
        st.main = crate::app::state::MainView::Tracks(TrackList::new(name, "", None));
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
    fn dump_playlist() {
        let mut st = browse_state();
        st.me_id = Some("me".into());
        st.saved_playlists.insert("dw".into(), true);
        if let crate::app::state::MainView::Tracks(l) = &mut st.main {
            l.cache_key = Some(crate::app::state::playlist_key("dw"));
            l.header.name = "Discover Weekly".into();
            l.header.subtitle = "by Spotify".into();
            l.header.owner_id = "spotify".into();
            l.header.description =
                "Your weekly mixtape of fresh music. Enjoy new discoveries and deep cuts \
                 chosen just for you. Updated every Monday."
                    .into();
            l.header.cover_url = Some("https://i.scdn.co/image/dw".into());
        }
        arrive_via(&mut st, "Playlists");
        for (i, l) in screen(&mut st, 100, 26).iter().enumerate() {
            println!("{i:2} |{l}|");
        }
    }

    #[test]
    #[ignore]
    fn dump_playlist_edit() {
        let mut st = browse_state();
        st.edit = Some(crate::app::state::PlaylistEdit {
            id: "p1".into(),
            name: "Road Trip".into(),
            description: "long drives and open windows".into(),
            field: crate::app::state::EditField::Name,
            pending: false,
            error: None,
            seq: 1,
        });
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
        let mut top = TrackList::new("Roy Hargrove", "top tracks", None);
        top.append(list.rows.items);
        let mut view = crate::app::state::ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Roy Hargrove".into(),
            image_url: Some("https://i.scdn.co/image/artist".into()),
            genres: vec!["jazz".into(), "hard bop".into()],
            top,
            albums: (0..12)
                .map(|i| {
                    let group = if i % 3 == 0 { "single" } else { "album" };
                    crate::app::state::AlbumItem {
                        id: format!("a{i}"),
                        name: format!("Record Number {i}"),
                        artists: "Roy Hargrove".into(),
                        release_year: (2010 - i).to_string(),
                        album_type: group.into(),
                        album_group: group.into(),
                        track_count: 4 + i as u32,
                        cover_url: Some(format!("https://i.scdn.co/image/a{i}")),
                    }
                })
                .collect::<Vec<_>>()
                .into(),
            tab: crate::app::state::ArtistTab::Albums,
            loading: false,
            error: None,
        };
        view.retab();
        st.main = crate::app::state::MainView::Artist(view);
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
    /// the page, so a column too narrow for real station names shows up rather
    /// than passing an assertion about invented ones.
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
        view.rows = (vec![
            RadioRow::Station(station("a", "Radio Paradise (Main Mix)")),
            RadioRow::Station(soma.clone()),
            RadioRow::Station(bbc),
        ])
        .into();
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
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
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
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
        });
        st.listen_back.push(crate::app::state::Listened::Spotify);
        for (i, l) in screen(&mut st, 100, 34).iter().enumerate() {
            println!("{i:2} |{l}|");
        }

        println!("\n--- and the same station, off air ---\n");
        let r = st.radio.as_mut().unwrap();
        r.is_playing = false;
        r.failure = Some("could not reach the station: operation timed out".into());
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

    /// A spinner only turns if something keeps drawing it, and a load is
    /// exactly when nothing else is asking the frame loop to.
    #[test]
    fn a_spinner_holds_the_frame_loop_awake() {
        let mut st = browse_state();
        // Paused: nothing is being waited for, so nothing is turning.
        st.playback.as_mut().unwrap().is_playing = false;
        assert!(!is_animating(&st));

        // Claims to play, but no sound out of it yet — the buffering window
        // the nav row says LOADING through.
        st.playback.as_mut().unwrap().is_playing = true;
        st.audio_tap.clear();
        let lines = screen(&mut st, 100, 34);
        assert!(table::ends_with_loading(&lines[0]), "{:?}", lines[0]);
        assert!(is_animating(&st));
    }

    /// And the box's own spinner, which is up only while a row's mark is out.
    #[test]
    fn the_box_holds_it_awake_while_a_mark_is_unanswered() {
        let mut st = browse_state();
        st.show_player = true;
        st.me_id = Some("me".into());
        // Paused, so the nav row's own spinner is not what is answering here.
        st.playback.as_mut().unwrap().is_playing = false;
        let uri = "spotify:track:t3".to_string();
        st.picker = Some(crate::app::state::PlaylistPicker {
            order: st.picker_order(&uri),
            uri,
            query: String::new(),
            selected: 0,
            offset: 0,
            pending: Default::default(),
            error: None,
            seq: 1,
        });
        assert!(is_animating(&st), "no playlist has been walked");

        for i in st.picker_rows() {
            let id = st.playlists[i].id.clone();
            st.playlist_tracks.insert(
                id,
                crate::app::state::PlaylistContents {
                    snapshot_id: "s".into(),
                    track_ids: Default::default(),
                },
            );
        }
        assert!(!is_animating(&st), "every visible row is answered");
    }

    /// `+ add` rides beside the `★` on both screens, in the same corner of the
    /// same title row — the pair is one control set, and one that appeared
    /// only after pressing `v` would be a control that moves.
    #[test]
    fn both_views_offer_add_to_playlist() {
        for player in [false, true] {
            let mut st = browse_state();
            st.show_player = player;
            st.liked.insert("spotify:track:t3".into(), true);
            let lines = screen(&mut st, 100, 34);
            // Beside the ★, not instead of it, and one space apart.
            assert!(
                lines.iter().any(|l| l.contains("★ liked + add")),
                "player={player} {lines:#?}"
            );
            assert!(!st.hit.add_btn.is_empty(), "player={player}");
            assert_eq!(
                st.hit.add_btn.x,
                st.hit.like_btn.right() + 1,
                "player={player}"
            );
        }
    }

    /// Without an account there are no playlists to add to, so the control
    /// that would open an empty box is not drawn at all.
    #[test]
    fn add_to_playlist_needs_spotify() {
        let mut st = browse_state();
        st.show_player = true;
        st.spotify = crate::app::state::SpotifyState::Off;
        let lines = screen(&mut st, 100, 34);
        assert!(!lines.iter().any(|l| l.contains("+ add")), "{lines:#?}");
        assert!(st.hit.add_btn.is_empty());
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
