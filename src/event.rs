use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use parking_lot::RwLock;
use ratatui::layout::{Position, Rect};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::command::{AppCommand, FetchSource};
use crate::app::state::{
    self as state, AppState, ArtistRow, BackTarget, CrumbTarget, HomeItem, InputMode, MainView,
    RadioMatch, RadioRow, RadioScope, RadioTab, SearchTab, SortKey, Station, Track, TrackList,
    ViewKey,
};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const SCROLL_LINES: i64 = 3;

pub fn handle_event(event: Event, state: &Arc<RwLock<AppState>>, tx: &UnboundedSender<AppCommand>) {
    match event {
        // Windows emits both Press and Release events; act on Press only.
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // Raw mode means the terminal never turns Ctrl-C into a signal, so
            // it would otherwise do nothing at all — and it is the first thing
            // someone reaches for. Handled above the mode dispatch so it also
            // quits from the search prompt, where a bare `q` is just a letter.
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                state.write().should_quit = true;
                return;
            }
            let mode = state.read().input_mode;
            match mode {
                InputMode::Search => handle_search_input(key, state, tx),
                InputMode::Normal => handle_normal(key, state, tx),
            }
        }
        Event::Mouse(mouse) => handle_mouse(mouse, state, tx),
        _ => {}
    }
}

fn handle_mouse(
    mouse: MouseEvent,
    state: &Arc<RwLock<AppState>>,
    tx: &UnboundedSender<AppCommand>,
) {
    let pos = Position {
        x: mouse.column,
        y: mouse.row,
    };
    let mut st = state.write();
    // Track the pointer for hover styling; the draw loop repaints on every
    // event, so Moved needs no handling beyond recording the position.
    st.mouse_pos = Some(pos);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => handle_click(&mut st, pos, tx),
        MouseEventKind::ScrollUp => handle_scroll(&mut st, pos, -1, tx),
        MouseEventKind::ScrollDown => handle_scroll(&mut st, pos, 1, tx),
        _ => {}
    }
}

fn handle_click(st: &mut AppState, pos: Position, tx: &UnboundedSender<AppCommand>) {
    if st.show_help {
        st.show_help = false;
        return;
    }
    // The mark is on every screen, in both views, and it is the one control
    // that means the same thing wherever it is clicked.
    if st.hit.home_btn.contains(pos) {
        st.input_mode = InputMode::Normal;
        st.input_buffer.clear();
        go_home(st);
        return;
    }
    // The playback status opposite it, on the same row and on every screen.
    // It is the deck's queue name said where you are already looking when you
    // want to know what is playing, so it toggles the player the same way —
    // and clears the input like the mark does, or a click here while typing
    // would fall into the cancel branch below and never reach this one.
    if st.hit.status.contains(pos) {
        st.input_mode = InputMode::Normal;
        st.input_buffer.clear();
        if st.show_player {
            st.show_player = false;
        } else {
            open_player(st, tx);
        }
        return;
    }
    // The search row is always on screen now, so clicking it is how you start
    // a search with the mouse. It has to be checked before the cancel below,
    // or clicking the box you are typing in would close it.
    if st.hit.search_box.contains(pos) {
        st.input_mode = InputMode::Search;
        return;
    }
    // A click anywhere else while typing cancels the input, then lands
    // normally.
    if st.input_mode == InputMode::Search {
        st.input_mode = InputMode::Normal;
        st.input_buffer.clear();
    }

    if let Some(tab) = st
        .hit
        .search_tabs
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|(_, tab)| *tab)
    {
        if st.search_tab != tab {
            st.search_tab = tab;
            st.main_to_top();
        }
        return;
    }

    // The radio strip. Unlike search's, each tab is a page of its own — see
    // `cycle_view_tab` — so clicking one navigates rather than re-cutting a
    // result set already in hand.
    if let Some(tab) = st
        .hit
        .radio_tabs
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|(_, tab)| *tab)
    {
        open_radio_tab(st, tab, tx);
        return;
    }

    // The artist page's album groups. Search's kind, not radio's: one fetch
    // holds every group, so a click re-cuts the catalogue in place.
    if let Some(tab) = st
        .hit
        .artist_tabs
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|(_, tab)| *tab)
    {
        if let MainView::Artist(v) = &mut st.main
            && v.tab != tab
        {
            v.set_tab(tab);
            st.main_to_top();
            let _ = tx.send(AppCommand::LoadArtistArt);
        }
        return;
    }

    // An album card's two controls, ahead of the list they sit in: the name
    // opens the record, the ▶ play starts it. A click anywhere else on the
    // card falls through and merely selects it, like any other row.
    let card_hit = |hits: &[(Rect, usize)]| {
        hits.iter()
            .find(|(rect, _)| rect.contains(pos))
            .map(|(_, row)| *row)
    };
    // Home rows open on one click, the way the left rail's entries did: this
    // is navigation, so there is no "play" for a second click to mean. The
    // whole two-line block is the target, not the name alone.
    if let Some(row) = card_hit(&st.hit.home_rows) {
        st.main_index = row;
        activate_selection(st, tx);
        return;
    }

    if let Some(row) = card_hit(&st.hit.album_names)
        .or_else(|| card_hit(&st.hit.card_play))
        .or_else(|| card_hit(&st.hit.card_shuffle))
    {
        let shuffle = card_hit(&st.hit.card_shuffle).is_some();
        let play = shuffle || card_hit(&st.hit.card_play).is_some();
        st.main_index = row;
        st.last_main_click = None;
        if play {
            play_selected_album(st, tx, shuffle);
        } else {
            open_album_of_selection(st, tx);
        }
        return;
    }

    // Main list: click selects, double-click plays; clicks on the artist
    // or album cell drill into that page.
    if st.hit.main_list.contains(pos) {
        // Through the line model where the view has one (Home's entries are
        // two lines and a spacer, the artist page's album cards four); a click
        // on a heading or a spacer resolves to nothing and selects nothing.
        let line = st.main_list.offset() + (pos.y - st.hit.main_list.y) as usize;
        let Some(index) = st.hit.main_item_at(line) else {
            return;
        };
        if index < st.main_len() {
            st.main_index = index;
            if st.hit.main_like_col.contains(pos) {
                st.last_main_click = None;
                toggle_like_selection(st, tx);
                return;
            }
            if st.hit.main_artist_col.contains(pos) {
                st.last_main_click = None;
                open_artist_of_selection(st, tx);
                return;
            }
            if st.hit.main_album_col.contains(pos) {
                st.last_main_click = None;
                open_album_of_selection(st, tx);
                return;
            }
            let double = st
                .last_main_click
                .take()
                .is_some_and(|(i, at)| i == index && at.elapsed() < DOUBLE_CLICK);
            if double {
                activate_selection(st, tx);
            } else {
                st.last_main_click = Some((index, Instant::now()));
            }
        }
        return;
    }

    // A crumb of the page's trail. Clicking one is a jump rather than a run
    // of single steps — `pop_to` restores that page's own scroll and
    // selection, not whatever the pages in between were left at.
    //
    // The player draws the same trail over the page underneath it, so a crumb
    // clicked there closes the view on the way: the crumbs it draws are the
    // browse screen's, and they have to land you where they say.
    if let Some((_, target)) = st
        .hit
        .crumbs
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|(r, t)| (*r, t.clone()))
    {
        st.show_player = false;
        match target {
            CrumbTarget::Depth(depth) => {
                if st.pop_to(depth) {
                    after_pop(st, tx);
                }
            }
            CrumbTarget::Artist { id, name } => {
                let uri = format!("spotify:artist:{id}");
                navigate(st, AppCommand::OpenArtist { id, uri, name }, tx);
            }
            // Never recorded: the browse screen's head is the page you are on.
            CrumbTarget::Current => {}
        }
        return;
    }

    // The head of the player's trail: it closes the view rather than popping
    // anything, because the page it names is the one already underneath.
    if st.hit.close_player.contains(pos) {
        st.show_player = false;
        return;
    }

    if st.hit.header_play_btn.contains(pos) {
        play_current_view(st, tx, false);
        return;
    }

    if st.hit.header_shuffle_btn.contains(pos) {
        play_current_view(st, tx, true);
        return;
    }

    // Player-view queue: click selects, double-click plays.
    if st.hit.player_queue.contains(pos) {
        let index = st.queue_list.offset() + (pos.y - st.hit.player_queue.y) as usize;
        if index < st.queue_len() {
            st.queue_index = index;
            let double = st
                .last_queue_click
                .take()
                .is_some_and(|(i, at)| i == index && at.elapsed() < DOUBLE_CLICK);
            if double {
                play_from_queue(st, tx);
            } else {
                st.last_queue_click = Some((index, Instant::now()));
            }
        }
        return;
    }

    // The visualizer is a big, otherwise-inert target: clicking it toggles
    // playback, the way clicking the artwork does in a desktop player.
    if st.hit.viz.contains(pos) {
        let _ = tx.send(AppCommand::PlayPause);
        return;
    }

    // Now-playing info row: artist / album names open their pages.
    if st.hit.now_artist.contains(pos) {
        open_deck_artist(st, tx);
        return;
    }
    if st.hit.now_album.contains(pos) {
        open_deck_album(st, tx);
        return;
    }

    // The deck's context row names the queue in both views, so clicking it
    // is the mouse's `v`: it opens the player from the bar and closes it
    // again from the player.
    if st.hit.queue_name.contains(pos) {
        if st.show_player {
            st.show_player = false;
        } else {
            open_player(st, tx);
        }
        return;
    }

    // The station row, under the transport while a station is on. Before the
    // liked control below, which wears the same `★` two rows up and means the
    // record rather than the station.
    if st.hit.station_country.contains(pos) {
        open_station_country(st, tx);
        return;
    }
    if st.hit.save_station_btn.contains(pos) {
        toggle_saved_station(st, tx);
        return;
    }

    // The deck's liked control is about the playing track, which is what the
    // row it sits on is about — not the selection on the page underneath.
    if st.hit.like_btn.contains(pos) {
        toggle_like_deck(st, tx);
        return;
    }

    if st.hit.play_btn.contains(pos) {
        let _ = tx.send(AppCommand::PlayPause);
    } else if st.hit.prev_btn.contains(pos) {
        let _ = tx.send(AppCommand::Prev);
    } else if st.hit.next_btn.contains(pos) {
        let _ = tx.send(AppCommand::Next);
    } else if st.hit.shuffle_btn.contains(pos) {
        let _ = tx.send(AppCommand::ToggleShuffle);
    } else if st.hit.volume_slider.contains(pos) {
        let track = st.hit.volume_slider;
        let ratio = (pos.x - track.x) as f64 / track.width.saturating_sub(1).max(1) as f64;
        let _ = tx.send(AppCommand::SetVolume((ratio * 100.0).round() as u8));
    } else if st.hit.gauge.contains(pos)
        && st.playback.is_some()
        && let Some(track) = st.queue.as_ref().and_then(|q| q.current())
    {
        let ratio =
            (pos.x - st.hit.gauge.x) as f64 / st.hit.gauge.width.saturating_sub(1).max(1) as f64;
        let _ = tx.send(AppCommand::SeekTo(
            (ratio * track.duration_ms as f64) as u64,
        ));
    }
}

fn handle_scroll(st: &mut AppState, pos: Position, delta: i64, tx: &UnboundedSender<AppCommand>) {
    if st.hit.main_list.contains(pos) {
        scroll_main(st, delta);
    } else if st.hit.player_queue.contains(pos) {
        let max = st
            .queue_len()
            .saturating_sub(st.hit.player_queue.height as usize) as i64;
        let new = (st.queue_list.offset() as i64 + delta * SCROLL_LINES).clamp(0, max);
        *st.queue_list.offset_mut() = new as usize;
    } else if st.hit.now_playing.contains(pos) {
        // Scrolling over the now-playing bar adjusts the volume.
        let _ = tx.send(AppCommand::VolumeRel(if delta < 0 { 5 } else { -5 }));
    }
}

/// Wheel scrolling moves the view, not the selection.
///
/// Scrolls by *line*, not by row: Home's entries are two lines apiece and the
/// artist page's album cards four, and where a view's rows are one line each
/// the two numbers are the same anyway.
fn scroll_main(st: &mut AppState, delta: i64) {
    let len = st.hit.main_scroll_len(st.main_len());
    let height = st.hit.main_list.height as usize;
    let max = len.saturating_sub(height) as i64;
    let new = (st.main_list.offset() as i64 + delta * SCROLL_LINES).clamp(0, max);
    *st.main_list.offset_mut() = new as usize;
}

fn handle_search_input(
    key: KeyEvent,
    state: &Arc<RwLock<AppState>>,
    tx: &UnboundedSender<AppCommand>,
) {
    let mut st = state.write();
    match key.code {
        KeyCode::Esc => {
            st.input_mode = InputMode::Normal;
            st.input_buffer.clear();
        }
        KeyCode::Enter => {
            let query = st.input_buffer.trim().to_string();
            st.input_mode = InputMode::Normal;
            st.input_buffer.clear();
            if !query.is_empty() {
                // Belt and braces: the prompt is not drawn over the player and
                // `/` is inert there, so this mode should not be reachable
                // with the player up. `navigate` would not close it if it
                // were — nothing there knows it is open — and results the
                // player was hiding would be worse than a redundant line.
                st.show_player = false;
                // One box, one query, both catalogues. Which of them you meant
                // is a tab on the results rather than a mode on the box: the
                // old prompt pointed at Spotify or at the station directory
                // depending on the page behind it, which meant you could not
                // reach a station without first walking to Radio, and could not
                // reach Spotify from there at all.
                //
                // Before the tab reset, so the snapshot keeps the tab the page
                // you are leaving was on.
                navigate(&mut st, AppCommand::Search(query), tx);
                st.search_tab = SearchTab::Tracks;
            }
        }
        KeyCode::Backspace => {
            st.input_buffer.pop();
        }
        KeyCode::Char(c) => st.input_buffer.push(c),
        _ => {}
    }
}

