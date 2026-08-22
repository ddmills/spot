//! The bottom bar on the browse screen: what is playing, how far in, and the
//! transport.
//!
//! It is [`super::deck`] with a sleeve on the left and a toast tucked into
//! the blank row above the progress track — the same rows the full player
//! draws, in the same order, from the same code. It used to say them very
//! differently: inside a rounded frame, with the keybinding spelled out in
//! every button (`▮▮ (space) pause`), a `│` between each pair, and the device
//! name in the corner. That was eleven separate marks for four facts, and it
//! made this the busiest thing on a screen it sits at the bottom of.
//!
//! The keys still work; `?` is where they are written down.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::deck;
use super::table::{art_w, fit};
use super::theme;
use crate::app::state::AppState;

/// Rows the sleeve spans: every row of the deck beside it.
const ART_H: u16 = deck::DECK_H;
/// Cells between the sleeve and the text.
const ART_GAP: u16 = 3;
/// Narrowest text column worth keeping the sleeve for. Below this the bar is
/// better off spending the cells on the track title.
const MIN_TEXT_W: u16 = 44;
/// Narrowest the toast may be squeezed to before it is dropped. Under this
/// there is not enough of a message left to be worth reading.
const TOAST_MIN_W: u16 = 14;

pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // Split borrows: playback/queue/toast/cover are read while hit areas are
    // written.
    let AppState {
        playback,
        queue,
        toast,
        hit,
        mouse_pos,
        cover,
        liked,
        ..
    } = state;
    let mouse = *mouse_pos;
    let cover = cover.as_deref();
    if area.height == 0 || area.width == 0 {
        return;
    }

    // The one rule left on the browse screen. Everything else lost its border,
    // but the bar still has to separate itself from the list above: without a
    // line there, a track row and the bar's first row are the same mark at the
    // same weight, and the eye has nothing to stop at.
    frame.render_widget(
        Paragraph::new(Line::styled("─".repeat(area.width as usize), theme::rule())),
        Rect { height: 1, ..area },
    );
    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    if body.height == 0 {
        return;
    }

    let Some(pb) = playback else {
        deck::no_playback_hint(frame, body);
        return;
    };

    // The sleeve, when there are rows and cells to spare for it. It is the
    // first thing shed, exactly as in the player: the bar still works without
    // artwork, and a title clipped to make room for a thumbnail does not.
    let sleeve_w = art_w(ART_H);
    let text = if body.height >= ART_H && body.width >= sleeve_w + ART_GAP + MIN_TEXT_W {
        let art = deck::sleeve(
            frame,
            Rect {
                width: sleeve_w,
                height: ART_H,
                ..body
            },
            pb,
            cover,
            hit,
        );
        Rect {
            x: art.right() + ART_GAP,
            width: body.width - art.width - ART_GAP,
            ..body
        }
    } else {
        body
    };

    // Rows 0-1: the title, then the metadata with the volume slider opposite.
    // The play state used to hold the title's right edge; it now sits under
    // the progress track, between previous and next.
    let like = pb
        .track_uri
        .as_ref()
        .and_then(|uri| liked.get(uri).copied());
    deck::masthead(frame, text, pb, deck::Note::Show, like, mouse, hit);

    // The whole bar is the wheel target, sleeve included, which is wider than
    // the two rows `masthead` claims for the player's benefit. Assigned after
    // it, not before.
    hit.now_playing = area;

    // Row 2: whatever the app last had to say, opposite nothing. It used to
    // share the transport's row; that row now has a button at each end.
    if text.height < 3 {
        return;
    }
    if let Some((msg, _)) = toast
        && text.width > TOAST_MIN_W
    {
        // `fit` pads to exactly the width; trim it back so the right-alignment
        // puts the text against the edge rather than the padding.
        let msg = fit(msg, (text.width - 1) as usize).trim_end().to_string();
        frame.render_widget(
            Paragraph::new(Line::styled(msg, Style::default().fg(theme::WARN)))
                .alignment(Alignment::Right),
            Rect {
                y: text.y + 2,
                width: text.width - 1,
                height: 1,
                ..text
            },
        );
    }

    // Row 3: elapsed, the track, time remaining.
    if text.height < 4 {
        return;
    }
    deck::progress(
        frame,
        Rect {
            y: text.y + 3,
            height: 1,
            ..text
        },
        pb,
        hit,
    );

    // Row 4: previous and next at opposite edges, the play state between them.
    if text.height < 5 {
        return;
    }
    deck::transport(
        frame,
        Rect {
            y: text.y + 4,
            height: 1,
            ..text
        },
        pb,
        mouse,
        hit,
    );

    // Row 5 is blank; row 6 names the queue and carries shuffle. Clicking the
    // name opens the player view — see `event.rs`.
    if text.height < deck::DECK_H {
        return;
    }
    deck::context_row(
        frame,
        Rect {
            y: text.y + 6,
            height: 1,
            ..text
        },
        pb,
        queue.as_ref(),
        mouse,
        hit,
    );
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::style::Color;

    use super::super::table::VOL_TRACK_W;
    use super::*;
    use crate::app::state::{PlaybackSnapshot, RepeatMode, Track, TrackList};

    /// Bar height under test: the rule, the deck, and the trailing blank —
    /// what `super::super::BAR_H` gives it on the browse screen.
    const BAR_H: u16 = 1 + deck::DECK_H + 1;

    /// Screen rows of the bar, counting the rule as 0. The deck's own rows
    /// start one lower, and the blank ones are not named.
    const TITLE_ROW: usize = 1;
    const META_ROW: usize = 2;
    const TOAST_ROW: usize = 3;
    const PROGRESS_ROW: usize = 4;
    const TRANSPORT_ROW: usize = 5;
    const CONTEXT_ROW: usize = 7;

    fn playing_state() -> AppState {
        let mut state = AppState::new();
        state.playback = Some(PlaybackSnapshot {
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
            fetched_at: Instant::now(),
        });
        let mut q = TrackList::new("My Mix", "by me", None, None);
        q.append(
            (0..24)
                .map(|i| Track {
                    uri: format!("spotify:track:t{i}"),
                    name: format!("Track {i}"),
                    artists: "Someone".into(),
                    album: "Album Name".into(),
                    release_year: "2020".into(),
                    duration_ms: 60_000,
                    track_number: i + 1,
                    album_id: None,
                    artist_id: None,
                    cover_url: None,
                })
                .collect(),
        );
        state.queue = Some(q);
        state
    }

    fn render(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, f.area(), state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn renders_a_rule_a_sleeve_and_the_deck() {
        let mut state = playing_state();
        let lines = render(&mut state, 120, BAR_H);
        // One rule, and it is the only box-drawing left on the bar.
        assert!(lines[0].starts_with("───"));
        assert!(
            !lines
                .iter()
                .skip(1)
                .any(|l| l.contains('│') || l.contains('╭'))
        );
        assert!(lines[TITLE_ROW].contains("♫ Song Title"));
        assert!(
            !lines[TITLE_ROW].contains("playing"),
            "{:?}",
            lines[TITLE_ROW]
        );
        assert!(lines[META_ROW].contains("Artist Name · Album Name · 2020"));
        assert!(lines[META_ROW].contains(" vol "));
        assert!(lines[META_ROW].contains(" 56% "));
        assert!(lines[PROGRESS_ROW].contains("1:23 ━"));
        // No grab handle on the track — only the volume slider keeps one.
        assert!(
            !lines[PROGRESS_ROW].contains('●'),
            "{:?}",
            lines[PROGRESS_ROW]
        );
        assert!(lines[PROGRESS_ROW].contains(" -2:22"));
        // Previous and next hold opposite ends of their own row, below the
        // progress track rather than beside the queue's name, with the play
        // state centred between them.
        assert!(lines[TRANSPORT_ROW].contains("◂◂ previous"));
        assert!(lines[TRANSPORT_ROW].contains("● playing"));
        assert!(lines[TRANSPORT_ROW].trim_end().ends_with("▸▸ next"));
        // The queue's name and length, with shuffle opposite.
        assert!(lines[CONTEXT_ROW].contains("My Mix · 24 tracks"));
        assert!(lines[CONTEXT_ROW].contains("shuffle off"));
        // Repeat is pinned to all by the client, so it is not a control here.
        assert!(!lines.iter().any(|l| l.contains("repeat")));

        // The key hints, the dividers and the device readout are all gone.
        let all = lines.join("\n");
        for gone in ["(space)", "(p)revious", "(n)ext", "▣ ", "▮▮"] {
            assert!(!all.contains(gone), "{gone:?} survived:\n{all}");
        }

        // Every control recorded a live hit rect on the row it was drawn on.
        for rect in [
            state.hit.play_btn,
            state.hit.prev_btn,
            state.hit.next_btn,
            state.hit.shuffle_btn,
            state.hit.queue_name,
            state.hit.volume_slider,
            state.hit.art,
        ] {
            assert!(!rect.is_empty());
        }
        assert_eq!(state.hit.play_btn.y as usize, TRANSPORT_ROW);
        assert_eq!(state.hit.volume_slider.y as usize, META_ROW);
        assert_eq!(state.hit.gauge.y as usize, PROGRESS_ROW);
        assert_eq!(state.hit.prev_btn.y as usize, TRANSPORT_ROW);
        assert_eq!(state.hit.next_btn.y as usize, TRANSPORT_ROW);
        assert_eq!(state.hit.queue_name.y as usize, CONTEXT_ROW);
        assert_eq!(state.hit.shuffle_btn.y as usize, CONTEXT_ROW);
        assert_eq!(state.hit.volume_slider.width, VOL_TRACK_W);
        // The wheel adjusts volume anywhere over the bar, sleeve included —
        // wider than the two rows `deck::masthead` claims for the player.
        assert_eq!(state.hit.now_playing.height, BAR_H);
    }

    /// The metadata row starts flush with the title's `♫` rather than under
    /// the title's first letter, so the masthead reads as one block.
    #[test]
    fn the_metadata_row_is_not_indented() {
        let mut state = playing_state();
        let lines = render(&mut state, 120, BAR_H);
        let col = |s: &str, c: char| s.chars().position(|x| x == c);
        assert_eq!(
            col(&lines[META_ROW], 'A'),
            col(&lines[TITLE_ROW], '♫'),
            "{:?} / {:?}",
            lines[META_ROW],
            lines[TITLE_ROW]
        );
    }

    /// The sleeve spans every row of the deck beside it, and is square: an
    /// R-row block is 2R cells wide. Every cell of it is painted, or the
    /// terminal's own ground shows through the half-blocks as stripes.
    #[test]
    fn the_sleeve_is_square_and_fully_painted() {
        let mut state = playing_state();
        let mut terminal = Terminal::new(TestBackend::new(120, BAR_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut state)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        let art = state.hit.art;
        assert_eq!((art.width, art.height), (art_w(ART_H), ART_H));
        assert_eq!(art.height, deck::DECK_H, "the sleeve should span the deck");
        assert_eq!(art.y, 1, "the sleeve should start under the rule");
        // No cover in the test, so this is the placeholder swatch: half-blocks
        // everywhere, with a single `♫` in the middle saying the artwork has
        // not arrived rather than that there is none.
        let note = Position {
            x: art.x + art.width / 2,
            y: art.y + art.height / 2,
        };
        for y in art.y..art.bottom() {
            for x in art.x..art.right() {
                let cell = buffer.cell(Position { x, y }).unwrap();
                let want = if (x, y) == (note.x, note.y) {
                    "♫"
                } else {
                    "▀"
                };
                assert_eq!(cell.symbol(), want, "wrong glyph at {x},{y}");
                assert!(matches!(cell.fg, Color::Rgb(..)), "no fg at {x},{y}");
                assert!(matches!(cell.bg, Color::Rgb(..)), "no bg at {x},{y}");
            }
        }
        // The text starts past the sleeve and its gap.
        assert!(state.hit.gauge.x >= art.right() + ART_GAP);
    }

    /// The sleeve goes before the text does: a bar with no artwork still
    /// works, and a title clipped to make room for a thumbnail does not.
    #[test]
    fn a_narrow_bar_drops_the_sleeve_and_keeps_the_title() {
        let mut state = playing_state();
        let lines = render(&mut state, 50, BAR_H);
        assert!(state.hit.art.is_empty());
        assert!(lines[TITLE_ROW].contains("Song Title"));
        assert!(lines[PROGRESS_ROW].contains("1:23"));
    }

    #[test]
    fn artist_and_album_names_record_click_rects() {
        let mut state = playing_state();
        let lines = render(&mut state, 100, BAR_H);
        for (rect, text) in [
            (state.hit.now_artist, "Artist Name"),
            (state.hit.now_album, "Album Name"),
        ] {
            assert_eq!(rect.y as usize, META_ROW);
            assert_eq!(rect.width as usize, text.len());
            let at_rect: String = lines[META_ROW]
                .chars()
                .skip(rect.x as usize)
                .take(rect.width as usize)
                .collect();
            assert_eq!(at_rect, text);
        }

        // Without ids there is nothing to open, so no rects are recorded.
        let mut state = playing_state();
        let pb = state.playback.as_mut().unwrap();
        pb.artist_id = None;
        pb.album_id = None;
        render(&mut state, 100, BAR_H);
        assert!(state.hit.now_artist.is_empty());
        assert!(state.hit.now_album.is_empty());
    }

    /// Nothing has loaded the playing context yet: shuffle still draws, and
    /// there is no name to click.
    #[test]
    fn the_context_row_survives_a_missing_queue() {
        let mut state = playing_state();
        state.queue = None;
        let lines = render(&mut state, 100, BAR_H);
        assert!(lines[CONTEXT_ROW].contains("shuffle off"));
        assert!(state.hit.queue_name.is_empty());
    }

    /// The state pill is padded to a fixed width, so nothing under the cursor
    /// moves when playback toggles.
    #[test]
    fn play_pause_toggle_keeps_the_layout_stable() {
        let mut playing = playing_state();
        render(&mut playing, 100, BAR_H);
        let mut paused = playing_state();
        paused.playback.as_mut().unwrap().is_playing = false;
        let lines = render(&mut paused, 100, BAR_H);
        assert!(lines[TRANSPORT_ROW].contains("■ paused"));
        assert_eq!(playing.hit.play_btn, paused.hit.play_btn);
        assert_eq!(playing.hit.prev_btn, paused.hit.prev_btn);
        assert_eq!(playing.hit.next_btn, paused.hit.next_btn);
        assert_eq!(playing.hit.volume_slider, paused.hit.volume_slider);
        assert_eq!(playing.hit.gauge, paused.hit.gauge);
    }

    #[test]
    fn hover_paints_a_pill_on_the_hovered_button_only() {
        let mut state = playing_state();
        render(&mut state, 100, BAR_H);
        let prev = state.hit.prev_btn;
        state.mouse_pos = Some(Position {
            x: prev.x,
            y: prev.y,
        });

        let mut terminal = Terminal::new(TestBackend::new(100, BAR_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let bg_at = |x: u16| buffer.cell(Position { x, y: prev.y }).unwrap().bg;
        assert_eq!(bg_at(prev.x), theme::DIM);
        assert_eq!(bg_at(prev.right() - 1), theme::DIM);
        assert_eq!(bg_at(state.hit.next_btn.x), Color::Reset);
    }

    /// The progress row is a readout: it seeks on a click, but it carries no
    /// grab handle, and hovering it puts no mark on the row either.
    #[test]
    fn the_progress_track_has_no_grab_handle() {
        let mut state = playing_state();
        render(&mut state, 100, BAR_H);
        let gauge = state.hit.gauge;
        state.mouse_pos = Some(Position {
            x: gauge.x + gauge.width / 2,
            y: gauge.y,
        });

        let mut terminal = Terminal::new(TestBackend::new(100, BAR_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row: Vec<_> = (0..100)
            .map(|x| buffer.cell(Position { x, y: gauge.y }).unwrap().clone())
            .collect();
        assert!(row.iter().all(|c| c.symbol() != "●"));
        assert!(row.iter().all(|c| c.fg != theme::accent_bright()));
        assert!(row.iter().all(|c| c.bg != theme::DIM));
    }

    /// The toast lost its frame title along with the frame, and then lost the
    /// transport's row to a button at each end of it. It lands on the blank
    /// row above the progress track, against the same right edge the volume
    /// slider uses.
    #[test]
    fn a_toast_lands_above_the_progress_track() {
        let mut state = playing_state();
        state.toast = Some(("added to queue".into(), Instant::now()));
        let lines = render(&mut state, 100, BAR_H);
        assert!(lines[TOAST_ROW].contains("added to queue"));
        assert!(
            lines[TRANSPORT_ROW].contains("◂◂ previous"),
            "the transport was displaced"
        );
        assert!(
            !lines[TOAST_ROW].contains("◂◂"),
            "the toast landed on the transport"
        );
    }

    /// A short toast lands against the right edge.
    #[test]
    fn a_short_toast_is_right_aligned() {
        let mut state = playing_state();
        state.toast = Some(("added to queue".into(), Instant::now()));
        let lines = render(&mut state, 120, BAR_H);
        assert!(
            !lines[TOAST_ROW].contains('…'),
            "a fitting toast should not be clipped"
        );
        // Right-aligned: only the reserved edge cell follows it.
        let end = lines[TOAST_ROW].rfind("added to queue").unwrap() + "added to queue".len();
        assert!(lines[TOAST_ROW][end..].trim().is_empty());
    }

    /// A message longer than its row is clipped rather than dropped — a
    /// truncated error beats a silent one.
    #[test]
    fn a_long_toast_is_clipped() {
        let mut state = playing_state();
        state.toast = Some((
            "load failed: request timed out contacting api.spotify.com".into(),
            Instant::now(),
        ));
        let lines = render(&mut state, 70, BAR_H);
        let row = &lines[TOAST_ROW];
        assert!(row.contains("load failed"), "the toast vanished: {row:?}");
        assert!(row.contains('…'), "the toast was not clipped: {row:?}");
    }

    /// Below the minimum there is not enough of a message left to be worth
    /// reading, so it is dropped whole.
    #[test]
    fn a_toast_gives_way_on_a_row_with_no_room() {
        let mut state = playing_state();
        state.toast = Some(("added to queue".into(), Instant::now()));
        let lines = render(&mut state, TOAST_MIN_W, BAR_H);
        assert!(
            !lines[TOAST_ROW].contains("added"),
            "a squeezed toast should go: {:?}",
            lines[TOAST_ROW]
        );
    }

    /// The volume track is click-mapped, so it draws the handle whose position
    /// matches that mapping — in both views, and unlike the progress track.
    #[test]
    fn the_volume_slider_shows_its_grab_handle() {
        let mut state = playing_state();
        let lines = render(&mut state, 120, BAR_H);
        let vol = state.hit.volume_slider;
        let track: String = lines[META_ROW]
            .chars()
            .skip(vol.x as usize)
            .take(vol.width as usize)
            .collect();
        assert!(
            track.contains('●'),
            "no knob on the volume track: {track:?}"
        );
    }

    #[test]
    fn nothing_playing_shows_a_hint() {
        let mut state = AppState::new();
        let lines = render(&mut state, 100, BAR_H);
        assert!(lines[1].contains("nothing playing"));
        assert!(state.hit.play_btn.is_empty());
    }

    #[test]
    fn short_bar_degrades_without_panicking() {
        for height in 0..BAR_H + 2 {
            let mut state = playing_state();
            render(&mut state, 100, height);
        }
        for width in 0..30 {
            let mut state = playing_state();
            render(&mut state, width, BAR_H);
        }
    }
}
