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
    self as state, AppState, ArtZoom, ArtistRow, BackTarget, ColKey, ConfirmTarget, ConfirmTrigger,
    CrumbTarget, EditField, EditTarget, HomeItem, InputMode, MainView, PICKER_ROWS, PlaylistEdit,
    RadioMatch, RadioRow, RadioScope, RadioTab, SearchTab, SpotifyState, Station, Track, TrackList,
    UpdateState, ViewKey,
};
use crate::link;

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
            // The add-to-playlist box owns every remaining key while it is
            // up: it has a field of its own, so a bare letter is a letter and
            // nothing underneath may read it as a command.
            if state.read().picker.is_some() {
                handle_picker_key(key, state, tx);
                return;
            }
            // The edit box owns the keyboard for the same reason, and it has
            // two fields to spend a letter on rather than one.
            if state.read().edit.is_some() {
                handle_edit_key(key, state, tx);
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
    // The expanded sleeve, under the help box that can draw over it. It covers
    // the screen, so any click was aimed at it and nothing under it can be what
    // the click meant.
    if st.art_zoom.is_some() {
        close_art_zoom(st);
        return;
    }
    // The article, under the boxes that own the keyboard. A click inside it is
    // someone finding their place in the text and must leave it alone; a click
    // outside is the way out, the rule the boxes below keep too.
    if st.bio.is_some() {
        if !st.hit.bio_box.contains(pos) {
            st.bio = None;
        }
        return;
    }
    // The add-to-playlist box, while one is up. Every branch returns: unlike
    // the help box above, a click that dismisses this one does *not* go on to
    // work whatever was under it — the box covers the deck's own controls, and
    // closing it is the whole of what that click meant.
    if st.picker.is_some() {
        // Before the dismiss below, for the reason the search row is checked
        // before it too: a click that missed the caret in the box you are
        // typing in must not close it and take the query with it.
        if st.hit.picker_field.contains(pos) {
            return;
        }
        if st.hit.picker_list.contains(pos) {
            click_picker_row(st, pos, tx);
            return;
        }
        if st.hit.picker_new.contains(pos) {
            open_new_playlist(st);
            return;
        }
        st.picker = None;
        return;
    }

    // The edit box, on the same terms as the picker above: every branch
    // returns, and a click that missed a caret must not close the box and
    // take the typing with it.
    if let Some(edit) = st.edit.as_ref() {
        let pending = edit.pending;
        if st.hit.edit_name.contains(pos) {
            if let Some(edit) = st.edit.as_mut().filter(|_| !pending) {
                edit.field = EditField::Name;
            }
            return;
        }
        if st.hit.edit_description.contains(pos) {
            if let Some(edit) = st.edit.as_mut().filter(|_| !pending) {
                edit.field = EditField::Description;
            }
            return;
        }
        if st.hit.edit_save.contains(pos) {
            submit_edit(st, tx);
            return;
        }
        st.edit = None;
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

    // The line about the artist in the header band. It is one sentence of
    // something longer, and clicking it is how you ask for the rest.
    if st.hit.artist_bio.contains(pos) {
        open_bio(st);
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
        select_row(st, row);
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

    // The sleeve, wherever one is drawn: it fills the screen with itself.
    // Above the main list, which an album card's own sleeve sits inside and
    // which would otherwise take the click as a row select.
    if let Some(art) = st.hit.art_at(pos) {
        let (source, seed) = (art.source.clone(), art.seed.clone());
        let cover_url = st.art_zoom_url(&source);
        st.art_zoom = Some(ArtZoom { source, seed });
        let _ = tx.send(AppCommand::LoadZoomCover { cover_url });
        return;
    }

    // Before the main list, which a failed page's control sits *inside*: the
    // pane records the whole body as the list so it still scrolls, and the
    // list's own branch swallows a click it cannot resolve to a row rather
    // than falling through.
    if st.hit.retry_btn.contains(pos) {
        retry_current_view(st, tx);
        return;
    }

    // The column header, which sits above the rows — so this branch and the
    // one below it can never resolve to each other. Same column flips the
    // arrow; a different one starts ascending.
    if let Some(key) = st
        .hit
        .column_headers
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|(_, key)| *key)
    {
        sort_by(st, key);
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
            if st.hit.main_share_col.contains(pos) {
                st.last_main_click = None;
                share_selection(st);
                return;
            }
            if st.hit.main_add_col.contains(pos) {
                st.last_main_click = None;
                add_selection_to_playlist(st, tx);
                return;
            }
            // Both tables credit artists in a column of their own, and each
            // name opens that artist rather than the row it sits on.
            if let Some(credit) = artist_link_at(&st.hit.main_artist_links, pos)
                .or_else(|| artist_link_at(&st.hit.album_artist_links, pos))
            {
                st.last_main_click = None;
                open_artist_link(st, tx, credit);
                return;
            }
            if st.hit.main_album_col.hit(pos) {
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

    // The credit line of a header band: each name opens that artist.
    if let Some(credit) = artist_link_at(&st.hit.header_artist_links, pos) {
        open_artist_link(st, tx, credit);
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

    // About the page itself, where a row's `⧉` under it is about one record on
    // the page.
    if st.hit.header_share_btn.contains(pos) {
        share_open_page(st);
        return;
    }

    if st.hit.header_save_btn.contains(pos) {
        toggle_saved_playlist(st, tx, ConfirmTrigger::Click(ConfirmTarget::HeaderUnfollow));
        return;
    }

    if st.hit.header_delete_btn.contains(pos) {
        delete_open_playlist(st, tx, ConfirmTrigger::Click(ConfirmTarget::HeaderDelete));
        return;
    }

    if st.hit.header_copy_btn.contains(pos) {
        open_playlist_copy(st);
        return;
    }

    if st.hit.header_edit_btn.contains(pos) {
        open_playlist_edit(st);
        return;
    }

    // Player-view queue: click selects, double-click plays. The row's own
    // `★` and `+` come first, as they do on the browse table.
    if st.hit.player_queue.contains(pos) {
        let index = st.player_list().list.offset() + (pos.y - st.hit.player_queue.y) as usize;
        if index < st.player_rows() {
            *st.player_list().index = index;
            if let Some(credit) = artist_link_at(&st.hit.queue_artist_links, pos) {
                st.last_queue_click = None;
                st.show_player = false;
                open_artist_link(st, tx, credit);
                return;
            }
            let like = st.hit.queue_like_col.contains(pos);
            let share = st.hit.queue_share_col.contains(pos);
            let add = st.hit.queue_add_col.contains(pos);
            if like || share || add {
                st.last_queue_click = None;
                // The row under the pointer, not the playing one: the list is
                // records you can act on, and the deck's own three two panes
                // down are what speak for what is playing. A row of a station's
                // list that Spotify could not place draws no controls at all,
                // so a click there landed on a blank cell.
                let Some(uri) = st.player_row_track(index).map(|t| t.uri.clone()) else {
                    return;
                };
                if like {
                    send_like(st, uri, tx);
                } else if share {
                    share_track(st, &uri);
                } else {
                    open_playlist_picker_for(st, uri, tx);
                }
                return;
            }
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

    // Now-playing info row: artist / album names open their pages. The record
    // may credit several artists, and the name under the pointer is the one
    // that opens.
    if let Some(credit) = artist_link_at(&st.hit.now_artist_links, pos) {
        // Before `navigate`, and unconditionally: clicking a name while
        // already on that artist's page is a no-op for the path but must
        // still get you out of the player and onto the page it names.
        st.show_player = false;
        open_deck_credit(st, tx, credit);
        return;
    }
    if st.hit.now_album.contains(pos) {
        open_deck_album(st, tx);
        return;
    }

    // The deck's context row names the queue in both views. From the bar the
    // name is the way into the player; in the player it is the heading the
    // queue hangs from, so clicking it folds that list away and back. The way
    // back out of the player is the `← <page>` pill, the status opposite the
    // mark, and `v`.
    if st.hit.queue_name.contains(pos) {
        if st.show_player {
            st.queue_folded = !st.queue_folded;
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
    // Back to the broadcast, from a record off the station's own list.
    if st.hit.radio_live_btn.contains(pos) {
        resume_station(st, tx);
        return;
    }

    // The deck's liked control is about the playing track, which is what the
    // row it sits on is about — not the selection on the page underneath.
    if st.hit.like_btn.contains(pos) {
        toggle_like_deck(st, tx);
        return;
    }
    // Its neighbours on the same row, about the same record.
    if st.hit.share_btn.contains(pos) {
        share_deck(st);
        return;
    }
    if st.hit.add_btn.contains(pos) {
        open_playlist_picker(st, tx);
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
    } else if st.hit.volume_label.contains(pos) {
        let _ = tx.send(AppCommand::ToggleMute);
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
    // The picture covers the screen and is whole, so there is nothing for the
    // wheel to move. It stops here rather than reaching the list underneath:
    // scrolling something you cannot see is worse than scrolling nothing.
    if st.art_zoom.is_some() {
        return;
    }
    // The article is the one thing on screen with more to show, so the wheel
    // is for it while it is up — and the page behind it must not move under a
    // paragraph you are halfway through.
    if st.bio.is_some() {
        scroll_bio(st, delta * SCROLL_LINES);
        return;
    }
    // The box covers the view, so the wheel belongs to it wherever it is
    // turned: scrolling a list you cannot see is worse than scrolling nothing.
    if st.picker.is_some() {
        scroll_picker(st, delta);
        cache_visible_playlists(st, tx);
        return;
    }
    if st.hit.main_list.contains(pos) {
        scroll_main(st, delta);
    } else if st.hit.player_queue.contains(pos) {
        let list = st.player_list();
        let max = list.len.saturating_sub(list.height as usize) as i64;
        let new = (list.list.offset() as i64 + delta * SCROLL_LINES).clamp(0, max);
        *list.list.offset_mut() = new as usize;
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
                // The box takes a pasted link as well as a query, because
                // Windows can route the `spotify:` scheme to an app but cannot
                // route an https host: an `open.spotify.com` URL copied out of
                // a browser has no other way in.
                match link::parse(&query) {
                    // Sent rather than navigated: the page is still a fetch
                    // away, and the client sends the open command once it
                    // knows what the link names. See `Client::open_link`.
                    Ok(target) => drop(tx.send(AppCommand::OpenLink(target))),
                    Err(link::ParseError::Unsupported(what)) => {
                        st.toast(format!("spot does not play {what}"));
                    }
                    // One box, one query, both catalogues. Which of them you
                    // meant is a tab on the results rather than a mode on the
                    // box: the old prompt pointed at Spotify or at the station
                    // directory depending on the page behind it, which meant
                    // you could not reach a station without first walking to
                    // Radio, and could not reach Spotify from there at all.
                    Err(link::ParseError::NotALink) => {
                        // Before the tab reset, so the Home frame the path is
                        // reset to keeps the tab it was pushed with.
                        navigate(&mut st, AppCommand::Search(query), tx);
                        // The first tab the strip has: Tracks with Spotify
                        // behind the box, and Stations when the directory is
                        // the whole of it.
                        st.search_tab = st.search_tabs()[0];
                    }
                }
            }
        }
        KeyCode::Backspace => {
            st.input_buffer.pop();
        }
        KeyCode::Char(c) => st.input_buffer.push(c),
        _ => {}
    }
}

/// Keys while the add-to-playlist box is open.
///
/// Arrows rather than `j`/`k`: the box has a text field, so a letter has to
/// stay a letter. Everything it does not name falls through to nothing, which
/// is what keeps `q` and `?` from reaching the app behind it.
fn handle_picker_key(
    key: KeyEvent,
    state: &Arc<RwLock<AppState>>,
    tx: &UnboundedSender<AppCommand>,
) {
    let mut st = state.write();
    let rows = st.picker_rows().len();
    match key.code {
        KeyCode::Esc => {
            st.picker = None;
            return;
        }
        KeyCode::Enter => {
            toggle_picker_row(&mut st, tx);
            return;
        }
        // A bare letter is query text here, so the one key that leaves the box
        // has to be a chord.
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            open_new_playlist(&mut st);
            return;
        }
        _ => {}
    }
    let Some(picker) = st.picker.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Up => {
            picker.selected = picker.selected.saturating_sub(1);
            picker.offset = picker.offset.min(picker.selected);
        }
        KeyCode::Down => {
            picker.selected = (picker.selected + 1).min(rows.saturating_sub(1));
            let last = picker.offset + PICKER_ROWS - 1;
            if picker.selected > last {
                picker.offset = picker.selected + 1 - PICKER_ROWS;
            }
        }
        KeyCode::Backspace => {
            picker.query.pop();
            picker.selected = 0;
            picker.offset = 0;
        }
        KeyCode::Char(c) => {
            picker.query.push(c);
            picker.selected = 0;
            picker.offset = 0;
        }
        _ => return,
    }
    cache_visible_playlists(&st, tx);
}

/// Longest name and blurb Spotify takes. Enforced here rather than trusted to
/// the API, so the refusal is a key that does nothing rather than a round trip
/// that comes back with the whole edit rejected.
const MAX_PLAYLIST_NAME: usize = 100;
const MAX_PLAYLIST_DESCRIPTION: usize = 300;

/// Keys while the edit box is up. Modal, like [`handle_picker_key`]: both
/// fields take bare letters, so nothing underneath may read one as a command.
fn handle_edit_key(key: KeyEvent, state: &Arc<RwLock<AppState>>, tx: &UnboundedSender<AppCommand>) {
    let mut st = state.write();
    match key.code {
        KeyCode::Esc => {
            st.edit = None;
            return;
        }
        KeyCode::Enter => {
            submit_edit(&mut st, tx);
            return;
        }
        _ => {}
    }
    let Some(edit) = st.edit.as_mut() else {
        return;
    };
    // A change in flight owns the text it was sent with; letting it be typed
    // over would leave the box disagreeing with what was asked for.
    if edit.pending {
        return;
    }
    match key.code {
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
            edit.field = edit.field.other();
        }
        KeyCode::Backspace => {
            match edit.field {
                EditField::Name => edit.name.pop(),
                EditField::Description => edit.description.pop(),
            };
        }
        KeyCode::Char(c) => match edit.field {
            EditField::Name if edit.name.chars().count() < MAX_PLAYLIST_NAME => edit.name.push(c),
            EditField::Description
                if edit.description.chars().count() < MAX_PLAYLIST_DESCRIPTION =>
            {
                edit.description.push(c)
            }
            _ => {}
        },
        _ => {}
    }
}

/// Open the edit box for the playlist page on screen.
///
/// Only your own: Spotify refuses the change for any other, and a box that
/// takes an edit it cannot deliver is worse than no box.
fn open_playlist_edit(st: &mut AppState) {
    if !st.owns_open_playlist() {
        return;
    }
    let Some(id) = st.open_playlist_id().map(str::to_string) else {
        return;
    };
    let MainView::Tracks(list) = &st.main else {
        return;
    };
    let (name, description) = (list.header.name.clone(), list.header.description.clone());
    st.edit_seq += 1;
    st.edit = Some(PlaylistEdit {
        target: EditTarget::Existing(id),
        name,
        description,
        field: EditField::Name,
        pending: false,
        error: None,
        seq: st.edit_seq,
    });
}

/// Trade the add-to-playlist box for the edit box in its create mode, carrying
/// the record and the name that was typed looking for one.
///
/// The picker goes: it owns the keyboard wherever both are open, so leaving it
/// up would leave the new box unable to be typed in.
fn open_new_playlist(st: &mut AppState) {
    let Some(picker) = st.picker.take() else {
        return;
    };
    st.edit_seq += 1;
    st.edit = Some(PlaylistEdit {
        target: EditTarget::New { uri: picker.uri },
        name: picker
            .query
            .trim()
            .chars()
            .take(MAX_PLAYLIST_NAME)
            .collect(),
        description: String::new(),
        field: EditField::Name,
        pending: false,
        error: None,
        seq: st.edit_seq,
    });
}

/// The header's `copy` control: open the edit box on a playlist of your own
/// that does not exist yet.
///
/// The one control a playlist you cannot edit still offers, which is the point
/// of it — a copy is how someone else's playlist becomes yours.
fn open_playlist_copy(st: &mut AppState) {
    let Some(id) = st.open_playlist_id().map(str::to_string) else {
        return;
    };
    // Asked before the box opens rather than after a name is typed: a refusal
    // is worth less the more work it throws away.
    if let Err(why) = copyable_tracks(st) {
        st.toast(why);
        return;
    }
    let MainView::Tracks(list) = &st.main else {
        return;
    };
    let name = format!("{} (copy)", list.header.name)
        .chars()
        .take(MAX_PLAYLIST_NAME)
        .collect();
    let description = list.header.description.clone();
    st.edit_seq += 1;
    st.edit = Some(PlaylistEdit {
        target: EditTarget::Copy { source_id: id },
        name,
        description,
        field: EditField::Name,
        pending: false,
        error: None,
        seq: st.edit_seq,
    });
}

/// The open playlist's records in playlist order, when spot can see all of
/// them.
///
/// A copy is written from the URIs spot holds, and a [`TrackList`] holds only
/// the rows `Api::playlist_tracks_page` could parse — a local file is not one
/// of them. Copying a list spot cannot see the whole of would make a record
/// quietly short of the one it claims to be, so it is refused instead.
fn copyable_tracks(st: &AppState) -> Result<Vec<String>, &'static str> {
    let MainView::Tracks(list) = &st.main else {
        return Err("there is no playlist here to copy");
    };
    if list.loading {
        return Err("still reading this playlist — copy it when the rows stop arriving");
    }
    if list
        .total
        .is_some_and(|total| total as usize != list.items.len())
    {
        return Err("spot cannot see every item on this playlist, so a copy would be short");
    }
    if list.items.is_empty() {
        return Err("there is nothing on this playlist to copy");
    }
    Ok(list.items.iter().map(|track| track.uri.clone()).collect())
}

/// Send what the box holds, and hold it inert until the answer lands.
fn submit_edit(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(edit) = st.edit.as_mut() else {
        return;
    };
    if edit.pending {
        return;
    }
    // A playlist has to be called something, and Spotify refuses a blank name
    // rather than keeping the old one.
    if edit.name.trim().is_empty() {
        edit.error = Some("a playlist needs a name".to_string());
        return;
    }
    let target = edit.target.clone();
    let name = edit.name.trim().to_string();
    let description = edit.description.trim().to_string();
    let seq = edit.seq;

    // A copy is written from the page behind the box, which the borrow above
    // gives up.
    let command = match target {
        EditTarget::Existing(id) => AppCommand::EditPlaylistDetails {
            id,
            name,
            description,
            seq,
        },
        EditTarget::New { uri } => AppCommand::CreatePlaylist {
            name,
            description,
            uri,
            seq,
        },
        EditTarget::Copy { .. } => match copyable_tracks(st) {
            Ok(uris) => AppCommand::CopyPlaylist {
                name,
                description,
                uris,
                seq,
            },
            Err(why) => {
                if let Some(edit) = st.edit.as_mut() {
                    edit.error = Some(why.to_string());
                }
                return;
            }
        },
    };
    if let Some(edit) = st.edit.as_mut() {
        edit.pending = true;
        edit.error = None;
    }
    let _ = tx.send(command);
}

/// `F` / the header's control: put the open playlist in the library, or take
/// it out.
///
/// Silent on a playlist you own — the control is not drawn there, and the key
/// must agree with it. Unfollowing your own playlist is how Spotify spells
/// deleting it, which is not what one keypress should mean.
fn toggle_saved_playlist(
    st: &mut AppState,
    tx: &UnboundedSender<AppCommand>,
    trigger: ConfirmTrigger,
) {
    if st.owns_open_playlist() {
        return;
    }
    let Some(id) = st.open_playlist_id().map(str::to_string) else {
        return;
    };
    // Unknown is not "no": until the check answers, there is nothing to flip
    // to that would not be a guess at what the library already holds.
    let Some(saved) = st.saved_playlists.get(&id).copied() else {
        return;
    };
    // The flip itself is the client's, as it is for a track's `★` — see
    // `send_like`. Doing it here as well would only be the same write twice.
    if !saved {
        let _ = tx.send(AppCommand::SetPlaylistSaved { id, saved: true });
        return;
    }
    let name = state::view_title(&st.main);
    ask_again(
        st,
        tx,
        trigger,
        format!("unfollow {name}"),
        AppCommand::SetPlaylistSaved { id, saved: false },
    );
}

/// `d` and the header's delete control: take a playlist you own out of the
/// library, which is how Spotify spells deleting it.
fn delete_open_playlist(
    st: &mut AppState,
    tx: &UnboundedSender<AppCommand>,
    trigger: ConfirmTrigger,
) {
    if !st.owns_open_playlist() {
        return;
    }
    let Some(id) = st.open_playlist_id().map(str::to_string) else {
        return;
    };
    let name = state::view_title(&st.main);
    ask_again(
        st,
        tx,
        trigger,
        format!("delete {name}"),
        AppCommand::SetPlaylistSaved { id, saved: false },
    );
}

/// Send a write that was already asked for, or arm it and say what asking
/// again would do.
///
/// `verb` names the write in the imperative, so the prompt reads as one
/// sentence — "press d again to delete LATE NIGHTS · Esc to cancel".
fn ask_again(
    st: &mut AppState,
    tx: &UnboundedSender<AppCommand>,
    trigger: ConfirmTrigger,
    verb: String,
    command: AppCommand,
) {
    if let Some(armed) = st.take_armed(trigger) {
        let _ = tx.send(armed);
        return;
    }
    let ask = match trigger {
        ConfirmTrigger::Key(key) => format!("press {key} again"),
        ConfirmTrigger::Click(_) => "click again".to_string(),
    };
    st.arm(trigger, format!("{ask} to {verb} · Esc to cancel"), command);
}

/// Ask for the contents of any row on screen that has not been walked yet.
///
/// The prefetch at sign-in normally leaves nothing to do here, so this is the
/// catch: a playlist made since that load, or one whose walk failed, is asked
/// about when it comes into view. The whole window goes every time and the
/// client drops what it already holds — one place decides what is visible
/// ([`AppState::picker_visible`]) and one place decides what is worth asking.
fn cache_visible_playlists(st: &AppState, tx: &UnboundedSender<AppCommand>) {
    let playlist_ids: Vec<String> = st
        .picker_visible()
        .into_iter()
        .filter(|i| st.picker_has(&st.playlists[*i].id).is_none())
        .map(|i| st.playlists[i].id.clone())
        .collect();
    if playlist_ids.is_empty() {
        return;
    }
    let _ = tx.send(AppCommand::CachePlaylistTracks { playlist_ids });
}

/// Open the add-to-playlist box for whatever the deck is about.
///
/// Reads [`AppState::deck_track`] rather than `playback`, as the liked control
/// beside it does, so a station's matched record is the subject where there is
/// one.
fn open_playlist_picker(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(uri) = st.deck_track().map(|t| t.uri.clone()) else {
        return radio_has_no_track(st, "track");
    };
    open_playlist_picker_for(st, uri, tx);
}

/// Open the box for one named record. The deck's control resolves the record
/// first and a track row already knows its own, so the resolution stays with
/// the caller and the box only ever handles a URI it has been handed.
fn open_playlist_picker_for(st: &mut AppState, uri: String, tx: &UnboundedSender<AppCommand>) {
    // The box short-circuits above the branch that cancels a half-typed
    // search, so it has to do that itself — as the mark and the status do.
    st.input_mode = InputMode::Normal;
    st.input_buffer.clear();
    st.picker_seq += 1;
    st.picker = Some(state::PlaylistPicker {
        order: st.picker_order(&uri),
        uri,
        query: String::new(),
        selected: 0,
        offset: 0,
        pending: Default::default(),
        error: None,
        seq: st.picker_seq,
    });
    cache_visible_playlists(st, tx);
}

/// Walk the box's window without moving its selection — the wheel is how you
/// look, and the selection is what you have chosen.
fn scroll_picker(st: &mut AppState, delta: i64) {
    let rows = st.picker_rows().len();
    let Some(picker) = st.picker.as_mut() else {
        return;
    };
    let max = rows.saturating_sub(PICKER_ROWS) as i64;
    picker.offset = (picker.offset as i64 + delta * SCROLL_LINES).clamp(0, max) as usize;
}

/// A click on one of the box's rows: select it and flip it, so one click is
/// the whole gesture. The rows are one line each, so the row is the line.
fn click_picker_row(st: &mut AppState, pos: Position, tx: &UnboundedSender<AppCommand>) {
    let line = (pos.y - st.hit.picker_list.y) as usize;
    let rows = st.picker_rows().len();
    let Some(picker) = st.picker.as_mut() else {
        return;
    };
    let row = picker.offset + line;
    if row >= rows {
        return;
    }
    picker.selected = row;
    toggle_picker_row(st, tx);
}

/// Put the box's record on the selected playlist, or take it off — whichever
/// the row is not already.
///
/// The box stays up either way: the rows are checkboxes, so one visit can
/// change several of them, and a box that closed on the first pick would make
/// the second a whole trip through the control again.
fn toggle_picker_row(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let rows = st.picker_rows();
    let Some(picker) = st.picker.as_ref() else {
        return;
    };
    let Some(playlist_id) = rows
        .get(picker.selected)
        .map(|i| st.playlists[*i].id.clone())
    else {
        return;
    };
    // A row already mid-change has nothing to flip to that is not the flip
    // already out; a second press would put the record on twice.
    if picker.pending.contains(&playlist_id) {
        return;
    }
    // A row whose mark is still unknown is inert. Reading it as "not on the
    // playlist" and adding is what the liked control does with an unknown
    // track, but a wrong guess there costs a duplicate on a real playlist,
    // where a wrong guess about liked costs nothing — the box says
    // "checking…" and this is over in a moment.
    let Some(on) = st.picker_has(&playlist_id) else {
        return;
    };
    let uri = picker.uri.clone();
    let seq = picker.seq;
    // Flipped before the request goes out so the row answers the press; the
    // client puts it back if the change is refused. The row stays where it
    // is: the order was settled when the box opened.
    if let Some(contents) = st.playlist_tracks.get_mut(&playlist_id) {
        let id = state::track_id(&uri).to_string();
        if on {
            contents.track_ids.remove(&id);
        } else {
            contents.track_ids.insert(id);
        }
    }
    if let Some(picker) = st.picker.as_mut() {
        picker.pending.insert(playlist_id.clone());
        picker.error = None;
    }
    let _ = tx.send(AppCommand::SetOnPlaylist {
        playlist_id,
        uri,
        on: !on,
        seq,
    });
}

/// Close the expanded sleeve and drop the screen-sized cover it was reading.
///
/// The cover is the largest single thing the app holds; keeping it against a
/// second look at the same sleeve is what `Client::zoom_covers` is for.
fn close_art_zoom(st: &mut AppState) {
    st.art_zoom = None;
    st.zoom_cover = None;
    // Orphan a fetch still in flight, the way opening a second sleeve would:
    // it has nothing left to fill, and letting it land would hold the biggest
    // buffer the app allocates for as long as nothing else claimed the slot.
    st.zoom_cover_generation += 1;
}

/// The keyboard while a sleeve fills the screen. Returns whether the key was
/// spent here.
///
/// An allowlist for what falls through, not a blocklist for what is caught:
/// every key not named below acts on a screen nobody can see. Transport, help
/// and quit are about the record and the app rather than about the screen
/// behind the picture, so they still mean what they mean.
fn handle_zoom_key(key: KeyEvent, state: &Arc<RwLock<AppState>>) -> bool {
    {
        let st = state.read();
        // The help box draws over the picture, so its own Esc comes first.
        if st.art_zoom.is_none() || st.show_help {
            return false;
        }
    }
    match key.code {
        KeyCode::Char(' ' | 'n' | 'p' | 'h' | 'l' | 's' | 'q' | '?' | '-' | '=' | '+') => false,
        KeyCode::Esc => {
            close_art_zoom(&mut state.write());
            true
        }
        // Everything else is navigation of a screen nobody can see. The
        // picture is whole, so there is nothing here for it to move either.
        _ => true,
    }
}

/// The keyboard while an article is open. Returns whether the key was spent
/// here.
///
/// The same allowlist the expanded sleeve keeps, and for the same reason:
/// transport, help and quit are about the record and about the app rather than
/// about the page behind the box, so they still mean what they mean. What is
/// caught here that the picture does not catch is scrolling — this surface has
/// more to show, and the page underneath must not move while you read.
fn handle_bio_key(key: KeyEvent, state: &Arc<RwLock<AppState>>) -> bool {
    {
        let st = state.read();
        // Under the help box, and under the sleeve, on their own terms.
        if st.bio.is_none() || st.show_help || st.art_zoom.is_some() {
            return false;
        }
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut st = state.write();
    let page = (st.hit.bio_body.height as i64).max(1);
    match key.code {
        KeyCode::Char(' ' | 'n' | 'p' | 'h' | 'l' | 's' | 'q' | '?' | '-' | '=' | '+') => false,
        // `i` opened it, so `i` closes it: a key that only ever opens is a key
        // you have to remember a second one for.
        KeyCode::Esc | KeyCode::Char('i') => {
            st.bio = None;
            true
        }
        KeyCode::Char('j') | KeyCode::Down => {
            scroll_bio(&mut st, 1);
            true
        }
        KeyCode::Char('k') | KeyCode::Up => {
            scroll_bio(&mut st, -1);
            true
        }
        KeyCode::Char('d') if ctrl => {
            scroll_bio(&mut st, page / 2);
            true
        }
        KeyCode::Char('u') if ctrl => {
            scroll_bio(&mut st, -page / 2);
            true
        }
        KeyCode::PageDown => {
            scroll_bio(&mut st, page);
            true
        }
        KeyCode::PageUp => {
            scroll_bio(&mut st, -page);
            true
        }
        KeyCode::Char('g') => {
            scroll_bio(&mut st, i64::MIN / 2);
            true
        }
        KeyCode::Char('G') => {
            scroll_bio(&mut st, i64::MAX / 2);
            true
        }
        // Everything else is navigation of a page you cannot see.
        _ => true,
    }
}

/// Move the article by `delta` lines, keeping its last line at the foot of the
/// box.
///
/// The wrap the offset counts is the one the last frame made, so a terminal
/// resized under the box scrolls by the measure it is showing rather than by
/// the one it was opened at.
fn scroll_bio(st: &mut AppState, delta: i64) {
    let height = st.hit.bio_body.height as usize;
    let Some(popup) = st.bio.as_mut() else { return };
    let max = popup.lines.len().saturating_sub(height) as i64;
    popup.offset = (popup.offset as i64).saturating_add(delta).clamp(0, max) as usize;
}

/// `i`, and the line about the artist in the header band: open the article.
///
/// A deliberate press gets an answer either way. The band's own line is simply
/// absent where there is nothing to say — that is how `genres` behaves and how
/// a page should — but a key that did nothing would read as a key that is
/// broken.
fn open_bio(st: &mut AppState) {
    let MainView::Artist(v) = &st.main else {
        return;
    };
    let (id, name, bio) = (v.id.clone(), v.name.clone(), v.bio.clone());
    match bio {
        state::BioState::Ready(bio) => st.bio = Some(state::BioPopup::new(id, name, bio)),
        state::BioState::Loading => st.toast("still reading about them"),
        state::BioState::Missing => st.toast("nothing written about this one"),
    }
}

fn handle_normal(key: KeyEvent, state: &Arc<RwLock<AppState>>, tx: &UnboundedSender<AppCommand>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Before the player, whose own Esc closes the player: with a sleeve up over
    // it, that would leave the picture on a screen that had changed underneath.
    if handle_zoom_key(key, state) {
        return;
    }
    // Under the sleeve and above everything else, for the same reason.
    if handle_bio_key(key, state) {
        return;
    }
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
            } else if st.confirm.take().is_some() {
                // An armed write is the nearest thing to something open, and
                // Esc is the key its own prompt names.
            } else if st.links.confirming.take().is_some() {
                // The armed Links row, for the same reason.
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
        KeyCode::Char('s') if on_air(&state.read()) => {
            state
                .write()
                .toast("radio is live — there is no queue to shuffle");
        }
        KeyCode::Char('h') | KeyCode::Char('l') if on_air(&state.read()) => {
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
        KeyCode::Char('F') => {
            toggle_saved_playlist(&mut state.write(), tx, ConfirmTrigger::Key('F'));
        }
        KeyCode::Char('E') => open_playlist_edit(&mut state.write()),
        KeyCode::Char('C') => open_playlist_copy(&mut state.write()),
        KeyCode::Char('d') => {
            delete_open_playlist(&mut state.write(), tx, ConfirmTrigger::Key('d'))
        }

        KeyCode::Char('b') => open_album_of_selection(&mut state.write(), tx),
        KeyCode::Char('B') => open_artist_of_selection(&mut state.write(), tx),
        KeyCode::Char('i') => open_bio(&mut state.write()),
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
    st.confirm = None;
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
        // The field is only on screen here, so the key that changes what it
        // shows lives here too. The toast is the only thing naming the mode —
        // the field itself carries no label.
        KeyCode::Char('V') => {
            let mut st = state.write();
            let mode = st.viz.cycle();
            st.toast(format!("visualizer: {}", mode.label()));
        }
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
        KeyCode::Char(
            '/' | '1' | '2' | 'b' | 'B' | 'o' | 'O' | 'a' | 'x' | 'F' | 'E' | 'C' | 'd' | '[' | ']',
        )
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
    let list = st.player_list();
    if list.len == 0 {
        return;
    }
    *list.index = (*list.index as i64 + delta).clamp(0, list.len as i64 - 1) as usize;
    queue_snap(st);
}

fn queue_set(st: &mut AppState, index: usize) {
    let list = st.player_list();
    if list.len == 0 {
        return;
    }
    *list.index = index.min(list.len - 1);
    queue_snap(st);
}

/// Half the player list's visible height (from last frame's hit rect),
/// falling back to 10 before the first draw.
fn queue_half_page(st: &AppState) -> i64 {
    let height = st.hit.player_queue.height as i64;
    if height == 0 { 10 } else { (height / 2).max(1) }
}

/// Bring the player's list back to its selection after a keyboard move.
fn queue_snap(st: &mut AppState) {
    let list = st.player_list();
    let height = list.height as usize;
    if height == 0 {
        return;
    }
    let index = *list.index;
    if index < list.list.offset() {
        *list.list.offset_mut() = index;
    } else if index >= list.list.offset() + height {
        *list.list.offset_mut() = index + 1 - height;
    }
}

/// Enter / double-click in the player's list: play the selected row.
///
/// Off the queue that is one instant command with no API behind it — spot owns
/// the play order. Under a station it is [`play_from_heard`], which has a
/// record to find first.
fn play_from_queue(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    if st.radio.is_some() {
        return play_from_heard(st, tx);
    }
    if st.queue_index < st.queue_len() {
        let _ = tx.send(AppCommand::JumpTo(st.queue_index));
    }
}

/// Enter on a row of the station's list: play what Spotify had for it.
///
/// A row the station only named has nothing to play, and says so the way every
/// other radio deck control does.
fn play_from_heard(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    let row = st.heard_index;
    let matched = match st.heard().get(row) {
        Some(heard) => heard.matched.clone(),
        None => return,
    };
    // The newest row is the record the station is on: asking for it means the
    // broadcast, not a copy of it started over from the beginning while the
    // station plays on somewhere in the middle.
    if row + 1 == st.heard().len() {
        return resume_station(st, tx);
    }
    if matched.track().is_none() {
        return say_no_track(st, &matched, "track");
    }
    let _ = tx.send(AppCommand::PlayHeard { row });
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

    // A search is a fresh start rather than one more step: the pages walked
    // through to reach it have nothing to do with the query, and keeping them
    // leaves Esc retracing a path the results are not part of. The command
    // always goes out — the new query has to win.
    if target == ViewKey::Search {
        st.reset_to_home();
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
pub(crate) fn navigate(st: &mut AppState, cmd: AppCommand, tx: &UnboundedSender<AppCommand>) {
    // Leaving the screen disarms the Links row for the same reason leaving the
    // row does: its second press claims the `spotify:` scheme, and coming back
    // to a page has to ask again. An armed write goes with it — every one of
    // them is about the page being left.
    st.links.confirming = None;
    st.confirm = None;
    if make_way(st, target_key(&cmd), tx) {
        // The count belongs to the page you were on, not to the one opening.
        st.retries = 0;
        let _ = tx.send(cmd);
    }
}

/// Open the page a link names, as a fresh path.
///
/// A link is the same gesture as a query — both are typed into the one box —
/// so it starts where a search starts rather than stacking onto whichever page
/// happened to be open. A link arriving from outside spot, off the `spotify:`
/// scheme or off the command line, lands here for the same reason: it is not a
/// step off the page that was open.
///
/// Nothing is pushed and nothing is walked back to, so this does not go through
/// [`make_way`]: the page the link names is the whole of what it asked for.
pub(crate) fn navigate_from_link(
    st: &mut AppState,
    cmd: AppCommand,
    tx: &UnboundedSender<AppCommand>,
) {
    st.links.confirming = None;
    st.confirm = None;
    st.reset_to_home();
    st.retries = 0;
    let _ = tx.send(cmd);
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
    // Going back is arriving at a page, so the count starts over here too, and
    // an armed write about the page being left goes with it.
    st.retries = 0;
    st.confirm = None;
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
                if let Some(id) = key.strip_prefix(state::PLAYLIST_KEY_PREFIX) {
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

/// Put the selection on `index`, and disarm whatever the row being left had
/// armed.
///
/// Only the Links row arms anything, and its second press is what claims the
/// `spotify:` scheme — so the arming has to belong to that row rather than to
/// the screen. Walking away and coming back must ask again.
fn select_row(st: &mut AppState, index: usize) {
    if st.main_index != index {
        st.links.confirming = None;
        st.confirm = None;
    }
    st.main_index = index;
}

fn move_selection(st: &mut AppState, delta: i64) {
    let len = st.main_len();
    if len == 0 {
        return;
    }
    select_row(
        st,
        (st.main_index as i64 + delta).clamp(0, len as i64 - 1) as usize,
    );
    snap_to_selection(st);
}

fn set_selection(st: &mut AppState, index: usize) {
    let len = st.main_len();
    if len == 0 {
        return;
    }
    select_row(st, index.min(len - 1));
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

/// The sort of the table on screen, when the page has one to read or write.
///
/// One resolution for every table, so `o`, `O` and a click on a header all
/// reach the same field. The Playlists page's sort is the odd one out — it
/// lives on the state rather than on its view, because [`MainView::Playlists`]
/// carries no data at all.
fn main_sort(st: &mut AppState) -> Option<&mut state::Sort> {
    let tab = st.search_tab;
    match &mut st.main {
        MainView::Playlists => Some(&mut st.playlists_sort),
        MainView::Tracks(list) => Some(&mut list.sort),
        MainView::Radio(v) => Some(&mut v.rows.sort),
        MainView::Search(r) => Some(match tab {
            SearchTab::Tracks => &mut r.tracks.sort,
            SearchTab::Albums => &mut r.albums.sort,
            SearchTab::Artists => &mut r.artists.sort,
            SearchTab::Playlists => &mut r.playlists.sort,
            SearchTab::Stations => &mut r.stations.sort,
        }),
        // The page has two lists and one header; the header is the top
        // tracks', so that is the one the keys and the click reach.
        MainView::Artist(v) => Some(&mut v.top.sort),
        MainView::Home => None,
    }
}

/// Re-cut the view for a sort the user just asked for.
///
/// The list goes back to the top rather than following the row that was
/// selected. A sort is asked for in order to read the list from its new
/// start, and jumping to wherever the old selection landed hides exactly
/// that — half the point of pressing the key.
fn apply_sort(st: &mut AppState) {
    st.resort_main();
    st.main_to_top();
}

/// Order the table on screen by `key`.
///
/// One column clicked over and over runs ascending, descending, then off —
/// back to the order the source sent. Without that last step the only way out
/// of a sort is to remember which key clears it, and there is nothing on
/// screen that says.
fn sort_by(st: &mut AppState, key: ColKey) {
    let Some(sort) = main_sort(st) else { return };
    *sort = match (sort.key == key, sort.ascending) {
        (true, true) => state::Sort {
            key,
            ascending: false,
        },
        (true, false) => state::Sort::default(),
        (false, _) => state::Sort {
            key,
            ascending: true,
        },
    };
    apply_sort(st);
}

/// `o`: step to the next sortable column of the table on screen.
///
/// Read off the header that was last drawn, so it covers every table and
/// never names a column the pane is too narrow to show.
fn cycle_sort(st: &mut AppState) {
    let keys = st.hit.sort_keys.clone();
    if keys.is_empty() {
        return;
    }
    let Some(sort) = main_sort(st) else { return };
    let next = match keys.iter().position(|&k| k == sort.key) {
        Some(i) => keys[(i + 1) % keys.len()],
        None => keys[0],
    };
    sort.key = next;
    sort.ascending = true;
    apply_sort(st);
}

/// `O`: flip the sort direction.
///
/// Including in fetch order, where it reads the list from the bottom up —
/// the `#` column has a direction like any other now that the number stays
/// with its track.
fn flip_sort(st: &mut AppState) {
    let Some(sort) = main_sort(st) else { return };
    sort.ascending = !sort.ascending;
    apply_sort(st);
}

/// ←/→: switch tabs on the three tabbed views.
///
/// Search's tabs are five cuts of one query already answered, so switching is
/// free — Stations came from a second catalogue, but it was asked at the same
/// moment and is in hand by the time you reach it. The artist page's album
/// groups are the same kind of thing: one fetch brought all four back. Radio's
/// are four different queries, so switching *navigates* — the new tab is its
/// own page, and Esc walks back to the chart it hangs from. See
/// [`open_radio_tab`].
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
            let tabs = st.search_tabs();
            let pos = tabs.iter().position(|t| *t == st.search_tab).unwrap_or(0) as i64;
            let n = tabs.len() as i64;
            st.search_tab = tabs[((pos + delta).rem_euclid(n)) as usize];
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

/// The Home row that decides where a clicked Spotify link opens.
///
/// Claiming the scheme takes two presses when there is another app to
/// displace: the first arms the row and names what it would replace, the
/// second does it. Giving the scheme back takes one — undo must never be
/// harder than the act. Nothing else in spot reaches
/// [`crate::protocol::register`], which is what keeps the claim to something
/// the user asked for twice.
#[cfg(windows)]
fn toggle_links(st: &mut AppState) {
    use crate::protocol::{self, Holder};

    if st.links.in_force {
        match protocol::unregister() {
            Ok(()) => {
                protocol::refresh(st);
                st.toast("Spotify links go back to where they were".to_string());
            }
            Err(e) => {
                log::error!("could not give the Spotify links back: {e:#}");
                st.toast("could not give the Spotify links back".to_string());
            }
        }
        return;
    }

    // Read now rather than trusted from the last draw: this press is about to
    // overwrite whatever is there, so what is there has to be current.
    let displaced = match protocol::status().holder {
        Holder::Other(app) => Some(app),
        Holder::Spot | Holder::Nobody => None,
    };
    let armed = st.links.confirming.take().is_some();
    if let Some(app) = &displaced
        && !armed
    {
        st.links.confirming = Some(format!(
            "Enter again to replace {app} · Esc to leave it alone"
        ));
        return;
    }

    // `force` only where the first press already said which app it displaces.
    match protocol::register(displaced.is_some()) {
        Ok(now) => {
            let outranked = !now.in_force();
            protocol::refresh(st);
            if outranked {
                // Written, and overruled by Windows' own default-apps answer.
                // Saying so is the whole point: a registration that silently
                // does nothing is the worst outcome this row has.
                st.toast(
                    "Windows keeps its own answer for Spotify links — set spot in Settings › Apps › Default apps"
                        .to_string(),
                );
            } else {
                st.toast("Spotify links now open in spot".to_string());
            }
        }
        Err(e) => {
            log::error!("could not claim the Spotify links: {e:#}");
            st.toast(format!("could not claim the Spotify links: {e}"));
        }
    }
}

#[cfg(not(windows))]
fn toggle_links(_st: &mut AppState) {}

/// Open a radio tab, from the strip or from ←/→.
///
/// The four tabs are peers under the chart rather than steps off it: whichever
/// one is clicked, and however deep in the directory the click comes from, the
/// path it leaves is `radio › <tab>`. Switching tabs four times leaves one step
/// back rather than four. Popular *is* the chart, so it walks back to it and
/// opens nothing.
fn open_radio_tab(st: &mut AppState, tab: RadioTab, tx: &UnboundedSender<AppCommand>) {
    let scope = tab.scope();
    let target = ViewKey::Radio(state::radio_key(&scope));
    // A tab already on the path is reached by walking back to it — `navigate`
    // does that below — and the frame waiting there still holds its rows.
    let on_path = st
        .view_stack
        .iter()
        .any(|snap| state::view_key(&snap.view).as_ref() == Some(&target));
    if !on_path {
        rewind_to_chart(st);
    }
    navigate(st, AppCommand::LoadRadio { scope }, tx);
}

/// Put the chart back under the cursor, so the tab about to open sits directly
/// on it.
///
/// No [`after_pop`]: nothing is being arrived at unless the chart is itself
/// where the click leads, and the chart brings its own rows and draws no view
/// cover. A path with no chart on it — a radio page opened without going
/// through the directory's front door — is left where it is.
fn rewind_to_chart(st: &mut AppState) {
    if is_chart(&st.main) {
        return;
    }
    let Some(depth) = st.view_stack.iter().position(|snap| is_chart(&snap.view)) else {
        return;
    };
    st.pop_to(depth);
    st.retries = 0;
}

fn is_chart(view: &MainView) -> bool {
    matches!(view, MainView::Radio(v) if v.scope == RadioScope::Popular)
}

/// Enter / click: drill into the selected row, or play it.
fn activate_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    // A page that was refused has no row for Enter to mean anything else by,
    // so Enter is its `↻ try again` — the keyboard's half of the control the
    // pane draws.
    if retry_current_view(st, tx) {
        return;
    }
    // Home and Playlists are navigation rather than playback, and both push
    // the page they leave so the pill on the next one leads back to it.
    match &st.main {
        MainView::Home => {
            let Some(item) = st.home_items().get(st.main_index).copied() else {
                return;
            };
            match item {
                // The row is a control, not a destination: it stays on Home
                // and reports through its own blurb. The client does the work
                // and decides whether the press means anything.
                HomeItem::Update => match st.update {
                    Some(UpdateState::Installed) => {
                        st.restart_request = true;
                        st.should_quit = true;
                    }
                    _ => {
                        let _ = tx.send(AppCommand::InstallUpdate);
                    }
                },
                // Also a control rather than a destination, and the only one
                // that reaches outside spot — see [`toggle_links`].
                HomeItem::Links => toggle_links(st),
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
                // The row asks; the frame loop answers, because the sign-in
                // needs the terminal back for the browser flow. Pressed while
                // one is already running, this does nothing.
                HomeItem::Spotify => {
                    if st.spotify != SpotifyState::Connecting {
                        st.connect_request = true;
                        st.spotify = SpotifyState::Connecting;
                    }
                }
            }
            return;
        }
        MainView::Playlists => {
            let Some(id) = st.playlist_row(st.main_index).map(|p| p.id.clone()) else {
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
            // Display order, not fetch order: once the tab sorts, what you see
            // is the play order the queue is built from.
            SearchTab::Tracks => results.tracks.get(index).map(|_| AppCommand::Play {
                tracks: results.tracks.rows().cloned().collect(),
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
            // Opened, not played — the same gesture every other row here
            // answers to. `x` is what plays one; see `play_without_opening`.
            SearchTab::Playlists => {
                results
                    .playlists
                    .get(index)
                    .map(|p| AppCommand::LoadPlaylistTracks {
                        playlist_id: p.id.clone(),
                    })
            }
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
    let natural = list.sort.is_natural();
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
    list.rows().cloned().collect()
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

/// Why the page on screen has nothing on it, when the reason is a refusal.
///
/// The Stations tab of a search answers for the directory and the other four
/// for Spotify, because that is which host each of them asked.
fn current_load_error(st: &AppState) -> Option<&state::LoadError> {
    // A page with rows on it is a page that answered, whatever else went
    // wrong later: the control is only ever drawn over a blank pane.
    if st.main_len() > 0 {
        return None;
    }
    match &st.main {
        MainView::Playlists => st.playlists_error.as_ref(),
        MainView::Tracks(list) => list.error.as_ref(),
        MainView::Artist(v) => v.error.as_ref(),
        MainView::Radio(v) => v.error.as_ref(),
        MainView::Search(results) => match st.search_tab {
            SearchTab::Stations => results.stations_error.as_ref(),
            _ => results.error.as_ref(),
        },
        MainView::Home => None,
    }
}

/// Ask for the failed page again.
///
/// Sent bare rather than through [`navigate`]: the page is already on screen,
/// and pushing it would leave a crumb pointing at the page you never left.
/// The client's own load prologue bumps `load_generation`, so a late answer
/// from the attempt that failed still exits at its guard.
fn retry_current_view(st: &mut AppState, tx: &UnboundedSender<AppCommand>) -> bool {
    let Some(cmd) = current_load_error(st).map(|e| e.retry.clone()) else {
        return false;
    };
    st.retries += 1;
    st.mark_reloading();
    let _ = tx.send(cmd);
    true
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
            .playlist_row(st.main_index)
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
        // Only on the Stations tab; the other three fall through to the
        // current view's list below, as they always have.
        MainView::Search(_) if st.search_tab == SearchTab::Stations => {
            if let Some(station) = selected_station(st).cloned() {
                play_station(st, station, tx);
            }
            return;
        }
        // A searched playlist's rows are not in hand either, so it takes the
        // same source shortcut a library one does. Enter opens it; this is
        // the gesture that only plays it.
        MainView::Search(results) if st.search_tab == SearchTab::Playlists => results
            .playlists
            .get(st.main_index)
            .map(|p| (p.id.clone(), p.name.clone())),
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

/// Whether a Spotify page is worth opening, and what to say when it is not.
///
/// The library screens only exist for an account that can play, so this only
/// ever bites on the radio deck: a station's matched record names an album and
/// an artist whatever the account is, and a page nothing can be played from is
/// a dead end rather than a destination.
fn spotify_pages_open(st: &mut AppState) -> bool {
    match &st.spotify {
        SpotifyState::Ready => true,
        SpotifyState::Limited(_) => {
            st.toast("this account cannot play Spotify tracks");
            false
        }
        _ => {
            st.toast("connect Spotify from Home to open its pages");
            false
        }
    }
}

/// `b`: browse into the selected item's album (track lists, search tracks,
/// or a search-album result).
fn open_album_of_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    if !spotify_pages_open(st) {
        return;
    }
    let from_track = |t: &crate::app::state::Track| {
        t.album_id.as_ref().map(|id| AppCommand::OpenAlbum {
            id: id.clone(),
            name: t.album.clone(),
            credits: t.credits.clone(),
            year: t.release_year.clone(),
            cover_url: t.cover_url.clone(),
        })
    };
    let cmd = match &st.main {
        MainView::Tracks(list) => list.get(st.main_index).and_then(from_track),
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

/// Open the artist the deck's record is filed under: what `B` means under a
/// station, which has no pointer to say which of several names it wanted.
///
/// Same rule as [`open_deck_album`], and the same reason it does not read
/// `playback`.
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

/// The artist a click at `pos` lands on, out of a run of recorded names.
///
/// One lookup for every table and masthead that records them — see
/// [`state::HitAreas::main_artist_links`].
fn artist_link_at(links: &[(Rect, state::Credit)], pos: Position) -> Option<state::Credit> {
    links
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|(_, credit)| credit.clone())
}

/// Open the artist page a clicked name leads to.
///
/// Goes through [`navigate`] like every other way in, so bouncing between an
/// album and its artist walks the path rather than growing it.
///
/// Ungated, like the deck's other two links and for the same reason: the deck
/// is about the record that is playing, and what is playing is always
/// something this session can reach.
fn open_deck_credit(st: &mut AppState, tx: &UnboundedSender<AppCommand>, credit: state::Credit) {
    let Some(cmd) = state::open_artist(&credit) else {
        return;
    };
    navigate(st, cmd, tx);
}

/// [`open_deck_credit`], for a name printed on a *page*.
///
/// Gated the way `b` and `B` are: opening a page is browsing Spotify, which an
/// account that cannot do it must be told about rather than left watching
/// nothing happen.
fn open_artist_link(st: &mut AppState, tx: &UnboundedSender<AppCommand>, credit: state::Credit) {
    if spotify_pages_open(st) {
        open_deck_credit(st, tx, credit);
    }
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

/// Whether a station's broadcast is what you are hearing.
///
/// The keys that have nothing to do under radio have plenty to do off air: a
/// record from the station's list is a Spotify track in a Spotify queue, so it
/// seeks and shuffles like one.
fn on_air(st: &AppState) -> bool {
    st.radio.as_ref().is_some_and(|r| !r.off_air)
}

/// The station row's `◂ live`: back to the broadcast.
///
/// A tune-in like any other, so the parked queue is put back, the record stops
/// and the station's list opens on what it is playing now — all of which
/// `Client::play_station` already does.
fn resume_station(st: &AppState, tx: &UnboundedSender<AppCommand>) {
    let Some(station) = st.radio.as_ref().map(|r| r.station.clone()) else {
        return;
    };
    let _ = tx.send(AppCommand::PlayStation {
        station: Box::new(station),
        attempt: 0,
    });
}

/// Say why a deck control did nothing, when the reason is radio.
///
/// Silence on a keypress is out of character here — `n`, `p`, `s`, `h` and `l`
/// all explain themselves under radio rather than appearing broken. Off radio
/// there is nothing to say: a Spotify track always has an album and an artist,
/// so the only way to arrive with nothing is to have nothing playing at all,
/// which the deck is already saying in as many words.
fn radio_has_no_track(st: &mut AppState, what: &str) {
    let Some(matched) = st.radio.as_ref().map(|r| r.matched.clone()) else {
        return;
    };
    say_no_track(st, &matched, what);
}

/// Why one announcement has no record to act on.
///
/// Taken apart from [`radio_has_no_track`] so a row of the station's list can
/// answer with its own state: the deck is about what is playing, and a row you
/// pressed Enter on is not.
fn say_no_track(st: &mut AppState, matched: &RadioMatch, what: &str) {
    let msg = match matched {
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
        credits: a.credits.clone(),
        year: a.release_year.clone(),
        cover_url: a.cover_url.clone(),
    }
}

/// `B`: browse into the selected item's artist (first credited artist for
/// tracks, or a search-artist result).
fn open_artist_of_selection(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    if !spotify_pages_open(st) {
        return;
    }
    let from_track = |t: &crate::app::state::Track| t.open_artist();
    let cmd = match &st.main {
        MainView::Tracks(list) => list.get(st.main_index).and_then(from_track),
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
        MainView::Tracks(list) => list.get(st.main_index).cloned(),
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
    match selected_track_uri(st) {
        Some(uri) => send_like(st, uri, tx),
        None => toggle_like_deck(st, tx),
    }
}

/// The track under the cursor on a browse page, if the page lists tracks at
/// all. Shared by the row's `★` and its `+` so the two controls can never
/// disagree about which record the row is.
fn selected_track_uri(st: &AppState) -> Option<String> {
    match &st.main {
        MainView::Tracks(list) => list.get(st.main_index).map(|t| t.uri.clone()),
        MainView::Search(results) if st.search_tab == SearchTab::Tracks => {
            results.tracks.get(st.main_index).map(|t| t.uri.clone())
        }
        MainView::Artist(v) => match v.row(st.main_index) {
            Some(ArtistRow::Track(t)) => Some(t.uri.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The `+` on a track row: the add-to-playlist box for that row's record.
///
/// Falls back to the deck's record when the page has no track under the
/// cursor, the way `L` does — the control is only drawn on rows that have
/// one, so the fallback is for the keyboard path rather than the click.
fn add_selection_to_playlist(st: &mut AppState, tx: &UnboundedSender<AppCommand>) {
    match selected_track_uri(st) {
        Some(uri) => open_playlist_picker_for(st, uri, tx),
        None => open_playlist_picker(st, tx),
    }
}

/// The `⧉` on a track row: that row's Spotify link on the clipboard.
///
/// Falls back to the deck's record the way the `+` beside it does.
fn share_selection(st: &mut AppState) {
    match selected_track_uri(st) {
        Some(uri) => share_track(st, &uri),
        None => share_deck(st),
    }
}

/// The deck's share control, about the record the deck is about — see
/// [`toggle_like_deck`] for why that is not always `playback`.
fn share_deck(st: &mut AppState) {
    let Some(uri) = st.deck_track().map(|t| t.uri.clone()) else {
        return radio_has_no_track(st, "track");
    };
    share_track(st, &uri);
}

/// A header band's `⧉ share`: the link to the page itself rather than to any
/// record on it. The control is drawn only where there is one, so a page that
/// cannot be linked to has nothing to report.
fn share_open_page(st: &mut AppState) {
    let Some(link) = st.open_page_link() else {
        return;
    };
    copy_link(st, &link.to_url());
}

/// Put a track's link on the clipboard.
///
/// A URI spot has already loaded a page or a queue from, so it parses — but a
/// link that would not parse is one spot cannot be sure of, and a malformed
/// string on the clipboard is worse than a refusal that says so.
fn share_track(st: &mut AppState, uri: &str) {
    match crate::link::parse(uri) {
        Ok(link) => copy_link(st, &link.to_url()),
        Err(_) => st.toast("that track has no link to share"),
    }
}

/// The clipboard write both share paths end in, and the one place the result
/// is reported.
fn copy_link(st: &mut AppState, url: &str) {
    match crate::clipboard::copy(url) {
        Ok(()) => st.toast("spotify url copied to clipboard"),
        Err(e) => st.toast(format!("could not copy: {e}")),
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
    use crate::app::state::{AlbumItem, ArtistView, Credit, Playlist, TrackList, TrackListKind};

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
            credits: vec![Credit {
                name: "Muse".into(),
                id: Some("r1".into()),
            }],
            cover_url: Some("https://i.scdn.co/image/abc".into()),
        }
    }

    fn artist_state() -> AppState {
        let mut st = connected();
        let mut top = TrackList::new("Muse", "top tracks", None);
        top.append(vec![track("Uprising", Some("a1"))]);
        st.main = MainView::Artist(ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Muse".into(),
            image_url: None,
            genres: Vec::new(),
            bio: state::BioState::default(),
            top,
            albums: vec![AlbumItem {
                id: "a1".into(),
                name: "Black Holes".into(),
                artists: "Muse".into(),
                credits: vec![Credit {
                    name: "Muse".into(),
                    id: Some("r1".into()),
                }],
                release_year: "2006".into(),
                album_type: "album".into(),
                album_group: "album".into(),
                track_count: 12,
                cover_url: Some("https://i.scdn.co/image/abc".into()),
            }]
            .into(),
            tab: crate::app::state::ArtistTab::Albums,
            loading: false,
            error: None,
        });
        st
    }

    fn channel() -> (UnboundedSender<AppCommand>, UnboundedReceiver<AppCommand>) {
        unbounded_channel()
    }

    /// State as a signed-in Premium account sees it: the library rows on Home
    /// and all five search tabs. `AppState::new` is the radio-only app, which
    /// is what spot starts as.
    fn connected() -> AppState {
        let mut st = AppState::new();
        st.spotify = SpotifyState::Ready;
        st
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

    /// A frame that drew one sleeve at `rect`.
    fn with_sleeve(st: &mut AppState, rect: Rect) {
        st.hit.art_blocks = vec![state::ArtHit {
            rect,
            source: state::ArtSource::Page(Some("https://i.scdn.co/image/abc".into())),
            seed: "Black Holes".into(),
        }];
    }

    #[test]
    fn clicking_a_sleeve_expands_it() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        with_sleeve(&mut st, Rect::new(1, 4, 20, 10));

        handle_click(&mut st, Position { x: 5, y: 6 }, &tx);
        let zoom = st.art_zoom.expect("the sleeve did not expand");
        assert_eq!(zoom.seed, "Black Holes");
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadZoomCover { cover_url: Some(u) }) if u.ends_with("abc")
        ));
    }

    /// The picture covers the screen, so a click lands on it wherever it is —
    /// and means only "put it away".
    #[test]
    fn a_click_anywhere_closes_the_expanded_art_and_does_nothing_else() {
        let (tx, _rx) = channel();
        let mut st = connected();
        st.main = MainView::Playlists;
        st.hit.home_btn = Rect::new(0, 0, 6, 1);
        st.art_zoom = Some(ArtZoom {
            source: state::ArtSource::Playing,
            seed: "s".into(),
        });
        st.zoom_cover = None;

        handle_click(&mut st, Position { x: 2, y: 0 }, &tx);
        assert!(st.art_zoom.is_none());
        assert!(
            matches!(st.main, MainView::Playlists),
            "the click under the picture went through"
        );
    }

    /// Help draws over the picture, so its own click comes first.
    #[test]
    fn help_closes_before_the_expanded_art() {
        let (tx, _rx) = channel();
        let mut st = connected();
        st.show_help = true;
        st.art_zoom = Some(ArtZoom {
            source: state::ArtSource::Playing,
            seed: "s".into(),
        });

        handle_click(&mut st, Position { x: 2, y: 2 }, &tx);
        assert!(!st.show_help);
        assert!(st.art_zoom.is_some(), "one click closed both");
    }

    /// A sleeve expanded from the player view must not have the player pulled
    /// out from under it: Esc puts the picture away first.
    #[test]
    fn esc_closes_the_expanded_art_before_the_player() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(connected()));
        {
            let mut st = state.write();
            st.show_player = true;
            st.art_zoom = Some(ArtZoom {
                source: state::ArtSource::Playing,
                seed: "s".into(),
            });
        }
        let esc = KeyEvent::from(KeyCode::Esc);

        handle_normal(esc, &state, &tx);
        assert!(state.read().art_zoom.is_none());
        assert!(state.read().show_player, "Esc reached the player as well");

        handle_normal(esc, &state, &tx);
        assert!(!state.read().show_player);
    }

    /// Transport is about the record, not about the screen behind the picture.
    #[test]
    fn transport_still_works_under_the_expanded_art() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(connected()));
        state.write().art_zoom = Some(ArtZoom {
            source: state::ArtSource::Playing,
            seed: "s".into(),
        });

        handle_normal(KeyEvent::from(KeyCode::Char(' ')), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::PlayPause)));
        assert!(state.read().art_zoom.is_some());
    }

    /// The picture is whole, so the wheel has nothing to move — and it must
    /// not reach the list underneath it either.
    #[test]
    fn the_wheel_does_nothing_under_the_expanded_art() {
        let (tx, _rx) = channel();
        let mut st = liked_state();
        st.hit.main_list = Rect::new(0, 4, 60, 10);
        st.art_zoom = Some(ArtZoom {
            source: state::ArtSource::Playing,
            seed: "s".into(),
        });
        let at = Position { x: 10, y: 6 };

        handle_scroll(&mut st, at, 1, &tx);
        assert_eq!(st.main_list.offset(), 0, "the list under it scrolled");
        assert!(st.art_zoom.is_some(), "the wheel closed the picture");
    }

    /// Clicking a header sorts by that column; clicking the same one again
    /// turns it round.
    #[test]
    fn clicking_a_header_sorts_by_its_column() {
        let (tx, _rx) = channel();
        let mut st = liked_state();
        // The header sits above the rows, which is what keeps the two
        // branches of the click apart.
        st.hit.main_list = Rect::new(0, 4, 60, 10);
        st.hit.column_headers = vec![(Rect::new(6, 2, 5, 1), state::ColKey::Title)];

        handle_click(&mut st, Position { x: 7, y: 2 }, &tx);
        let MainView::Tracks(list) = &st.main else {
            unreachable!()
        };
        assert_eq!(list.sort.key, state::ColKey::Title);
        assert!(list.sort.ascending);
        assert_eq!(list.get(0).unwrap().name, "Hysteria");

        handle_click(&mut st, Position { x: 7, y: 2 }, &tx);
        let MainView::Tracks(list) = &st.main else {
            unreachable!()
        };
        assert!(!list.sort.ascending, "the same column did not turn round");
        assert_eq!(list.get(0).unwrap().name, "Starlight");

        // A third click clears it: back to the order the source sent.
        handle_click(&mut st, Position { x: 7, y: 2 }, &tx);
        let MainView::Tracks(list) = &st.main else {
            unreachable!()
        };
        assert_eq!(list.sort, state::Sort::default());
        assert_eq!(list.get(0).unwrap().name, "Starlight");
        assert_eq!(list.get(1).unwrap().name, "Hysteria");
    }

    /// `o` walks the header that was drawn, so it can never name a column the
    /// pane was too narrow to show.
    #[test]
    fn o_cycles_only_the_columns_on_screen() {
        let mut st = liked_state();
        // A narrow pane: no year, no album, no track number.
        st.hit.sort_keys = vec![
            state::ColKey::Title,
            state::ColKey::Artist,
            state::ColKey::Time,
        ];
        for expected in [
            state::ColKey::Title,
            state::ColKey::Artist,
            state::ColKey::Time,
            state::ColKey::Title,
        ] {
            cycle_sort(&mut st);
            let MainView::Tracks(list) = &st.main else {
                unreachable!()
            };
            assert_eq!(list.sort.key, expected);
            assert!(list.sort.ascending, "a new column did not start ascending");
        }
    }

    /// Every table sorts, not only the track lists: `o` on the Playlists page
    /// reaches the sort that lives beside it on the state.
    #[test]
    fn the_playlists_page_sorts_too() {
        let mut st = connected();
        st.main = MainView::Playlists;
        st.set_playlists(vec![
            playlist("p1", "trendy", "dm"),
            playlist("p2", "Ambient", "dm"),
        ]);
        st.hit.sort_keys = vec![state::ColKey::Title, state::ColKey::Owner];

        cycle_sort(&mut st);
        assert_eq!(st.playlists_sort.key, state::ColKey::Title);
        assert_eq!(st.playlist_row(0).unwrap().name, "Ambient");
        // The library itself is untouched: the add-to-playlist box freezes
        // indices into it.
        assert_eq!(st.playlists[0].name, "trendy");
    }

    /// A search's playlist row opens like every other row here. It only ever
    /// played before, which left a searched playlist with no way in at all.
    #[test]
    fn a_searched_playlist_opens_on_enter_and_plays_on_x() {
        let searched = || {
            let (tx, rx) = channel();
            let mut st = connected();
            st.search_tab = SearchTab::Playlists;
            st.main = MainView::Search(state::SearchResults {
                query: "jazz".into(),
                playlists: vec![playlist("p1", "Blue Note", "someone")].into(),
                ..Default::default()
            });
            (st, tx, rx)
        };

        let (mut st, tx, mut rx) = searched();
        activate_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadPlaylistTracks { ref playlist_id }) if playlist_id == "p1"
        ));

        let (mut st, tx, mut rx) = searched();
        play_without_opening(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::PlayFetched {
                source: FetchSource::Playlist { ref id },
                ..
            }) if id == "p1"
        ));
    }

    /// `F` is about the playlist you are looking at, and only a playlist
    /// someone else made — Spotify has no unfollow for your own that is not a
    /// delete, so the key must agree with the control that is not drawn.
    #[test]
    fn the_save_key_acts_only_on_a_playlist_you_do_not_own() {
        let page = |owner: &str| {
            let mut st = connected();
            st.me_id = Some("me".into());
            let mut list = TrackList::new("Blue Note", "", None);
            list.cache_key = Some(state::playlist_key("p1"));
            list.header.owner_id = owner.into();
            st.main = MainView::Tracks(list);
            st.saved_playlists.insert("p1".into(), true);
            st
        };

        let (tx, mut rx) = channel();
        let mut theirs = page("someone");
        let ask = ConfirmTrigger::Key('F');
        toggle_saved_playlist(&mut theirs, &tx, ask);
        assert!(rx.try_recv().is_err(), "unfollowed on the first press");
        assert!(theirs.confirm.is_some(), "the first press said nothing");
        toggle_saved_playlist(&mut theirs, &tx, ask);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::SetPlaylistSaved { ref id, saved: false }) if id == "p1"
        ));

        let mut mine = page("me");
        toggle_saved_playlist(&mut mine, &tx, ask);
        toggle_saved_playlist(&mut mine, &tx, ask);
        assert!(rx.try_recv().is_err(), "your own playlist was unsaved");
    }

    /// Saving is the one direction that acts at once: it takes nothing away,
    /// and the control beside it is how to undo it.
    #[test]
    fn the_save_key_keeps_a_playlist_on_the_first_press() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.me_id = Some("me".into());
        let mut list = TrackList::new("Blue Note", "", None);
        list.cache_key = Some(state::playlist_key("p1"));
        list.header.owner_id = "someone".into();
        st.main = MainView::Tracks(list);
        st.saved_playlists.insert("p1".into(), false);

        toggle_saved_playlist(&mut st, &tx, ConfirmTrigger::Key('F'));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::SetPlaylistSaved { ref id, saved: true }) if id == "p1"
        ));
        assert!(st.confirm.is_none(), "saving armed a prompt");
    }

    /// The two asks have to be the same ask. A prompt armed by one control and
    /// answered by another would fire a write nobody asked for twice.
    #[test]
    fn a_different_ask_disarms_rather_than_firing() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.me_id = Some("me".into());
        let mut list = TrackList::new("Blue Note", "", None);
        list.cache_key = Some(state::playlist_key("p1"));
        list.header.owner_id = "someone".into();
        st.main = MainView::Tracks(list);
        st.saved_playlists.insert("p1".into(), true);

        toggle_saved_playlist(&mut st, &tx, ConfirmTrigger::Key('F'));
        assert!(st.confirm.is_some());
        toggle_saved_playlist(
            &mut st,
            &tx,
            ConfirmTrigger::Click(ConfirmTarget::HeaderUnfollow),
        );
        assert!(rx.try_recv().is_err(), "the other ask fired it");
        assert!(
            st.confirm.is_some(),
            "the other ask armed nothing of its own"
        );
    }

    /// Until the check answers there is nothing to flip to, so the key does
    /// nothing rather than guessing at what the library holds.
    #[test]
    fn the_save_key_waits_for_the_check() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.me_id = Some("me".into());
        let mut list = TrackList::new("Blue Note", "", None);
        list.cache_key = Some(state::playlist_key("p1"));
        list.header.owner_id = "someone".into();
        st.main = MainView::Tracks(list);

        toggle_saved_playlist(&mut st, &tx, ConfirmTrigger::Key('F'));
        assert!(rx.try_recv().is_err());
    }

    /// The edit box opens on your own playlist, filled with what the page
    /// already shows, and refuses to open on anyone else's.
    #[test]
    fn the_edit_box_opens_only_on_your_own_playlist() {
        let page = |owner: &str| {
            let mut st = connected();
            st.me_id = Some("me".into());
            let mut list = TrackList::new("Blue Note", "by me", None);
            list.cache_key = Some(state::playlist_key("p1"));
            list.header.owner_id = owner.into();
            list.header.description = "hard bop".into();
            st.main = MainView::Tracks(list);
            st
        };

        let mut mine = page("me");
        open_playlist_edit(&mut mine);
        let edit = mine.edit.expect("no box on your own playlist");
        assert_eq!(edit.target, EditTarget::Existing("p1".into()));
        assert_eq!(edit.name, "Blue Note");
        assert_eq!(edit.description, "hard bop");

        let mut theirs = page("someone");
        open_playlist_edit(&mut theirs);
        assert!(theirs.edit.is_none());
    }

    /// Copy is the one control a playlist you cannot edit still offers, and it
    /// opens the same box the rest of them do — the name is yours before
    /// anything is written.
    #[test]
    fn the_copy_box_opens_on_a_playlist_you_do_not_own() {
        let mut st = copyable("someone", 2);
        open_playlist_copy(&mut st);
        let edit = st.edit.expect("no box on someone else's playlist");
        assert_eq!(
            edit.target,
            EditTarget::Copy {
                source_id: "p1".into()
            }
        );
        assert_eq!(edit.name, "Blue Note (copy)");
        assert_eq!(edit.description, "hard bop");
    }

    /// A copy is written from the rows spot holds, and a page holding fewer
    /// than the playlist does would make a record short of the one it claims
    /// to be. Refused before the name is typed, not after.
    #[test]
    fn a_copy_is_refused_where_the_page_cannot_see_every_row() {
        let short = |edit: fn(&mut TrackList)| {
            let mut st = copyable("someone", 2);
            let MainView::Tracks(list) = &mut st.main else {
                unreachable!()
            };
            edit(list);
            st
        };

        let mut loading = short(|list| list.loading = true);
        open_playlist_copy(&mut loading);
        assert!(loading.edit.is_none(), "copied a page still arriving");
        assert!(loading.toast.is_some(), "refused without saying why");

        let mut partial = short(|list| list.total = Some(9));
        open_playlist_copy(&mut partial);
        assert!(partial.edit.is_none(), "copied a page missing rows");

        let mut empty = short(|list| {
            list.rows = state::SortedList::from_items(Vec::new());
            list.total = Some(0);
        });
        open_playlist_copy(&mut empty);
        assert!(empty.edit.is_none(), "copied nothing");
    }

    /// The copy carries the page's rows in playlist order, so the client
    /// writes them without asking Spotify for a list it already has.
    #[test]
    fn a_submitted_copy_carries_the_rows_in_order() {
        let (tx, mut rx) = channel();
        let mut st = copyable("someone", 2);
        open_playlist_copy(&mut st);
        submit_edit(&mut st, &tx);
        match rx.try_recv() {
            Ok(AppCommand::CopyPlaylist { name, uris, .. }) => {
                assert_eq!(name, "Blue Note (copy)");
                assert_eq!(uris, vec!["spotify:track:t0", "spotify:track:t1"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            st.edit.is_some_and(|edit| edit.pending),
            "the box did not go inert"
        );
    }

    /// Delete is for your own playlist, and it asks twice — the control sits a
    /// cell from ▶ play and there is no way back from the first press.
    #[test]
    fn deleting_a_playlist_asks_twice_and_only_on_your_own() {
        let (tx, mut rx) = channel();
        let ask = ConfirmTrigger::Key('d');

        let mut mine = copyable("me", 1);
        delete_open_playlist(&mut mine, &tx, ask);
        assert!(rx.try_recv().is_err(), "deleted on the first press");
        assert!(mine.confirm.is_some(), "the first press said nothing");
        delete_open_playlist(&mut mine, &tx, ask);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::SetPlaylistSaved { ref id, saved: false }) if id == "p1"
        ));

        let mut theirs = copyable("someone", 1);
        delete_open_playlist(&mut theirs, &tx, ask);
        delete_open_playlist(&mut theirs, &tx, ask);
        assert!(rx.try_recv().is_err(), "deleted someone else's playlist");
    }

    /// Walking away is an answer. A prompt left standing over a page nobody is
    /// on any more would fire on a keypress meant for something else.
    #[test]
    fn leaving_the_page_calls_off_an_armed_write() {
        let (tx, _rx) = channel();
        let ask = ConfirmTrigger::Key('d');

        let mut st = copyable("me", 1);
        delete_open_playlist(&mut st, &tx, ask);
        select_row(&mut st, 1);
        assert!(st.confirm.is_none(), "moving off the row kept it armed");

        delete_open_playlist(&mut st, &tx, ask);
        go_home(&mut st);
        assert!(st.confirm.is_none(), "going home kept it armed");
    }

    /// A playlist page holding `count` records, owned by `owner`.
    fn copyable(owner: &str, count: usize) -> AppState {
        let mut st = connected();
        st.me_id = Some("me".into());
        let mut list = TrackList::new("Blue Note", "by me", Some(count as u32));
        list.cache_key = Some(state::playlist_key("p1"));
        list.header.owner_id = owner.into();
        list.header.description = "hard bop".into();
        list.rows = state::SortedList::from_items(
            (0..count)
                .map(|i| Track {
                    uri: format!("spotify:track:t{i}"),
                    ..track("One", None)
                })
                .collect(),
        );
        st.main = MainView::Tracks(list);
        st
    }

    /// A blank name is refused here rather than by Spotify: the round trip
    /// would come back having rejected the blurb along with it.
    #[test]
    fn an_unnamed_playlist_is_refused_before_it_is_sent() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.edit = Some(PlaylistEdit {
            target: EditTarget::Existing("p1".into()),
            name: "  ".into(),
            description: "hard bop".into(),
            field: EditField::Name,
            pending: false,
            error: None,
            seq: 1,
        });

        submit_edit(&mut st, &tx);
        assert!(rx.try_recv().is_err());
        let edit = st.edit.expect("the box closed on a refusal");
        assert!(!edit.pending);
        assert!(edit.error.is_some());
    }

    /// A change in flight owns the text it went out with, so a second Enter
    /// cannot send the same edit twice.
    #[test]
    fn a_pending_edit_is_sent_once() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.edit = Some(PlaylistEdit {
            target: EditTarget::Existing("p1".into()),
            name: " Blue Note ".into(),
            description: " hard bop ".into(),
            field: EditField::Name,
            pending: false,
            error: None,
            seq: 1,
        });

        submit_edit(&mut st, &tx);
        // Trimmed: a name typed with a stray space is the name without it.
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::EditPlaylistDetails { ref name, ref description, .. })
                if name == "Blue Note" && description == "hard bop"
        ));
        submit_edit(&mut st, &tx);
        assert!(rx.try_recv().is_err());
    }

    /// The Tracks tab of a search plays what is on screen, like every other
    /// list — it sent raw fetch order before the tab could sort.
    #[test]
    fn a_sorted_search_tab_plays_its_display_order() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.search_tab = SearchTab::Tracks;
        st.main = MainView::Search(state::SearchResults {
            query: "muse".into(),
            tracks: vec![
                track("Starlight", Some("a1")),
                track("Hysteria", Some("a1")),
            ]
            .into(),
            ..Default::default()
        });
        st.hit.sort_keys = vec![state::ColKey::Title];
        cycle_sort(&mut st);

        activate_selection(&mut st, &tx);
        match rx.try_recv() {
            Ok(AppCommand::Play { tracks, start, .. }) => {
                assert_eq!(tracks[0].name, "Hysteria", "the queue is not display order");
                // The sort put the cursor back on row 0, which is what Enter
                // then plays.
                assert_eq!(start, 0);
            }
            other => panic!("sent {other:?}"),
        }
    }

    /// A sort is asked for in order to read the list from its new start, so it
    /// takes the view there rather than chasing the row that was selected.
    #[test]
    fn sorting_returns_the_view_to_the_top() {
        let mut st = liked_state();
        st.hit.sort_keys = vec![state::ColKey::Title];
        st.hit.main_list = Rect::new(0, 4, 60, 10);
        // Down the list, and scrolled away from the top.
        st.main_index = 1;
        *st.main_list.offset_mut() = 1;

        cycle_sort(&mut st);
        assert_eq!(st.main_index, 0);
        assert_eq!(st.main_list.offset(), 0);

        // And the same the other way round: `O` does not chase it either.
        st.main_index = 1;
        flip_sort(&mut st);
        assert_eq!(st.main_index, 0);
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
            list.sort = state::Sort {
                key: state::ColKey::Title,
                ascending: true,
            };
            list.rebuild();
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

    /// The failed page's own control, reached by pointer.
    #[test]
    fn clicking_try_again_asks_for_the_page_again() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        let mut list = TrackList::new("Muse", "", None);
        list.error = Some(state::LoadError::new(
            "429 Too Many Requests",
            AppCommand::LoadPlaylistTracks {
                playlist_id: "p1".into(),
            },
        ));
        st.main = MainView::Tracks(list);
        st.hit.retry_btn = Rect::new(20, 6, 11, 1);

        // The pane records the whole body as the list so it still scrolls, so
        // the control sits *inside* `main_list`. The list's branch resolves a
        // click on a page with no rows to nothing and returns, so a retry
        // tested after it would never be reached.
        st.hit.main_list = Rect::new(0, 2, 60, 12);

        handle_click(&mut st, Position { x: 22, y: 6 }, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadPlaylistTracks { playlist_id }) if playlist_id == "p1"
        ));
        assert!(
            st.view_stack.is_empty(),
            "the retry pushed the page it never left"
        );
        assert_eq!(st.retries, 1, "the press was not counted");
    }

    /// The spinner has to be up before the client has done anything: a rate
    /// limit refuses in less time than a frame takes, so a page left showing
    /// its old refusal until the client answers never shows a spinner at all.
    #[test]
    fn the_press_puts_the_page_back_in_flight_on_the_spot() {
        let (tx, _rx) = channel();
        let mut st = connected();
        let mut list = TrackList::new("Muse", "", None);
        list.error = Some(state::LoadError::new(
            "429 Too Many Requests",
            AppCommand::LoadLikedSongs,
        ));
        st.main = MainView::Tracks(list);

        assert!(retry_current_view(&mut st, &tx));
        let MainView::Tracks(list) = &st.main else {
            unreachable!()
        };
        assert!(list.loading, "the page did not go back in flight");
        assert!(list.error.is_none(), "the old refusal stood over the retry");
    }

    /// The count is about the page you are on, so arriving at another one
    /// starts it over.
    #[test]
    fn opening_a_page_forgets_the_previous_one_s_retries() {
        let (tx, _rx) = channel();
        let mut st = connected();
        st.retries = 4;
        navigate(&mut st, AppCommand::LoadLikedSongs, &tx);
        assert_eq!(st.retries, 0);
    }

    /// And by keyboard: a page with no rows has nothing else Enter could
    /// mean, so Enter is the same control.
    #[test]
    fn enter_on_a_refused_page_asks_for_it_again() {
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        let MainView::Artist(v) = &mut st.main else {
            unreachable!()
        };
        v.top = TrackList::new("Muse", "top tracks", None);
        v.albums = state::SortedList::new();
        v.error = Some(state::LoadError::new(
            "no route to host",
            AppCommand::OpenArtist {
                id: "r1".into(),
                uri: "spotify:artist:r1".into(),
                name: "Muse".into(),
            },
        ));

        activate_selection(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::OpenArtist { id, .. }) if id == "r1"
        ));

        // And a page that answered keeps Enter for its rows.
        let (tx, mut rx) = channel();
        let mut st = artist_state();
        activate_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Play { .. })));
    }

    /// A page that came back with rows is a page that answered: a failure
    /// recorded part-way through must not take Enter away from them.
    #[test]
    fn a_half_loaded_page_keeps_enter_for_its_rows() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        let mut list = TrackList::new("Muse", "", None);
        list.append(vec![track("Uprising", Some("a1"))]);
        list.error = Some(state::LoadError::new(
            "the rest of the pages never came",
            AppCommand::LoadLikedSongs,
        ));
        st.main = MainView::Tracks(list);

        activate_selection(&mut st, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Play { .. })));
    }

    /// Clicking the liked column likes that row — and only likes it: the
    /// click must not also arm the double-click that would start playback.
    #[test]
    fn clicking_the_liked_column_likes_that_row() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        st.hit.main_list = Rect::new(0, 0, 90, 10);
        st.hit.main_like_col = Rect::new(4, 0, 1, 2);

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

    /// The artist cell is a link the width of the name it prints. Past that
    /// the row is only a row, so a click in the padding selects and arms the
    /// double-click rather than opening a page the pointer never lit.
    #[test]
    fn the_artist_link_stops_where_its_name_does() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        st.spotify = SpotifyState::Ready;
        st.hit.main_list = Rect::new(0, 0, 90, 10);
        // A six-cell name on each of the first two rows, inside a column
        // twenty cells wide.
        st.hit.main_artist_links = (0..2)
            .map(|row| {
                (
                    Rect::new(20, row, 6, 1),
                    Credit {
                        name: "Muse".into(),
                        id: Some("r1".into()),
                    },
                )
            })
            .collect();

        handle_click(&mut st, Position { x: 25, y: 1 }, &tx);
        assert_eq!(st.main_index, 1, "the click did not select the row");
        assert!(matches!(rx.try_recv(), Ok(AppCommand::OpenArtist { .. })));
        assert!(
            st.last_main_click.is_none(),
            "the artist cell armed a double-click"
        );

        // One cell past the name: the row, not the link.
        handle_click(&mut st, Position { x: 26, y: 1 }, &tx);
        assert!(rx.try_recv().is_err(), "the padding opened the artist");
        assert!(st.last_main_click.is_some());
    }

    /// A record credits several artists on one line, and each name leads to a
    /// different page. The one the pointer is on is the one that opens.
    #[test]
    fn each_name_of_a_credit_line_opens_its_own_artist() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        st.spotify = SpotifyState::Ready;
        st.hit.main_list = Rect::new(0, 0, 90, 10);
        // `Zedd, Alessia Cara` at column 20: two names with the separator
        // between them belonging to neither.
        st.hit.main_artist_links = vec![
            (
                Rect::new(20, 0, 4, 1),
                Credit {
                    name: "Zedd".into(),
                    id: Some("zedd".into()),
                },
            ),
            (
                Rect::new(26, 0, 12, 1),
                Credit {
                    name: "Alessia Cara".into(),
                    id: Some("cara".into()),
                },
            ),
        ];

        handle_click(&mut st, Position { x: 30, y: 0 }, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::OpenArtist { ref id, ref name, .. })
                if id == "cara" && name == "Alessia Cara"
        ));

        // The `, ` between them is not a third control.
        handle_click(&mut st, Position { x: 25, y: 0 }, &tx);
        assert!(rx.try_recv().is_err(), "the separator opened an artist");
        assert!(
            st.last_main_click.is_some(),
            "the click did not fall through to the row"
        );
    }

    /// The `+` beside it opens the box for that row's record, not for whatever
    /// is playing — and like the star, it does not arm a double-click.
    #[test]
    fn clicking_the_add_column_opens_the_box_for_that_row() {
        let (tx, mut rx) = channel();
        let mut st = liked_state();
        // Something else is playing, so a box opened for the deck's record
        // would name the wrong track.
        start_playing(&mut st);
        st.hit.main_list = Rect::new(0, 0, 90, 10);
        st.hit.main_add_col = Rect::new(80, 0, 1, 2);

        handle_click(&mut st, Position { x: 80, y: 1 }, &tx);
        assert_eq!(st.main_index, 1, "the click did not select the row");
        assert_eq!(
            st.picker.as_ref().map(|p| p.uri.as_str()),
            Some("spotify:track:Hysteria")
        );
        assert!(
            st.last_main_click.is_none(),
            "the add cell armed a double-click"
        );
        // Opening the box asks for the marks of the playlists it will show.
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::CachePlaylistTracks { .. }) | Err(_)
        ));
    }

    /// The queue's own pair acts on the row under the pointer. The player view
    /// is about the playing record, but its list is not — a `+` on row three
    /// that added row one would be a control that lies.
    #[test]
    fn the_queues_pair_acts_on_the_row_not_the_playing_track() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.show_player = true;
        st.set_queue(Some(crate::app::queue::Queue::new(
            vec![
                track("Starlight", Some("a1")),
                track("Hysteria", Some("a1")),
            ],
            0,
            "My Mix",
        )));
        st.hit.player_queue = Rect::new(0, 0, 80, 10);
        st.hit.queue_like_col = Rect::new(60, 0, 1, 2);
        st.hit.queue_add_col = Rect::new(62, 0, 1, 2);

        handle_click(&mut st, Position { x: 60, y: 1 }, &tx);
        assert_eq!(st.queue_index, 1);
        assert!(
            matches!(rx.try_recv(), Ok(AppCommand::SetLiked { uri, liked })
                if uri == "spotify:track:Hysteria" && liked)
        );
        assert!(st.last_queue_click.is_none());

        handle_click(&mut st, Position { x: 62, y: 0 }, &tx);
        assert_eq!(
            st.picker.as_ref().map(|p| p.uri.as_str()),
            Some("spotify:track:Starlight")
        );
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

    /// The word and the track sit side by side and mean different things, so
    /// the cell the pointer is on decides which of the two it gets.
    #[test]
    fn the_volume_label_mutes_where_the_track_sets_a_percent() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.hit.volume_label = Rect::new(60, 20, 4, 1);
        st.hit.volume_slider = Rect::new(64, 20, 16, 1);

        handle_click(&mut st, Position { x: 62, y: 20 }, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::ToggleMute)));

        handle_click(&mut st, Position { x: 64, y: 20 }, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::SetVolume(0))));

        handle_click(&mut st, Position { x: 79, y: 20 }, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::SetVolume(100))));
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
        let mut st = connected();
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
        st.hit.now_artist_links = vec![(
            Rect::new(4, 9, 6, 1),
            Credit {
                name: "Muse".into(),
                id: Some("r1".into()),
            },
        )];

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
        st.set_playlists(vec![playlist("p1", "trendy", "dm")]);

        for _ in 0..4 {
            activate_selection(&mut st, &tx);
        }
        assert_eq!(st.view_stack.len(), 1, "{:?}", labels(&st));
        // Every click still asks for the page — only the history is deduped.
        let sent = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert_eq!(sent, 4);
    }

    /// A new query takes the old one's place rather than stacking beside it.
    /// Otherwise ten refinements leave ten frames, each holding a whole cloned
    /// `SearchResults`.
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

    /// …and from a page *above* an earlier search, the new query starts the
    /// path over instead of adding a second search to it.
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

    /// A search from several steps in is a fresh start: the pages walked
    /// through to reach it have nothing to do with the query, and Home comes
    /// back with the row it was left on.
    #[test]
    fn a_search_starts_the_path_over() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.main_index = 2;
        st.push_view();
        st.main = MainView::Playlists;
        st.push_view();
        st.main = MainView::Tracks(TrackList::new("Black Holes", "Muse · 2006", None));
        assert_eq!(labels(&st), ["playlists", "Black Holes"]);

        navigate(&mut st, AppCommand::Search("pixies".into()), &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "pixies"));
        assert_eq!(st.view_stack.len(), 1, "{:?}", labels(&st));
        assert!(matches!(st.view_stack[0].view, MainView::Home));
        assert_eq!(
            st.view_stack[0].main_index, 2,
            "Home lost the row it was on"
        );

        st.main = MainView::Search(crate::app::state::SearchResults {
            query: "pixies".into(),
            ..Default::default()
        });
        assert_eq!(labels(&st), ["“pixies”"]);
    }

    /// A link out of the same box starts the same fresh path — and so does one
    /// arriving from outside spot, which lands on the same call.
    #[test]
    fn a_link_starts_the_path_over_too() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.push_view();
        st.main = MainView::Playlists;
        st.push_view();
        st.main = MainView::Tracks(TrackList::new("Black Holes", "Muse · 2006", None));

        navigate_from_link(
            &mut st,
            AppCommand::OpenArtist {
                id: "r1".into(),
                uri: "spotify:artist:r1".into(),
                name: "Muse".into(),
            },
            &tx,
        );
        assert!(matches!(rx.try_recv(), Ok(AppCommand::OpenArtist { ref id, .. }) if id == "r1"));
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
        let mut st = connected();
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
            list.items[0].album_id = Some("a2".into());
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
            cover_url: None,
            public: None,
            collaborative: false,
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
        let mut st = connected();
        st.set_playlists(vec![playlist("p1", "trendy", "dm")]);

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

    /// The search box takes a pasted link as well as a query.
    ///
    /// This is the whole of the `open.spotify.com` story: Windows can route the
    /// `spotify:` scheme to an app but cannot route an https host, so a link
    /// copied out of a browser has no way in but this one.
    #[test]
    fn a_pasted_link_opens_rather_than_searching() {
        const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(connected()));

        for (typed, expected) in [
            (
                format!("https://open.spotify.com/album/{ID}?si=x"),
                crate::link::Link::Album(ID.into()),
            ),
            (
                format!("spotify:track:{ID}"),
                crate::link::Link::Track(ID.into()),
            ),
            (
                format!("spotify:user:someone:playlist:{ID}"),
                crate::link::Link::Playlist(ID.into()),
            ),
        ] {
            {
                let mut st = state.write();
                st.input_mode = InputMode::Search;
                st.input_buffer = typed.clone();
            }
            handle_search_input(KeyEvent::from(KeyCode::Enter), &state, &tx);
            match rx.try_recv() {
                Ok(AppCommand::OpenLink(target)) => assert_eq!(target, expected, "{typed}"),
                other => panic!("{typed} sent {other:?}"),
            }
            // Sent rather than navigated: the page is still a fetch away, so
            // nothing may be pushed for it yet.
            assert!(state.read().view_stack.is_empty(), "{typed}");
        }
    }

    /// A query is still a query. Only what parses as a link is diverted, or
    /// the box would stop being a search box.
    #[test]
    fn ordinary_text_still_searches() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(connected()));
        state.write().input_mode = InputMode::Search;
        state.write().input_buffer = "drive-by truckers".to_string();

        handle_search_input(KeyEvent::from(KeyCode::Enter), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "drive-by truckers"));
    }

    /// A podcast link is a Spotify link spot cannot play. It says so rather
    /// than searching for the URL as if it were words.
    #[test]
    fn a_podcast_link_says_so_rather_than_searching() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(connected()));
        state.write().input_mode = InputMode::Search;
        state.write().input_buffer =
            "https://open.spotify.com/episode/4uLU6hMCjMI75M1A2tKUQC".to_string();

        handle_search_input(KeyEvent::from(KeyCode::Enter), &state, &tx);
        assert!(rx.try_recv().is_err(), "nothing is fetched");
        let said = state.read().toast.as_ref().map(|(text, _)| text.clone());
        assert!(
            said.as_deref().is_some_and(|t| t.contains("podcasts")),
            "{said:?}"
        );
    }

    /// What the client does once a link resolves: the page opens the way a
    /// click on the same record would, so `Esc` returns one step rather than
    /// none or two.
    #[test]
    fn a_resolved_link_lands_on_the_back_stack_like_a_click() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        assert!(st.view_stack.is_empty());

        navigate(
            &mut st,
            AppCommand::OpenAlbum {
                id: "alb".into(),
                name: "Global Warming".into(),
                credits: vec![Credit {
                    name: "Pitbull".into(),
                    id: Some("pit".into()),
                }],
                year: "2012".into(),
                cover_url: None,
            },
            &tx,
        );
        assert!(matches!(rx.try_recv(), Ok(AppCommand::OpenAlbum { .. })));
        assert_eq!(st.view_stack.len(), 1, "exactly one step back");
    }

    /// The second press that claims the `spotify:` scheme has to belong to the
    /// Links row, not to the screen. Anything that moves away puts the
    /// question back, so a press landing somewhere else can never answer it.
    #[test]
    fn walking_away_from_the_links_row_asks_again() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        let armed = || Some("Enter again to replace Spotify".to_string());

        st.links.confirming = armed();
        move_selection(&mut st, 1);
        assert!(st.links.confirming.is_none(), "moving off it");

        st.links.confirming = armed();
        set_selection(&mut st, 0);
        assert!(st.links.confirming.is_none(), "clicking another row");

        st.links.confirming = armed();
        navigate(&mut st, AppCommand::LoadLikedSongs, &tx);
        assert!(st.links.confirming.is_none(), "leaving Home");
        let _ = rx.try_recv();

        // Staying put does not. `j` at the bottom of the list moves nothing
        // and is not an answer either way — and the bottom is where Links
        // sits.
        let mut st = connected();
        let bottom = st.main_len() - 1;
        set_selection(&mut st, bottom);
        st.links.confirming = armed();
        move_selection(&mut st, 1);
        assert_eq!(st.main_index, bottom);
        assert!(st.links.confirming.is_some(), "staying on it");
    }

    /// Home's rows without the Links entry, which turns on the platform rather
    /// than on the account and is a control rather than a destination.
    fn destinations(st: &AppState) -> Vec<HomeItem> {
        st.home_items()
            .into_iter()
            .filter(|item| *item != HomeItem::Links)
            .collect()
    }

    /// Liked Songs and Discover Weekly are Home rows of their own. Discover
    /// Weekly is Spotify's, so it is only a row when you follow it — and only
    /// when Spotify is the one who made it.
    #[test]
    fn home_lists_discover_weekly_only_when_you_follow_it() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        assert_eq!(
            destinations(&st),
            vec![HomeItem::LikedSongs, HomeItem::Playlists, HomeItem::Radio]
        );

        // Someone else's playlist of the same name is not Spotify's.
        st.set_playlists(vec![playlist("p1", "Discover Weekly", "dm")]);
        assert_eq!(
            destinations(&st),
            vec![HomeItem::LikedSongs, HomeItem::Playlists, HomeItem::Radio]
        );

        st.playlists
            .push(playlist("dw", "Discover Weekly", "spotify"));
        assert_eq!(
            destinations(&st),
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
        let mut st = connected();
        st.set_playlists(vec![playlist("p1", "trendy", "dm")]);
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
        let mut st = connected();
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
        list.header.credits = vec![Credit {
            name: "Muse".into(),
            id: Some("r1".into()),
        }];
        list.append(vec![track("Starlight", Some("a1"))]);
        st.main = MainView::Tracks(list);

        // Empty stack: up to the artist the header credits.
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

    /// The deck draws the queue's name in both views, and it means the list
    /// under it in each: from the bar that list is a screen away, so the name
    /// opens the player; in the player it is right there, so the name folds it
    /// away and back.
    #[test]
    fn the_queue_name_opens_the_player_then_folds_the_queue() {
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
        assert!(!st.queue_folded, "the queue opened folded");

        handle_click(&mut st, on_name, &tx);
        assert!(st.queue_folded, "the name should fold the queue");
        assert!(st.show_player, "folding the queue closed the player");

        handle_click(&mut st, on_name, &tx);
        assert!(!st.queue_folded, "the name should unfold the queue");

        // A click that misses the name leaves both where they were.
        handle_click(&mut st, Position { x: 40, y: 9 }, &tx);
        assert!(st.show_player && !st.queue_folded);
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
        view.rows = rows.into();
        view.loading = false;
        MainView::Radio(view)
    }

    fn live_radio(station: Station) -> crate::app::state::RadioPlayback {
        crate::app::state::RadioPlayback {
            station,
            is_playing: true,
            started_at: Instant::now(),
            title: Arc::new(parking_lot::Mutex::new(None)),
            channels: Default::default(),
            volume_percent: 50,
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
            off_air: false,
            probed: None,
        }
    }

    /// Home's Radio row opens the chart, not your saved list: Saved is empty
    /// until you have kept something, and a destination that opens onto
    /// nothing is a dead end.
    #[test]
    fn the_home_radio_row_opens_the_chart() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        // Found rather than assumed last: the Links row sits below Radio on
        // Windows.
        st.main_index = st
            .home_items()
            .iter()
            .position(|item| *item == HomeItem::Radio)
            .expect("Home always offers Radio");

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
            stations: vec![station].into(),
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
        let mut st = connected();
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

    /// The four tabs are peers under the chart, so walking the strip does not
    /// deepen the path: three tabs on from Popular there is still one step
    /// back, and it leads to the chart.
    #[test]
    fn walking_the_radio_strip_does_not_deepen_the_path() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.push_view();
        st.main = radio_page(RadioScope::Popular, vec![]);

        for scope in [
            RadioScope::Countries,
            RadioScope::Genres,
            RadioScope::Favorites,
        ] {
            open_radio_tab(&mut st, scope.tab(), &tx);
            assert!(matches!(rx.try_recv(), Ok(AppCommand::LoadRadio { scope: s }) if s == scope));
            // The page is installed when the directory answers.
            st.main = radio_page(scope, vec![]);
        }
        assert_eq!(labels(&st), ["radio", "saved stations"]);
    }

    /// …and a tab clicked from a page drilled in under another one comes back
    /// to the chart rather than hanging off the country you were looking at.
    #[test]
    fn a_radio_tab_from_a_drilled_in_page_returns_to_the_chart() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.push_view();
        st.main = radio_page(RadioScope::Popular, vec![]);

        open_radio_tab(&mut st, RadioTab::Countries, &tx);
        st.main = radio_page(RadioScope::Countries, vec![]);
        navigate(
            &mut st,
            AppCommand::LoadRadio {
                scope: RadioScope::Country("GB".into()),
            },
            &tx,
        );
        st.main = radio_page(RadioScope::Country("GB".into()), vec![]);
        assert_eq!(labels(&st), ["radio", "countries", "GB"]);
        while rx.try_recv().is_ok() {}

        open_radio_tab(&mut st, RadioTab::Genres, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::LoadRadio {
                scope: RadioScope::Genres
            })
        ));
        st.main = radio_page(RadioScope::Genres, vec![]);
        assert_eq!(labels(&st), ["radio", "genres"]);
    }

    /// The tab you are already drilled in under is on the path, so it is
    /// walked back to: the list it holds comes back rather than being asked
    /// for a second time.
    #[test]
    fn the_tab_a_drilled_in_page_hangs_from_is_walked_back_to() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.push_view();
        st.main = radio_page(RadioScope::Popular, vec![]);

        open_radio_tab(&mut st, RadioTab::Countries, &tx);
        st.main = radio_page(
            RadioScope::Countries,
            vec![RadioRow::Facet {
                key: "GB".into(),
                label: "The United Kingdom".into(),
                count: 2146,
            }],
        );
        activate_selection(&mut st, &tx);
        st.main = radio_page(RadioScope::Country("GB".into()), vec![]);
        while rx.try_recv().is_ok() {}

        open_radio_tab(&mut st, RadioTab::Countries, &tx);
        let sent: Vec<AppCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            !sent
                .iter()
                .any(|c| matches!(c, AppCommand::LoadRadio { .. })),
            "the country list was asked for again: {sent:?}"
        );
        assert_eq!(labels(&st), ["radio", "countries"]);
        let MainView::Radio(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.rows.len(), 1, "the frame came back without its rows");
    }

    /// Popular *is* the chart the other three hang from, so its tab walks back
    /// to it and opens nothing.
    #[test]
    fn the_popular_tab_walks_back_to_the_chart() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.push_view();
        st.main = radio_page(
            RadioScope::Popular,
            vec![RadioRow::Station(test_station("a", "Radio Paradise"))],
        );

        open_radio_tab(&mut st, RadioTab::Genres, &tx);
        st.main = radio_page(RadioScope::Genres, vec![]);
        while rx.try_recv().is_ok() {}

        open_radio_tab(&mut st, RadioTab::Popular, &tx);
        let sent: Vec<AppCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            !sent
                .iter()
                .any(|c| matches!(c, AppCommand::LoadRadio { .. })),
            "the chart was asked for again: {sent:?}"
        );
        assert_eq!(labels(&st), ["radio"]);
        let MainView::Radio(v) = &st.main else {
            unreachable!()
        };
        assert_eq!(v.rows.len(), 1, "the chart came back without its rows");
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
        v.albums.items.push(AlbumItem {
            id: "a2".into(),
            name: "Hysteria".into(),
            artists: "Muse".into(),
            credits: vec![Credit {
                name: "Muse".into(),
                id: Some("r1".into()),
            }],
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
        assert_eq!(v.albums.display, vec![1]);

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
        v.albums.items.push(AlbumItem {
            id: "a2".into(),
            name: "Hysteria".into(),
            artists: "Muse".into(),
            credits: vec![Credit {
                name: "Muse".into(),
                id: Some("r1".into()),
            }],
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
        assert_eq!(v.albums.display, vec![1]);
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
        assert_eq!(v.albums.display, vec![0]);
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

    /// `V` walks the visualizer modes without leaving the view, and says which
    /// one it landed on — the field carries no label of its own.
    #[test]
    fn shift_v_cycles_the_visualizer() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        state.write().show_player = true;

        let start = state.read().viz.mode;
        let mut seen = vec![start];
        for _ in 0..3 {
            assert!(handle_player_key(
                KeyEvent::from(KeyCode::Char('V')),
                &state,
                &tx
            ));
            let st = state.read();
            assert!(st.show_player, "the key closed the view");
            let mode = st.viz.mode;
            assert!(
                st.toast
                    .as_ref()
                    .is_some_and(|(msg, _)| msg.contains(mode.label())),
                "{mode:?} was not named: {:?}",
                st.toast
            );
            seen.push(mode);
        }
        assert_eq!(seen.last(), Some(&start), "the cycle does not come round");
        let mut distinct = seen[..3].to_vec();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            3,
            "a mode repeated inside one cycle: {seen:?}"
        );
    }

    /// And it is the player view's key alone: on a browse page `V` is free for
    /// whatever the normal handler makes of it, not a silent no-op on a field
    /// that is not drawn.
    #[test]
    fn shift_v_does_nothing_from_the_browse_screen() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        let before = state.read().viz.mode;
        handle_normal(KeyEvent::from(KeyCode::Char('V')), &state, &tx);
        assert_eq!(state.read().viz.mode, before);
        assert!(state.read().toast.is_none());
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
        let state = Arc::new(RwLock::new(connected()));

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

    /// Without an account the answer has one tab, so the results open on it.
    /// Landing on Tracks would put an empty page in front of a search that
    /// found stations.
    #[test]
    fn a_search_without_an_account_opens_on_stations() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(AppState::new()));
        {
            let mut st = state.write();
            st.input_mode = InputMode::Search;
            st.input_buffer = "jazz".into();
        }
        handle_search_input(KeyEvent::from(KeyCode::Enter), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Search(q)) if q == "jazz"));
        assert_eq!(state.read().search_tab, SearchTab::Stations);
    }

    /// Playing, signed in, with two playlists of your own and the box's
    /// controls where the player would have drawn them.
    fn adding() -> AppState {
        let mut st = connected();
        st.show_player = true;
        st.me_id = Some("me".into());
        st.set_playlists(vec![
            state::Playlist {
                id: "p1".into(),
                name: "Late Night".into(),
                track_count: 1,
                owner: "me".into(),
                owner_id: "me".into(),
                snapshot_id: "s".into(),
                cover_url: None,
                public: None,
                collaborative: false,
            },
            state::Playlist {
                id: "p2".into(),
                name: "Someone Else's".into(),
                track_count: 1,
                owner: "them".into(),
                owner_id: "them".into(),
                snapshot_id: "s".into(),
                cover_url: None,
                public: None,
                collaborative: false,
            },
        ]);
        start_playing(&mut st);
        st.hit.add_btn = Rect::new(70, 0, 5, 1);
        st
    }

    /// `adding`, with the box open and every visible row's mark answered for,
    /// which is the state a pick can actually be made from.
    fn adding_open(tx: &UnboundedSender<AppCommand>) -> AppState {
        let mut st = adding();
        cache_playlists(&mut st, &[]);
        handle_click(&mut st, Position { x: 72, y: 0 }, tx);
        st.hit.picker_list = Rect::new(10, 10, 40, 10);
        st
    }

    fn push_playlists(st: &mut AppState, count: usize) {
        for i in 0..count {
            st.playlists.push(state::Playlist {
                id: format!("x{i}"),
                name: format!("Playlist {i}"),
                track_count: 1,
                owner: "me".into(),
                owner_id: "me".into(),
                snapshot_id: "s".into(),
                cover_url: None,
                public: None,
                collaborative: false,
            });
        }
    }

    /// Walk every playlist in `st`, with `holding` the ids of the ones the
    /// playing record is on — the state the prefetch leaves behind.
    fn cache_playlists(st: &mut AppState, holding: &[&str]) {
        let uri = st
            .deck_track()
            .map(|t| t.uri.clone())
            .unwrap_or_else(|| "spotify:track:Uprising".into());
        let track = state::track_id(&uri).to_string();
        let ids: Vec<String> = st.playlists.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            let on = holding.contains(&id.as_str());
            st.playlist_tracks.insert(
                id,
                state::PlaylistContents {
                    snapshot_id: "s".into(),
                    track_ids: on.then(|| track.clone()).into_iter().collect(),
                },
            );
        }
    }

    /// The control opens the box for whatever the deck is about, and the box
    /// holds on to that record: the deck can move on while it is open.
    #[test]
    fn the_add_control_opens_the_box_for_the_playing_record() {
        let (tx, _rx) = channel();
        let mut st = adding();
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        let picker = st.picker.as_ref().expect("no box opened");
        assert_eq!(picker.uri, "spotify:track:Uprising");
        assert!(picker.query.is_empty());
    }

    /// Opening the box asks for the contents of the rows it is showing, and
    /// of those only — a walk costs a request per hundred tracks.
    #[test]
    fn opening_the_box_asks_for_the_rows_it_shows() {
        let (tx, mut rx) = channel();
        let mut st = adding();
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        match rx.try_recv() {
            Ok(AppCommand::CachePlaylistTracks { playlist_ids }) => {
                assert_eq!(playlist_ids, vec!["p1".to_string()], "a followed playlist");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The prefetch has normally answered for everything by the time the box
    /// opens, and a box that already knows asks for nothing.
    #[test]
    fn a_box_over_cached_playlists_asks_for_nothing() {
        let (tx, mut rx) = channel();
        let st = adding_open(&tx);
        assert!(st.picker.is_some());
        assert!(
            !matches!(rx.try_recv(), Ok(AppCommand::CachePlaylistTracks { .. })),
            "it walked a playlist it already held"
        );
    }

    /// The playlists the record is already on open at the top, and checking a
    /// row leaves it where it is — a list that re-sorts under the pointer is
    /// worse than one that waits until next time.
    #[test]
    fn the_box_opens_on_playlist_rows_first_and_holds_that_order() {
        let (tx, _rx) = channel();
        let mut st = adding();
        st.playlists.push(state::Playlist {
            id: "p3".into(),
            name: "Morning".into(),
            track_count: 1,
            owner: "me".into(),
            owner_id: "me".into(),
            snapshot_id: "s".into(),
            cover_url: None,
            public: None,
            collaborative: false,
        });
        cache_playlists(&mut st, &["p3"]);
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        st.hit.picker_list = Rect::new(10, 10, 40, 10);
        assert_eq!(
            st.picker_rows(),
            vec![2, 0],
            "the row it is on is not first"
        );

        handle_click(&mut st, Position { x: 20, y: 10 }, &tx);
        assert_eq!(
            st.picker_rows(),
            vec![2, 0],
            "the row moved under the click"
        );
    }

    /// One click on a row is the whole gesture: it flips that row, and the box
    /// stays up so the next playlist is one more click and not another trip
    /// through the control.
    #[test]
    fn a_click_on_a_row_flips_it_and_leaves_the_box_up() {
        let (tx, mut rx) = channel();
        let mut st = adding_open(&tx);
        while rx.try_recv().is_ok() {}
        handle_click(&mut st, Position { x: 20, y: 10 }, &tx);
        match rx.try_recv() {
            Ok(AppCommand::SetOnPlaylist {
                playlist_id,
                uri,
                on,
                ..
            }) => {
                assert_eq!(playlist_id, "p1");
                assert_eq!(uri, "spotify:track:Uprising");
                assert!(on, "an off row asked to be taken off");
            }
            other => panic!("{other:?}"),
        }
        assert!(st.picker.is_some(), "the box closed on a pick");
        assert!(st.picker.as_ref().unwrap().pending.contains("p1"));
        // The mark answers the press rather than the round trip; the client
        // puts it back if the change is refused.
        assert_eq!(st.picker_has("p1"), Some(true));
    }

    /// And a row already on the playlist asks to come off it, which is the
    /// half of the control that makes it a checkbox.
    #[test]
    fn a_click_on_a_checked_row_takes_the_record_off() {
        let (tx, mut rx) = channel();
        let mut st = adding();
        cache_playlists(&mut st, &["p1"]);
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        st.hit.picker_list = Rect::new(10, 10, 40, 10);
        while rx.try_recv().is_ok() {}
        handle_click(&mut st, Position { x: 20, y: 10 }, &tx);
        match rx.try_recv() {
            Ok(AppCommand::SetOnPlaylist { on, .. }) => assert!(!on),
            other => panic!("{other:?}"),
        }
        assert_eq!(st.picker_has("p1"), Some(false));
    }

    /// A second press on a row already mid-change would put the record on
    /// twice.
    #[test]
    fn a_second_press_while_one_is_in_flight_does_nothing() {
        let (tx, mut rx) = channel();
        let mut st = adding_open(&tx);
        while rx.try_recv().is_ok() {}
        st.picker.as_mut().unwrap().pending.insert("p1".into());
        toggle_picker_row(&mut st, &tx);
        assert!(rx.try_recv().is_err());
    }

    /// The flip goes into the cached contents, which is where the mark is
    /// read from — so one walk of a playlist answers for every record, and a
    /// change made here is a change every record sees.
    #[test]
    fn a_toggle_flips_the_id_in_the_cached_contents() {
        let (tx, _rx) = channel();
        let mut st = adding_open(&tx);
        let id = state::track_id("spotify:track:Uprising").to_string();
        assert!(!st.playlist_tracks["p1"].track_ids.contains(&id));
        toggle_picker_row(&mut st, &tx);
        assert!(st.playlist_tracks["p1"].track_ids.contains(&id));
        // The client rolls a refusal back the same way, over the same set.
        st.picker.as_mut().unwrap().pending.clear();
        toggle_picker_row(&mut st, &tx);
        assert!(!st.playlist_tracks["p1"].track_ids.contains(&id));
    }

    /// A row whose mark is not known yet is inert: reading `·` as "not on it"
    /// and adding would leave a duplicate on a real playlist.
    #[test]
    fn a_row_that_has_not_answered_yet_cannot_be_flipped() {
        let (tx, mut rx) = channel();
        let mut st = adding();
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        while rx.try_recv().is_ok() {}
        assert_eq!(st.picker_has("p1"), None);
        toggle_picker_row(&mut st, &tx);
        assert!(rx.try_recv().is_err());
        assert!(st.picker.as_ref().unwrap().pending.is_empty());
    }

    /// Clicking off closes the box — and does not go on to work whatever it
    /// was covering, which the help box's own dismiss deliberately does.
    #[test]
    fn a_click_off_the_box_closes_it_and_nothing_else() {
        let (tx, mut rx) = channel();
        let mut st = adding_open(&tx);
        while rx.try_recv().is_ok() {}
        st.hit.play_btn = Rect::new(0, 20, 3, 1);
        handle_click(&mut st, Position { x: 1, y: 20 }, &tx);
        assert!(st.picker.is_none());
        assert!(rx.try_recv().is_err(), "the click reached the transport");
    }

    /// The field keeps the box open: a click that missed the caret and closed
    /// it would take the query with it.
    #[test]
    fn a_click_in_the_field_keeps_the_box_open() {
        let (tx, _rx) = channel();
        let mut st = adding();
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        st.hit.picker_field = Rect::new(10, 9, 40, 1);
        handle_click(&mut st, Position { x: 20, y: 9 }, &tx);
        assert!(st.picker.is_some());
    }

    /// The control trades the box for one that makes the playlist the query
    /// was looking for, carrying both the record and the name typed for it.
    #[test]
    fn the_new_playlist_control_opens_a_create_box() {
        let (tx, _rx) = channel();
        let mut st = adding_open(&tx);
        st.picker.as_mut().unwrap().query = " Roadtrip ".into();
        st.hit.picker_new = Rect::new(10, 21, 15, 1);
        handle_click(&mut st, Position { x: 12, y: 21 }, &tx);

        assert!(st.picker.is_none(), "the box that owns the keys stayed up");
        let edit = st.edit.expect("no create box");
        assert_eq!(
            edit.target,
            EditTarget::New {
                uri: "spotify:track:Uprising".into()
            }
        );
        assert_eq!(edit.name, "Roadtrip");
        assert!(edit.description.is_empty());
        assert_eq!(edit.field, EditField::Name);
    }

    /// The same from the keyboard. A chord, because a bare letter in this box
    /// is query text.
    #[test]
    fn ctrl_n_opens_a_create_box() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(adding_open(&tx)));
        handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            &state,
            &tx,
        );
        let st = state.read();
        assert!(st.picker.is_none());
        assert!(matches!(
            st.edit.as_ref().map(|e| &e.target),
            Some(EditTarget::New { .. })
        ));
    }

    /// Sending a create box asks for the playlist and holds the box inert
    /// until the answer lands — there is nothing to show until Spotify says
    /// the playlist is there.
    #[test]
    fn a_create_box_asks_for_the_playlist_and_waits() {
        let (tx, mut rx) = channel();
        let mut st = connected();
        st.edit_seq = 7;
        st.edit = Some(PlaylistEdit {
            target: EditTarget::New {
                uri: "spotify:track:x".into(),
            },
            name: " Roadtrip ".into(),
            description: " long drives ".into(),
            field: EditField::Name,
            pending: false,
            error: None,
            seq: 7,
        });

        submit_edit(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::CreatePlaylist { ref name, ref description, ref uri, seq })
                if name == "Roadtrip"
                    && description == "long drives"
                    && uri == "spotify:track:x"
                    && seq == 7
        ));
        assert!(
            st.edit.as_ref().expect("the box closed early").pending,
            "the box is live while the create is out"
        );
        submit_edit(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "the create went out twice");
    }

    /// While the box is up it owns the keyboard: a letter is a letter, and
    /// the keys it would otherwise be do not reach the app behind it.
    #[test]
    fn the_box_swallows_the_keys_the_app_would_read() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(adding()));
        {
            let mut st = state.write();
            handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        }
        for c in ['q', '?', 'l'] {
            handle_event(Event::Key(KeyEvent::from(KeyCode::Char(c))), &state, &tx);
        }
        let st = state.read();
        assert!(!st.should_quit && !st.show_help);
        assert_eq!(st.picker.as_ref().unwrap().query, "q?l");
    }

    /// Esc closes it, as it closes every other overlay.
    #[test]
    fn esc_closes_the_box() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(adding()));
        {
            let mut st = state.write();
            handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        }
        handle_event(Event::Key(KeyEvent::from(KeyCode::Esc)), &state, &tx);
        assert!(state.read().picker.is_none());
        assert!(state.read().show_player, "and it left the view alone");
    }

    /// The arrows walk the rows the box is showing, and stop at its ends.
    #[test]
    fn the_arrows_walk_the_rows() {
        let (tx, _rx) = channel();
        // One playlist of your own, so there is nowhere to walk to.
        let state = Arc::new(RwLock::new(adding()));
        {
            let mut st = state.write();
            handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        }
        let down = |state: &Arc<RwLock<AppState>>| {
            handle_event(Event::Key(KeyEvent::from(KeyCode::Down)), state, &tx)
        };
        down(&state);
        assert_eq!(state.read().picker.as_ref().unwrap().selected, 0);

        let mut two = adding();
        two.playlists.push(state::Playlist {
            id: "p3".into(),
            name: "Lunch".into(),
            track_count: 1,
            owner: "me".into(),
            owner_id: "me".into(),
            snapshot_id: "s".into(),
            cover_url: None,
            public: None,
            collaborative: false,
        });
        let state = Arc::new(RwLock::new(two));
        {
            let mut st = state.write();
            handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        }
        down(&state);
        assert_eq!(state.read().picker.as_ref().unwrap().selected, 1);
        handle_event(Event::Key(KeyEvent::from(KeyCode::Up)), &state, &tx);
        assert_eq!(state.read().picker.as_ref().unwrap().selected, 0);
    }

    /// The box covers the view, so the wheel is its own wherever it is turned
    /// — and it stops at the ends of the list rather than running past them.
    #[test]
    fn the_wheel_walks_the_box_and_not_the_view_behind_it() {
        let (tx, _rx) = channel();
        let mut st = adding();
        push_playlists(&mut st, PICKER_ROWS + 4);
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        let before = st.main_list.offset();
        handle_scroll(&mut st, Position { x: 0, y: 0 }, 1, &tx);
        assert_eq!(st.picker.as_ref().unwrap().offset, SCROLL_LINES as usize);
        assert_eq!(st.main_list.offset(), before, "the view behind it moved");

        // Rows own: the one from `adding` plus the ones just pushed.
        let rows = st.picker_rows().len();
        for _ in 0..10 {
            handle_scroll(&mut st, Position { x: 0, y: 0 }, 1, &tx);
        }
        assert_eq!(st.picker.as_ref().unwrap().offset, rows - PICKER_ROWS);
        for _ in 0..10 {
            handle_scroll(&mut st, Position { x: 0, y: 0 }, -1, &tx);
        }
        assert_eq!(st.picker.as_ref().unwrap().offset, 0);
    }

    /// Scrolling brings rows into view whose marks nothing has answered for,
    /// so the window that moved is the window that gets asked about.
    #[test]
    fn scrolling_asks_about_the_rows_it_brings_into_view() {
        let (tx, mut rx) = channel();
        let mut st = adding();
        push_playlists(&mut st, PICKER_ROWS + 4);
        handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
        while rx.try_recv().is_ok() {}
        handle_scroll(&mut st, Position { x: 0, y: 0 }, 1, &tx);
        match rx.try_recv() {
            Ok(AppCommand::CachePlaylistTracks { playlist_ids }) => {
                assert_eq!(playlist_ids.len(), PICKER_ROWS);
                assert!(
                    playlist_ids.contains(&"x11".to_string()),
                    "{playlist_ids:?}"
                );
                assert!(!playlist_ids.contains(&"p1".to_string()), "it scrolled off");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Typing resets the selection: the rows under it have changed, and row 2
    /// of the old set is not row 2 of the new one.
    #[test]
    fn typing_puts_the_selection_back_at_the_top() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(adding()));
        {
            let mut st = state.write();
            handle_click(&mut st, Position { x: 72, y: 0 }, &tx);
            st.picker.as_mut().unwrap().selected = 1;
            st.picker.as_mut().unwrap().offset = 1;
        }
        handle_event(Event::Key(KeyEvent::from(KeyCode::Char('l'))), &state, &tx);
        let st = state.read();
        let picker = st.picker.as_ref().unwrap();
        assert_eq!((picker.selected, picker.offset), (0, 0));
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
        st.hit.now_artist_links = vec![(
            Rect::new(4, 9, 6, 1),
            Credit {
                name: "Muse".into(),
                id: Some("r1".into()),
            },
        )];

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
        let mut st = connected();
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

    /// The same two keys on an account that cannot stream: a station's record
    /// is still named, but the pages behind it hold nothing that can be
    /// played, so the deck says so rather than opening a dead end.
    #[test]
    fn b_and_shift_b_say_why_when_spotify_cannot_play() {
        let (tx, mut rx) = channel();
        let mut st = AppState::new();
        st.spotify = SpotifyState::Limited("no Premium".into());
        st.main = radio_page(RadioScope::Popular, Vec::new());
        st.radio = Some(matched_radio());

        open_album_of_selection(&mut st, &tx);
        open_artist_of_selection(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "no page is opened");
        assert!(st.toast.is_some(), "and the deck says why");
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

    /// A station on the deck with three records behind it: two Spotify placed,
    /// and one it could only name.
    fn radio_player_state() -> AppState {
        use crate::app::state::{Heard, RadioMatch};

        let mut st = connected();
        st.show_player = true;
        st.radio = Some(live_radio(test_station("s1", "Groove Salad")));
        let placed = |name: &str| Heard {
            announced: format!("Muse - {name}"),
            matched: RadioMatch::Matched(Box::new(track(name, Some("a1")))),
            at: Instant::now(),
        };
        st.radio_heard.insert(
            "s1".into(),
            vec![
                placed("Alpha"),
                Heard {
                    announced: "Groove Salad - commercial free".into(),
                    matched: RadioMatch::Unmatched,
                    at: Instant::now(),
                },
                placed("Gamma"),
            ],
        );
        st
    }

    /// Enter on a row of the station's list asks for that row, and asks for
    /// nothing else: the record is played without the page leaving the
    /// station or the queue being replaced under it.
    #[test]
    fn enter_on_a_station_row_asks_for_that_row() {
        let (tx, mut rx) = channel();
        let mut st = radio_player_state();
        st.heard_index = 0;

        play_from_queue(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::PlayHeard { row: 0 })
        ));
        assert!(rx.try_recv().is_err(), "one command, not two");
        assert!(st.show_player, "the page stays on the station");
    }

    /// The newest row is the record the station is on, so asking for it means
    /// the broadcast — not a copy of it started over from the beginning while
    /// the station plays on somewhere in the middle.
    #[test]
    fn enter_on_the_newest_row_joins_the_broadcast() {
        let (tx, mut rx) = channel();
        let mut st = radio_player_state();
        st.heard_index = 2;

        play_from_queue(&mut st, &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::PlayStation { station, attempt: 0 }) if station.uuid == "s1"
        ));
        assert!(rx.try_recv().is_err(), "and nothing is queued for it");
    }

    /// A row the station only named has nothing to play, and says so rather
    /// than doing nothing.
    #[test]
    fn enter_on_a_row_the_station_only_named_says_why() {
        let (tx, mut rx) = channel();
        let mut st = radio_player_state();
        st.heard_index = 1;

        play_from_queue(&mut st, &tx);
        assert!(rx.try_recv().is_err(), "nothing to play");
        assert!(
            st.toast
                .as_ref()
                .is_some_and(|(m, _)| m.contains("not on Spotify")),
            "{:?}",
            st.toast
        );
    }

    /// The cursor and the wheel walk the station's rows, not the queue kept
    /// behind them.
    #[test]
    fn the_cursor_walks_the_stations_rows_not_the_kept_queue() {
        let (tx, _rx) = channel();
        let mut st = radio_player_state();
        start_playing(&mut st);
        st.hit.player_queue = Rect::new(0, 0, 80, 2);

        queue_move(&mut st, 1);
        assert_eq!(st.heard_index, 1);
        assert_eq!(st.queue_index, 0, "the kept queue did not move");

        queue_set(&mut st, usize::MAX);
        assert_eq!(st.heard_index, 2, "G reaches the newest row");
        assert_eq!(st.heard_list.offset(), 1, "and the view came with it");

        handle_scroll(&mut st, Position::new(0, 0), -1, &tx);
        assert_eq!(st.heard_list.offset(), 0, "the wheel walked the same rows");
        assert_eq!(st.queue_list.offset(), 0, "and left the kept queue alone");

        handle_scroll(&mut st, Position::new(0, 0), 1, &tx);
        assert_eq!(st.heard_list.offset(), 1, "and stops at the last row");
    }

    /// The `★ ⧉ +` on a station's row act on the row under the pointer, and a
    /// row with no record draws none of them to click.
    #[test]
    fn the_stations_pair_acts_on_the_row_under_the_pointer() {
        let (tx, mut rx) = channel();
        let mut st = radio_player_state();
        st.hit.player_queue = Rect::new(0, 0, 80, 3);
        st.hit.queue_like_col = Rect::new(60, 0, 3, 3);

        handle_click(&mut st, Position::new(61, 2), &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::SetLiked { uri, liked: true }) if uri.ends_with("Gamma")
        ));

        handle_click(&mut st, Position::new(61, 1), &tx);
        assert!(rx.try_recv().is_err(), "a row with no record has no star");
    }

    /// `◂ live` on the station row tunes the station back in, which is what
    /// puts the parked queue back and the stream on air.
    #[test]
    fn the_live_control_tunes_the_station_back_in() {
        let (tx, mut rx) = channel();
        let mut st = radio_player_state();
        st.radio.as_mut().unwrap().off_air = true;
        st.hit.radio_live_btn = Rect::new(70, 20, 6, 1);

        handle_click(&mut st, Position::new(72, 20), &tx);
        assert!(matches!(
            rx.try_recv(),
            Ok(AppCommand::PlayStation { station, attempt: 0 }) if station.uuid == "s1"
        ));
    }

    /// Off air there is a record to seek and a queue to shuffle, so the keys
    /// that explain themselves under a broadcast simply work.
    #[test]
    fn seek_and_shuffle_stop_apologising_once_the_stream_stands_down() {
        let mut st = radio_player_state();
        assert!(on_air(&st));
        st.radio.as_mut().unwrap().off_air = true;
        assert!(!on_air(&st));
    }

    fn artist_with_bio(bio: state::BioState) -> AppState {
        let mut st = AppState::new();
        st.main = MainView::Artist(ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Muse".into(),
            image_url: None,
            genres: Vec::new(),
            bio,
            top: TrackList::new("Muse", "top tracks", None),
            albums: Vec::new().into(),
            tab: state::ArtistTab::Albums,
            loading: false,
            error: None,
        });
        st
    }

    fn article() -> state::BioState {
        state::BioState::Ready(std::sync::Arc::new(state::ArtistBio {
            text: "Muse are an English rock band from Teignmouth.".into(),
            image_url: None,
            source_url: "https://en.wikipedia.org/wiki/Muse_(band)".into(),
        }))
    }

    /// A deliberate press gets an answer either way — the band's own line can
    /// be silently absent, but a key that did nothing would read as broken.
    #[test]
    fn i_opens_the_article_and_says_so_when_there_is_none() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(artist_with_bio(article())));
        handle_normal(KeyEvent::from(KeyCode::Char('i')), &state, &tx);
        assert!(state.read().bio.is_some());

        for (bio, waiting) in [
            (state::BioState::Missing, false),
            (state::BioState::Loading, true),
        ] {
            let state = Arc::new(RwLock::new(artist_with_bio(bio)));
            handle_normal(KeyEvent::from(KeyCode::Char('i')), &state, &tx);
            let st = state.read();
            assert!(st.bio.is_none(), "opened a box over nothing");
            assert!(st.toast.is_some(), "said nothing about why");
            assert_eq!(
                st.toast.as_ref().map(|t| t.0.contains("still reading")),
                Some(waiting)
            );
        }
    }

    /// `i` opened it, so `i` closes it, and so does `Esc` — before Esc means
    /// the back key it means on every page.
    #[test]
    fn the_article_closes_on_i_and_on_esc() {
        let (tx, _rx) = channel();
        for key in ['i', '\u{1b}'] {
            let state = Arc::new(RwLock::new(artist_with_bio(article())));
            state.write().push_view();
            handle_normal(KeyEvent::from(KeyCode::Char('i')), &state, &tx);
            let code = match key {
                '\u{1b}' => KeyCode::Esc,
                c => KeyCode::Char(c),
            };
            handle_normal(KeyEvent::from(code), &state, &tx);
            let st = state.read();
            assert!(st.bio.is_none(), "`{key}` left the box open");
            assert!(
                matches!(st.main, MainView::Artist(_)),
                "`{key}` closed the box and walked the path as well"
            );
        }
    }

    /// The transport keys still mean what they mean: the box is a reading
    /// surface over a page that is still playing.
    #[test]
    fn transport_keys_fall_through_the_article() {
        let (tx, mut rx) = channel();
        let state = Arc::new(RwLock::new(artist_with_bio(article())));
        handle_normal(KeyEvent::from(KeyCode::Char('i')), &state, &tx);
        handle_normal(KeyEvent::from(KeyCode::Char(' ')), &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(AppCommand::PlayPause)));
        assert!(state.read().bio.is_some(), "a transport key closed the box");
    }

    /// Scrolling stops at both ends rather than running off either.
    #[test]
    fn the_article_scrolls_within_its_own_length() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(artist_with_bio(article())));
        handle_normal(KeyEvent::from(KeyCode::Char('i')), &state, &tx);
        {
            let mut st = state.write();
            st.hit.bio_body.height = 4;
            let popup = st.bio.as_mut().unwrap();
            popup.lines = (0..10).map(|i| format!("line {i}")).collect();
        }
        handle_normal(KeyEvent::from(KeyCode::Char('G')), &state, &tx);
        assert_eq!(state.read().bio.as_ref().unwrap().offset, 6);
        handle_normal(KeyEvent::from(KeyCode::Char('j')), &state, &tx);
        assert_eq!(
            state.read().bio.as_ref().unwrap().offset,
            6,
            "ran past the end"
        );
        handle_normal(KeyEvent::from(KeyCode::Char('g')), &state, &tx);
        assert_eq!(state.read().bio.as_ref().unwrap().offset, 0);
        handle_normal(KeyEvent::from(KeyCode::Char('k')), &state, &tx);
        assert_eq!(
            state.read().bio.as_ref().unwrap().offset,
            0,
            "ran past the top"
        );
    }

    /// The wheel is for the box while it is up: the page behind it must not
    /// move under a paragraph you are halfway through.
    #[test]
    fn the_wheel_belongs_to_the_article() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(artist_with_bio(article())));
        handle_normal(KeyEvent::from(KeyCode::Char('i')), &state, &tx);
        let mut st = state.write();
        st.hit.bio_body.height = 4;
        st.bio.as_mut().unwrap().lines = (0..40).map(|i| format!("line {i}")).collect();
        handle_scroll(&mut st, Position { x: 0, y: 0 }, 1, &tx);
        assert_eq!(st.bio.as_ref().unwrap().offset, SCROLL_LINES as usize);
    }

    /// A click inside the box leaves it alone; a click outside is the way out.
    #[test]
    fn a_click_outside_the_article_dismisses_it() {
        let (tx, _rx) = channel();
        let state = Arc::new(RwLock::new(artist_with_bio(article())));
        handle_normal(KeyEvent::from(KeyCode::Char('i')), &state, &tx);
        let mut st = state.write();
        st.hit.bio_box = Rect {
            x: 10,
            y: 5,
            width: 40,
            height: 20,
        };
        handle_click(&mut st, Position { x: 20, y: 10 }, &tx);
        assert!(st.bio.is_some(), "a click in the text closed the box");
        handle_click(&mut st, Position { x: 1, y: 1 }, &tx);
        assert!(st.bio.is_none(), "a click outside left it open");
    }
}