fn handle_normal(key: KeyEvent, state: &Arc<RwLock<AppState>>, tx: &UnboundedSender<AppCommand>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // The player view captures navigation for its queue; transport and
    // global keys fall through to the normal handling below.
    if state.read().show_player && handle_player_key(key, state, tx) {
        return;
    }
    match key.code {
        KeyCode::Char('q') => state.write().should_quit = true,
        KeyCode::Char('?') => {
            let mut st = state.write();
            st.show_help = !st.show_help;
        }
        // An overlay first if one is up; otherwise Esc is the back key the
        // header's ← is, whichever page draws one. Every page below Home draws
        // one, so Esc means the same thing on all of them. On Home there is
        // nothing behind it and Esc does nothing.
        KeyCode::Esc => {
            let mut st = state.write();
            if st.show_help {
                st.show_help = false;
            } else {
                go_back_or_up(&mut st, tx);
            }
        }

        // Transport. Play/pause, volume and skip mean something to either
        // engine and are routed by the client — on a station the last of those
        // is the station either side of this one. Shuffle and seek have no
        // meaning for a live broadcast, so they say so rather than reaching
        // Spirc and quietly starting Spotify underneath the stream.
        KeyCode::Char(' ') => drop(tx.send(AppCommand::PlayPause)),
        KeyCode::Char('s') if state.read().radio.is_some() => {
            state
                .write()
                .toast("radio is live — there is no queue to shuffle");
        }
        KeyCode::Char('h') | KeyCode::Char('l') if state.read().radio.is_some() => {
            state
                .write()
                .toast("radio is live — there is nothing to seek");
        }
        KeyCode::Char('n') => drop(tx.send(AppCommand::Next)),
        KeyCode::Char('p') => drop(tx.send(AppCommand::Prev)),
        KeyCode::Char('h') => drop(tx.send(AppCommand::SeekRel(-5000))),
        KeyCode::Char('l') => drop(tx.send(AppCommand::SeekRel(5000))),
        KeyCode::Char('-') => drop(tx.send(AppCommand::VolumeRel(-5))),
        KeyCode::Char('=') | KeyCode::Char('+') => drop(tx.send(AppCommand::VolumeRel(5))),
        KeyCode::Char('s') => drop(tx.send(AppCommand::ToggleShuffle)),
        KeyCode::Char('R') => drop(tx.send(AppCommand::Refresh)),

        // Navigation
        KeyCode::Char('j') | KeyCode::Down => move_selection(&mut state.write(), 1),
        KeyCode::Char('k') | KeyCode::Up => move_selection(&mut state.write(), -1),
        KeyCode::Char('d') if ctrl => {
            let mut st = state.write();
            let step = half_page(&st);
            move_selection(&mut st, step);
        }
        KeyCode::Char('u') if ctrl => {
            let mut st = state.write();
            let step = half_page(&st);
            move_selection(&mut st, -step);
        }
        KeyCode::Char('g') => set_selection(&mut state.write(), 0),
        KeyCode::Char('G') => set_selection(&mut state.write(), usize::MAX),
        // There is one pane, so no key moves focus between panes.
        KeyCode::Char('H') => go_home(&mut state.write()),
        KeyCode::Char('v') => open_player(&mut state.write(), tx),

        // Tab strips (search view and artist view)
        KeyCode::Left | KeyCode::Char('[') => cycle_view_tab(&mut state.write(), -1, tx),
        KeyCode::Right | KeyCode::Char(']') => cycle_view_tab(&mut state.write(), 1, tx),

        // Sorting (track views only)
        KeyCode::Char('o') => cycle_sort(&mut state.write()),
        KeyCode::Char('O') => flip_sort(&mut state.write()),

        KeyCode::Char('/') => {
            let mut st = state.write();
            st.input_mode = InputMode::Search;
            st.input_buffer.clear();
        }
        KeyCode::Enter => activate_selection(&mut state.write(), tx),
        KeyCode::Char('a') => queue_selection(&mut state.write(), tx),
        KeyCode::Char('L') => toggle_like_selection(&mut state.write(), tx),

        KeyCode::Char('b') => open_album_of_selection(&mut state.write(), tx),
        KeyCode::Char('B') => open_artist_of_selection(&mut state.write(), tx),
        KeyCode::Char('x') => play_without_opening(&mut state.write(), tx),
        KeyCode::Backspace => go_back(&mut state.write(), tx),
        _ => {}
    }
}

/// `H`, and the `♫ spot` mark: go to Home.
///
/// Home is the bottom of the stack rather than one more page on top of it, so
/// this clears the history instead of pushing to it — arriving somewhere by
/// skipping every page in between leaves no honest "page I came from" to go
/// back to. It also closes the player, which would otherwise hide the view it
/// just changed.
fn go_home(st: &mut AppState) {
    st.show_player = false;
    st.view_stack.clear();
    st.main = MainView::Home;
    st.main_to_top();
}

/// `v`: open the player view. The queue is always present and always
/// correct — spot wrote it on the play — so there is nothing to fetch.
fn open_player(st: &mut AppState, _tx: &UnboundedSender<AppCommand>) {
    st.show_player = true;
}

/// Keys captured while the player view is shown. Returns false for keys
/// that should fall through to the normal handler (transport, help, quit).
fn handle_player_key(
    key: KeyEvent,
    state: &Arc<RwLock<AppState>>,
    tx: &UnboundedSender<AppCommand>,
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('v') => state.write().show_player = false,
        KeyCode::Esc => {
            // Close the help overlay first if it is on top.
            let mut st = state.write();
            if st.show_help {
                st.show_help = false;
            } else {
                st.show_player = false;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => queue_move(&mut state.write(), 1),
        KeyCode::Char('k') | KeyCode::Up => queue_move(&mut state.write(), -1),
        KeyCode::Char('d') if ctrl => {
            let mut st = state.write();
            let step = queue_half_page(&st);
            queue_move(&mut st, step);
        }
        KeyCode::Char('u') if ctrl => {
            let mut st = state.write();
            let step = queue_half_page(&st);
            queue_move(&mut st, -step);
        }
        KeyCode::Char('g') => queue_set(&mut state.write(), 0),
        KeyCode::Char('G') => queue_set(&mut state.write(), usize::MAX),
        KeyCode::Enter => play_from_queue(&mut state.write(), tx),
        // Browse-only keys are inert here rather than acting on the
        // invisible panes underneath. `/` is one of them again: the header
        // draws no prompt over the player, so the key would put you in a mode
        // with nothing on screen to show what you were typing.
        KeyCode::Char('/' | '1' | '2' | 'b' | 'B' | 'o' | 'O' | 'a' | 'x' | '[' | ']')
        | KeyCode::Tab
        | KeyCode::BackTab
        | KeyCode::Backspace
        | KeyCode::Left
        | KeyCode::Right => {}
        _ => return false,
    }
    true
}

fn queue_move(st: &mut AppState, delta: i64) {
    let len = st.queue_len();
    if len == 0 {
        return;
    }
    st.queue_index = (st.queue_index as i64 + delta).clamp(0, len as i64 - 1) as usize;
    queue_snap(st);
}

fn queue_set(st: &mut AppState, index: usize) {
    let len = st.queue_len();
    if len == 0 {
        return;
    }
    st.queue_index = index.min(len - 1);
    queue_snap(st);
}

/// Half the queue list's visible height (from last frame's hit rect),
/// falling back to 10 before the first draw.
fn queue_half_page(st: &AppState) -> i64 {
    let height = st.hit.player_queue.height as i64;
    if height == 0 { 10 } else { (height / 2).max(1) }
}

/// Bring the queue view back to its selection after a keyboard move.
fn queue_snap(st: &mut AppState) {
    let height = st.hit.player_queue.height as usize;
    if height == 0 {
        return;
    }
    let index = st.queue_index;
    let list = &mut st.queue_list;
    if index < list.offset() {
        *list.offset_mut() = index;
    } else if index >= list.offset() + height {
        *list.offset_mut() = index + 1 - height;
    }
}

/// Enter / double-click in the queue: play the selected row. The queue is
/// the play order, so this is one instant command with no API behind it.
fn play_from_queue(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    if st.queue_index < st.queue_len() {
        let _ = tx.send(AppCommand::JumpTo(st.queue_index));
    }
}

/// Backspace: pop the view stack. A restored track view that was still
/// loading has a stale generation (its fetch task already exited), so
/// re-issue the load; the cache makes that instant for finished fetches.
/// The header's ← control, and Esc on an album page: whatever
/// [`AppState::back_target`] resolved to, so the label and the action agree.
fn go_back_or_up(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    match st.back_target() {
        Some(BackTarget::History(_)) => go_back(st, tx),
        // Nothing behind an album page: go up to its artist instead.
        Some(BackTarget::Artist { id, name }) => {
            let uri = format!("spotify:artist:{id}");
            navigate(st, AppCommand::OpenArtist { id, uri, name }, tx);
        }
        None => {}
    }
}

/// The page a command opens, or `None` for a command that does not navigate.
///
/// The whole point of resolving this here rather than after the fetch: every
/// command that opens a page carries the id it opens, so a move can be judged
/// against the path at the moment of the click — long before the client has
/// anything to install.
fn target_key(cmd: &AppCommand) -> Option<ViewKey> {
    match cmd {
        AppCommand::LoadLikedSongs => Some(ViewKey::Tracks(state::liked_key())),
        AppCommand::LoadPlaylistTracks { playlist_id } => {
            Some(ViewKey::Tracks(state::playlist_key(playlist_id)))
        }
        AppCommand::OpenAlbum { id, .. } => Some(ViewKey::Tracks(state::album_key(id))),
        AppCommand::OpenArtist { id, .. } => Some(ViewKey::Artist(id.clone())),
        AppCommand::Search(_) => Some(ViewKey::Search),
        AppCommand::LoadRadio { scope } => Some(ViewKey::Radio(state::radio_key(scope))),
        _ => None,
    }
}

/// Make room on the path for a move to `target`, and say whether there is
/// still a page left to open.
///
/// `false` means the move resolved to walking *back* — the caller must not
/// send its command, because the page is already on screen or has just been
/// restored from the stack.
///
/// This is what keeps the path a path. Stacking a fresh copy for a page you
/// can reach from two directions grows the history by two a trip — bouncing
/// between an album and its artist leaves `Esc` to walk the whole loop back
/// out. Revisiting shortens the path instead.
///
/// One guard covers every command and every entry point, because the same case
/// arrives many ways: an album page shows an Album column naming the album you
/// are already on, and an artist page's top track credits the artist whose
/// page it is, so `B` there resolves to the page on screen.
fn make_way(st: &mut AppState, target: Option<ViewKey>, tx: &UnboundedSender<AppCommand>) -> bool {
    let Some(target) = target else {
        st.push_view();
        return true;
    };
    let here = state::view_key(&st.main).as_ref() == Some(&target);
    let below = st
        .view_stack
        .iter()
        .position(|snap| state::view_key(&snap.view).as_ref() == Some(&target));

    // Search is one slot: the new query takes the old one's place wherever it
    // sat. Truncating rather than popping, because the *new* query has to win
    // — restoring the old results would leave the command unsent.
    if target == ViewKey::Search {
        match below {
            Some(depth) => st.view_stack.truncate(depth),
            None if !here => st.push_view(),
            None => {}
        }
        return true;
    }

    if here {
        return false;
    }
    if let Some(depth) = below {
        if st.pop_to(depth) {
            after_pop(st, tx);
        }
        return false;
    }
    st.push_view();
    true
}

/// Open the page `cmd` leads to, walking back to it instead when it is already
/// on the path. Every navigation site goes through here.
fn navigate(st: &mut AppState, cmd: AppCommand, tx: &UnboundedSender<AppCommand>) {
    if make_way(st, target_key(&cmd), tx) {
        let _ = tx.send(cmd);
    }
}

fn go_back(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    if !st.pop_view() {
        return;
    }
    after_pop(st, tx);
}

/// Re-fetch what a restored view needs but did not bring with it. Shared by
/// the one-step back and by a crumb click, which pops several at once.
fn after_pop(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    // The restored view brings its own header back, but the decoded sleeve in
    // `view_cover` belongs to whatever was opened *after* it. Re-request the
    // one this page wants; a cache hit installs it on the spot, and a page
    // with no artwork clears the slot rather than inheriting.
    let cover_url = match &st.main {
        MainView::Tracks(list) => list.header.cover_url.clone(),
        _ => None,
    };
    let _ = tx.send(AppCommand::LoadViewCover { cover_url });
    match &st.main {
        MainView::Tracks(list) if list.loading => match list.cache_key.as_deref() {
            Some("liked") => drop(tx.send(AppCommand::LoadLikedSongs)),
            Some(key) => {
                if let Some(id) = key.strip_prefix("playlist:") {
                    let _ = tx.send(AppCommand::LoadPlaylistTracks {
                        playlist_id: id.to_string(),
                    });
                }
            }
            None => {}
        },
        MainView::Artist(v) if v.loading => {
            let _ = tx.send(AppCommand::OpenArtist {
                id: v.id.clone(),
                uri: v.uri.clone(),
                name: v.name.clone(),
            });
        }
        // A search frozen onto the stack mid-flight: the task that would have
        // filled it checked the view it was about to write to, found the page
        // the user had moved on to, and exited. Nothing is coming, so without
        // this the restored page would sit on "searching…" for ever. One
        // command rather than two — the query is one thing, and re-asking the
        // half that already answered costs a round trip against a page that
        // would otherwise never finish.
        MainView::Search(r) if r.stations_loading => {
            let _ = tx.send(AppCommand::Search(r.query.clone()));
        }
        _ => {}
    }
}

/// Half the pane's visible height (from last frame's hit rect), falling back
/// to 10 before the first draw.
fn half_page(st: &AppState) -> i64 {
    let height = main_rows_on_screen(st) as i64;
    if height == 0 { 10 } else { (height / 2).max(1) }
}

/// Rows the main pane is showing, which is its height in lines except where
/// the view has a line model — Home's entries are two lines and a spacer,
/// an artist page's album cards four apiece, and a half page there is half the
/// *rows*, not half the lines.
fn main_rows_on_screen(st: &AppState) -> usize {
    let height = st.hit.main_list.height as usize;
    if st.hit.main_lines.is_empty() {
        return height;
    }
    let mut rows = 0;
    let mut last = None;
    for row in st
        .hit
        .main_lines
        .iter()
        .skip(st.main_list.offset())
        .take(height)
    {
        if row.is_some() && *row != last {
            rows += 1;
            last = *row;
        }
    }
    rows
}

fn move_selection(st: &mut AppState, delta: i64) {
    let len = st.main_len();
    if len == 0 {
        return;
    }
    st.main_index = (st.main_index as i64 + delta).clamp(0, len as i64 - 1) as usize;
    snap_to_selection(st);
}

fn set_selection(st: &mut AppState, index: usize) {
    let len = st.main_len();
    if len == 0 {
        return;
    }
    st.main_index = index.min(len - 1);
    snap_to_selection(st);
}

/// Bring the view back to the selection after a keyboard move, in case the
/// wheel had scrolled it out of sight.
///
/// Snaps a *line span* rather than a row index: most rows are one line, so
/// their span is trivial, but a Home entry is two and an album card four, and
/// a heading above one has to come into view with it.
fn snap_to_selection(st: &mut AppState) {
    let height = st.hit.main_list.height as usize;
    let span = st.hit.main_span(st.main_index);
    let (Some((start, end)), true) = (span, height > 0) else {
        return;
    };
    let list = &mut st.main_list;
    if start < list.offset() {
        *list.offset_mut() = start;
    } else if end > list.offset() + height {
        // A span taller than the viewport still shows its top.
        *list.offset_mut() = (end - height).min(start);
    }
}

/// `o`: cycle the track view's sort column (resets to ascending on change).
fn cycle_sort(st: &mut AppState) {
    let MainView::Tracks(list) = &mut st.main else {
        return;
    };
    list.sort.key = match list.sort.key {
        SortKey::Position => SortKey::Title,
        SortKey::Title => SortKey::Artist,
        SortKey::Artist => SortKey::Album,
        SortKey::Album => SortKey::Duration,
        SortKey::Duration => SortKey::Position,
    };
    list.sort.ascending = true;
    st.resort_main();
    snap_to_selection(st);
}

/// `O`: flip the sort direction (meaningless for Position order).
fn flip_sort(st: &mut AppState) {
    let MainView::Tracks(list) = &mut st.main else {
        return;
    };
    if list.sort.key == SortKey::Position {
        return;
    }
    list.sort.ascending = !list.sort.ascending;
    st.resort_main();
    snap_to_selection(st);
}

/// ←/→: switch tabs on the three tabbed views.
///
/// Search's tabs are five cuts of one query already answered, so switching is
/// free — Stations came from a second catalogue, but it was asked at the same
/// moment and is in hand by the time you reach it. The artist page's album
/// groups are the same kind of thing: one fetch brought all four back. Radio's
/// are four different queries, so switching *navigates* — the new tab is its
/// own page, and Esc walks back to the one you left.
fn cycle_view_tab(st: &mut AppState, delta: i64, tx: &UnboundedSender<AppCommand>) {
    if let MainView::Artist(v) = &mut st.main {
        // Only the groups this artist has records in, so ←/→ never lands on
        // an empty page.
        let tabs = v.tabs();
        if tabs.len() > 1 {
            let pos = tabs.iter().position(|t| *t == v.tab).unwrap_or(0) as i64;
            let n = tabs.len() as i64;
            let tab = tabs[((pos + delta).rem_euclid(n)) as usize];
            v.set_tab(tab);
            st.main_to_top();
            let _ = tx.send(AppCommand::LoadArtistArt);
        }
        return;
    }
    match &st.main {
        MainView::Search(_) => {
            let pos = SearchTab::ALL
                .iter()
                .position(|t| *t == st.search_tab)
                .unwrap_or(0) as i64;
            let n = SearchTab::ALL.len() as i64;
            st.search_tab = SearchTab::ALL[((pos + delta).rem_euclid(n)) as usize];
            st.main_to_top();
        }
        MainView::Radio(v) => {
            let current = v.scope.tab();
            let pos = RadioTab::ALL
                .iter()
                .position(|t| *t == current)
                .unwrap_or(0) as i64;
            let n = RadioTab::ALL.len() as i64;
            let tab = RadioTab::ALL[((pos + delta).rem_euclid(n)) as usize];
            open_radio_tab(st, tab, tx);
        }
        _ => {}
    }
}

/// Open a radio tab, from the strip or from ←/→.
fn open_radio_tab(st: &mut AppState, tab: RadioTab, tx: &UnboundedSender<AppCommand>) {
    navigate(st, AppCommand::LoadRadio { scope: tab.scope() }, tx);
}

/// Enter / click: drill into the selected row, or play it.
fn activate_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    // Home and Playlists are navigation rather than playback, and both push
    // the page they leave so the pill on the next one leads back to it.
    match &st.main {
        MainView::Home => {
            let Some(item) = st.home_items().get(st.main_index).copied() else {
                return;
            };
            match item {
                HomeItem::LikedSongs => navigate(st, AppCommand::LoadLikedSongs, tx),
                HomeItem::DiscoverWeekly => {
                    // Resolved before the view is pushed: a row that leads
                    // nowhere must not leave a crumb on the page you never
                    // left.
                    let Some(id) = st.discover_weekly().map(|p| p.id.clone()) else {
                        return;
                    };
                    navigate(st, AppCommand::LoadPlaylistTracks { playlist_id: id }, tx);
                }
                // The only page installed here rather than fetched, so it
                // takes `make_way` directly instead of going through a
                // command that does not exist.
                HomeItem::Playlists => {
                    if make_way(st, Some(ViewKey::Playlists), tx) {
                        st.main = MainView::Playlists;
                        st.main_to_top();
                    }
                }
                // Straight to the chart rather than to your saved stations:
                // the list is empty until you have kept something, and a
                // destination that opens onto nothing is a dead end.
                HomeItem::Radio => navigate(
                    st,
                    AppCommand::LoadRadio {
                        scope: RadioScope::Popular,
                    },
                    tx,
                ),
            }
            return;
        }
        MainView::Playlists => {
            let Some(id) = st.playlists.get(st.main_index).map(|p| p.id.clone()) else {
                return;
            };
            navigate(st, AppCommand::LoadPlaylistTracks { playlist_id: id }, tx);
            return;
        }
        // Radio is half navigation and half playback: a country or genre is a
        // door, a station is a thing to play. Neither goes through the Spotify
        // path below, so both are resolved here.
        MainView::Radio(v) => {
            match v.rows.get(st.main_index) {
                Some(RadioRow::Facet { key, .. }) => {
                    let scope = match v.scope.tab() {
                        RadioTab::Genres => RadioScope::Genre(key.clone()),
                        _ => RadioScope::Country(key.clone()),
                    };
                    navigate(st, AppCommand::LoadRadio { scope }, tx);
                }
                Some(RadioRow::Station(s)) => play_station(st, s.clone(), tx),
                None => {}
            }
            return;
        }
        // A station in a search result is the same thing as a station on a
        // radio page, and is played the same way. It has to be resolved up
        // here rather than in the `cmd` match below: that one holds `st`
        // immutably for the length of the match, and `play_station` needs it
        // mutably.
        MainView::Search(results) if st.search_tab == SearchTab::Stations => {
            let Some(station) = results.stations.get(st.main_index).cloned() else {
                return;
            };
            play_station(st, station, tx);
            return;
        }
        _ => {}
    }

    let index = st.main_index;
    let cmd = match &st.main {
        // Handled above; the match must still be total.
        MainView::Home | MainView::Playlists | MainView::Radio(_) => None,
        // Every play carries its tracks: what you see is the play order the
        // queue is built from. The list's cache key rides along only in its
        // natural order — a sorted view is a snapshot of what was on screen,
        // and later pages must not append to it out of order.
        MainView::Tracks(list) => {
            if list.display.get(index).is_none() {
                return;
            }
            Some(play_list(list, index, false))
        }
        MainView::Artist(v) => match v.row(index) {
            // The visible top-tracks list, played directly.
            Some(ArtistRow::Track(_)) => Some(AppCommand::Play {
                tracks: display_tracks(&v.top),
                start: index,
                name: v.name.clone(),
                key: None,
                loading: false,
                shuffle: false,
            }),
            // A card's row opens its album; its own ▶ play is what starts the
            // record without leaving the page.
            Some(ArtistRow::Album(a)) => Some(open_album_item(a)),
            None => None,
        },
        MainView::Search(results) => match st.search_tab {
            SearchTab::Tracks => results.tracks.get(index).map(|_| AppCommand::Play {
                tracks: results.tracks.clone(),
                start: index,
                name: "Search results".to_string(),
                key: None,
                loading: false,
                shuffle: false,
            }),
            SearchTab::Albums => results.albums.get(index).map(open_album_item),
            SearchTab::Artists => results.artists.get(index).map(|a| AppCommand::OpenArtist {
                id: a.id.clone(),
                uri: a.uri.clone(),
                name: a.name.clone(),
            }),
            SearchTab::Playlists => results
                .playlists
                .get(index)
                .map(|p| AppCommand::PlayFetched {
                    source: FetchSource::Playlist { id: p.id.clone() },
                    name: p.name.clone(),
                    shuffle: false,
                }),
            // Resolved above, before this match takes its borrow.
            SearchTab::Stations => None,
        },
    };
    let Some(cmd) = cmd else { return };
    // Drill-ins replace the view; keep a way back. Everything else here is
    // playback and leaves the path alone.
    if matches!(
        cmd,
        AppCommand::OpenAlbum { .. } | AppCommand::OpenArtist { .. }
    ) {
        navigate(st, cmd, tx);
    } else {
        let _ = tx.send(cmd);
    }
}

/// The `Play` a track list's row `start` sends: the display order as the
/// play order, with the source key attached only when the rows are in fetch
/// order — the one order later pages can honestly extend.
fn play_list(list: &TrackList, start: usize, shuffle: bool) -> AppCommand {
    let natural = list.sort.key == SortKey::Position;
    AppCommand::Play {
        tracks: display_tracks(list),
        start,
        name: list.header.name.clone(),
        key: list.cache_key.clone().filter(|_| natural),
        loading: natural && list.loading,
        shuffle,
    }
}

/// The tracks of a list in display order, cloned.
fn display_tracks(list: &TrackList) -> Vec<Track> {
    list.display
        .iter()
        .map(|&i| list.tracks[i].clone())
        .collect()
}

/// Start playback of whatever the main pane is showing, from the top
/// (header ▶ Play, and the main-pane half of `x`).
/// Start a station, saying so before the connection has had time to happen.
///
/// The toast is not decoration: connecting takes a second or two and prefetches
/// five more, and until audio arrives nothing else on the screen would have
/// changed. An HLS station is refused here rather than in the client, because
/// this is the only place that can say so while the row is still under the
/// cursor.
/// The station under the cursor, wherever stations are listed.
///
/// Two pages list them now — a radio page, and the Stations tab of a search —
/// and `Enter`, `x` and `L` all mean the same thing on both. One definition, so
/// the three keys cannot come to disagree about which row they are on.
/// Whether the rows under the cursor are stations at all.
///
/// Distinct from [`selected_station`] returning `Some`: a radio page's facet
/// rows, and a Stations tab the directory has not answered yet, are station
/// pages with no station under the cursor. Keys that act on a station must stop
/// there rather than fall through to whatever a Spotify page would have done.
fn lists_stations(st: &AppState) -> bool {
    match &st.main {
        MainView::Radio(_) => true,
        MainView::Search(_) => st.search_tab == SearchTab::Stations,
        _ => false,
    }
}

fn selected_station(st: &AppState) -> Option<&Station> {
    match &st.main {
        MainView::Radio(v) => match v.rows.get(st.main_index) {
            Some(RadioRow::Station(s)) => Some(s),
            // A country or genre row is a door, not a station.
            _ => None,
        },
        MainView::Search(r) if st.search_tab == SearchTab::Stations => {
            r.stations.get(st.main_index)
        }
        _ => None,
    }
}

fn play_station(st: &mut AppState, station: Station, tx: &UnboundedSender<AppCommand>) {
    // Enter on the station already playing stops it, which is the only way to
    // stop radio without starting something else. It needs no key of its own:
    // pressing play on the thing that is playing is the same gesture as
    // pressing pause, and this is the row that says which station that is.
    //
    // Unless it is not playing at all. A station that would not come up stays
    // on the deck saying so, and Enter on its row is the same ask as `▶ play`
    // over it: try again. Stopping what never started says nothing.
    if st
        .radio
        .as_ref()
        .is_some_and(|r| r.station.uuid == station.uuid && !r.failed())
    {
        st.toast(format!("stopped {}", station.name));
        let _ = tx.send(AppCommand::StopRadio);
        return;
    }
    if station.hls {
        st.toast("that station streams over HLS, which spot can't play yet");
        return;
    }
    st.toast(format!("tuning in to {}…", station.name));
    let _ = tx.send(AppCommand::PlayStation {
        station: Box::new(station),
        attempt: 0,
    });
}

fn play_current_view(st: &mut AppState, tx: &UnboundedSender<AppCommand>, shuffle: bool) {
    let cmd = match &st.main {
        MainView::Tracks(list) if !list.display.is_empty() => Some(play_list(list, 0, shuffle)),
        MainView::Tracks(_) => None,
        // The page's top tracks, played as the list they are: spot owns the
        // queue, and the top tracks are the page's own answer to "play this
        // artist".
        MainView::Artist(v) if !v.top.display.is_empty() => Some(AppCommand::Play {
            tracks: display_tracks(&v.top),
            start: 0,
            name: v.name.clone(),
            key: None,
            loading: false,
            shuffle,
        }),
        _ => None,
    };
    if let Some(cmd) = cmd {
        let _ = tx.send(cmd);
    }
}

/// `x`: play without opening — the selected playlist on a page that lists
/// playlists, or the current view's list anywhere else.
fn play_without_opening(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    // The two playlist branches play a source whose rows are not in hand:
    // the client fetches the pages and fills the queue as they land. Liked
    // Songs is the one destination with no such source shortcut worth the
    // trip — opening it is the way in, as it always has been.
    let playlist = match &st.main {
        MainView::Playlists => st
            .playlists
            .get(st.main_index)
            .map(|p| (p.id.clone(), p.name.clone())),
        MainView::Home => match st.home_items().get(st.main_index) {
            Some(HomeItem::LikedSongs) => {
                st.toast("open liked songs to play them");
                return;
            }
            Some(HomeItem::DiscoverWeekly) => {
                st.discover_weekly().map(|p| (p.id.clone(), p.name.clone()))
            }
            _ => None,
        },
        // A station is played, never opened, so `x` and Enter are the same
        // gesture here — on a radio page and on a search's Stations tab alike.
        // On a facet row there is nothing to play.
        MainView::Radio(_) => {
            if let Some(station) = selected_station(st).cloned() {
                play_station(st, station, tx);
            }
            return;
        }
        // Only on the Stations tab; the other four fall through to the
        // current view's list below, as they always have.
        MainView::Search(_) if st.search_tab == SearchTab::Stations => {
            if let Some(station) = selected_station(st).cloned() {
                play_station(st, station, tx);
            }
            return;
        }
        _ => {
            play_current_view(st, tx, false);
            return;
        }
    };
    if let Some((id, name)) = playlist {
        let _ = tx.send(AppCommand::PlayFetched {
            source: FetchSource::Playlist { id },
            name,
            shuffle: false,
        });
    }
}

/// `b`: browse into the selected item's album (track lists, search tracks,
/// or a search-album result).
fn open_album_of_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let from_track = |t: &crate::app::state::Track| {
        t.album_id.as_ref().map(|id| AppCommand::OpenAlbum {
            id: id.clone(),
            name: t.album.clone(),
            artists: t.artists.clone(),
            year: t.release_year.clone(),
            cover_url: t.cover_url.clone(),
        })
    };
    let cmd = match &st.main {
        MainView::Tracks(list) => list
            .display
            .get(st.main_index)
            .and_then(|&i| from_track(&list.tracks[i])),
        // The artist page names albums in both of its sections: the Album
        // cell of a top track, and the cards under them.
        MainView::Artist(v) => match v.row(st.main_index) {
            Some(ArtistRow::Track(t)) => from_track(t),
            Some(ArtistRow::Album(a)) => Some(open_album_item(a)),
            None => None,
        },
        MainView::Search(results) => match st.search_tab {
            SearchTab::Tracks => results.tracks.get(st.main_index).and_then(from_track),
            SearchTab::Albums => results.albums.get(st.main_index).map(open_album_item),
            _ => None,
        },
        _ => None,
    };
    let Some(cmd) = cmd else {
        // No track row under the cursor. Under a station that means the deck,
        // which is the one thing every screen has in common while radio plays
        // — the same fallback `L` has always had, for the same reason. Left
        // alone off radio: `b` deliberately does not reach for the playing
        // track on a page that simply has no albums on it.
        return radio_deck_fallback(st, tx, open_deck_album);
    };
    // An album page still shows an Album column, which names the album you are
    // already on, so this is one of the places a re-open resolves to the page
    // on screen. `navigate` catches that wherever it happens — including the
    // Album column of a *different* page that leads back to one already on the
    // path.
    navigate(st, cmd, tx);
}

/// Run a deck control in place of a selection key, but only under a station.
///
/// `b` and `B` mean "the row under the cursor" everywhere they have something
/// to point at. A radio page has no track rows at all, so under a station they
/// would otherwise be dead keys on the one screen where the deck is the only
/// thing naming a record.
fn radio_deck_fallback(
    st: &mut AppState,
    tx: &UnboundedSender<AppCommand>,
    open: fn(&mut AppState, &UnboundedSender<AppCommand>),
) {
    if st.radio.is_some() {
        open(st, tx);
    }
}

/// Open the album of whatever the deck is about, from the sleeve or from the
/// album name in the now-playing bar / player masthead. One resolution for
/// both, so the two controls that mean the same thing cannot drift apart.
///
/// "Whatever the deck is about" and not "the playing track": while a station is
/// on, `playback` still holds the last Spotify track and it is not what is
/// making any sound. [`AppState::deck_track`] is what keeps every deck control
/// pointing at the same record.
fn open_deck_album(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(cmd) = st.deck_track().and_then(|t| t.open_album()) else {
        return radio_has_no_track(st, "album");
    };
    // The page opens in the main view, which the player would hide. Before
    // `navigate`, and unconditionally: clicking the sleeve while already on
    // that album's page leaves the path alone but must still close the player.
    st.show_player = false;
    navigate(st, cmd, tx);
}

/// Open the artist of whatever the deck is about. Same rule as
/// [`open_deck_album`], and the same reason it does not read `playback`.
fn open_deck_artist(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(cmd) = st.deck_track().and_then(|t| t.open_artist()) else {
        return radio_has_no_track(st, "artist");
    };
    // Before `navigate`, and unconditionally: clicking this while already on
    // the artist's page is a no-op for the path but must still get you out of
    // the player and onto the page it names.
    st.show_player = false;
    navigate(st, cmd, tx);
}

/// The station row's country: open the directory's page for it.
///
/// Same shape as [`open_deck_album`] and [`open_deck_artist`], and the same
/// reason it closes the player first — the page opens in the main view, which
/// the player would otherwise sit on top of.
///
/// The directory is queried by ISO code, not by name (see
/// [`crate::radio::api::RadioApi::by_country`]), so a station the directory gave
/// no code for has nowhere to lead. The row draws the name inert in that case,
/// which means this cannot normally be reached; the guard is here because a
/// silent `unwrap` on a field the network fills is not a guard.
fn open_station_country(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(code) = st
        .radio
        .as_ref()
        .map(|r| r.station.countrycode.clone())
        .filter(|c| !c.is_empty())
    else {
        return;
    };
    st.show_player = false;
    navigate(
        st,
        AppCommand::LoadRadio {
            scope: RadioScope::Country(code),
        },
        tx,
    );
}

/// The station row's `★`: keep the playing station, or drop it.
///
/// The same command the `L` key sends from a station row in the directory
/// ([`toggle_like_selection`]) — the difference is only which station is meant.
/// Here it is the one making the sound, which is what every other control on
/// the deck is about.
fn toggle_saved_station(st: &AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(station) = st.radio.as_ref().map(|r| r.station.clone()) else {
        return;
    };
    let _ = tx.send(AppCommand::ToggleSavedStation(Box::new(station)));
}

/// Say why a deck control did nothing, when the reason is radio.
///
/// Silence on a keypress is out of character here — `n`, `p`, `s`, `h` and `l`
/// all explain themselves under radio rather than appearing broken. Off radio
/// there is nothing to say: a Spotify track always has an album and an artist,
/// so the only way to arrive with nothing is to have nothing playing at all,
/// which the deck is already saying in as many words.
fn radio_has_no_track(st: &mut AppState, what: &str) {
    let Some(r) = st.radio.as_ref() else { return };
    let msg = match &r.matched {
        RadioMatch::Searching => "still looking that one up".to_string(),
        RadioMatch::Unmatched => format!("that track is not on Spotify — no {what} to open"),
        RadioMatch::None | RadioMatch::Matched(_) => {
            "radio is live — this station is not saying what it is playing".to_string()
        }
    };
    st.toast(msg);
}

/// The command that opens an album row's page, artwork and all.
fn open_album_item(a: &crate::app::state::AlbumItem) -> AppCommand {
    AppCommand::OpenAlbum {
        id: a.id.clone(),
        name: a.name.clone(),
        artists: a.artists.clone(),
        year: a.release_year.clone(),
        cover_url: a.cover_url.clone(),
    }
}

/// `B`: browse into the selected item's artist (first credited artist for
/// tracks, or a search-artist result).
fn open_artist_of_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let from_track = |t: &crate::app::state::Track| {
        t.artist_id.as_ref().map(|id| AppCommand::OpenArtist {
            id: id.clone(),
            uri: format!("spotify:artist:{id}"),
            name: crate::app::state::first_artist(&t.artists),
        })
    };
    let cmd = match &st.main {
        MainView::Tracks(list) => list
            .display
            .get(st.main_index)
            .and_then(|&i| from_track(&list.tracks[i])),
        MainView::Artist(v) => match v.row(st.main_index) {
            Some(ArtistRow::Track(t)) => from_track(t),
            _ => None,
        },
        MainView::Search(results) => match st.search_tab {
            SearchTab::Tracks => results.tracks.get(st.main_index).and_then(from_track),
            SearchTab::Artists => {
                results
                    .artists
                    .get(st.main_index)
                    .map(|a| AppCommand::OpenArtist {
                        id: a.id.clone(),
                        uri: a.uri.clone(),
                        name: a.name.clone(),
                    })
            }
            _ => None,
        },
        _ => None,
    };
    // On an artist page this resolves to the page's *own* artist — the first
    // name credited on a top track is the artist whose page it is — so
    // `navigate` sees the target is the page on screen and does nothing.
    match cmd {
        Some(cmd) => navigate(st, cmd, tx),
        // See `open_album_of_selection`: the deck is the fallback, under a
        // station only.
        None => radio_deck_fallback(st, tx, open_deck_artist),
    }
}

/// `a`: put the selected track next in the queue (track lists only).
fn queue_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let track = match &st.main {
        MainView::Tracks(list) => list
            .display
            .get(st.main_index)
            .map(|&i| list.tracks[i].clone()),
        MainView::Search(results) if st.search_tab == SearchTab::Tracks => {
            results.tracks.get(st.main_index).cloned()
        }
        MainView::Artist(v) => match v.row(st.main_index) {
            Some(ArtistRow::Track(t)) => Some(t.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(track) = track {
        let _ = tx.send(AppCommand::QueueInsertNext(track));
    }
}

/// `L`, and the liked column: like or unlike the track the view is about.
///
/// Which track that is depends on where you are. In a track list it is the
/// selected row. In the player it is the *playing* track — the whole screen is
/// about that record, and the queue underneath is a list you press Enter on
/// rather than one the view is reporting. Everywhere else there is no track
/// under the cursor to mean, so it falls back to what is playing, which is the
/// one track every screen has in common (the deck names it at the bottom of
/// all of them).
fn toggle_like_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    if st.show_player {
        return toggle_like_deck(st, tx);
    }
    // On a station row `L` keeps the station. Same key, same gesture — the
    // difference is only that a station is kept in a file of spot's own,
    // because the directory has no account to keep it in.
    //
    // The guard is `lists_stations` rather than `selected_station(..).is_some()`
    // on purpose. A radio page's facet rows, and an empty Stations tab, have no
    // station under the cursor but are still station pages: falling through
    // from there would drop into the `None` arm below and quietly like the
    // *playing Spotify track*, which is not what anyone pressed `L` for.
    if lists_stations(st) {
        if let Some(station) = selected_station(st).cloned() {
            let _ = tx.send(AppCommand::ToggleSavedStation(Box::new(station)));
        }
        return;
    }
    let uri = match &st.main {
        MainView::Tracks(list) => list
            .display
            .get(st.main_index)
            .map(|&i| list.tracks[i].uri.clone()),
        MainView::Search(results) if st.search_tab == SearchTab::Tracks => {
            results.tracks.get(st.main_index).map(|t| t.uri.clone())
        }
        MainView::Artist(v) => match v.row(st.main_index) {
            Some(ArtistRow::Track(t)) => Some(t.uri.clone()),
            _ => None,
        },
        _ => None,
    };
    match uri {
        Some(uri) => send_like(st, uri, tx),
        None => toggle_like_deck(st, tx),
    }
}

/// The deck's liked control, and `L` wherever there is no track row to mean.
///
/// Reads the deck's subject rather than `playback` for the reason given on
/// [`open_deck_album`]: under a station, `playback` is a record that stopped
/// playing when the stream started.
fn toggle_like_deck(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(uri) = st.deck_track().map(|t| t.uri.clone()) else {
        return radio_has_no_track(st, "track");
    };
    send_like(st, uri, tx);
}

/// Flip whatever is known about `uri` and ask the client to make it so.
///
/// An unknown track is treated as unliked, so the first press saves it: the
/// saved check runs for every list we load and for the playing track, so by
/// the time there is a mark to press the answer is nearly always in hand.
fn send_like(st: &AppState, uri: String, tx: &UnboundedSender<AppCommand>) {
    let liked = !st.liked.get(&uri).copied().unwrap_or(false);
    let _ = tx.send(AppCommand::SetLiked { uri, liked });
}

/// Play the selected album straight from its card, without opening it — the
/// card's own ▶ play, and the artist page's answer to `x`.
fn play_selected_album(st: &mut AppState, tx: &UnboundedSender<AppCommand>, shuffle: bool) {
    let MainView::Artist(v) = &st.main else {
        return;
    };
    let Some(ArtistRow::Album(a)) = v.row(st.main_index) else {
        return;
    };
    // The card holds no rows, so the client fetches them: the first page
    // starts the record, and the rest stream into the queue behind it.
    let _ = tx.send(AppCommand::PlayFetched {
        source: FetchSource::Album {
            id: a.id.clone(),
            year: a.release_year.clone(),
        },
        name: a.name.clone(),
        shuffle,
    });
}

#[cfg(test)]
mod tests {

    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    use super::*;
    use crate::app::state::{AlbumItem, ArtistView, Playlist, TrackList, TrackListKind};

    fn track(name: &str, album_id: Option<&str>) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            artists: "Muse".into(),
            album: "Black Holes".into(),
            release_year: "2006".into(),
            duration_ms: 1000,
            track_number: 1,
            album_id: album_id.map(Into::into),
            artist_id: Some("r1".into()),
            cover_url: Some("https://i.scdn.co/image/abc".into()),
        }
    }

    fn artist_state() -> AppState {
        let mut st = AppState::new();
        let mut top = TrackList::new("Muse", "top tracks", None);
        top.append(vec![track("Uprising", Some("a1"))]);
        st.main = MainView::Artist(ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Muse".into(),
            image_url: None,
            genres: Vec::new(),
            top,
            albums: vec![AlbumItem {
                id: "a1".into(),
                name: "Black Holes".into(),
                artists: "Muse".into(),
                release_year: "2006".into(),
                album_type: "album".into(),
                album_group: "album".into(),
                track_count: 12,
                cover_url: Some("https://i.scdn.co/image/abc".into()),
            }],
            display: vec![0],
            tab: crate::app::state::ArtistTab::Albums,
            loading: false,
        });
        st
    }

    fn channel() -> (UnboundedSender<AppCommand>, UnboundedReceiver<AppCommand>) {
        unbounded_channel()
    }

    /// Put "Uprising" on: install the queue and transport state the client
    /// would have written for it, which is all the deck controls read.
    fn start_playing(st: &mut AppState) {
        let q = crate::app::queue::Queue::new(vec![track("Uprising", Some("a1"))], 0, "Q");
        st.queue = Some(q);
        st.playback = Some(crate::app::state::Playback::started(50, false));
    }

    /// A two-track list with the first row already saved.
    fn liked_state() -> AppState {
        let mut st = AppState::new();
        let mut list = TrackList::new("Black Holes", "Muse · 2006", None);
        list.kind = TrackListKind::Album;
        list.append(vec![
            track("Starlight", Some("a1")),
            track("Hysteria", Some("a1")),
        ]);
        st.main = MainView::Tracks(list);
        st.liked.insert("spotify:track:Starlight".into(), true);
        st
    }

    /// Every play carries its tracks in display order: the queue the client
    /// installs is exactly the list on screen, started at the clicked row.
    #[test]
    fn activating_a_row_sends_the_display_order() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        // "Hysteria".
        st.main_index = 1;

        activate_selection(&mut st, &tx);

        match rx.try_recv() {
            Ok(AppCommand::Play {
                tracks,
                start,
                name,
                ..
            }) => {
                assert_eq!(start, 1);
                assert_eq!(name, "Black Holes");
                let names: Vec<&str> = tracks.iter().map(|t| t.name.as_str()).collect();
                assert_eq!(names, ["Starlight", "Hysteria"]);
            }
            other => panic!("sent {other:?}"),
        }
    }

    /// A sorted view plays as the snapshot it is: the rows go out in display
    /// order, and the cache key stays behind — later pages arrive in fetch
    /// order and must not be appended to a queue that is not in it.
    #[test]
    fn a_sorted_view_plays_its_rows_without_the_source_key() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        if let MainView::Tracks(list) = &mut st.main {
            list.cache_key = Some(state::album_key("a1"));
            list.loading = true;
            list.sort = crate::app::state::TrackSort {
                key: SortKey::Title,
                ascending: true,
            };
            list.rebuild_display();
        }
        st.resort_main();

        activate_selection(&mut st, &tx);
        match rx.try_recv() {
            Ok(AppCommand::Play { key, loading, .. }) => {
                assert!(key.is_none(), "a sorted view kept its key");
                assert!(!loading);
            }
            other => panic!("sent {other:?}"),
        }

        // In natural order the key rides along, so the queue can grow with
        // the view.
        let mut st = liked_state();
        if let MainView::Tracks(list) = &mut st.main {
            list.cache_key = Some(state::album_key("a1"));
            list.loading = true;
        }
        activate_selection(&mut st, &tx);
        match rx.try_recv() {
            Ok(AppCommand::Play { key, loading, .. }) => {
                assert_eq!(key.as_deref(), Some("album:a1"));
                assert!(loading);
            }
            other => panic!("sent {other:?}"),
        }
    }

    /// Enter on a queue row is one instant command: the queue is the play
    /// order, so the client only has to point at the row.
    #[test]
    fn enter_on_a_queue_row_sends_a_jump() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.queue = Some(crate::app::queue::Queue::new(
            vec![track("Starlight", None), track("Hysteria", None)],
            0,
            "Q",
        ));
        st.queue_index = 1;
        play_from_queue(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::JumpTo(1))));

        // A cursor past the end of the queue jumps nowhere.
        st.queue_index = 7;
        play_from_queue(&mut st, &tx);
        assert!(rx.try_recv().is_err());
    }

    /// `a` carries the whole selected track, so the client can put it in the
    /// queue without a fetch.
    #[test]
    fn a_sends_the_selected_track_to_play_next() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        st.main_index = 1;
        queue_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::QueueInsertNext(t)) if t.name == "Hysteria"
        ));
    }

    /// The header's ▶ and `x` play the view from the top.
    #[test]
    fn playing_a_view_from_the_top_starts_at_row_zero() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();

        play_current_view(&mut st, &tx, false);

        match rx.try_recv() {
            Ok(AppCommand::Play {
                start,
                tracks,
                shuffle,
                ..
            }) => {
                assert_eq!(start, 0);
                assert_eq!(tracks.len(), 2);
                assert!(!shuffle);
            }
            other => panic!("sent {other:?}"),
        }
    }

    /// The header's shuffle pill plays the same view, mixed.
    #[test]
    fn the_header_shuffle_button_plays_the_view_shuffled() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();

        play_current_view(&mut st, &tx, true);

        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::Play { shuffle: true, .. })
        ));
    }

    /// `L` flips whatever is known about the selected row: the saved one is
    /// unsaved, and a row nothing is known about is saved.
    #[test]
    fn like_toggles_the_selected_row_both_ways() {
        for (index, uri, want) in [
            (0, "spotify:track:Starlight", false),
            (1, "spotify:track:Hysteria", true),
        ] {
            let (tx, mut rx) = channel();
            let mut st = liked_state();
            st.main_index = index;
            toggle_like_selection(&mut st, &tx);
            match rx.try_recv() {
                Ok(AppCommand::SetLiked { uri: got, liked }) => {
                    assert_eq!(got, uri);
                    assert_eq!(liked, want, "row {index} flipped the wrong way");
                }
                other => panic!("row {index} sent {other:?}"),
            }
        }
    }

    /// In the player the view is about the playing track, not about whichever
    /// queue row the cursor happens to be resting on.
    #[test]
    fn like_in_the_player_targets_the_playing_track() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        st.show_player = true;
        // The selection says Starlight; playback says Uprising.
        st.main_index = 0;
        start_playing(&mut st);
        toggle_like_selection(&mut st, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::SetLiked { uri, liked })
                if uri == "spotify:track:Uprising" && liked)
        );
    }

    /// Nothing selectable on the page, so `L` falls back to what is playing —
    /// the one track every screen has in common.
    #[test]
    fn like_on_a_page_without_tracks_falls_back_to_playback() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        start_playing(&mut st);
        toggle_like_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::SetLiked { uri, .. })
                if uri == "spotify:track:Uprising"));

        // With nothing playing either there is no track to mean, and the key
        // does nothing rather than guessing.
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        toggle_like_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err());
    }

    /// Clicking the liked column likes that row — and only likes it: the
    /// click must not also arm the double-click that would start playback.
    #[test]
    fn clicking_the_liked_column_likes_that_row() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        st.hit.main_list = Rect::new(0, 0, 90, 10);
        st.hit.main_like_col = Rect::new(4, 0, 2, 2);

        handle_click(&mut st, Position { x: 4, y: 1 }, &tx);
        assert_eq!(st.main_index, 1, "the click did not select the row");
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::SetLiked { uri, liked })
                if uri == "spotify:track:Hysteria" && liked)
        );
        assert!(
            st.last_main_click.is_none(),
            "the liked cell armed a double-click"
        );

        // A click on the same row outside the column is an ordinary select.
        handle_click(&mut st, Position { x: 20, y: 1 }, &tx);
        assert!(rx.try_recv().is_err());
        assert!(st.last_main_click.is_some());
    }

    /// The deck's liked control is about the playing track, whichever page is
    /// underneath it.
    #[test]
    fn clicking_the_decks_control_likes_the_playing_track() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        start_playing(&mut st);
        st.hit.like_btn = Rect::new(70, 20, 9, 1);
        handle_click(&mut st, Position { x: 72, y: 20 }, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::SetLiked { uri, liked })
                if uri == "spotify:track:Uprising" && liked)
        );
    }

    /// Both halves of the artist page name albums on screen, so both must
    /// open them: the Album cell of a top track (row 0 here) and the card
    /// under it (row 1). Both hover like links, so both must act like one.
    #[test]
    fn the_artist_page_opens_albums_from_tracks_and_cards() {
        // Either route arrives with the sleeve, so both land on the same
        // artwork header band rather than one degrading to the text one.
        for row in [0, 1] {
            let (tx, mut rx) = channel();
            let mut st = artist_state();
            st.main_index = row;
            open_album_of_selection(&mut st, &tx);
            match rx.try_recv() {
                Ok(AppCommand::OpenAlbum {
                    id,
                    name,
                    cover_url,
                    ..
                }) => {
                    assert_eq!(id, "a1");
                    assert_eq!(name, "Black Holes");
                    assert_eq!(
                        cover_url.as_deref(),
                        Some("https://i.scdn.co/image/abc"),
                        "row {row} opened the album without its sleeve"
                    );
                }
                other => panic!("row {row} sent {other:?}"),
            }
            assert_eq!(st.view_stack.len(), 1, "row {row} left no way back");
        }
    }

    /// The name is the link and the ▶ play is the button; the rest of a card
    /// is just a row. A whole card that opens its album makes a five-line
    /// region behave like one word, and leaves no way to select a card without
    /// leaving the page.
    #[test]
    fn only_a_card_name_opens_its_album() {
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        // What the draw records for a card at rows 4..9 of the pane: the name
        // on its first line, the ▶ play two lines down. Row 1 is the album.
        st.hit.main_list = Rect::new(0, 0, 40, 20);
        st.hit.main_lines = vec![
            Some(0),
            None,
            None,
            None,
            Some(1),
            Some(1),
            Some(1),
            Some(1),
        ];
        st.hit.album_names = vec![(Rect::new(10, 4, 11, 1), 1)];
        st.hit.card_play = vec![(Rect::new(10, 6, 8, 1), 1)];

        // The metadata line, directly under the name: same card, not a link.
        handle_click(&mut st, Position { x: 10, y: 5 }, &tx);
        assert_eq!(st.main_index, 1, "the click did not even select the card");
        assert!(rx.try_recv().is_err(), "the metadata line opened the album");

        // Past the end of the name on its own line: still not the link.
        st.last_main_click = None;
        handle_click(&mut st, Position { x: 25, y: 4 }, &tx);
        assert!(
            rx.try_recv().is_err(),
            "the name's padding opened the album"
        );

        // The name itself opens it, on one click.
        st.last_main_click = None;
        handle_click(&mut st, Position { x: 10, y: 4 }, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::OpenAlbum { .. })));
    }

    /// A card's ▶ play starts the record where it stands; the rest of the
    /// card opens it. Two controls, two meanings, one row.
    #[test]
    fn an_album_card_plays_without_opening() {
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        st.main_index = 1;
        play_selected_album(&mut st, &tx, false);
        match rx.try_recv() {
            Ok(AppCommand::PlayFetched {
                source: FetchSource::Album { id, .. },
                name,
                shuffle,
            }) => {
                assert_eq!(id, "a1");
                assert_eq!(name, "Black Holes");
                assert!(!shuffle);
            }
            other => panic!("sent {other:?}"),
        }
        assert!(st.view_stack.is_empty(), "playing a card navigated away");

        // A top track is not a card, so the control cannot land on one.
        st.main_index = 0;
        play_selected_album(&mut st, &tx, false);
        assert!(rx.try_recv().is_err());
    }

    /// A card's shuffle pill starts the record mixed, without opening it.
    #[test]
    fn an_album_cards_shuffle_plays_it_shuffled() {
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        st.hit.main_list = Rect::new(0, 0, 40, 20);
        st.hit.main_lines = vec![Some(0), None, None, None, Some(1), Some(1), Some(1)];
        st.hit.card_shuffle = vec![(Rect::new(20, 6, 7, 1), 1)];

        handle_click(&mut st, Position { x: 20, y: 6 }, &tx);

        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::PlayFetched { shuffle: true, .. })
        ));
        assert!(st.view_stack.is_empty(), "shuffling a card navigated away");
    }

    /// The artist page's `B` must not re-open the artist page.
    ///
    /// It resolves the artist off the selected *top track*, and the first name
    /// credited on a top track is the artist whose page it is — so an
    /// unguarded press stacks a copy of the page and navigates nowhere. Five
    /// presses, five crumbs, five Backspaces to undo. Same via a single click
    /// on the Artist column, which has no double-click gate.
    #[test]
    fn the_artist_page_does_not_reopen_itself() {
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        st.push_view();

        for _ in 0..5 {
            open_artist_of_selection(&mut st, &tx);
        }
        assert!(rx.try_recv().is_err(), "re-opened the artist it is showing");
        assert_eq!(st.view_stack.len(), 1, "{:?}", st.view_stack.len());
    }

    /// Revisiting a page walks *back* to it rather than stacking a second
    /// copy, so bouncing between an album and its artist cannot grow the path.
    ///
    /// A guard that checks only the album you are standing on lets the path
    /// grow by two a round trip, because on the artist page you are standing
    /// on an artist. Nine trips fill the twenty-frame stack and evict Home.
    #[test]
    fn revisiting_a_page_walks_back_to_it() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        // Home → the album, from a track row.
        st.push_view();
        let mut list = TrackList::new("Black Holes", "Muse · 2006", None);
        list.kind = TrackListKind::Album;
        list.cache_key = Some(state::album_key("a1"));
        list.append(vec![track("Starlight", Some("a1"))]);
        st.main = MainView::Tracks(list);

        // → its artist.
        open_artist_of_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::OpenArtist { id, .. }) if id == "r1"));
        assert_eq!(st.view_stack.len(), 2);
        st.main = artist_state().main;

        // → back to that same album, off one of the artist's cards. Nothing is
        // opened: the page is already on the path, so the path shortens to it.
        // The restore still re-points the sleeve, as any pop does.
        open_album_of_selection(&mut st, &tx);
        let sent: Vec<AppCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            !sent
                .iter()
                .any(|c| matches!(c, AppCommand::OpenAlbum { .. })),
            "re-opened a page already behind us: {sent:?}"
        );
        assert_eq!(st.view_stack.len(), 1, "the path grew instead of shrinking");
        assert!(matches!(st.main, MainView::Tracks(_)), "{:?}", st.main);
    }

    /// The bar's links are drawn on every page and in the player, so they were
    /// the easiest way to stack duplicates — and unlike the album column they
    /// had no guard at all. Clicking one while already on the page it names is
    /// now a no-op for the path, but must still close the player: that is what
    /// you clicked it for.
    #[test]
    fn the_now_playing_artist_link_closes_the_player_without_pushing() {
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        st.push_view();
        start_playing(&mut st);
        st.show_player = true;
        st.hit.now_artist = Rect::new(4, 9, 6, 1);

        handle_click(&mut st, Position { x: 5, y: 9 }, &tx);
        assert!(!st.show_player, "the click did not leave the player");
        assert!(rx.try_recv().is_err(), "re-opened the artist it is showing");
        assert_eq!(st.view_stack.len(), 1);
    }

    /// The target is only installed when the client answers, so until it does
    /// every further click pushes the *unchanged* current view, so four clicks
    /// on a Home row must not leave four copies of Home behind them.
    #[test]
    fn mashing_a_row_leaves_one_frame() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.playlists = vec![playlist("p1", "trendy", "dm")];

        for _ in 0..4 {
            activate_selection(&mut st, &tx);
        }
        assert_eq!(st.view_stack.len(), 1, "{:?}", labels(&st));
        // Every click still asks for the page — only the history is deduped.
        let sent = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert_eq!(sent, 4);
    }

    /// Search is one slot: a new query takes the old one's place wherever it
    /// sat, rather than stacking beside it. Otherwise ten refinements leave
    /// ten frames, each holding a whole cloned `SearchResults`.
    #[test]
    fn a_new_search_replaces_the_old_one() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();

        // From Home the first search is a step.
        navigate(&mut st, AppCommand::Search("muse".into()), &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "muse"));
        assert_eq!(st.view_stack.len(), 1);
        st.main = MainView::Search(crate::app::state::SearchResults {
            query: "muse".into(),
            ..Default::default()
        });

        // Searching again from the results replaces rather than stacks.
        navigate(&mut st, AppCommand::Search("pixies".into()), &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "pixies"));
        assert_eq!(labels(&st), ["“muse”"], "the old query was stacked");
    }

    /// …and from a page *above* an earlier search, the new query takes that
    /// search's place on the path instead of adding a second one.
    ///
    /// Truncating rather than popping, because the new query has to win —
    /// restoring the old results would leave the command unsent.
    #[test]
    fn a_search_from_deeper_in_replaces_the_one_on_the_path() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        // Home, then the search, then a page above it.
        st.push_view();
        st.main = MainView::Search(crate::app::state::SearchResults {
            query: "muse".into(),
            ..Default::default()
        });
        st.push_view();
        st.main = MainView::Playlists;

        navigate(&mut st, AppCommand::Search("pixies".into()), &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "pixies"));
        assert_eq!(st.view_stack.len(), 1, "{:?}", labels(&st));
        assert!(matches!(st.view_stack[0].view, MainView::Home));
    }

    /// Overflow drops the frame above the root, never the root. Home is the
    /// bottom of this stack and the thing back exists to reach; losing it
    /// leaves an album page with nothing behind it, which sends `Esc`
    /// oscillating between that album and its artist forever.
    #[test]
    fn overflowing_the_stack_keeps_home_at_the_bottom() {
        let mut st = AppState::new();
        // Home at the bottom, then more pages than the stack holds.
        st.push_view();
        for i in 0..40 {
            let mut list = TrackList::new(format!("page {i}"), "", None);
            list.cache_key = Some(state::playlist_key(&i.to_string()));
            st.main = MainView::Tracks(list);
            st.push_view();
        }
        assert!(
            matches!(st.view_stack[0].view, MainView::Home),
            "the root was evicted"
        );
        assert!(st.view_stack.len() <= 20);
    }

    /// An album page still prints an Album column, naming the album you are
    /// already on. Opening it again would stack a duplicate and make the back
    /// control lead nowhere.
    #[test]
    fn an_album_page_does_not_reopen_itself() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        let mut list = TrackList::new("Black Holes", "Muse · 2006", None);
        list.kind = TrackListKind::Album;
        // The identity is the cache key, not the context URI: the latter is
        // `None` for Liked Songs and empty for a playlist that was not in
        // `playlists` when it opened, so unrelated pages compare equal on it.
        list.cache_key = Some(state::album_key("a1"));
        list.append(vec![track("Starlight", Some("a1"))]);
        st.main = MainView::Tracks(list);

        open_album_of_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "re-opened the album it is showing");
        assert!(st.view_stack.is_empty());

        // A different album from the same page still opens.
        if let MainView::Tracks(list) = &mut st.main {
            list.tracks[0].album_id = Some("a2".into());
        }
        open_album_of_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::OpenAlbum { id, .. }) if id == "a2"));
    }

    /// Esc goes back from any page that draws a `←`, which is every page below
    /// Home.
    #[test]
    fn esc_goes_back_from_any_page_below_home() {
        let esc = KeyEvent::from(KeyCode::Esc);

        let make = |kind| {
            let mut st = AppState::new();
            let mut before = TrackList::new("Liked Songs", "", None);
            before.append(vec![track("Uprising", Some("a1"))]);
            st.main = MainView::Tracks(before);
            st.push_view();
            let mut list = TrackList::new("Black Holes", "Muse · 2006", None);
            list.kind = kind;
            list.append(vec![track("Starlight", Some("a1"))]);
            st.main = MainView::Tracks(list);
            Arc::new(RwLock::new(st))
        };

        let (tx, _rx) = channel();
        for kind in [TrackListKind::Album, TrackListKind::Playlist] {
            let state = make(kind);
            handle_normal(esc, &state, &tx);
            assert!(state.read().view_stack.is_empty(), "{kind:?} did not pop");
            assert!(
                matches!(&state.read().main, MainView::Tracks(l) if l.header.name == "Liked Songs"),
                "{kind:?} restored the wrong page"
            );
        }

        // Home is the bottom of the stack: there is nothing behind it, so Esc
        // has nothing to do.
        let state = Arc::new(RwLock::new(AppState::new()));
        handle_normal(esc, &state, &tx);
        assert!(matches!(state.read().main, MainView::Home));
    }

    fn playlist(id: &str, name: &str, owner_id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: name.into(),
            track_count: 18,
            owner: owner_id.into(),
            owner_id: owner_id.into(),
            snapshot_id: "s".into(),
        }
    }

    /// The trail as the section row spells it: ancestors, then the page.
    fn labels(st: &AppState) -> Vec<String> {
        st.trail().into_iter().map(|c| c.label).collect()
    }

    /// Home → Playlists → a playlist, each step adding a crumb that names the
    /// page it came from. Without this the trails on the two pages below Home
    /// lead nowhere.
    #[test]
    fn drilling_down_from_home_leaves_a_trail() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.playlists = vec![playlist("p1", "trendy", "dm")];

        assert!(matches!(st.main, MainView::Home));
        assert_eq!(st.back_target(), None, "Home has nowhere to go back to");

        // Without Discover Weekly the rows are Liked Songs, Playlists, Radio.
        st.main_index = 1;
        activate_selection(&mut st, &tx);
        assert!(matches!(st.main, MainView::Playlists));
        // Home draws no crumb of its own — the mark beside the path is the
        // way there — so the path below it starts at the page you opened.
        assert_eq!(labels(&st), ["playlists"]);
        assert_eq!(st.view_stack.len(), 1, "Home is still on the stack");

        activate_selection(&mut st, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::LoadPlaylistTracks { playlist_id }) if playlist_id == "p1")
        );
        assert_eq!(st.view_stack.len(), 2);
        // The page is pushed on the click and the tracks arrive later, so
        // until they do the head is still the list you clicked from.
        st.main = MainView::Tracks(TrackList::new("trendy", "", None));
        // Each drill-in adds a step rather than replacing the one before it,
        // which is what the trail on the section row spells out.
        assert_eq!(labels(&st), ["playlists", "trendy"]);
    }

    /// Liked Songs and Discover Weekly are Home rows of their own. Discover
    /// Weekly is Spotify's, so it is only a row when you follow it — and only
    /// when Spotify is the one who made it.
    #[test]
    fn home_lists_discover_weekly_only_when_you_follow_it() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        assert_eq!(
            st.home_items(),
            vec![HomeItem::LikedSongs, HomeItem::Playlists, HomeItem::Radio]
        );

        // Someone else's playlist of the same name is not Spotify's.
        st.playlists = vec![playlist("p1", "Discover Weekly", "dm")];
        assert_eq!(
            st.home_items(),
            vec![HomeItem::LikedSongs, HomeItem::Playlists, HomeItem::Radio]
        );

        st.playlists
            .push(playlist("dw", "Discover Weekly", "spotify"));
        assert_eq!(
            st.home_items(),
            vec![
                HomeItem::LikedSongs,
                HomeItem::DiscoverWeekly,
                HomeItem::Playlists,
                HomeItem::Radio
            ]
        );

        // Row 0 opens Liked Songs, row 1 the real Discover Weekly.
        activate_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::LoadLikedSongs)));

        st.main = MainView::Home;
        st.main_index = 1;
        activate_selection(&mut st, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::LoadPlaylistTracks { playlist_id }) if playlist_id == "dw")
        );
    }

    /// A Home row opens on one click, the way the left rail's entries did:
    /// there is no "play" for a second click to mean, and the target is the
    /// whole two-line block rather than the name alone.
    #[test]
    fn a_single_click_anywhere_on_a_home_row_opens_it() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.playlists = vec![playlist("p1", "trendy", "dm")];
        // What the draw records for two entries at rows 4..6 and 7..9.
        st.hit.home_rows = vec![(Rect::new(1, 4, 90, 2), 0), (Rect::new(1, 7, 90, 2), 1)];

        // The blurb line, past the end of the name: still the same control.
        handle_click(&mut st, Position { x: 60, y: 5 }, &tx);
        assert_eq!(st.main_index, 0);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::LoadLikedSongs)));

        st.main = MainView::Home;
        handle_click(&mut st, Position { x: 3, y: 7 }, &tx);
        assert!(
            matches!(st.main, MainView::Playlists),
            "one click, one open"
        );

        // The gap between two entries belongs to neither.
        let mut st = AppState::new();
        st.hit.home_rows = vec![(Rect::new(1, 4, 90, 2), 0), (Rect::new(1, 7, 90, 2), 1)];
        handle_click(&mut st, Position { x: 3, y: 6 }, &tx);
        assert!(matches!(st.main, MainView::Home));
        assert!(rx.try_recv().is_err());
    }

    /// The mark skips the whole stack rather than pushing one more page onto
    /// it — arriving somewhere by jumping over every page in between leaves no
    /// honest "page I came from".
    #[test]
    fn the_mark_goes_home_and_clears_the_history() {
        let (tx, _rx) = channel();
        let mut st = AppState::new();
        st.hit.home_btn = Rect::new(0, 0, 6, 1);
        st.main = MainView::Playlists;
        st.push_view();
        st.main = MainView::Search(crate::app::state::SearchResults {
            query: "muse".into(),
            ..Default::default()
        });
        st.main_index = 4;
        st.show_player = true;

        handle_click(&mut st, Position { x: 2, y: 0 }, &tx);
        assert!(matches!(st.main, MainView::Home));
        assert!(
            st.view_stack.is_empty(),
            "Home stacked onto its own history"
        );
        assert_eq!(st.main_index, 0);
        assert!(!st.show_player, "the player would have hidden the new view");
    }

    /// A crumb is a jump, not a run of single steps. Clicking `HOME` from
    /// three pages deep restores Home's own scroll and selection rather than
    /// whatever the two pages in between were left at.
    #[test]
    fn clicking_a_crumb_jumps_straight_to_its_page() {
        let (tx, _rx) = channel();
        let mut st = AppState::new();
        // Home, scrolled and with its second row picked, then two drill-ins.
        st.main_index = 1;
        st.push_view();
        st.main = MainView::Playlists;
        st.main_index = 4;
        st.push_view();
        st.main = MainView::Tracks(TrackList::new("Black Holes", "", None));
        st.main_index = 7;

        st.hit.crumbs = vec![
            (Rect::new(0, 2, 4, 1), CrumbTarget::Depth(0)),
            (Rect::new(10, 2, 9, 1), CrumbTarget::Depth(1)),
        ];
        handle_click(&mut st, Position { x: 1, y: 2 }, &tx);

        assert!(matches!(st.main, MainView::Home));
        assert!(st.view_stack.is_empty(), "the pages above it stayed behind");
        assert_eq!(st.main_index, 1, "Home's own selection, not the album's");
    }

    /// The player draws the browse screen's trail over the page underneath it,
    /// so a crumb clicked there has to land you where it says — which means
    /// closing the view on the way.
    #[test]
    fn a_crumb_clicked_in_the_player_closes_it_and_navigates() {
        let (tx, _rx) = channel();
        let mut st = AppState::new();
        st.push_view();
        st.main = MainView::Playlists;
        st.show_player = true;
        st.hit.crumbs = vec![(Rect::new(60, 0, 4, 1), CrumbTarget::Depth(0))];

        handle_click(&mut st, Position { x: 61, y: 0 }, &tx);
        assert!(!st.show_player);
        assert!(matches!(st.main, MainView::Home));
    }

    /// The head of the player's trail closes the view; it does not pop
    /// anything, because the page it names is the one already underneath.
    #[test]
    fn the_players_back_pill_only_closes_the_player() {
        let (tx, _rx) = channel();
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        st.push_view();
        st.show_player = true;
        st.hit.close_player = Rect::new(20, 0, 9, 1);

        handle_click(&mut st, Position { x: 22, y: 0 }, &tx);
        assert!(!st.show_player);
        assert_eq!(st.view_stack.len(), 1, "closing the player popped a view");
        assert!(matches!(st.main, MainView::Playlists));
    }

    /// The header's ← and Esc share one resolution, so the label and the
    /// action can never disagree — including the up-to-the-artist fallback.
    #[test]
    fn the_back_control_follows_back_target() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        let mut list = TrackList::new("Black Holes", "Muse · 2006", None);
        list.kind = TrackListKind::Album;
        list.append(vec![track("Starlight", Some("a1"))]);
        st.main = MainView::Tracks(list);

        // Empty stack: up to the artist the tracks credit.
        go_back_or_up(&mut st, &tx);
        match rx.try_recv() {
            Ok(AppCommand::OpenArtist { id, uri, name }) => {
                assert_eq!(
                    (id.as_str(), uri.as_str(), name.as_str()),
                    ("r1", "spotify:artist:r1", "Muse")
                );
            }
            other => panic!("{other:?}"),
        }
        // The album is on the stack now, so back from the artist returns to it.
        assert_eq!(st.view_stack.len(), 1);
    }

    /// The deck draws the queue's name in both views, so clicking it is the
    /// mouse's `v`: it opens the player and closes it again.
    #[test]
    fn clicking_the_queue_name_toggles_the_player_view() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.hit.queue_name = Rect {
            x: 4,
            y: 9,
            width: 6,
            height: 1,
        };
        let on_name = Position { x: 5, y: 9 };

        handle_click(&mut st, on_name, &tx);
        assert!(st.show_player, "the name should open the player");
        // Nothing to fetch on the way in: the queue is always present and
        // always correct, because spot wrote it on the play.
        assert!(rx.try_recv().is_err());

        handle_click(&mut st, on_name, &tx);
        assert!(!st.show_player, "clicking it again should close the player");

        // A click that misses the name leaves the view where it was.
        handle_click(&mut st, Position { x: 40, y: 9 }, &tx);
        assert!(!st.show_player);
    }

    fn test_station(uuid: &str, name: &str) -> Station {
        Station {
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

    fn radio_page(scope: RadioScope, rows: Vec<RadioRow>) -> MainView {
        let mut view = crate::app::state::RadioView::new(scope, 0);
        view.rows = rows;
        view.loading = false;
        MainView::Radio(view)
    }

    fn live_radio(station: Station) -> crate::app::state::RadioPlayback {
        crate::app::state::RadioPlayback {
            station,
            is_playing: true,
            started_at: Instant::now(),
            title: Arc::new(parking_lot::Mutex::new(None)),
            volume_percent: 50,
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
        }
    }

    /// Home's Radio row opens the chart, not your saved list: Saved is empty
    /// until you have kept something, and a destination that opens onto
    /// nothing is a dead end.
    #[test]
    fn the_home_radio_row_opens_the_chart() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main_index = st.home_items().len() - 1;
        assert_eq!(st.home_items()[st.main_index], HomeItem::Radio);

        activate_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadRadio {
                scope: RadioScope::Popular
            })
        ));
        // The page is pushed on the keypress and the directory answers later,
        // so until it does the head is still the page you left.
        st.main = radio_page(RadioScope::Popular, vec![]);
        assert_eq!(labels(&st), ["radio"]);
    }

    /// A facet row is a door and a station row is a thing to play, so Enter
    /// means two things on the one page.
    #[test]
    fn enter_drills_into_a_facet_and_plays_a_station() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();

        st.main = radio_page(
            RadioScope::Countries,
            vec![RadioRow::Facet {
                key: "GB".into(),
                label: "The United Kingdom".into(),
                count: 2146,
            }],
        );
        activate_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadRadio { scope: RadioScope::Country(code) }) if code == "GB"
        ));

        st.main = radio_page(
            RadioScope::Popular,
            vec![RadioRow::Station(test_station("a", "Radio Paradise"))],
        );
        activate_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::PlayStation { station: s, .. }) if s.uuid == "a"
        ));
    }

    /// The genre tab queries by tag and the country tab by code. The two rows
    /// are the same shape, so drilling in has to pick the right query from the
    /// page it is on rather than from the row.
    #[test]
    fn a_genre_facet_queries_by_tag() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = radio_page(
            RadioScope::Genres,
            vec![RadioRow::Facet {
                key: "jazz".into(),
                label: "jazz".into(),
                count: 900,
            }],
        );
        activate_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadRadio { scope: RadioScope::Genre(tag) }) if tag == "jazz"
        ));
    }

    /// Enter on the station already playing stops it — the only way to stop
    /// radio without starting something else, and it needs no key of its own.
    #[test]
    fn enter_on_the_playing_station_stops_it() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        let station = test_station("a", "Radio Paradise");
        st.radio = Some(live_radio(station.clone()));
        st.main = radio_page(RadioScope::Popular, vec![RadioRow::Station(station)]);

        activate_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::StopRadio)));
    }

    /// HLS rows are listed rather than hidden, because dropping them would
    /// silently remove the BBC — so pressing Enter on one has to say why
    /// nothing happened instead of failing quietly.
    #[test]
    fn an_hls_station_is_refused_with_a_reason() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        let mut station = test_station("c", "BBC Radio 6 Music");
        station.hls = true;
        st.main = radio_page(RadioScope::Popular, vec![RadioRow::Station(station)]);

        activate_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "nothing should be sent");
        let (msg, _) = st.toast.as_ref().expect("the refusal must be said");
        assert!(msg.contains("HLS"), "{msg:?}");
    }

    /// `L` keeps a station. The same key likes a track, because it is the same
    /// gesture — only the store behind it differs.
    #[test]
    fn l_saves_the_selected_station() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = radio_page(
            RadioScope::Popular,
            vec![RadioRow::Station(test_station("a", "Radio Paradise"))],
        );
        toggle_like_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::ToggleSavedStation(s)) if s.uuid == "a"
        ));

        // On a facet row there is nothing to keep, and `L` must not fall
        // through to liking whatever Spotify happens to be paused on.
        st.main = radio_page(
            RadioScope::Countries,
            vec![RadioRow::Facet {
                key: "GB".into(),
                label: "GB".into(),
                count: 1,
            }],
        );
        start_playing(&mut st);
        toggle_like_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "a facet row likes nothing");
    }

    /// A search view's Stations tab, with one station on it.
    fn station_search(station: Station) -> MainView {
        MainView::Search(crate::app::state::SearchResults {
            query: "jazz".into(),
            stations: vec![station],
            ..Default::default()
        })
    }

    /// A station found through search is the same object as one found on a
    /// radio page, so Enter plays it the same way.
    #[test]
    fn enter_on_a_station_result_plays_it() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = station_search(test_station("a", "Radio Paradise"));
        st.search_tab = SearchTab::Stations;
        activate_selection(&mut st, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::PlayStation { station: s, .. }) if s.uuid == "a")
        );
    }

    /// …and Enter on the station already playing stops it, as on a radio page.
    #[test]
    fn enter_on_the_playing_station_result_stops_it() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        let station = test_station("a", "Radio Paradise");
        st.main = station_search(station.clone());
        st.search_tab = SearchTab::Stations;
        st.radio = Some(live_radio(station));
        activate_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::StopRadio)));
    }

    /// `x` is the same gesture as Enter on a station, wherever the row is.
    #[test]
    fn x_on_a_station_result_is_the_same_as_enter() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = station_search(test_station("a", "Radio Paradise"));
        st.search_tab = SearchTab::Stations;
        play_without_opening(&mut st, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::PlayStation { station: s, .. }) if s.uuid == "a")
        );
    }

    /// The guard that matters: `L` on a station result keeps the *station*.
    /// Without it the lookup below would come back empty and the key would
    /// fall through to liking whatever Spotify happens to be playing.
    #[test]
    fn l_on_a_station_result_saves_the_station_not_the_playing_track() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = station_search(test_station("a", "Radio Paradise"));
        st.search_tab = SearchTab::Stations;
        start_playing(&mut st);
        toggle_like_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::ToggleSavedStation(s)) if s.uuid == "a"
        ));
        assert!(rx.try_recv().is_err(), "nothing else may follow");

        // And an empty Stations tab likes nothing at all, rather than falling
        // through to the playing track.
        st.main = MainView::Search(crate::app::state::SearchResults {
            query: "jazz".into(),
            ..Default::default()
        });
        toggle_like_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "an empty tab likes nothing");
    }

    /// A station has no album, no artist and no Spotify URI, so the browse
    /// and queue keys mean nothing on one.
    #[test]
    fn browse_keys_do_nothing_on_a_station_result() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = station_search(test_station("a", "Radio Paradise"));
        st.search_tab = SearchTab::Stations;
        open_album_of_selection(&mut st, &tx);
        open_artist_of_selection(&mut st, &tx);
        queue_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "a station browses nowhere");
    }

    /// Search's tabs are cuts of one answer already in hand, Stations
    /// included: reaching it costs no fetch and no navigation.
    #[test]
    fn left_and_right_reach_the_stations_tab() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = station_search(test_station("a", "Radio Paradise"));
        st.search_tab = SearchTab::Tracks;

        cycle_view_tab(&mut st, -1, &tx);
        assert_eq!(st.search_tab, SearchTab::Stations, "←  wraps onto it");
        cycle_view_tab(&mut st, 1, &tx);
        assert_eq!(st.search_tab, SearchTab::Tracks);
        for _ in 0..4 {
            cycle_view_tab(&mut st, 1, &tx);
        }
        assert_eq!(st.search_tab, SearchTab::Stations, "and → walks to it");
        assert!(rx.try_recv().is_err(), "switching tabs fetches nothing");
    }

    /// Radio's tabs are four different queries, so ←/→ navigates rather than
    /// re-cutting a result set already in hand.
    #[test]
    fn arrows_navigate_between_radio_tabs() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = radio_page(RadioScope::Popular, vec![]);

        cycle_view_tab(&mut st, 1, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadRadio {
                scope: RadioScope::Countries
            })
        ));

        // Drilling into a country leaves the Countries tab lit, so the next
        // step along is Genres rather than Countries over again.
        st.main = radio_page(RadioScope::Country("GB".into()), vec![]);
        cycle_view_tab(&mut st, 1, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadRadio {
                scope: RadioScope::Genres
            })
        ));
    }

    /// The artist page's groups are cuts of one answer, so ←/→ re-cuts them
    /// where it stands: no command goes out, and nothing is pushed for Esc to
    /// walk back off. The wrap runs both ways, over the groups that have
    /// records and no others.
    #[test]
    fn arrows_re_cut_the_artist_page_without_leaving_it() {
        use crate::app::state::ArtistTab;
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        let MainView::Artist(v) = &mut st.main else {
            unreachable!()
        };
        v.albums.push(AlbumItem {
            id: "a2".into(),
            name: "Hysteria".into(),
            artists: "Muse".into(),
            release_year: "2003".into(),
            album_type: "single".into(),
            album_group: "single".into(),
            track_count: 1,
            cover_url: None,
        });
        v.retab();
        let depth = st.view_stack.len();

        cycle_view_tab(&mut st, 1, &tx);
        // Sleeves, and nothing else: the records are already in hand, and
        // only the art of a group nobody opened was left unfetched.
        assert!(matches!(rx.try_recv(), Ok(AppCommand::LoadArtistArt)));
        assert!(
            rx.try_recv().is_err(),
            "switching a group asked for the records again"
        );
        assert_eq!(
            st.view_stack.len(),
            depth,
            "switching a group pushed a page"
        );
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.tab, ArtistTab::Singles);
        assert_eq!(v.display, vec![1]);

        // Two groups, so one more step wraps back to the first.
        cycle_view_tab(&mut st, 1, &tx);
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.tab, ArtistTab::Albums);
        cycle_view_tab(&mut st, -1, &tx);
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.tab, ArtistTab::Singles);
    }

    /// Clicking a group label picks that group, the way clicking a search tab
    /// picks a cut of the query — in place, with the list back at the top.
    #[test]
    fn clicking_an_album_group_re_cuts_the_page() {
        use crate::app::state::ArtistTab;
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        let MainView::Artist(v) = &mut st.main else {
            unreachable!()
        };
        v.albums.push(AlbumItem {
            id: "a2".into(),
            name: "Hysteria".into(),
            artists: "Muse".into(),
            release_year: "2003".into(),
            album_type: "single".into(),
            album_group: "single".into(),
            track_count: 1,
            cover_url: None,
        });
        v.retab();
        st.main_index = 1;
        st.hit.artist_tabs = vec![(
            Rect {
                x: 10,
                y: 18,
                width: 7,
                height: 1,
            },
            ArtistTab::Singles,
        )];

        handle_click(&mut st, Position { x: 12, y: 18 }, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::LoadArtistArt)));
        assert!(
            rx.try_recv().is_err(),
            "a group click asked for the records again"
        );
        assert_eq!(
            st.main_index, 0,
            "the list stayed where the last tab left it"
        );
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.tab, ArtistTab::Singles);
        assert_eq!(v.display, vec![1]);
    }

    /// One group is no choice: the strip is not drawn, and ←/→ has nothing to
    /// do rather than flicking the same tab back at you.
    #[test]
    fn arrows_do_nothing_on_a_one_group_catalogue() {
        use crate::app::state::ArtistTab;
        let (tx, _rx) = channel();
        let mut st = artist_state();
        cycle_view_tab(&mut st, 1, &tx);
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.tab, ArtistTab::Albums);
        assert_eq!(v.display, vec![0]);
    }

    /// While a station is live the Spotify-only transport keys must not reach
    /// Spirc: doing so would start Spotify playing underneath the stream.
    #[test]
    fn shuffle_and_seek_are_refused_while_radio_is_live() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        state.write().radio = Some(live_radio(test_station("a", "Radio Paradise")));

        for key in ['s', 'h', 'l'] {
            handle_normal(KeyEvent::from(KeyCode::Char(key)), &state, &tx);
            assert!(
                rx.try_recv().is_err(),
                "`{key}` must not reach Spotify while radio is live"
            );
            assert!(state.read().toast.is_some(), "`{key}` should say why");
            state.write().toast = None;
        }

        // Play/pause and volume mean the same thing to either engine, so they
        // go through and the client routes them.
        handle_normal(KeyEvent::from(KeyCode::Char(' ')), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::PlayPause)));
        handle_normal(KeyEvent::from(KeyCode::Char('=')), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::VolumeRel(5))));
    }

    /// Whichever engine owns the device owns the transport. While a station is
    /// on, previous and next mean the station either side of it, and the
    /// client is where that is decided — so the keys go through untouched.
    #[test]
    fn next_and_previous_reach_the_client_while_radio_is_live() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        state.write().radio = Some(live_radio(test_station("a", "Radio Paradise")));

        handle_normal(KeyEvent::from(KeyCode::Char('n')), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Next)));
        handle_normal(KeyEvent::from(KeyCode::Char('p')), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Prev)));
        assert!(state.read().toast.is_none(), "neither key should say no");
    }

    /// The deck's two step controls are the same rects the Spotify deck's are,
    /// so the mouse sends the same two commands from either row.
    #[test]
    fn the_radio_step_controls_send_prev_and_next() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.radio = Some(live_radio(test_station("a", "Radio Paradise")));
        st.hit.prev_btn = Rect::new(0, 20, 11, 1);
        st.hit.next_btn = Rect::new(53, 20, 7, 1);
        handle_click(&mut st, Position { x: 2, y: 20 }, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Prev)));
        handle_click(&mut st, Position { x: 55, y: 20 }, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Next)));
    }

    /// The one search box asks both catalogues, from wherever it is pressed. A
    /// prompt that points at whichever one the page behind it came from makes
    /// the same keystroke do two different things depending on where you are
    /// standing.
    #[test]
    fn the_prompt_searches_both_catalogues_from_every_page() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));

        // A radio page, where the page's own catalogue is the station
        // directory.
        {
            let mut st = state.write();
            st.main = radio_page(RadioScope::Popular, vec![]);
            st.input_mode = InputMode::Search;
            st.input_buffer = "jazz".into();
        }
        handle_search_input(KeyEvent::from(KeyCode::Enter), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "jazz"));
        assert_eq!(state.read().search_tab, SearchTab::Tracks);

        // And everywhere else, the same command.
        {
            let mut st = state.write();
            st.main = MainView::Home;
            st.input_mode = InputMode::Search;
            st.input_buffer = "jazz".into();
        }
        handle_search_input(KeyEvent::from(KeyCode::Enter), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "jazz"));
    }

    /// The player draws no prompt (see [`crate::ui::top_row`]), so `/` is
    /// inert there: nothing on screen would show what you were typing.
    #[test]
    fn slash_is_inert_in_the_player() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        state.write().show_player = true;
        handle_normal(KeyEvent::from(KeyCode::Char('/')), &state, &tx);
        assert_eq!(
            state.read().input_mode,
            InputMode::Normal,
            "nothing on screen would show what you were typing"
        );
        assert!(state.read().show_player, "and it does not leave the view");

        // `v` first, and then it works like anywhere else.
        state.write().show_player = false;
        handle_normal(KeyEvent::from(KeyCode::Char('/')), &state, &tx);
        assert_eq!(state.read().input_mode, InputMode::Search);
    }

    /// The prompt is not drawn over the player and `/` is inert there, so
    /// this state should not be reachable — but if it ever is, the results
    /// have to be visible when they land.
    #[test]
    fn a_query_submitted_from_the_player_closes_it() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        {
            let mut st = state.write();
            st.show_player = true;
            st.input_mode = InputMode::Search;
            st.input_buffer = "muse".into();
        }
        handle_search_input(KeyEvent::from(KeyCode::Enter), &state, &tx);
        assert!(!state.read().show_player);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "muse"));
    }

    /// Backing out of the prompt is not a navigation, so it leaves you where
    /// you were — as does submitting nothing.
    #[test]
    fn leaving_the_prompt_empty_handed_keeps_the_player_open() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        for key in [KeyCode::Esc, KeyCode::Enter] {
            {
                let mut st = state.write();
                st.show_player = true;
                st.input_mode = InputMode::Search;
                st.input_buffer = "  ".into();
            }
            handle_search_input(KeyEvent::from(key), &state, &tx);
            assert!(state.read().show_player, "{key:?} left the player");
            assert_eq!(state.read().input_mode, InputMode::Normal);
        }
    }

    /// The nav row's playback status is the deck's queue name said where you
    /// are already looking when you want to know what is playing, so it opens
    /// and closes the player the same way — including from the player itself,
    /// where it is the row's own way back out.
    #[test]
    fn the_status_toggles_the_player_from_either_screen() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.hit.status = Rect {
            x: 68,
            y: 0,
            width: 11,
            height: 1,
        };
        let on_status = Position { x: 70, y: 0 };

        handle_click(&mut st, on_status, &tx);
        assert!(st.show_player, "the status should open the player");
        handle_click(&mut st, on_status, &tx);
        assert!(!st.show_player, "and close it again");

        // Nothing to fetch on the way in: the queue is spot's own and needs
        // no reload, whatever is playing.
        start_playing(&mut st);
        handle_click(&mut st, on_status, &tx);
        assert!(rx.try_recv().is_err());
        st.show_player = false;

        // Clicked while typing it does not lose the mode quietly to the
        // "a click elsewhere cancels the input" branch: it clears the box
        // itself, the way the mark beside it does.
        st.show_player = false;
        st.input_mode = InputMode::Search;
        st.input_buffer = "muse".into();
        handle_click(&mut st, on_status, &tx);
        assert!(st.show_player, "the click still reached the status");
        assert_eq!(st.input_mode, InputMode::Normal);
        assert!(st.input_buffer.is_empty());

        // A click that misses it leaves the view where it was.
        handle_click(&mut st, Position { x: 40, y: 0 }, &tx);
        assert!(st.show_player);
    }

    /// A station with a Spotify record found for what it announced.
    fn matched_radio() -> crate::app::state::RadioPlayback {
        let mut r = live_radio(test_station("s1", "Adroit Jazz"));
        *r.title.lock() = Some("Peter Appleyard - Frenesi".into());
        let mut t = track("Frenesi", Some("alb1"));
        t.uri = "spotify:track:Frenesi".into();
        r.matched = RadioMatch::Matched(Box::new(t));
        r
    }

    /// The bug the deck-subject accessor exists to stop. While a station
    /// plays, `playback` still names the last Spotify track, so reading it
    /// directly makes `★` like a record that stopped playing when the stream
    /// started.
    #[test]
    fn the_decks_control_likes_the_matched_track_not_the_kept_snapshot() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        start_playing(&mut st);
        st.radio = Some(matched_radio());
        st.hit.like_btn = Rect::new(70, 20, 9, 1);

        handle_click(&mut st, Position { x: 72, y: 20 }, &tx);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::SetLiked { uri, liked })
                if uri == "spotify:track:Frenesi" && liked),
            "the kept Spotify track was liked instead of the station's record"
        );
    }

    /// A station spot could not place has nothing to save, and says so rather
    /// than falling through to the record behind the stream.
    #[test]
    fn the_decks_control_says_why_it_did_nothing_without_a_match() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        start_playing(&mut st);
        let mut r = live_radio(test_station("s1", "Adroit Jazz"));
        r.matched = RadioMatch::Unmatched;
        st.radio = Some(r);
        st.hit.like_btn = Rect::new(70, 20, 9, 1);

        handle_click(&mut st, Position { x: 72, y: 20 }, &tx);
        assert!(rx.try_recv().is_err(), "liked something while radio played");
        assert!(st.toast.is_some(), "a dead control must explain itself");
    }

    #[test]
    fn clicking_the_radio_decks_artist_opens_the_matched_artist() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        start_playing(&mut st);
        st.radio = Some(matched_radio());
        st.show_player = true;
        st.hit.now_artist = Rect::new(4, 9, 6, 1);

        handle_click(&mut st, Position { x: 5, y: 9 }, &tx);
        assert!(
            !st.show_player,
            "the page opens in the view the player hides"
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::OpenArtist { ref id, .. }) if id == "r1"
        ));
    }

    #[test]
    fn clicking_the_radio_decks_album_opens_the_matched_album() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        start_playing(&mut st);
        st.radio = Some(matched_radio());
        st.hit.now_album = Rect::new(20, 9, 6, 1);

        handle_click(&mut st, Position { x: 21, y: 9 }, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::OpenAlbum { ref id, .. }) if id == "alb1"
        ));
    }

    /// A radio page has no track rows, so `b` and `B` mean the deck there.
    #[test]
    fn b_and_shift_b_on_a_radio_page_open_the_matched_tracks_pages() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.main = radio_page(RadioScope::Popular, Vec::new());
        st.radio = Some(matched_radio());

        open_album_of_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::OpenAlbum { ref id, .. }) if id == "alb1"
        ));

        open_artist_of_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::OpenArtist { ref id, .. }) if id == "r1"
        ));
    }

    /// Off radio the fallback must not fire: `b` on a page with no albums on
    /// it has always been a no-op, and reaching for the playing track there
    /// would be a new behaviour nobody asked for.
    #[test]
    fn b_still_does_nothing_on_a_spotify_page_with_no_album_row() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        start_playing(&mut st);
        open_album_of_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err());
    }

    /// `L` on a station row keeps the *station*, even while that very station
    /// is the one playing and has a matched record on the deck. Same key, two
    /// subjects, and the row under the cursor is what decides.
    #[test]
    fn shift_l_on_a_station_row_still_saves_the_station_while_it_plays() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        let station = test_station("s1", "Adroit Jazz");
        st.main = radio_page(
            RadioScope::Popular,
            vec![RadioRow::Station(station.clone())],
        );
        st.radio = Some(matched_radio());

        toggle_like_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::ToggleSavedStation(s)) if s.uuid == station.uuid
        ));
    }

    /// The station row's `★` keeps the station making the sound, not the
    /// record on the masthead above it — the two wear the same mark on the
    /// same deck, so this is the pair that has to be told apart.
    #[test]
    fn the_station_rows_star_saves_the_station_not_its_matched_record() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        start_playing(&mut st);
        st.radio = Some(matched_radio());
        st.hit.like_btn = Rect::new(70, 20, 9, 1);
        st.hit.save_station_btn = Rect::new(70, 24, 7, 1);

        handle_click(&mut st, Position { x: 72, y: 24 }, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::ToggleSavedStation(s)) if s.uuid == "s1"
        ));
    }

    /// The country opens the directory's page for it, and gets out of the
    /// player on the way — the page it opens is in the view the player covers.
    #[test]
    fn the_station_rows_country_opens_that_countrys_page() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.show_player = true;
        st.radio = Some(live_radio(test_station("s1", "Adroit Jazz")));
        st.hit.station_country = Rect::new(14, 24, 28, 1);

        handle_click(&mut st, Position { x: 20, y: 24 }, &tx);
        assert!(!st.show_player, "the player would cover the page it opened");
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadRadio { scope: RadioScope::Country(code) }) if code == "US"
        ));
    }

    /// The directory gave no code to ask by, so there is nothing to open. The
    /// row draws such a country inert, but a click resolved against a stale
    /// rect must not send a query for `""` either.
    #[test]
    fn a_station_with_no_country_code_opens_nothing() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        let mut station = test_station("s1", "Adroit Jazz");
        station.countrycode = String::new();
        st.radio = Some(live_radio(station));
        st.hit.station_country = Rect::new(14, 24, 28, 1);

        handle_click(&mut st, Position { x: 20, y: 24 }, &tx);
        assert!(rx.try_recv().is_err());
    }
}
