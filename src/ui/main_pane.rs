use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::columns::{Cell, ColKey, GUTTER, Layout, right, row_spans, scroll_col};
use super::theme;
use crate::app::state::{
    AppState, Credit, Crumb, CrumbTarget, HitAreas, HomeItem, LoadError, MainView, NowStatus,
    SearchTab, Sort, Station, StationNow, TextCol, Track, format_duration,
};

/// Playback context needed to mark the playing row, copied out of the queue
/// before the draw split-borrow.
struct PlayMarks {
    /// URI of the playing track, if any.
    uri: Option<String>,
    /// The playing queue's source key (`"playlist:<id>"`, …), for marking the
    /// row of the playlist it came out of.
    context: Option<String>,
}

/// What the saved page knows about the stations on it, resolved once per frame.
struct NowBoard<'a> {
    /// Readings taken by `Client::refresh_station_now`, keyed by uuid.
    probed: &'a std::collections::HashMap<String, StationNow>,
    /// The station on the deck and what it is announcing. It is already saying
    /// so to the player, so its row reads that rather than opening a second
    /// connection to a stream this machine is already holding open.
    live: Option<(String, Option<String>)>,
}

impl NowBoard<'_> {
    fn status(&self, station: &Station) -> Option<NowStatus> {
        if let Some((uuid, title)) = &self.live
            && *uuid == station.uuid
        {
            return Some(match title {
                Some(t) => NowStatus::Title(t.clone()),
                None => NowStatus::Quiet,
            });
        }
        self.probed.get(&station.uuid).map(|n| n.status.clone())
    }
}

pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // The search input is a permanent row above this pane, not a box inside
    // it; see `super::top_row`.
    let list_area = area;

    let loading = state.loading;
    let retries = state.retries;
    let search_tab = state.search_tab;
    let main_index = state.main_index;
    let mouse = state.mouse_pos;
    let playing_queue = state.playback.as_ref().and(state.queue.as_ref());
    let marks = PlayMarks {
        uri: playing_queue
            .and_then(|q| q.current())
            .map(|t| t.uri.clone()),
        context: playing_queue.and_then(|q| q.source_key.clone()),
    };
    let me_id = state.me_id.clone();
    // Resolved before the split borrow below, which takes the view this reads.
    let controls = header_controls(state);
    // The station playing, if one is, so a radio page can mark its row the way
    // a track table marks the playing track.
    let playing_station = state.radio.as_ref().map(|r| r.station.url.clone());
    // `now_title` takes the decoder thread's lock, so it is read once here
    // rather than once per row.
    let live_now = state
        .radio
        .as_ref()
        .map(|r| (r.station.uuid.clone(), r.now_title()));
    // Home's rows and their tails, resolved before the split borrow below —
    // both read `playlists`, which the borrow takes.
    let home: Vec<(HomeItem, String, String)> = state
        .home_items()
        .into_iter()
        .map(|item| {
            (
                item,
                state.home_count(item),
                state.home_blurb(item).to_string(),
            )
        })
        .collect();
    // The strip the search page draws: four of the five tabs are Spotify's,
    // and without an account there is only the directory's own.
    let search_tabs = state.search_tabs();
    // Split borrows: the view data is read while the list state and hit
    // areas are written.
    let AppState {
        main,
        playlists,
        playlists_error,
        playlists_sort,
        playlists_display,
        radio_favorites,
        radio_now,
        main_list,
        hit,
        liked,
        view_cover,
        page_art,
        ..
    } = state;
    let liked = &*liked;
    let radio_favorites = &*radio_favorites;
    let now_board = NowBoard {
        probed: &*radio_now,
        live: live_now,
    };
    let page_art = &*page_art;
    let playlists = &*playlists;
    let playlists_error = &*playlists_error;
    let playlists_display = &*playlists_display;
    let playlists_sort = *playlists_sort;
    // The *browsed* album's sleeve, not the playing one — see
    // `AppState::view_cover`.
    //
    // A decoded `Cover` knows the URL it came from, and it is checked against
    // the view it is about to be drawn on. `pop_view` restores a header
    // without issuing a fetch, so a cover can outlive the page it belongs to:
    // without this, navigating back to an album you looked at earlier would
    // hang the *last* album's artwork on it. A mismatch draws the placeholder,
    // which is what the band shows while any fetch is in flight anyway.
    let view_cover = match main {
        MainView::Tracks(list) => view_cover
            .as_deref()
            .filter(|c| list.header.cover_url.as_deref() == Some(c.url.as_str())),
        _ => None,
    };

    match main {
        MainView::Home => draw_home(frame, list_area, &home, main_index, main_list, hit, mouse),
        MainView::Playlists => draw_playlists(
            frame,
            list_area,
            playlists,
            playlists_display,
            playlists_sort,
            playlists_error.as_ref(),
            loading,
            me_id.as_deref(),
            main_index,
            main_list,
            retries,
            hit,
            &marks,
            mouse,
        ),
        MainView::Tracks(list) => draw_tracks(
            frame, list_area, list, view_cover, loading, controls, main_index, main_list, retries,
            hit, &marks, liked, mouse,
        ),
        MainView::Search(results) => draw_search(
            frame,
            list_area,
            results,
            loading,
            search_tab,
            &search_tabs,
            main_index,
            main_list,
            retries,
            hit,
            &marks,
            mouse,
            liked,
            radio_favorites,
            playing_station.as_deref(),
        ),
        MainView::Artist(v) => draw_artist(
            frame, list_area, v, page_art, main_index, main_list, retries, hit, &marks, mouse,
            liked,
        ),
        MainView::Radio(v) => draw_radio(
            frame,
            list_area,
            v,
            radio_favorites,
            playing_station.as_deref(),
            &now_board,
            main_index,
            main_list,
            retries,
            hit,
            mouse,
        ),
    }
}

/// Lines one Home entry takes: its name, then the dim line under it.
const HOME_ENTRY_H: usize = 2;
/// Blank rows between Home entries. The nav packed its entries flush because
/// it had 30 cells and a scrollbar to fit them in; a page with a handful of
/// rows on it can spend the space saying they are separate destinations.
const HOME_ENTRY_GAP: usize = 1;
/// Cells the Home entries are indented from the section label, so the label
/// reads as a heading over them rather than as the first of them.
const HOME_INDENT: usize = 2;

/// The landing view: the destinations the app opens onto.
///
/// The rhythm is the left nav's — a name, then a dim line saying what it holds
/// — so a new entry needs nothing here but another [`HomeItem`] and the page
/// it lands on.
///
/// Every entry is one control two lines tall: the whole block is clickable and
/// hovering it lights the *name*, which is the part that says where the row
/// goes. Lighting all of it would mean a filled bar, which this UI spends on
/// nothing (see [`super::table::selection_style`]), and lighting the name
/// alone would mean a six-cell target on a full-width row.
///
/// [`HomeItem`]: crate::app::state::HomeItem
fn draw_home(
    frame: &mut Frame,
    area: Rect,
    items: &[(HomeItem, String, String)],
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) {
    let inner = body_area(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let indent = " ".repeat(HOME_INDENT);
    // The count is right-aligned against the pane, so the name and the number
    // sit at the two ends of a row the way a table's first and last column do.
    let name_w = (inner.width as usize).saturating_sub(HOME_INDENT);
    // Lines are planned against the scroll offset before anything is drawn:
    // an entry's name cannot be lit until its rect is known, and its rect
    // depends on where the entry lands on screen.
    let stride = HOME_ENTRY_H + HOME_ENTRY_GAP;
    super::clamp_offset(list_state, items.len() * stride, inner.height as usize);
    let offset = list_state.offset();

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut owner: Vec<Option<usize>> = Vec::new();
    for (i, (item, count, blurb)) in items.iter().enumerate() {
        if i > 0 {
            for _ in 0..HOME_ENTRY_GAP {
                lines.push(Line::default());
                owner.push(None);
            }
        }
        let rect = entry_rect(inner, offset, lines.len(), HOME_ENTRY_H);
        hit.home_rows.push((rect, i));

        // The name keeps its natural width so a hover pill is the size of the
        // word; the gap to the count is spaces of its own.
        let title = fit(item.title(), name_w.saturating_sub(count.len()));
        let title = title.trim_end().to_string();
        let pad = name_w.saturating_sub(super::table::width(&title) + count.len());
        let mut name = Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled(title, theme::text()),
            Span::raw(" ".repeat(pad)),
            Span::styled(count.clone(), theme::dim()),
        ]);
        if i == main_index {
            super::table::apply_selection(&mut name);
        }
        if mouse.is_some_and(|m| rect.contains(m)) {
            let title = &mut name.spans[1];
            title.style = super::table::hover_style(title.style);
        }
        lines.push(name);
        owner.push(Some(i));
        lines.push(Line::styled(
            format!("{indent}{}", fit(blurb, name_w)),
            theme::dim(),
        ));
        owner.push(Some(i));
    }

    hit.main_lines = owner;
    hit.main_list = inner;
    frame.render_stateful_widget(
        List::new(lines.into_iter().map(ListItem::new).collect::<Vec<_>>()),
        inner,
        list_state,
    );
}

/// Where `height` content lines starting at line `start` land on screen, given
/// the list's scroll `offset`. Empty when they are scrolled out of `inner`,
/// which can never be hit.
fn entry_rect(inner: Rect, offset: usize, start: usize, height: usize) -> Rect {
    let Some(top) = start.checked_sub(offset) else {
        return Rect::default();
    };
    Rect {
        y: inner.y.saturating_add(top as u16),
        height: height as u16,
        ..inner
    }
    .intersection(inner)
}

/// The playlists, one row each.
///
/// At full pane width the count is a column rather than a second line under
/// the name, so the page holds twice as many rows and can afford to say who
/// owns them.
#[allow(clippy::too_many_arguments)]
fn draw_playlists(
    frame: &mut Frame,
    area: Rect,
    playlists: &[crate::app::state::Playlist],
    display: &[usize],
    sort: Sort,
    error: Option<&LoadError>,
    loading: bool,
    me_id: Option<&str>,
    main_index: usize,
    list_state: &mut ListState,
    retries: u32,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    mouse: Option<Position>,
) {
    let inner = body_area(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Before the column header, which is a heading for a table this page does
    // not have: a page that is still asking, or that was refused, has nothing
    // to put columns over.
    if playlists.is_empty() && (error.is_some() || loading) {
        hit.main_list = inner;
        match error {
            Some(e) => error_message(frame, inner, e, retries, hit, mouse),
            None => loading_message(frame, inner, "loading playlists…"),
        }
        return;
    }

    let layout = Layout::resolve(&super::columns::playlists(), inner.width as usize, 0);
    let rows_area = layout.draw_header(frame, inner, Some(sort), mouse, hit);
    hit.main_list = rows_area;

    // Playlists only. Liked Songs is a Home row of its own: it is not a
    // playlist, so it does not belong under a heading that says it is.
    let rows: Vec<_> = display.iter().filter_map(|&i| playlists.get(i)).collect();
    let count = rows.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);
    let hover = super::table::hovered_row(rows_area, list_state.offset(), count, mouse);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let playing =
                marks.context.as_deref() == Some(crate::app::state::playlist_key(&p.id).as_str());
            super::table::hover_row(
                playlist_row(p, &layout, me_id, playing, i == main_index),
                Some(i) == hover,
            )
        })
        .collect();
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// One station row. `now` is what it is announcing, where the page asked.
fn station_row(
    s: &crate::app::state::Station,
    layout: &Layout,
    saved: bool,
    playing: bool,
    selected: bool,
    now: Option<NowStatus>,
) -> ListItem<'static> {
    let dim = theme::dim();
    // An HLS station is listed but cannot be played, so it is drawn as
    // something that is there rather than something that is offered. Hiding
    // them would quietly remove the BBC and most national broadcasters from
    // the directory, which is a worse lie than a dim row.
    let name_style = if s.hls { dim } else { theme::text() };
    let spans = row_spans(layout, |cell, spans| match cell.key {
        ColKey::Mark => spans.push(match playing {
            true => Span::styled("♫ ", theme::accent()),
            false => Span::raw(" ".repeat(cell.width)),
        }),
        // The saved mark reuses the track table's `★`, and for the same
        // reason: this is the same gesture on the same key, and a second
        // glyph for it would say there were two kinds of keeping.
        ColKey::Saved => {
            let mark = match saved {
                true => super::table::LIKED_MARK,
                false => "",
            };
            spans.push(Span::styled(fit(mark, cell.width), theme::accent()));
        }
        ColKey::Station => spans.push(Span::styled(fit(&s.name, cell.width), name_style)),
        // A station that was reached and says nothing reads the same as one
        // that would not answer: both are "no record to name", and the row has
        // no room to spell the difference. What separates them is that the
        // first is a settled answer and the second will be asked again.
        ColKey::Now => {
            let (text, style) = match &now {
                Some(NowStatus::Title(t)) => (t.as_str(), theme::text()),
                Some(NowStatus::Probing) => ("…", dim),
                _ => ("—", dim),
            };
            let style = match playing {
                true => theme::accent(),
                false => style,
            };
            spans.push(Span::styled(fit(text, cell.width), style));
        }
        ColKey::Tags => spans.push(Span::styled(fit(&s.tags, cell.width), dim)),
        ColKey::Where => {
            let where_ = match s.countrycode.is_empty() {
                true => s.country.as_str(),
                false => s.countrycode.as_str(),
            };
            spans.push(Span::styled(fit(where_, cell.width), dim));
        }
        ColKey::Stream => spans.push(Span::styled(right(&s.quality(), cell.width), dim)),
        _ => spans.push(Span::raw(" ".repeat(cell.width))),
    });
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// A table of stations: the header, a spacer, the rows, and the scrollbar.
///
/// Two pages list stations — a radio page and a search's Stations tab — and
/// this is the whole of what they have in common, so neither can drift away
/// from the other's column widths or marks. In the spirit of
/// [`render_track_table`] and [`render_album_table`].
#[allow(clippy::too_many_arguments)]
fn render_station_table(
    frame: &mut Frame,
    body: Rect,
    stations: &[&crate::app::state::Station],
    favorites: &[crate::app::state::Station],
    playing_url: Option<&str>,
    now: Option<&NowBoard>,
    sort: Sort,
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) {
    let layout = Layout::resolve(
        &super::columns::stations(now.is_some()),
        body.width as usize,
        0,
    );
    let rows_area = layout.draw_header(frame, body, Some(sort), mouse, hit);
    hit.main_list = rows_area;

    let count = stations.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);
    let hover = super::table::hovered_row(rows_area, list_state.offset(), count, mouse);
    let items: Vec<ListItem> = stations
        .iter()
        .enumerate()
        .map(|(i, s)| {
            super::table::hover_row(
                station_row(
                    s,
                    &layout,
                    favorites.iter().any(|f| f.uuid == s.uuid),
                    playing_url == Some(s.url.as_str()),
                    i == main_index,
                    now.and_then(|board| board.status(s)),
                ),
                Some(i) == hover,
            )
        })
        .collect();
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// One country or genre row: a name and how many stations are behind it.
fn facet_row(
    key: &str,
    label: &str,
    count: u32,
    layout: &Layout,
    selected: bool,
) -> ListItem<'static> {
    let spans = row_spans(layout, |cell, spans| match cell.key {
        ColKey::Code => spans.push(Span::styled(
            fit(&key.to_uppercase(), cell.width),
            theme::dim(),
        )),
        ColKey::Name => spans.push(Span::styled(fit(label, cell.width), theme::text())),
        ColKey::Stations => spans.push(Span::styled(
            right(&count.to_string(), cell.width),
            theme::dim(),
        )),
        _ => spans.push(Span::raw(" ".repeat(cell.width))),
    });
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// The radio directory: a tab strip over one table of rows.
///
/// Every scope draws through here — the chart, a country's stations, the ones
/// you kept — because they are all the same table with a different query
/// behind them. Drilling into a country pushes another of these pages, so the
/// trail reads `HOME › COUNTRIES › GB` and Esc walks back out of it.
#[allow(clippy::too_many_arguments)]
fn draw_radio(
    frame: &mut Frame,
    area: Rect,
    view: &crate::app::state::RadioView,
    favorites: &[crate::app::state::Station],
    playing_url: Option<&str>,
    now: &NowBoard,
    main_index: usize,
    list_state: &mut ListState,
    retries: u32,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) {
    use crate::app::state::{RadioRow, RadioScope, RadioTab};

    let facets = matches!(view.rows.first(), Some(RadioRow::Facet { .. }));
    let inner = body_area(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The tab strip, then a blank, then the table — the same rhythm the search
    // page uses, so the two browse screens scroll the same way.
    let mut body = inner;
    if inner.height >= 4 {
        let row = Rect { height: 1, ..inner };
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut x = row.x;
        tab_segments(
            &mut spans,
            &mut x,
            row,
            mouse,
            &RadioTab::ALL,
            view.scope.tab(),
            RadioTab::title,
            &mut hit.radio_tabs,
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
        body = Rect {
            y: inner.y + 2,
            height: inner.height - 2,
            ..inner
        };
    }

    if view.rows.is_empty() {
        hit.main_list = body;
        match &view.error {
            Some(e) => error_message(frame, body, e, retries, hit, mouse),
            None if view.loading => loading_message(frame, body, "loading stations…"),
            None => empty_message(frame, body, radio_empty_hint(view)),
        }
        return;
    }

    // A station list is the same table the Stations tab of a search draws, so
    // it goes through the same builder.
    if !facets {
        let stations: Vec<&crate::app::state::Station> = view
            .rows
            .rows()
            .filter_map(|r| match r {
                RadioRow::Station(s) => Some(s),
                RadioRow::Facet { .. } => None,
            })
            .collect();
        // Only the saved page asks what its stations are playing, so only it
        // has a column for the answer — see `columns::stations`.
        let now = matches!(view.scope, RadioScope::Favorites).then_some(now);
        render_station_table(
            frame,
            body,
            &stations,
            favorites,
            playing_url,
            now,
            view.rows.sort,
            main_index,
            list_state,
            hit,
            mouse,
        );
        return;
    }

    // Countries and genres are their own two-column table, headed by what its
    // rows are — the tab above says which list you are on, not what the
    // number beside each row counts.
    let countries = !matches!(view.scope.tab(), RadioTab::Genres);
    let label = match countries {
        true => "Country",
        false => "Genre",
    };
    let layout = Layout::resolve(
        &super::columns::facets(label, countries),
        body.width as usize,
        0,
    );
    let rows_area = layout.draw_header(frame, body, Some(view.rows.sort), mouse, hit);
    hit.main_list = rows_area;
    let count = view.rows.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);
    let hover = super::table::hovered_row(rows_area, list_state.offset(), count, mouse);
    let items: Vec<ListItem> = view
        .rows
        .rows()
        .enumerate()
        .map(|(i, row)| {
            let item = match row {
                RadioRow::Facet { key, label, count } => {
                    facet_row(key, label, *count, &layout, i == main_index)
                }
                // Unreachable: `facets` is read off the first row, and the
                // directory never mixes the two kinds in one answer.
                RadioRow::Station(s) => facet_row("", &s.name, 0, &layout, i == main_index),
            };
            super::table::hover_row(item, Some(i) == hover)
        })
        .collect();
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// What to say on a radio page with nothing on it.
fn radio_empty_hint(view: &crate::app::state::RadioView) -> &'static str {
    use crate::app::state::RadioScope;
    match view.scope {
        RadioScope::Favorites => "no saved stations yet — press L on one to keep it",
        _ => "nothing here",
    }
}

/// Rows an album card's sleeve spans, and the gap between it and the card's
/// text. Smaller than the header band's block — a card is a row in a list, and
/// a sleeve as big as the page's own would compete with it — but big enough
/// that a record is recognisable from its cover, which is the point of a card.
const CARD_ART_H: u16 = 4;
const CARD_GAP: u16 = 2;
/// Lines one card occupies: the sleeve's rows, then a blank between cards.
const CARD_H: usize = CARD_ART_H as usize + 1;
/// Narrowest text column a card keeps its sleeve for. Below it the cards go
/// text-only rather than squeezing a name into a handful of cells.
const MIN_CARD_TEXT_W: u16 = 24;
/// The play control on a card, and on the header bands above it.
const PLAY_PILL: &str = "▶ play";
/// The shuffle control beside every ▶ play, wearing the same ▶: the dedicated
/// shuffle glyphs are emoji- or ambiguous-width, which would drift the
/// recorded hit rect away from what the terminal draws.
const SHUFFLE_PILL: &str = "▶ shuffle";

/// The playlist controls, beside ▶ play. `★`/`☆` rather than words, matching
/// the mark a track row already carries for the same idea.
const SAVED_PILL: &str = "★ saved";
const SAVE_PILL: &str = "☆ save";
const EDIT_PILL: &str = "edit";

/// The share control every header band carries, right of ▶ shuffle. It copies
/// the link to the page itself, where a track row's `⧉` copies the link to one
/// record on it — the same mark for the same idea at two scales.
const SHARE_PILL: &str = "⧉ share";

/// Narrowest text column that seats both card pills with their gap; below it
/// the card keeps ▶ play alone.
fn card_pills_min_w() -> usize {
    super::table::width(PLAY_PILL) + 1 + super::table::width(SHUFFLE_PILL)
}

/// The playlist-only controls a header band carries, settled before the draw
/// because only the state knows them and the band is handed a view.
#[derive(Default, Clone, Copy)]
struct HeaderControls {
    /// Draw the save control, and whether it reads as already saved. `None`
    /// on a page that is not a playlist, on one still being asked about, and
    /// on one you own — Spotify spells deleting your own playlist as unsaving
    /// it, and that does not belong under one keypress beside ▶ play.
    save: Option<bool>,
    /// Draw the edit control. Only a playlist you own takes it; Spotify
    /// refuses the change for any other.
    edit: bool,
    /// Draw the share control. Off on a page with no link of its own to give —
    /// see [`AppState::open_page_link`].
    share: bool,
}

/// What the open page offers beyond playing itself.
fn header_controls(st: &AppState) -> HeaderControls {
    let share = st.open_page_link().is_some();
    let Some(id) = st.open_playlist_id() else {
        return HeaderControls {
            share,
            ..Default::default()
        };
    };
    if st.owns_open_playlist() {
        return HeaderControls {
            save: None,
            edit: true,
            share,
        };
    }
    HeaderControls {
        save: st.saved_playlists.get(id).copied(),
        edit: false,
        share,
    }
}

/// Append a dim pill after the ones already laid down, and return its hit
/// rect. Accent under the pointer, which is all `hover_style` promotes.
fn pill_segment(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    area: Rect,
    mouse: Option<Position>,
    label: &'static str,
) -> Rect {
    spans.push(Span::raw(" "));
    *x = x.saturating_add(1);
    super::table::segment(
        spans,
        x,
        area,
        mouse,
        vec![Span::styled(label, theme::dim())],
    )
}

/// The save control. Accent when the playlist is in the library and dim when
/// it is not — the same pair of styles a track row's `★` uses to say it.
fn save_segment(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    area: Rect,
    mouse: Option<Position>,
    saved: bool,
) -> Rect {
    spans.push(Span::raw(" "));
    *x = x.saturating_add(1);
    let (label, style) = match saved {
        true => (SAVED_PILL, theme::accent()),
        false => (SAVE_PILL, theme::dim()),
    };
    super::table::segment(spans, x, area, mouse, vec![Span::styled(label, style)])
}

/// Append the shuffle pill after a band's ▶ play and return its hit rect.
/// Dim at rest; under the pointer it takes the accent, which `hover_style`
/// keeps — it only promotes DIM text.
fn shuffle_segment(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    area: Rect,
    mouse: Option<Position>,
) -> Rect {
    spans.push(Span::raw(" "));
    *x = x.saturating_add(1);
    let rect = Rect {
        x: *x,
        y: area.y,
        width: super::table::width(SHUFFLE_PILL) as u16,
        height: 1,
    };
    let hovered = mouse.is_some_and(|m| rect.contains(m)) && rect.right() <= area.right();
    super::table::segment(
        spans,
        x,
        area,
        mouse,
        vec![Span::styled(
            SHUFFLE_PILL,
            if hovered {
                theme::accent()
            } else {
                theme::dim()
            },
        )],
    )
}

/// One line of the artist page's scrolling body.
///
/// The page is a single list with two kinds of row in it, so the lines are
/// planned before anything is drawn: the plan gives the scroll length, the
/// line-to-row model the mouse resolves through, and the screen position of
/// every sleeve that has to be painted over the list afterwards.
enum ArtistLine {
    /// A section label ("Top Tracks", "Albums").
    Heading(&'static str),
    /// The track table's column header.
    TrackHeader,
    /// A top track, by index into `top.display`.
    Track(usize),
    /// The album-group tab strip, under the "Albums" heading.
    Tabs,
    /// One row of an album card: `row` counts from the top of its sleeve, and
    /// `album` indexes `display` — the cards of the open tab, not the whole
    /// catalogue.
    Card {
        album: usize,
        row: u16,
    },
    Blank,
}

impl ArtistLine {
    /// The selectable row this line belongs to, in the page's flat index
    /// space: top tracks first, then albums. `split` is where albums start.
    fn item(&self, split: usize) -> Option<usize> {
        match *self {
            ArtistLine::Track(i) => Some(i),
            ArtistLine::Card { album, .. } => Some(split + album),
            _ => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_artist(
    frame: &mut Frame,
    area: Rect,
    v: &crate::app::state::ArtistView,
    page_art: &crate::cover::CoverCache,
    main_index: usize,
    list_state: &mut ListState,
    retries: u32,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    mouse: Option<Position>,
    liked: &std::collections::HashMap<String, bool>,
) {
    let inner = body_area(area);
    let body = artist_band(frame, inner, v, page_art, hit, mouse);
    if v.len() == 0 {
        hit.main_list = body;
        match &v.error {
            Some(e) => error_message(frame, body, e, retries, hit, mouse),
            None if v.loading => loading_message(frame, body, "loading…"),
            None => empty_message(frame, body, "nothing to show for this artist"),
        }
        return;
    }
    artist_body(
        frame, body, v, page_art, main_index, list_state, hit, marks, liked, mouse,
    );
}

/// The artist page's header band: the photo, the name, what Spotify says the
/// artist plays, what the page holds, and ▶ play for the artist's own context.
///
/// Deliberately the same shape as an album's [`header_band`] — a portrait
/// where the sleeve goes, the name at the top of the column beside it, the
/// control at the bottom of that column — because they are the same kind of
/// page and must not read as two different products.
fn artist_band(
    frame: &mut Frame,
    inner: Rect,
    v: &crate::app::state::ArtistView,
    page_art: &crate::cover::CoverCache,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) -> Rect {
    if inner.height < 8 {
        return inner;
    }
    let gray = theme::text();
    let dim = theme::dim();

    let photo_w = super::table::art_w(ART_H);
    let cover = v.image_url.as_deref().and_then(|u| page_art.get(u));
    let art = (v.image_url.is_some()
        && inner.height >= ART_BAND_H + MIN_TABLE_H
        && inner.width >= photo_w + ART_GAP + MIN_META_W)
        .then(|| {
            let art = Rect {
                width: photo_w,
                height: ART_H,
                ..inner
            };
            super::table::draw_art(frame, art, cover.as_deref(), &v.id);
            art
        });
    let text = match art {
        Some(art) => Rect {
            x: art.right() + ART_GAP,
            width: inner.width - photo_w - ART_GAP,
            ..inner
        },
        None => inner,
    };
    let stacked = art.is_some();

    // Genres are what Spotify has instead of a bio. They are also deprecated
    // upstream, so this is usually empty and the line simply is not there.
    let genres: String = v
        .genres
        .iter()
        .take(3)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" · ");
    let counts = Span::styled(artist_counts(v), dim);

    let info_area = Rect { height: 1, ..text };
    let counts_w = if stacked {
        0
    } else {
        (counts.width() as u16).min(info_area.width)
    };
    let name_area = Rect {
        width: info_area.width.saturating_sub(counts_w + 1),
        ..info_area
    };
    let mut left = vec![Span::styled(
        v.name.clone(),
        theme::bright().add_modifier(Modifier::BOLD),
    )];
    // Sharing the row costs the genres their own line, so they take it only
    // when the whole run fits: a genre list clipped mid-word beside a name
    // reads as damage rather than as detail.
    let sharing = super::table::width(&v.name) + super::table::width(&genres) + 2;
    if !genres.is_empty() && !stacked && sharing <= name_area.width as usize {
        left.push(Span::styled(format!("  {genres}"), gray));
    }
    frame.render_widget(Paragraph::new(Line::from(left)), name_area);
    let row = |n: u16| Rect {
        y: text.y + n,
        height: 1,
        ..text
    };
    if stacked {
        // Stacked under the name, in order, skipping what Spotify did not
        // give us — the counts belong against the name, not floating a row
        // below an empty genre line.
        let mut n = 1;
        if !genres.is_empty() {
            frame.render_widget(Paragraph::new(Line::styled(genres, gray)), row(n));
            n += 1;
        }
        frame.render_widget(Paragraph::new(Line::from(counts)), row(n));
    } else {
        frame.render_widget(
            Paragraph::new(Line::from(counts)),
            Rect {
                x: info_area.right().saturating_sub(counts_w),
                width: counts_w,
                ..info_area
            },
        );
    }

    let play_area = row(if stacked { 4 } else { 1 });
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut x = play_area.x;
    hit.header_play_btn = super::table::segment(
        &mut spans,
        &mut x,
        play_area,
        mouse,
        vec![Span::styled(PLAY_PILL, theme::accent())],
    );
    hit.header_shuffle_btn = shuffle_segment(&mut spans, &mut x, play_area, mouse);
    // Unconditional, unlike the list header's: an artist page is opened by id,
    // so there is always a link to give.
    hit.header_share_btn = pill_segment(&mut spans, &mut x, play_area, mouse, SHARE_PILL);
    frame.render_widget(Paragraph::new(Line::from(spans)), play_area);

    let used = if stacked { ART_BAND_H } else { TEXT_BAND_H };
    Rect {
        y: inner.y + used,
        height: inner.height - used,
        ..inner
    }
}
/// "30 records", or nothing until the catalogue lands.
///
/// Records rather than albums: the catalogue holds every group now — singles,
/// compilations and the records the artist only plays on — and calling that
/// total "albums" would disagree with the tab of that name under it.
///
/// The whole catalogue, not the open tab. The band is about the artist, and a
/// number that changed every time you switched tab would be about the strip.
///
/// The top tracks are not counted here. They are a numbered list a few rows
/// below, under a heading that names them — a band reading "10 top tracks"
/// over a list running 1 to 10 is reading the screen back to you.
fn artist_counts(v: &crate::app::state::ArtistView) -> String {
    match v.albums.len() {
        0 => String::new(),
        1 => "1 record".to_string(),
        n => format!("{n} records"),
    }
}

/// The artist page's one scrolling list: the top tracks, then the catalogue as
/// album cards.
#[allow(clippy::too_many_arguments)]
fn artist_body(
    frame: &mut Frame,
    body: Rect,
    v: &crate::app::state::ArtistView,
    page_art: &crate::cover::CoverCache,
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    liked: &std::collections::HashMap<String, bool>,
    mouse: Option<Position>,
) {
    hit.main_list = body;
    let width = body.width as usize;
    let height = body.height as usize;
    let split = v.top.len();
    let layout = Layout::resolve(&super::columns::tracks(false), width, v.top.items.len());

    // Plan the lines first: their count is the scroll length, and their order
    // is what the mouse and the keyboard both resolve through.
    let mut plan: Vec<ArtistLine> = Vec::new();
    if split > 0 {
        plan.push(ArtistLine::Heading("Top Tracks"));
        // A blank under the heading, as under "Albums". The column header used
        // to sit straight beneath it, which put two header-weight lines back
        // to back and made the section label look like part of the table.
        plan.push(ArtistLine::Blank);
        plan.push(ArtistLine::TrackHeader);
        // A blank under the column header too, the gap every other table on
        // the browse screen keeps between its header and its rows.
        plan.push(ArtistLine::Blank);
        plan.extend((0..split).map(ArtistLine::Track));
        plan.push(ArtistLine::Blank);
    }
    let tracks_at = (split > 0).then_some(4usize);
    let cards_at = plan.len();
    // One group is no choice, so its strip would only say what the heading
    // above it already says.
    let tabs = v.tabs();
    let strip = tabs.len() > 1;
    if !v.albums.is_empty() {
        plan.push(ArtistLine::Heading("Albums"));
        plan.push(ArtistLine::Blank);
        if strip {
            // Strip, blank, then the rows — the rhythm the radio and search
            // pages keep above their tables.
            plan.push(ArtistLine::Tabs);
            plan.push(ArtistLine::Blank);
        }
        for album in 0..v.albums.len() {
            plan.extend((0..CARD_ART_H).map(|row| ArtistLine::Card { album, row }));
            plan.push(ArtistLine::Blank);
        }
    }
    let cards_from = cards_at + if strip { 4 } else { 2 };

    super::clamp_offset(list_state, plan.len(), height);
    let offset = list_state.offset();
    hit.main_lines = plan.iter().map(|l| l.item(split)).collect();
    // Screen row of a planned line, when it is inside the viewport.
    let screen_y = |line: usize| {
        (line >= offset && line < offset + height).then(|| body.y + (line - offset) as u16)
    };

    // Cards keep their sleeve only where the text beside it still has room.
    let art_w = if body.width >= CARD_ART_W + CARD_GAP + MIN_CARD_TEXT_W {
        CARD_ART_W
    } else {
        0
    };
    let indent = if art_w > 0 { art_w + CARD_GAP } else { 0 };
    let card_text_w = width.saturating_sub(indent as usize);

    // The tab strip scrolls with the body, so it is a control only while it
    // is on screen: off it, `segment` clips every rect to nothing and the
    // labels cannot be clicked where they are not drawn.
    hit.artist_tabs.clear();
    let mut tab_spans: Vec<Span<'static>> = Vec::new();
    if strip {
        let row = Rect {
            x: body.x,
            y: screen_y(cards_at + 2).unwrap_or(body.y),
            width: body.width,
            height: u16::from(screen_y(cards_at + 2).is_some()),
        };
        let mut x = row.x;
        tab_segments(
            &mut tab_spans,
            &mut x,
            row,
            mouse,
            &tabs,
            v.tab,
            crate::app::state::ArtistTab::title,
            &mut hit.artist_tabs,
        );
    }

    // The controls on each visible card — its name, its ▶ play, and its
    // shuffle — recorded before the rows are built, so hovering one can
    // light it.
    hit.card_play.clear();
    hit.card_shuffle.clear();
    hit.album_names.clear();
    for (album, a) in v.albums.rows().enumerate() {
        let first = cards_from + album * CARD_H;
        let push = |hits: &mut Vec<(Rect, usize)>, row: u16, dx: u16, w: u16| {
            let Some(y) = screen_y(first + row as usize) else {
                return;
            };
            let rect = Rect {
                x: body.x + indent + dx,
                y,
                width: w,
                height: 1,
            }
            .intersection(body);
            if !rect.is_empty() {
                hits.push((rect, split + album));
            }
        };
        // The link is the name as printed — not the cell it sits in, which is
        // padded to the pane's width and would make the whole row a link.
        let name_w = super::table::width(fit(&a.name, card_text_w).trim_end()) as u16;
        push(&mut hit.album_names, 0, 0, name_w);
        push(
            &mut hit.card_play,
            CARD_PLAY_ROW,
            0,
            super::table::width(PLAY_PILL) as u16,
        );
        if card_text_w >= card_pills_min_w() {
            push(
                &mut hit.card_shuffle,
                CARD_PLAY_ROW,
                (super::table::width(PLAY_PILL) + 1) as u16,
                super::table::width(SHUFFLE_PILL) as u16,
            );
        }
    }
    let over = |hits: &[(Rect, usize)]| {
        mouse.and_then(|m| {
            hits.iter()
                .find(|(r, _)| r.contains(m))
                .map(|(_, row)| row - split)
        })
    };
    let hover_album = over(&hit.album_names);
    let hover_play = over(&hit.card_play);
    let hover_shuffle = over(&hit.card_shuffle);

    // Clickable cells of the track block, clipped to the rows on screen.
    let mut hover_cell: Option<(usize, HoverCol)> = None;
    if let Some(first) = tracks_at {
        let visible = (first.max(offset), (first + split).min(offset + height));
        if visible.1 > visible.0 {
            let rows: Vec<&Track> = (visible.0 - first..visible.1 - first)
                .map(|i| &v.top.items[v.top.display[i]])
                .collect();
            track_cells(
                &layout,
                body,
                body.y + (visible.0 - offset) as u16,
                &rows,
                hit,
            );
            let artist_x = layout
                .cell(ColKey::Artist)
                .map(|c| body.x.saturating_add(c.x as u16));
            // The column, then the row: both rects sit inside the track
            // block, so the row arithmetic is only safe once one of them has
            // claimed the pointer.
            hover_cell = mouse
                .and_then(|m| hovered_cell(hit, m, artist_x).map(|col| (m, col)))
                .map(|(m, col)| (offset + (m.y - body.y) as usize - first, col));
        }
    }

    // Row positions are in display order; the playing marker resolves by URI.
    let playing = marks.uri.as_deref().and_then(|uri| v.top.position_of(uri));
    // The column header is a planned line inside the scrolling body rather
    // than a fixed paragraph above it, so it is drawn at whatever row the
    // scroll has put it on — and its hit rects follow it off screen the way
    // the album-group strip above does.
    let header_at = tracks_at.map(|first| first - 2);
    let header_row = Rect {
        x: body.x,
        y: header_at.and_then(screen_y).unwrap_or(body.y),
        width: body.width,
        height: u16::from(header_at.and_then(screen_y).is_some()),
    };
    let header_line = layout.header_line(Some(v.top.sort), header_row, mouse, hit);
    // The pointer lands on a planned line, and only a track line is a table
    // row: a heading, a blank or a card is not something a wash would be about.
    let hover = super::table::hovered_row(body, offset, plan.len(), mouse);
    let items: Vec<ListItem> = plan
        .iter()
        .enumerate()
        .map(|(at, line)| {
            let item = match *line {
                ArtistLine::Heading(text) => ListItem::new(Line::styled(
                    text.to_string(),
                    theme::text().add_modifier(Modifier::BOLD),
                )),
                ArtistLine::TrackHeader => ListItem::new(header_line.clone()),
                ArtistLine::Track(i) => {
                    let ti = v.top.display[i];
                    let t = &v.top.items[ti];
                    track_row(
                        t,
                        &layout,
                        track_no(t, ti, false),
                        if Some(i) == playing {
                            RowMark::Playing
                        } else {
                            RowMark::None
                        },
                        i == main_index,
                        liked.get(&t.uri).copied(),
                        hover_cell.and_then(|(row, col)| (row == i).then_some(col)),
                    )
                }
                ArtistLine::Tabs => ListItem::new(Line::from(tab_spans.clone())),
                ArtistLine::Card { album, row } => card_line(
                    &v.albums.items[v.albums.display[album]],
                    row,
                    indent as usize,
                    card_text_w,
                    split + album == main_index,
                    hover_album == Some(album),
                    hover_play == Some(album),
                    hover_shuffle == Some(album),
                ),
                ArtistLine::Blank => ListItem::new(Line::default()),
            };
            let track = matches!(*line, ArtistLine::Track(_));
            super::table::hover_row(item, track && Some(at) == hover)
        })
        .collect();
    frame.render_stateful_widget(List::new(items), body, list_state);

    // The sleeves go on last: they are painted cells, not list rows, so they
    // are drawn over the block the rows left blank for them, clipped to the
    // body so a card scrolling off the top slides under it.
    if art_w > 0 {
        for (album, a) in v.albums.rows().enumerate() {
            let first = cards_from + album * CARD_H;
            let top = body.y as i64 + first as i64 - offset as i64;
            if top + CARD_ART_H as i64 <= body.y as i64 || top >= body.bottom() as i64 {
                continue;
            }
            let Ok(y) = u16::try_from(top.max(0)) else {
                continue;
            };
            // A card clipped at the top would need its sleeve to start above
            // the pane, which a `Rect` cannot express; it goes without one
            // rather than drawing a squashed picture.
            if top < 0 {
                continue;
            }
            let cover = a.cover_url.as_deref().and_then(|u| page_art.get(u));
            super::table::draw_art_clipped(
                frame,
                Rect {
                    x: body.x,
                    y,
                    width: art_w,
                    height: CARD_ART_H,
                },
                body,
                cover.as_deref(),
                &a.id,
            );
        }
    }

    super::table::draw_scrollbar(frame, scroll_col(body), plan.len(), offset);
}

/// Cells an album card's sleeve occupies. See [`super::table::art_w`].
const CARD_ART_W: u16 = super::table::art_w(CARD_ART_H);

/// The card row that carries its ▶ play, counting from the top of the sleeve.
///
/// Name, metadata, then the control, with the sleeve running a row past them:
/// the three lines that say something are kept together rather than spread to
/// fill the art's height.
const CARD_PLAY_ROW: u16 = 2;

/// One line of an album card: the name, its metadata, a blank, then ▶ play,
/// each indented past the sleeve.
#[allow(clippy::too_many_arguments)]
fn card_line(
    a: &crate::app::state::AlbumItem,
    row: u16,
    indent: usize,
    text_w: usize,
    selected: bool,
    hovered: bool,
    play_hover: bool,
    shuffle_hover: bool,
) -> ListItem<'static> {
    let pad = Span::raw(" ".repeat(indent));
    let mut spans = vec![pad];
    match row {
        // The name is the link, so it lights under the pointer — the same
        // affordance the Album cell of a track table gives.
        0 => spans.extend(cell_spans(&a.name, text_w, theme::text(), hovered)),
        1 => spans.push(Span::styled(fit(&album_meta(a), text_w), theme::dim())),
        CARD_PLAY_ROW => {
            let style = theme::accent();
            let style = if play_hover {
                super::table::hover_style(style)
            } else {
                style
            };
            // The control keeps its own colour when the card is selected: it
            // is a separate target, and brightening it would read as the
            // thing selected.
            spans.push(Span::styled(PLAY_PILL, style));
            if text_w >= card_pills_min_w() {
                spans.push(Span::raw(" "));
                let style = if shuffle_hover {
                    super::table::hover_style(theme::accent())
                } else {
                    theme::dim()
                };
                spans.push(Span::styled(SHUFFLE_PILL, style));
            }
            return ListItem::new(Line::from(spans));
        }
        _ => {}
    }
    let mut line = Line::from(spans);
    // The name carries the selection on its own. Brightening the metadata too
    // would put two bold runs on one card and leave nothing for the name to
    // be the loudest thing in.
    if selected && row == 0 {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// "2006 · 10 tracks" — every part Spotify actually reported.
///
/// The kind of record is not among them. The cards are grouped by it now, and
/// a card reading "Single" under a tab reading "Singles" says it twice.
fn album_meta(a: &crate::app::state::AlbumItem) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !a.release_year.is_empty() {
        parts.push(a.release_year.clone());
    }
    if a.track_count > 0 {
        parts.push(format!(
            "{} {}",
            a.track_count,
            if a.track_count == 1 {
                "track"
            } else {
                "tracks"
            }
        ));
    }
    parts.join(" · ")
}

/// "2 hr 15 min" / "45 min" for a summed duration.
fn format_total_duration(ms: u64) -> String {
    let mins = ms / 60_000;
    if mins >= 60 {
        format!("{} hr {} min", mins / 60, mins % 60)
    } else {
        format!("{mins} min")
    }
}

/// Rows a sleeve spans in a header band, and the narrowest text column worth
/// keeping it for. Twenty cells wide, by [`super::table::art_w`].
///
/// Larger than every other block in the app — the bottom bar's thumbnail, the
/// artist page's album cards — because these are the two views that are
/// *about* a record or a performer rather than reporting which one is playing.
/// The artist band draws its photo at the same size on purpose: the two
/// mastheads are one shape, and a page whose portrait was half its neighbour's
/// sleeve would read as a different design rather than as the same one.
///
/// The band keeps its sleeve at 16 rows and 63 cells, and sheds it below
/// either — see `stacked`.
const ART_H: u16 = 10;
const ART_GAP: u16 = 3;
const MIN_META_W: u16 = 40;
/// Rows the band spends beside a sleeve: the sleeve itself, then a blank
/// under it.
const ART_BAND_H: u16 = ART_H + 1;
/// Rows the same band spends without one: name, subtitle, totals, a blank
/// between the metadata and the control, the control, and a blank under it.
///
/// The layout is the sleeve's, minus the sleeve. What a playlist has varies —
/// some have a cover, some a blurb, most neither — and a page that rearranged
/// itself around each combination would read as several different pages rather
/// than as one page with more or less to say.
const STACK_BAND_H: u16 = 6;
/// Rows the compact band spends: name, subtitle and totals sharing one row,
/// the control under it, then a blank. The degradation for a pane too short
/// to seat the real thing.
const TEXT_BAND_H: u16 = 3;
/// Rows the track table must still get for a band to be worth its own height:
/// its column header, a spacer, and enough rows to be a list. A seven-row band
/// over a three-row table is a worse screen than no sleeve at all — the same
/// judgement `player::Rows::MIN_ART_QUEUE` makes about its queue.
const MIN_TABLE_H: u16 = 6;

/// Summary band above a track table: the record's own sleeve when it has one,
/// then name + subtitle + blurb + totals, a clickable ▶ play for the whole
/// context, and whatever else the page can be done to. Returns the area left
/// for the table.
///
/// Skipped entirely on short panes, and the sleeve is shed before the text is
/// — the same order the player sheds its own cover in.
#[allow(clippy::too_many_arguments)]
fn header_band(
    frame: &mut Frame,
    inner: Rect,
    list: &crate::app::state::TrackList,
    cover: Option<&crate::cover::Cover>,
    loading: bool,
    controls: HeaderControls,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) -> Rect {
    if inner.height < 8 {
        return inner;
    }
    let gray = theme::text();
    let dim = theme::dim();
    let accent = theme::accent();

    // The sleeve, when the page has one and there are rows and cells to spare.
    // Only a record cover reaches here: an album's own, or a playlist's when
    // Spotify made it something better than a mosaic of four others.
    let sleeve_w = super::table::art_w(ART_H);
    let art = (list.header.cover_url.is_some()
        && inner.height >= ART_BAND_H + MIN_TABLE_H
        && inner.width >= sleeve_w + ART_GAP + MIN_META_W)
        .then(|| {
            let art = Rect {
                width: sleeve_w,
                height: ART_H,
                ..inner
            };
            super::table::draw_art(frame, art, cover, &list.header.name);
            art
        });
    let text = match art {
        Some(art) => Rect {
            x: art.right() + ART_GAP,
            width: inner.width - sleeve_w - ART_GAP,
            ..inner
        },
        None => inner,
    };

    // The stacked layout is what a page wears whether or not it has a sleeve
    // — losing the art must not also lose the shape. Only a pane too short to
    // seat it falls back to the one-row band, and that is a degradation rather
    // than a second design. The blurb is a row of the same block, so it is
    // counted before deciding there is room.
    let blurb = !list.header.description.is_empty();
    let stack_h = STACK_BAND_H + u16::from(blurb);
    let stacked = art.is_some() || inner.height >= stack_h + MIN_TABLE_H;

    // Row 0: name + subtitle on the left, totals right-aligned.
    let info_area = Rect { height: 1, ..text };
    let total_ms: u64 = list.items.iter().map(|t| t.duration_ms).sum();
    let count = list.items.len();
    let dur = format_total_duration(total_ms);
    let totals = Span::styled(
        match (loading, list.total) {
            (true, Some(total)) => format!("{count} of {total} tracks · {dur}"),
            (true, None) => format!("{count} tracks · {dur}+"),
            (false, _) => format!("{count} tracks · {dur}"),
        },
        dim,
    );
    // Stacked, the totals get a row of their own: a name squeezed against a
    // right-aligned count reads as two things fighting rather than as a
    // heading. The compact band has no row to spare and puts up with it.
    let totals_w = if stacked {
        0
    } else {
        (totals.width() as u16).min(info_area.width)
    };
    let name_area = Rect {
        width: info_area.width.saturating_sub(totals_w + 1),
        ..info_area
    };
    let mut left = vec![Span::styled(
        list.header.name.clone(),
        theme::bright().add_modifier(Modifier::BOLD),
    )];
    if !list.header.subtitle.is_empty() && !stacked {
        left.push(Span::styled(format!("  {}", list.header.subtitle), gray));
    }
    frame.render_widget(Paragraph::new(Line::from(left)), name_area);

    // Lines are laid down in order and skip what is not there, rather than
    // taking fixed rows — most playlists have no blurb, and a totals line
    // floating a row below an absent one reads as damage. The same shape
    // `artist_band` uses for its genres.
    let row = |n: u16| Rect {
        y: text.y + n,
        height: 1,
        ..text
    };
    let mut n = 1;
    if stacked && !list.header.subtitle.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(subtitle_spans(list, row(n), gray, mouse, hit))),
            row(n),
        );
        n += 1;
    }
    // The blurb goes with the sleeve rather than with the name: both are what
    // a page sheds first, and the compact band has no row to spend on prose.
    if blurb && stacked {
        let description = super::table::fit(&list.header.description, text.width as usize);
        frame.render_widget(Paragraph::new(Line::styled(description, dim)), row(n));
        n += 1;
    }
    if stacked {
        frame.render_widget(Paragraph::new(Line::from(totals)), row(n));
        n += 1;
    } else {
        frame.render_widget(
            Paragraph::new(Line::from(totals)),
            Rect {
                x: info_area.right().saturating_sub(totals_w),
                width: totals_w,
                ..info_area
            },
        );
    }

    // The ▶ play pill and what else this page can be done to. Beside a sleeve
    // it sits low in the block, so the metadata above it and the controls
    // below read as two groups rather than one list — and it stays inside the
    // sleeve's own rows however many lines the metadata spent.
    let play_area = row(if stacked { (n + 1).min(ART_H - 1) } else { n });
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut x = play_area.x;
    hit.header_play_btn = super::table::segment(
        &mut spans,
        &mut x,
        play_area,
        mouse,
        vec![Span::styled(PLAY_PILL, accent)],
    );
    hit.header_shuffle_btn = shuffle_segment(&mut spans, &mut x, play_area, mouse);
    hit.header_share_btn = match controls.share {
        true => pill_segment(&mut spans, &mut x, play_area, mouse, SHARE_PILL),
        false => Rect::default(),
    };
    hit.header_save_btn = match controls.save {
        Some(saved) => save_segment(&mut spans, &mut x, play_area, mouse, saved),
        None => Rect::default(),
    };
    hit.header_edit_btn = match controls.edit {
        true => pill_segment(&mut spans, &mut x, play_area, mouse, EDIT_PILL),
        false => Rect::default(),
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), play_area);

    // A blank under the control row, so the column header below is not flush
    // against it — and never less than the sleeve, which the table must clear
    // however few lines the metadata beside it spent.
    let floor = match art {
        Some(_) => ART_BAND_H,
        None => 0,
    };
    let used = (play_area.y - text.y + 2).max(floor);
    Rect {
        y: inner.y + used,
        height: inner.height.saturating_sub(used),
        ..inner
    }
}

/// A header band's subtitle row: the credited artists as links, then whatever
/// else the subtitle says about the record.
///
/// A page with no credits — a playlist, Liked Songs — prints its subtitle
/// whole and records nothing, so a click there reaches whatever is underneath
/// rather than a target nothing on screen offered.
///
/// The year comes off the end of the subtitle the client built (see
/// `Client::load_album_view`), so the two spellings of the same row cannot
/// drift apart.
fn subtitle_spans(
    list: &crate::app::state::TrackList,
    row: Rect,
    style: Style,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) -> Vec<Span<'static>> {
    let subtitle = super::table::fit(&list.header.subtitle, row.width as usize);
    if list.header.credits.is_empty() {
        return vec![Span::styled(subtitle.trim_end().to_string(), style)];
    }
    let (cell, runs) = super::table::credit_line(&list.header.credits, row.width as usize);
    let cell = cell.trim_end();
    let names = Rect {
        width: super::table::width(cell) as u16,
        height: 1,
        ..row
    }
    .intersection(row);
    let hovered = super::table::hovered_credit(names, &runs, mouse);
    let mut spans = super::table::credit_spans(cell, &runs, style, hovered);
    super::table::credit_links(names, &runs, &mut hit.header_artist_links);
    // The rest of the subtitle, which is the year and the separator before it.
    let rest = subtitle
        .trim_end()
        .strip_prefix(cell)
        .unwrap_or_default()
        .to_string();
    if !rest.is_empty() {
        spans.push(Span::styled(rest, theme::dim()));
    }
    spans
}

/// Render a row of clickable tab segments and record their hit rects.
#[allow(clippy::too_many_arguments)]
fn tab_segments<T: Copy + PartialEq>(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    row: Rect,
    mouse: Option<Position>,
    tabs: &[T],
    active: T,
    title: impl Fn(T) -> &'static str,
    hits: &mut Vec<(Rect, T)>,
) {
    for (i, &tab) in tabs.iter().enumerate() {
        if i > 0 {
            // Two spaces, not a `│`. With the active tab in accent-bold and
            // the rest dim, the strip already reads as a set of choices, and
            // the rules were three more marks saying so again.
            spans.push(Span::raw("  "));
            *x += 2;
        }
        let style = if tab == active {
            theme::accent().add_modifier(Modifier::BOLD)
        } else {
            // Dim, not TEXT: an inactive tab is a thing you could pick, not
            // body copy, and it should sit behind the rows it labels.
            theme::dim()
        };
        let rect =
            super::table::segment(spans, x, row, mouse, vec![Span::styled(title(tab), style)]);
        hits.push((rect, tab));
    }
}

/// Longest a single crumb is spelled before eliding.
pub(super) const HEAD_W: usize = 24;
/// Longest an *ancestor* crumb is spelled. Shorter than the head: the page you
/// are on is what the row is about, while a step behind it only has to be
/// recognizable enough to aim at. Three steps then fit an 80-column row, where
/// at the head's width only two did.
pub(super) const ANCESTOR_W: usize = 14;
/// What separates two crumbs. Wide enough that the path reads as steps rather
/// than as one phrase, and pointing the way the trail is read.
pub(super) const CRUMB_SEP: &str = "  ›  ";
/// Crumbs drawn before the trail starts shedding: the root, then up to three
/// more ending at the page you are on.
pub(super) const MAX_CRUMBS: usize = 4;
/// Stands in for what was shed out of the middle. Inert on purpose: the crumbs
/// on either side of it are both controls, and an ellipsis that led somewhere
/// too would be a third answer to a question already answered twice.
pub(super) const CRUMB_ELLIPSIS: &str = "…";

/// One crumb's text: capped, and uppercased to sit in the section label's row.
/// `fit` pads to width, which a crumb must not do.
pub(super) fn crumb_text(label: &str, width: usize) -> String {
    super::table::fit(&label.to_uppercase(), width)
        .trim_end()
        .to_string()
}

/// Lay a trail out into spans and return the hit rect of each crumb that
/// leads somewhere, in trail order.
///
/// Shared with the player view, which draws the same trail right-aligned over
/// the page waiting underneath it.
/// `shown` is what survived [`fit_trail`], and `ellipsis` puts the `…` between
/// its root and the rest — the trail sheds out of its *middle*, so both ends
/// of the path stay on the row.
pub(super) fn crumb_spans(
    shown: &[Crumb],
    loading: bool,
    ellipsis: bool,
) -> Vec<(Vec<Span<'static>>, Option<CrumbTarget>)> {
    let mut out: Vec<(Vec<Span<'static>>, Option<CrumbTarget>)> = Vec::new();
    let sep = || Span::styled(CRUMB_SEP, theme::dim());
    let Some((head, rest)) = shown.split_last() else {
        return out;
    };
    for (i, crumb) in rest.iter().enumerate() {
        // Ancestors are dim: they are where you came from, and hovering one
        // lights it (see `table::hover_style`). Exactly one accent run stays
        // on the row, as when the row held a single section label.
        out.push((
            vec![
                Span::styled(crumb_text(&crumb.label, ANCESTOR_W), theme::dim()),
                sep(),
            ],
            Some(crumb.target.clone()),
        ));
        if ellipsis && i == 0 {
            out.push((
                vec![Span::styled(CRUMB_ELLIPSIS, theme::dim()), sep()],
                None,
            ));
        }
    }
    let mut text = crumb_text(&head.label, HEAD_W);
    if loading {
        text.push_str(" (LOADING…)");
    }
    out.push((
        vec![Span::styled(
            text,
            theme::accent().add_modifier(Modifier::BOLD),
        )],
        Some(head.target.clone()),
    ));
    out
}

/// Cells a laid-out trail occupies.
pub(super) fn trail_width(shown: &[Crumb], loading: bool, ellipsis: bool) -> u16 {
    crumb_spans(shown, loading, ellipsis)
        .iter()
        .flat_map(|(spans, _)| spans.iter())
        .map(|s| s.width() as u16)
        .sum()
}

/// The crumbs of `trail` that actually fit `avail`, and whether an `…` stands
/// between the root and the rest.
///
/// **Both ends survive.** The head is the page you are on and the root is
/// where the path starts, so what gives is the middle. Shedding from the front
/// instead — which is what this did first — took `HOME` off the row exactly
/// when the path was long enough to want it, leaving only the wordmark to get
/// back and no sense of how far in you were.
///
/// The head is never shed: a row too narrow for even one crumb draws the
/// page's own name clipped, which is what the section label did before there
/// was a trail at all.
pub(super) fn fit_trail(trail: &[Crumb], loading: bool, avail: u16) -> (Vec<Crumb>, bool) {
    let last = trail.len().saturating_sub(1);
    // Room for the root plus the steps nearest the page you are on.
    let mut start = trail.len().saturating_sub(MAX_CRUMBS - 1);
    loop {
        // At `start <= 1` nothing is missing, so there is nothing to stand in
        // for: the trail draws whole.
        let (shown, ellipsis) = if start <= 1 {
            (trail.to_vec(), false)
        } else {
            let mut shown = vec![trail[0].clone()];
            shown.extend_from_slice(&trail[start..]);
            (shown, true)
        };
        if trail_width(&shown, loading, ellipsis) <= avail {
            return (shown, ellipsis);
        }
        if start >= last {
            // Nothing left to shed but the root. A pane this narrow gets the
            // page's own name and no path — a clipped root names a destination
            // the crumb does not lead to.
            return (trail[last..].to_vec(), false);
        }
        start += 1;
    }
}

/// Lay a trail out across `row`, starting at its left edge, and record every
/// crumb that leads somewhere in `hit.crumbs`. Returns the head crumb's rect,
/// which is empty unless `head_live`.
///
/// `head_live` is the one thing the two views disagree about. On the browse
/// screen the head is the page you are already on, so it is a title and not a
/// control: it must not light under the mouse, or it would offer a click that
/// does nothing. In the player the same trail is drawn over the page waiting
/// *underneath*, so its head is the way out.
///
/// Shared so the two cannot drift: the same path, the same widths, the same
/// styles, differing only in where the row starts and how the head behaves.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_trail(
    frame: &mut Frame,
    row: Rect,
    trail: &[Crumb],
    loading: bool,
    ellipsis: bool,
    head_live: bool,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) -> Rect {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut x = row.x;
    let mut head = Rect::default();
    for (parts, target) in crumb_spans(trail, loading, ellipsis) {
        // The separator trails its crumb rather than sitting in it, so only
        // the name is a target: the gap between two crumbs belongs to neither,
        // and a hover pill stretched across it would say otherwise.
        let (name, tail) = parts.split_at(1);
        match target {
            // An ellipsis is not a control — see `CRUMB_ELLIPSIS`.
            None => {
                let w: usize = parts.iter().map(|s| s.width()).sum();
                spans.extend(parts);
                x = x.saturating_add(w as u16);
                continue;
            }
            Some(CrumbTarget::Current) => {
                let hover = if head_live { mouse } else { None };
                head = super::table::segment(&mut spans, &mut x, row, hover, name.to_vec());
                if !head_live {
                    head = Rect::default();
                }
            }
            Some(target) => {
                let rect = super::table::segment(&mut spans, &mut x, row, mouse, name.to_vec());
                if !rect.is_empty() {
                    hit.crumbs.push((rect, target));
                }
            }
        }
        let w: usize = tail.iter().map(|s| s.width()).sum();
        spans.extend(tail.to_vec());
        x = x.saturating_add(w as u16);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), row);
    head
}

/// What a page contributes to the header [`super::top_row`] draws above it.
///
/// The path and the count sit on a row the pane does not own — the two views
/// share that row, and the player draws it over whatever page is waiting
/// underneath. So a page hands these up rather than drawing them: see
/// [`page_header`].
#[derive(Default)]
pub(super) struct PageHeader {
    /// Whether the page's own contents are still in flight. Spelled out after
    /// the head of the path, which is what names the page.
    pub loading: bool,
    /// A total to pin opposite the path, where a page has one.
    pub count: Option<Span<'static>>,
}

/// Read a page's [`PageHeader`] off the state.
///
/// `loading` is per page rather than global: `state.loading` is about the
/// pages that fetch through the client, while the artist and radio views
/// carry their own flag, and the two list pages only say so while they have
/// nothing to show yet.
pub(super) fn page_header(st: &AppState) -> PageHeader {
    let plural = |n: usize, one: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    };
    match &st.main {
        MainView::Home => PageHeader::default(),
        MainView::Playlists => PageHeader {
            loading: st.loading && st.playlists.is_empty(),
            count: (!st.playlists.is_empty())
                .then(|| Span::styled(plural(st.playlists.len(), "playlist"), theme::dim())),
        },
        // A track page has a flag of its own for the rows still coming in,
        // over and above the client-wide one — see `draw_tracks`.
        MainView::Tracks(list) => PageHeader {
            loading: st.loading || list.loading,
            count: None,
        },
        MainView::Search(_) => PageHeader {
            loading: st.loading,
            count: None,
        },
        MainView::Artist(v) => PageHeader {
            loading: v.loading,
            count: None,
        },
        MainView::Radio(v) => PageHeader {
            // The pane is the one that says so here: a directory page has
            // nothing above its tab strip *but* the trail, so the word on the
            // row and the spinner under it were the same news said twice.
            loading: false,
            count: (!v.rows.is_empty()).then(|| {
                let facets = matches!(
                    v.rows.first(),
                    Some(crate::app::state::RadioRow::Facet { .. })
                );
                let word = if facets { "entry" } else { "station" };
                let label = match (facets, v.rows.len()) {
                    (true, 1) => "1 entry".to_string(),
                    (true, n) => format!("{n} entries"),
                    (false, n) => plural(n, word),
                };
                Span::styled(label, theme::dim())
            }),
        },
    }
}

/// The pane's content area: everything but the column the scrollbar rides in.
///
/// The pane draws no label and no path of its own: both live in the header two
/// rows above, drawn once for both views — see [`super::top_row`].
fn body_area(area: Rect) -> Rect {
    Rect {
        width: area.width.saturating_sub(GUTTER),
        ..area
    }
}

/// Centered dim hint for a view with nothing to list.
fn empty_message(frame: &mut Frame, inner: Rect, text: &str) {
    centered_line(
        frame,
        inner,
        0,
        Line::styled(text.to_string(), theme::dim()),
    );
}

/// Centered spinner and label while a view's own contents are in flight.
///
/// The page saying so itself, rather than the nav row's `● LOADING`, which is
/// about what is *playing*: a pane that draws nothing at all while it waits
/// reads exactly like one that came back with nothing.
fn loading_message(frame: &mut Frame, inner: Rect, text: &str) {
    let label = format!("{} {text}", super::table::spinner());
    centered_line(frame, inner, 0, Line::styled(label, theme::dim()));
}

/// The control a failed page carries.
const RETRY_PILL: &str = "↻ try again";

/// Centered failure notice with [`RETRY_PILL`] under it.
///
/// The reason is kept on the page rather than left to the toast, which
/// expires in seconds and draws on a bottom bar that is not there at all
/// while nothing is playing — so the commonest failure said nothing you
/// could still read by the time you looked.
fn error_message(
    frame: &mut Frame,
    inner: Rect,
    err: &LoadError,
    retries: u32,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) {
    // `fit` pads to exactly the width; trim it back so the centring has the
    // message to work on rather than the padding.
    let message = super::table::fit(&err.message, inner.width as usize)
        .trim_end()
        .to_string();
    centered_line(frame, inner, 0, Line::styled(message, theme::warn()));

    // What the press did, when the answer was the same refusal. A rate limit
    // refuses in less time than a frame takes, so the spinner between the two
    // failures is never on screen; this line is what moves.
    if retries > 0 {
        let tally = match retries {
            1 => "asked again once".to_string(),
            n => format!("asked again {n} times"),
        };
        centered_line(frame, inner, 1, Line::styled(tally, theme::dim()));
    }

    // The control is drawn only where it fits whole. One that ran off the
    // pane would still record a rect, and a click on the row it would have
    // taken would retry a page that never offered to be retried.
    if inner.height < 3 {
        return;
    }
    let row = Rect {
        y: inner.y + inner.height / 2 + 3,
        height: 1,
        ..inner
    };
    let width = super::table::width(RETRY_PILL) as u16;
    if row.bottom() > inner.bottom() || width > inner.width {
        return;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let start = row.x + (row.width - width) / 2;
    let mut x = start;
    hit.retry_btn = super::table::segment(
        &mut spans,
        &mut x,
        row,
        mouse,
        vec![Span::styled(RETRY_PILL, theme::accent())],
    );
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x: start,
            width,
            ..row
        },
    );
}

/// One line across the pane, `offset` rows from its middle.
fn centered_line(frame: &mut Frame, inner: Rect, offset: i16, line: Line<'static>) {
    if inner.height == 0 {
        return;
    }
    let row = Rect {
        y: (inner.y + inner.height / 2).saturating_add_signed(offset),
        height: 1,
        ..inner
    };
    if row.bottom() > inner.bottom() {
        return;
    }
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), row);
}

use super::table::{ADD_W, LIKE_W, RowAction, SHARE_W, action_spans};

/// Which clickable cell of a track row the mouse is over.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverCol {
    Like,
    Share,
    Add,
    /// One credited artist, named by a column inside it — the pointer's own,
    /// measured from the left edge of the Artist cell. A record credits
    /// several and each leads somewhere else, so "the Artist column" does not
    /// say which name to light.
    Artist(u16),
    Album,
}

impl HoverCol {
    /// The action-run control this is, if it is one of them. The two text
    /// columns are drawn by this module and have no counterpart in the run.
    fn action(self) -> Option<RowAction> {
        match self {
            HoverCol::Like => Some(RowAction::Like),
            HoverCol::Share => Some(RowAction::Share),
            HoverCol::Add => Some(RowAction::Add),
            HoverCol::Artist(_) | HoverCol::Album => None,
        }
    }
}

use super::table::fit;

/// Cells `text` actually prints in a `width`-wide cell.
///
/// What [`cell_spans`] lights, so a link's target covers the run the pointer
/// lit and nothing else. A name too long for its column is truncated to fill
/// it, so that one's target is the whole column.
fn printed_w(text: &str, width: usize) -> u16 {
    super::table::width(fit(text, width).trim_end()) as u16
}

/// A cell of `width` columns holding `text`, as spans, lit when `hovered`.
///
/// The light goes on the text alone and the padding stays bare: a cell padded
/// out to its column width is mostly empty, and a pill drawn across all of it
/// would read as a selected row rather than as the run under the pointer.
fn cell_spans(text: &str, width: usize, style: Style, hovered: bool) -> Vec<Span<'static>> {
    let cell = fit(text, width);
    if !hovered {
        return vec![Span::styled(cell, style)];
    }
    // `fit` pads with ASCII spaces, so what it trims off in bytes it also
    // trims off in columns.
    let lit = cell.trim_end();
    let pad = cell.len() - lit.len();
    vec![
        Span::styled(lit.to_string(), super::table::hover_style(style)),
        Span::raw(" ".repeat(pad)),
    ]
}

/// A credit line in a `width`-column cell, as spans, with the name the pointer
/// is on lit.
///
/// [`cell_spans`] lights a whole cell, which is right where the cell is one
/// link. A credit cell is several, so the light goes on the name `hover` names
/// and the ones either side of it stay unlit — they lead somewhere else.
fn credit_cell(
    credits: &[Credit],
    width: usize,
    style: Style,
    hover: Option<HoverCol>,
) -> Vec<Span<'static>> {
    let (cell, runs) = super::table::credit_line(credits, width);
    let lit = match hover {
        Some(HoverCol::Artist(dx)) => runs
            .iter()
            .position(|run| dx >= run.dx && dx < run.dx.saturating_add(run.width)),
        _ => None,
    };
    super::table::credit_spans(&cell, &runs, style, lit)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowMark {
    None,
    Playing,
}

/// What a track row prints in the `#` column.
///
/// An album numbers by the track's own position on the record; every other
/// list numbers by its position in the order it arrived, `index` into the
/// list's `items`. Either way the number belongs to the *track*, not to the
/// row it happens to be on — sorting by Title and reading 1, 2, 3 down the
/// side would say the sort had renumbered the record.
fn track_no(track: &Track, index: usize, album: bool) -> u32 {
    match album {
        true => track.track_number,
        false => index as u32 + 1,
    }
}

fn track_row(
    t: &Track,
    layout: &Layout,
    no: u32,
    mark: RowMark,
    selected: bool,
    liked: Option<bool>,
    hover: Option<HoverCol>,
) -> ListItem<'static> {
    let dim = theme::dim();
    let accent_bold = theme::accent().add_modifier(Modifier::BOLD);
    // Three weights, the way the player queue does it: the title at TEXT,
    // everything supporting it at DIM, and the playing row in accent.
    // `Style::default()` here would leak the raw terminal foreground, the one
    // unthemed colour on the page.
    let name_style = if mark == RowMark::Playing {
        accent_bold
    } else {
        theme::text()
    };
    // Where the star lands, so the selection restyle below can put its accent
    // back.
    let mut star_at = None;
    let spans = row_spans(layout, |cell, spans| match cell.key {
        ColKey::Mark => spans.push(match mark {
            RowMark::None => Span::raw(" ".repeat(cell.width)),
            RowMark::Playing => Span::styled("▶ ", accent_bold),
        }),
        ColKey::No => {
            let no = match no > 0 {
                true => no.to_string(),
                false => String::new(),
            };
            spans.push(Span::styled(right(&no, cell.width), dim));
        }
        ColKey::Title => spans.push(Span::styled(fit(&t.name, cell.width), name_style)),
        // Artist and album cells are clickable; hovering lights them. In the
        // artist cell the light goes on one name of several — see
        // [`credit_cell`].
        ColKey::Artist => spans.extend(credit_cell(&t.credits, cell.width, dim, hover)),
        ColKey::Album => spans.extend(cell_spans(
            &t.album,
            cell.width,
            dim,
            hover == Some(HoverCol::Album),
        )),
        ColKey::Year => spans.push(Span::styled(fit(&t.release_year, cell.width), dim)),
        ColKey::Actions => {
            star_at = Some(spans.len());
            spans.extend(action_spans(liked, hover.and_then(HoverCol::action)));
        }
        ColKey::Time => spans.push(Span::styled(
            right(&format_duration(t.duration_ms), cell.width),
            dim,
        )),
        _ => spans.push(Span::raw(" ".repeat(cell.width))),
    });
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
        // Selection restyles every span, which would leave the selected row's
        // star reading the same as an unsaved one — the one row whose state
        // you cannot check by moving off it. Put the accent back, the way the
        // queue puts its playing marker back.
        if let Some(i) = star_at.filter(|_| liked == Some(true))
            && let Some(span) = line.spans.get_mut(i)
        {
            span.style = theme::accent().add_modifier(Modifier::BOLD);
        }
    }
    ListItem::new(line)
}

fn album_row(
    a: &crate::app::state::AlbumItem,
    layout: &Layout,
    playing: bool,
    selected: bool,
    hovered: bool,
    hover: Option<HoverCol>,
) -> ListItem<'static> {
    let dim = theme::dim();
    let spans = row_spans(layout, |cell, spans| match cell.key {
        ColKey::Mark => spans.push(match playing {
            true => Span::styled("♫ ", theme::accent()),
            false => Span::raw(" ".repeat(cell.width)),
        }),
        // The name is the link, so it lights under the pointer — the same
        // affordance `track_row` gives the album cell of a track table.
        ColKey::Album => spans.extend(cell_spans(&a.name, cell.width, theme::text(), hovered)),
        ColKey::Artist => spans.extend(credit_cell(&a.credits, cell.width, dim, hover)),
        ColKey::Year => spans.push(Span::styled(fit(&a.release_year, cell.width), dim)),
        ColKey::Type => spans.push(Span::styled(fit(&a.album_type, cell.width), dim)),
        ColKey::Tracks => {
            // 0 is what the source reports when it does not know, and a
            // record with no tracks on it is not a thing.
            let count = match a.track_count {
                0 => String::new(),
                n => n.to_string(),
            };
            spans.push(Span::styled(right(&count, cell.width), dim));
        }
        _ => spans.push(Span::raw(" ".repeat(cell.width))),
    });
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// One artist row: the Artists tab of a search, which has one column and the
/// marker every other table carries.
fn artist_row(
    a: &crate::app::state::ArtistItem,
    layout: &Layout,
    selected: bool,
) -> ListItem<'static> {
    let spans = row_spans(layout, |cell, spans| match cell.key {
        ColKey::Artist => spans.push(Span::styled(fit(&a.name, cell.width), theme::text())),
        _ => spans.push(Span::raw(" ".repeat(cell.width))),
    });
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// One playlist row.
///
/// `me_id` blanks the Owner cell for playlists you own, so the column reads as
/// "these are the ones you follow". Twenty rows repeating your own name is not
/// information. `None` — the search view, or before `/v1/me` lands — prints
/// every owner, which is what that view wants anyway: a search result is
/// somebody else's playlist far more often than not.
fn playlist_row(
    p: &crate::app::state::Playlist,
    layout: &Layout,
    me_id: Option<&str>,
    playing: bool,
    selected: bool,
) -> ListItem<'static> {
    let dim = theme::dim();
    let owner = match me_id {
        Some(me) if me == p.owner_id => "",
        _ => p.owner.as_str(),
    };
    let spans = row_spans(layout, |cell, spans| match cell.key {
        ColKey::Mark => spans.push(Span::styled(
            match playing {
                true => "♫ ".to_string(),
                false => " ".repeat(cell.width),
            },
            theme::accent(),
        )),
        ColKey::Title => spans.push(Span::styled(fit(&p.name, cell.width), theme::text())),
        ColKey::Owner => spans.push(Span::styled(fit(owner, cell.width), dim)),
        ColKey::Tracks => spans.push(Span::styled(
            right(&p.track_count.to_string(), cell.width),
            dim,
        )),
        _ => spans.push(Span::raw(" ".repeat(cell.width))),
    });
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// Render the album table (header + scrollable rows) inside `inner`.
#[allow(clippy::too_many_arguments)]
fn render_album_table(
    frame: &mut Frame,
    inner: Rect,
    albums: &crate::app::state::SortedList<crate::app::state::AlbumItem>,
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    mouse: Option<Position>,
) {
    let layout = Layout::resolve(&crate::ui::columns::albums(), inner.width as usize, 0);
    let rows_area = match albums.is_empty() {
        true => inner,
        false => layout.draw_header(frame, inner, Some(albums.sort), mouse, hit),
    };
    hit.main_list = rows_area;
    let count = albums.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);

    // The album name is a link here the same way the Album *column* is one in
    // a track table: single click opens the album. The target is the name as
    // printed on each row, clipped to the rows actually filled.
    let filled_rows = (count.saturating_sub(list_state.offset()) as u16).min(rows_area.height);
    let name = layout.cell(ColKey::Album);
    let name_w = layout.width_of(ColKey::Album);
    hit.main_album_col = TextCol {
        rect: Rect {
            x: rows_area.x + name.map_or(0, |c| c.x) as u16,
            y: rows_area.y,
            width: name_w as u16,
            height: filled_rows,
        }
        .intersection(rows_area),
        widths: albums
            .rows()
            .skip(list_state.offset())
            .take(filled_rows as usize)
            .map(|a| printed_w(&a.name, name_w))
            .collect(),
    };
    // Each credited artist is a link too, on the same terms the Artist column
    // of a track table gives them.
    let artist = layout.cell(ColKey::Artist);
    let artist_w = layout.width_of(ColKey::Artist);
    let artist_x = artist.map(|c| rows_area.x.saturating_add(c.x as u16));
    for (row, a) in albums
        .rows()
        .skip(list_state.offset())
        .take(filled_rows as usize)
        .enumerate()
    {
        let cell = Rect {
            x: artist_x.unwrap_or(rows_area.x),
            y: rows_area.y.saturating_add(row as u16),
            width: artist_w as u16,
            height: 1,
        }
        .intersection(rows_area);
        let (_, runs) = super::table::credit_line(&a.credits, artist_w);
        super::table::credit_links(cell, &runs, &mut hit.album_artist_links);
    }
    let hover_name = mouse
        .filter(|m| hit.main_album_col.hit(*m))
        .map(|m| list_state.offset() + (m.y - rows_area.y) as usize);
    let hover_artist = mouse
        .filter(|m| hit.album_artist_links.iter().any(|(r, _)| r.contains(*m)))
        .and_then(|m| {
            let row = list_state.offset() + (m.y - rows_area.y) as usize;
            let dx = m.x.saturating_sub(artist_x?);
            Some((row, HoverCol::Artist(dx)))
        });
    let hover = super::table::hovered_row(rows_area, list_state.offset(), count, mouse);

    let items: Vec<ListItem> = albums
        .rows()
        .enumerate()
        .map(|(i, a)| {
            let playing =
                marks.context.as_deref() == Some(crate::app::state::album_key(&a.id).as_str());
            super::table::hover_row(
                album_row(
                    a,
                    &layout,
                    playing,
                    i == main_index,
                    hover_name == Some(i),
                    hover_artist.and_then(|(row, col)| (row == i).then_some(col)),
                ),
                Some(i) == hover,
            )
        })
        .collect();
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// Render the track table (header row + scrollable rows) inside `inner`,
/// the area within a pane's borders.
#[allow(clippy::too_many_arguments)]
fn render_track_table(
    frame: &mut Frame,
    inner: Rect,
    tracks: &crate::app::state::SortedList<Track>,
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    liked: &std::collections::HashMap<String, bool>,
    album: bool,
    mouse: Option<Position>,
) {
    let layout = Layout::resolve(
        &super::columns::tracks(album),
        inner.width as usize,
        tracks.items.len(),
    );
    let rows_area = match tracks.is_empty() {
        true => inner,
        false => layout.draw_header(frame, inner, Some(tracks.sort), mouse, hit),
    };
    hit.main_list = rows_area;
    super::clamp_offset(list_state, tracks.len(), rows_area.height as usize);

    // Clickable cells: the `★ ⧉ +` run, each credited artist, and the album,
    // clipped to the rows actually on screen. The rows come along whole, so
    // each target is the name that row prints.
    let visible: Vec<&Track> = tracks
        .display
        .iter()
        .skip(list_state.offset())
        .take(rows_area.height as usize)
        .map(|&ti| &tracks.items[ti])
        .collect();
    track_cells(&layout, rows_area, rows_area.y, &visible, hit);
    let artist_x = layout
        .cell(ColKey::Artist)
        .map(|c| rows_area.x.saturating_add(c.x as u16));
    let hover_cell: Option<(usize, HoverCol)> = mouse.and_then(|m| {
        let row = |y: u16| list_state.offset() + (y - rows_area.y) as usize;
        hovered_cell(hit, m, artist_x).map(|col| (row(m.y), col))
    });
    let hover = super::table::hovered_row(rows_area, list_state.offset(), tracks.len(), mouse);
    // Row positions are in display order; the playing marker resolves by URI.
    let playing = marks.uri.as_deref().and_then(|uri| tracks.position_of(uri));
    let items: Vec<ListItem> = tracks
        .display
        .iter()
        .enumerate()
        .map(|(i, &ti)| {
            let t = &tracks.items[ti];
            let mark = if Some(i) == playing {
                RowMark::Playing
            } else {
                RowMark::None
            };
            super::table::hover_row(
                track_row(
                    t,
                    &layout,
                    track_no(t, ti, album),
                    mark,
                    i == main_index,
                    liked.get(&t.uri).copied(),
                    hover_cell.and_then(|(row, col)| (row == i).then_some(col)),
                ),
                Some(i) == hover,
            )
        })
        .collect();
    let count = items.len();
    let list = List::new(items);
    frame.render_stateful_widget(list, rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// Record the clickable columns of a track table: the `★ ⧉ +` run, every
/// credited artist, and the album, each clipped to the rows on screen.
///
/// `rows` is every row on screen, top row first. It gives the columns their
/// height, the album column the width of each row's printed name, and the
/// artist column one target per credit — so a target is the run the pointer
/// lit rather than the padded cell around it, and a record with three artists
/// leads to three pages.
///
/// One definition for both places a track table is drawn — the browse pane and
/// the artist page's top-tracks block — so the two cannot come to disagree
/// about where a control is.
fn track_cells(layout: &Layout, body: Rect, top: u16, rows: &[&Track], hit: &mut HitAreas) {
    let col = |cell: Option<&Cell>, dx: usize, width: usize| {
        let Some(cell) = cell else {
            return Rect::default();
        };
        Rect {
            x: body.x.saturating_add((cell.x + dx) as u16),
            y: top,
            width: width as u16,
            height: rows.len() as u16,
        }
        .intersection(body)
    };
    let actions = layout.cell(ColKey::Actions);
    hit.main_like_col = col(actions, 0, LIKE_W);
    // Flush against each other: every cell carries its own padding, so the
    // three targets meet.
    hit.main_share_col = col(actions, LIKE_W, SHARE_W);
    hit.main_add_col = col(actions, LIKE_W + SHARE_W, ADD_W);
    let artist = layout.cell(ColKey::Artist);
    let artist_w = artist.map_or(0, |c| c.width);
    for (row, t) in rows.iter().enumerate() {
        let cell = col(artist, 0, artist_w).intersection(Rect {
            y: top.saturating_add(row as u16),
            height: 1,
            ..body
        });
        let (_, runs) = super::table::credit_line(&t.credits, artist_w);
        super::table::credit_links(cell, &runs, &mut hit.main_artist_links);
    }
    let album = layout.cell(ColKey::Album);
    let album_w = album.map_or(0, |c| c.width);
    hit.main_album_col = TextCol {
        rect: col(album, 0, album_w),
        widths: rows.iter().map(|t| printed_w(&t.album, album_w)).collect(),
    };
}

/// Which clickable cell of a track table the pointer is inside.
///
/// `artist_x` is where the Artist cell starts on screen, so a hit on one of
/// its names can be handed back as an offset the row renderer can find the run
/// by — it draws its own runs and never sees the screen.
fn hovered_cell(hit: &HitAreas, m: Position, artist_x: Option<u16>) -> Option<HoverCol> {
    if hit.main_like_col.contains(m) {
        Some(HoverCol::Like)
    } else if hit.main_share_col.contains(m) {
        Some(HoverCol::Share)
    } else if hit.main_add_col.contains(m) {
        Some(HoverCol::Add)
    } else if hit.main_album_col.hit(m) {
        Some(HoverCol::Album)
    } else {
        let x = artist_x.filter(|_| hit.main_artist_links.iter().any(|(r, _)| r.contains(m)))?;
        Some(HoverCol::Artist(m.x.saturating_sub(x)))
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tracks(
    frame: &mut Frame,
    area: Rect,
    list: &crate::app::state::TrackList,
    cover: Option<&crate::cover::Cover>,
    global_loading: bool,
    controls: HeaderControls,
    main_index: usize,
    list_state: &mut ListState,
    retries: u32,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    liked: &std::collections::HashMap<String, bool>,
    mouse: Option<Position>,
) {
    let loading = list.loading || global_loading;
    let inner = body_area(area);
    let body = header_band(frame, inner, list, cover, loading, controls, hit, mouse);
    // A page that failed before its first row said so only through the toast
    // until now, and drew the very line an empty one draws. A page that
    // failed part-way through keeps the rows it got and leaves the news to
    // the toast: there is a list on screen, and it is not blank.
    if list.display.is_empty() {
        hit.main_list = body;
        match &list.error {
            Some(e) => error_message(frame, body, e, retries, hit, mouse),
            None if loading => loading_message(frame, body, "loading…"),
            None => empty_message(frame, body, "this playlist is empty"),
        }
        return;
    }
    render_track_table(
        frame,
        body,
        &list.rows,
        main_index,
        list_state,
        hit,
        marks,
        liked,
        list.is_album(),
        mouse,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_search(
    frame: &mut Frame,
    area: Rect,
    results: &crate::app::state::SearchResults,
    loading: bool,
    search_tab: SearchTab,
    tabs: &[SearchTab],
    main_index: usize,
    list_state: &mut ListState,
    retries: u32,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    mouse: Option<Position>,
    liked: &std::collections::HashMap<String, bool>,
    favorites: &[crate::app::state::Station],
    playing_url: Option<&str>,
) {
    let inner = body_area(area);

    let tab_len = match search_tab {
        SearchTab::Tracks => results.tracks.len(),
        SearchTab::Albums => results.albums.len(),
        SearchTab::Artists => results.artists.len(),
        SearchTab::Playlists => results.playlists.len(),
        SearchTab::Stations => results.stations.len(),
    };

    // Whether the tab you are on is still waiting on its own half of the
    // answer. Two hosts answer this page and the directory is usually the
    // slower, so a bare "0 results" over an empty Stations tab would report a
    // failure that has not happened yet.
    let pending = match search_tab {
        SearchTab::Stations => results.stations_loading,
        _ => loading,
    };

    // Header band: query bold with the result count right-aligned, then the
    // tab strip. On short panes the tabs keep a single row.
    let mut body = inner;
    if inner.height >= 8 {
        let info_area = Rect { height: 1, ..inner };
        let totals = Span::styled(
            if pending && tab_len == 0 {
                "searching…".to_string()
            } else {
                format!("{tab_len} results")
            },
            theme::dim(),
        );
        let totals_w = (totals.width() as u16).min(info_area.width);
        let totals_area = Rect {
            x: info_area.right().saturating_sub(totals_w),
            width: totals_w,
            ..info_area
        };
        let query_area = Rect {
            width: info_area.width.saturating_sub(totals_w + 1),
            ..info_area
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                results.query.clone(),
                theme::bright().add_modifier(Modifier::BOLD),
            ))),
            query_area,
        );
        frame.render_widget(Paragraph::new(Line::from(totals)), totals_area);
        let row = Rect {
            y: inner.y + 1,
            height: 1,
            ..inner
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut x = row.x;
        tab_segments(
            &mut spans,
            &mut x,
            row,
            mouse,
            tabs,
            search_tab,
            SearchTab::title,
            &mut hit.search_tabs,
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
        body = Rect {
            y: inner.y + 3,
            height: inner.height - 3,
            ..inner
        };
    } else if inner.height >= 4 {
        let row = Rect { height: 1, ..inner };
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut x = row.x;
        tab_segments(
            &mut spans,
            &mut x,
            row,
            mouse,
            tabs,
            search_tab,
            SearchTab::title,
            &mut hit.search_tabs,
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
        body = Rect {
            y: inner.y + 2,
            height: inner.height - 2,
            ..inner
        };
    }

    if tab_len == 0 {
        hit.main_list = body;
        // Each half answers for its own tabs, on the same reasoning that gives
        // the directory its own `stations_loading`: a tab is empty because the
        // host *it* asked would not answer, and neither half speaks for the
        // other.
        let error = match search_tab {
            SearchTab::Stations => &results.stations_error,
            _ => &results.error,
        };
        if let Some(e) = error {
            error_message(frame, body, e, retries, hit, mouse);
            return;
        }
        if pending {
            loading_message(frame, body, "searching…");
            return;
        }
        // "no stations results for" would be the generic template's answer
        // here, so this tab spells its own.
        let text = match search_tab {
            SearchTab::Stations => format!("no stations for \"{}\"", results.query),
            _ => format!(
                "no {} results for \"{}\"",
                search_tab.title().to_lowercase(),
                results.query
            ),
        };
        empty_message(frame, body, &text);
        return;
    }

    // The directory's own table, drawn exactly as a radio page draws it: same
    // columns, same ★ on a station you keep, same ♫ on the one that is
    // playing. A station is the same object wherever you found it, and a
    // second layout for it would say it was not.
    if search_tab == SearchTab::Stations {
        let stations: Vec<&crate::app::state::Station> = results.stations.rows().collect();
        render_station_table(
            frame,
            body,
            &stations,
            favorites,
            playing_url,
            None,
            results.stations.sort,
            main_index,
            list_state,
            hit,
            mouse,
        );
        return;
    }

    if search_tab == SearchTab::Tracks {
        render_track_table(
            frame,
            body,
            &results.tracks,
            main_index,
            list_state,
            hit,
            marks,
            liked,
            false,
            mouse,
        );
        return;
    }

    if search_tab == SearchTab::Albums {
        render_album_table(
            frame,
            body,
            &results.albums,
            main_index,
            list_state,
            hit,
            marks,
            mouse,
        );
        return;
    }

    let width = body.width as usize;
    let (columns, sort) = match search_tab {
        SearchTab::Playlists => (super::columns::playlists(), results.playlists.sort),
        _ => (super::columns::artists(), results.artists.sort),
    };
    let layout = Layout::resolve(&columns, width, 0);
    let rows_area = layout.draw_header(frame, body, Some(sort), mouse, hit);
    hit.main_list = rows_area;

    let count = match search_tab {
        SearchTab::Artists => results.artists.len(),
        _ => results.playlists.len(),
    };
    super::clamp_offset(list_state, count, rows_area.height as usize);
    let hover = super::table::hovered_row(rows_area, list_state.offset(), count, mouse);
    let wash = |i: usize, item| super::table::hover_row(item, Some(i) == hover);
    let items: Vec<ListItem> = match search_tab {
        SearchTab::Tracks | SearchTab::Albums | SearchTab::Stations => unreachable!(),
        SearchTab::Artists => results
            .artists
            .rows()
            .enumerate()
            .map(|(i, a)| wash(i, artist_row(a, &layout, i == main_index)))
            .collect(),
        SearchTab::Playlists => results
            .playlists
            .rows()
            .enumerate()
            // Search results name every owner: one of them being yours is
            // the exception here, not the rule. The playing marker still
            // applies — a result can be the queue you are listening to.
            .map(|(i, p)| {
                let playing = marks.context.as_deref()
                    == Some(crate::app::state::playlist_key(&p.id).as_str());
                wash(i, playlist_row(p, &layout, None, playing, i == main_index))
            })
            .collect(),
    };
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    use super::*;
    use crate::app::state::{AppState, Playlist, SearchResults};

    fn track(name: &str, artists: &str) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            artists: artists.into(),
            album: "Album Name".into(),
            release_year: "2020".into(),
            duration_ms: 83_000,
            track_number: 1,
            album_id: None,
            credits: artists
                .split(", ")
                .map(|name| Credit {
                    name: name.into(),
                    id: Some(format!("id-{name}")),
                })
                .collect(),
            cover_url: None,
        }
    }

    fn tracks_state(tracks: Vec<Track>) -> AppState {
        let mut st = AppState::new();
        let mut list = crate::app::state::TrackList::new("My List", "", None);
        list.append(tracks);
        st.main = MainView::Tracks(list);
        st
    }

    /// A page that carries its identity, so the header can offer a link to it.
    fn page_state(kind: crate::app::state::TrackListKind, cache_key: &str) -> AppState {
        let mut st = tracks_state(vec![track("Alpha", "Ann")]);
        if let MainView::Tracks(list) = &mut st.main {
            list.kind = kind;
            list.cache_key = Some(cache_key.into());
        }
        st
    }

    /// Mark `uri` as the playing track: a one-row queue pointing at it, and
    /// the transport state that says something is on.
    fn play_uri(st: &mut AppState, uri: &str) {
        let mut t = track("x", "x");
        t.uri = uri.into();
        st.queue = Some(crate::app::queue::Queue::new(vec![t], 0, "Q"));
        st.playback = Some(crate::app::state::Playback::started(50, false));
    }

    /// Rows the header takes above the pane. The path a page draws is on it,
    /// so these tests draw it too: the row belongs to the header, but
    /// [`fit_trail`] and [`draw_trail`] are what put a path on it.
    const HEAD: usize = super::super::HEAD_H as usize;
    /// The row within that band the path lands on.
    const PATH: usize = super::super::SEARCH_H as usize;

    /// Draw the header and the pane under it, as [`super::super::draw`] lays
    /// them out. Row 0 is the mark and the search prompt, row [`PATH`] the
    /// path, row [`HEAD`] the first row of the pane.
    fn screen(state: &mut AppState, frame: &mut Frame) {
        let head = Rect {
            height: frame.area().height.min(HEAD as u16),
            ..frame.area()
        };
        let body = Rect {
            y: frame.area().y + head.height,
            height: frame.area().height - head.height,
            ..frame.area()
        };
        let page = page_header(state);
        super::super::top_row::draw(frame, head, state, page);
        draw(frame, body, state);
    }

    fn render(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| screen(state, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    /// A playlist page with a cover of its own, as Discover Weekly has.
    fn playlist_state() -> AppState {
        let mut st = tracks_state(vec![track("One", "Alela"), track("Two", "Sera")]);
        if let MainView::Tracks(list) = &mut st.main {
            list.cache_key = Some(crate::app::state::playlist_key("dw"));
            list.header.name = "Discover Weekly".into();
            list.header.subtitle = "by Spotify".into();
            list.header.owner_id = "spotify".into();
            list.header.cover_url = Some("https://i.scdn.co/image/dw".into());
        }
        st
    }

    fn album_state() -> AppState {
        let mut st = tracks_state(vec![track("One", "Donna"), track("Two", "Donna")]);
        if let MainView::Tracks(list) = &mut st.main {
            list.kind = crate::app::state::TrackListKind::Album;
            list.header.name = "Dance In The Street".into();
            list.header.subtitle = "Donna The Buffalo · 2018".into();
            list.header.credits = vec![Credit {
                name: "Donna The Buffalo".into(),
                id: Some("r1".into()),
            }];
            list.header.cover_url = Some("https://i.scdn.co/image/abc".into());
        }
        st
    }

    /// There is one album page, not two. The header band draws the art layout
    /// only when the view has a sleeve, so a route that opens an album with
    /// `cover_url: None` gets the cramped text band instead. Every route
    /// supplies the sleeve, so every route renders the same page.
    #[test]
    fn an_album_opened_from_a_track_row_renders_the_same_page() {
        let sleeve = "https://i.scdn.co/image/abc";
        // What `open_album_of_selection` hands a track row's album, and what
        // an album row hands its own — the same fields, so the same header.
        let from_row = |cover: Option<&str>| {
            let mut st = album_state();
            if let MainView::Tracks(list) = &mut st.main {
                list.header.cover_url = cover.map(Into::into);
            }
            let mut out = st;
            let lines = render(&mut out, 90, 24);
            (lines, out)
        };

        let (with_art, _) = from_row(Some(sleeve));
        let (no_art, _) = from_row(None);
        assert_ne!(with_art, no_art, "the two layouts really are different");

        // The sleeve branch is the good one: art on the left, metadata stacked
        // beside it, and the table pushed down past the taller band.
        let w = super::super::table::art_w(ART_H) as usize;
        assert!(with_art[4].chars().take(w).all(|c| c == '▀' || c == '♫'));
        assert!(with_art[5].contains("Donna The Buffalo · 2018"));
        assert!(with_art[15].contains("Title"));
        // Without one it collapses onto three rows and loses the artwork.
        assert!(!no_art[4].chars().take(w).all(|c| c == '▀' || c == '♫'));
    }

    /// The album page is the one browse view that is *about* a record, so it
    /// is the one that gets a sleeve. It is 10 rows and therefore 20 cells, and
    /// the metadata stacks beside it rather than sharing a row with the count.
    #[test]
    fn an_album_page_draws_its_sleeve_beside_stacked_metadata() {
        let mut st = album_state();
        let lines = render(&mut st, 90, 24);
        assert!(lines[PATH].contains("DANCE IN THE STREET"));
        // Sleeve occupies the left 20 cells of the ten band rows.
        // No cover is decoded in the test, so this is the placeholder swatch:
        // half-blocks with a single ♫ in the middle.
        let w = super::super::table::art_w(ART_H) as usize;
        for row in lines.iter().take(14).skip(4) {
            let sleeve: String = row.chars().take(w).collect();
            assert!(
                sleeve.chars().all(|c| c == '▀' || c == '♫'),
                "not a sleeve row: {row:?}"
            );
        }
        // Metadata stacks in the column beside it.
        assert!(lines[4].contains("Dance In The Street"));
        assert!(lines[5].contains("Donna The Buffalo · 2018"));
        assert!(lines[6].contains("2 tracks"));
        assert!(lines[8].contains("▶ play"));
        assert!(lines[8].contains("shuffle"));
        assert!(!st.hit.header_play_btn.is_empty());
        assert!(!st.hit.header_shuffle_btn.is_empty());
        // The table starts after the band and its spacer.
        assert!(lines[15].contains("Title"));
    }

    /// The masthead's credit line is a run of links, not one label: each name
    /// is its own target, and the year after them is not a target at all.
    #[test]
    fn the_album_masthead_links_each_credited_artist() {
        let mut st = album_state();
        if let MainView::Tracks(list) = &mut st.main {
            list.header.subtitle = "Donna The Buffalo, Jeb Puryear · 2018".into();
            list.header.credits.push(Credit {
                name: "Jeb Puryear".into(),
                id: Some("r2".into()),
            });
        }
        let lines = render(&mut st, 90, 24);
        assert!(lines[5].contains("Donna The Buffalo, Jeb Puryear · 2018"));

        let links = &st.hit.header_artist_links;
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].1.id.as_deref(), Some("r1"));
        assert_eq!(links[1].1.id.as_deref(), Some("r2"));
        assert_eq!(links[0].0.width, "Donna The Buffalo".len() as u16);
        assert_eq!(links[1].0.width, "Jeb Puryear".len() as u16);
        assert_eq!(links[0].0.y, 5, "not on the subtitle row");
        assert_eq!(links[1].0.x, links[0].0.right() + 2);
        // The year is past the last name, so nothing there opens a page.
        let year = Position {
            x: links[1].0.right() + 3,
            y: 5,
        };
        assert!(!links.iter().any(|(rect, _)| rect.contains(year)));
    }

    /// A page with nobody credited — a playlist, Liked Songs — prints its
    /// subtitle whole and records nothing, so a click there reaches the page
    /// underneath rather than a target nothing offered.
    #[test]
    fn a_playlist_masthead_records_no_artist_links() {
        let mut st = tracks_state(vec![track("Alpha", "Ann")]);
        if let MainView::Tracks(list) = &mut st.main {
            list.header.subtitle = "by dmills".into();
            list.header.cover_url = Some("https://i.scdn.co/image/abc".into());
        }
        let lines = render(&mut st, 90, 24);
        assert!(lines[5].contains("by dmills"));
        assert!(st.hit.header_artist_links.is_empty());
    }

    /// Arrive at the state's page from Home, so its trail has a real ancestor
    /// instead of a snapshot of the page it is already on.
    fn from_home(st: &mut AppState) {
        let page = std::mem::replace(&mut st.main, MainView::Home);
        st.push_view();
        st.main = page;
    }

    /// Every page below Home is drilled into from somewhere, so every one of
    /// them spells the path that got it there, the playlist page included.
    ///
    /// The trail is anchored at the margin, which is the point of it: a `←
    /// <name>` pill sitting after a section label whose width is the page's
    /// kind lands the one control that means "go back" in a different column
    /// on every page. The mark is a row above the path, so the path starts
    /// where every other line of content does.
    ///
    /// Home contributes no crumb at either end — the mark above the path is
    /// already the way there.
    #[test]
    fn pages_spell_the_path_that_reached_them() {
        // The column every path starts in.
        let x0 = 0;

        let mut st = album_state();
        from_home(&mut st);
        st.main_index = 0;
        let lines = render(&mut st, 90, 22);
        assert!(
            st.hit.crumbs.is_empty(),
            "Home is the only step behind it, and it draws none: {:?}",
            st.hit.crumbs
        );
        assert!(
            lines[PATH].contains("DANCE IN THE STREET"),
            "{:?}",
            lines[PATH]
        );
        assert!(!lines[PATH].contains('›'), "{:?}", lines[PATH]);

        // And the head starts in the same column whatever the page is called
        // — which the pill, drawn after a variable-width label, never did.
        // Columns, not bytes: the `♫` in the mark is three bytes wide and one
        // cell.
        let col =
            |line: &str, needle: &str| line.find(needle).map(|b| line[..b].chars().count() as u16);
        assert_eq!(col(&lines[PATH], "DANCE"), Some(x0));

        for (mut st, name) in [
            (artist_state(), "MUSE"),
            (tracks_state(vec![track("One", "Donna")]), "MY LIST"),
            // Search too: it replaces the page you were on, and names itself
            // by its query, so the row says what the list below it is
            // answering as well as where Esc would put you.
            (search_state(), "“MUSE”"),
        ] {
            from_home(&mut st);
            let lines = render(&mut st, 90, 22);
            assert_eq!(
                col(&lines[PATH], name),
                Some(x0),
                "{name}: {:?}",
                lines[PATH]
            );
        }
    }

    /// The whole path, not just one step of it. The pill this replaced could
    /// only ever name the page immediately behind; the stack held the rest
    /// and nothing on screen said so.
    #[test]
    fn a_deep_page_shows_every_step_that_reached_it() {
        let mut st = album_state();
        st.main = MainView::Home;
        st.push_view();
        st.main = MainView::Artist(crate::app::state::ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Donna The Buffalo".into(),
            image_url: None,
            genres: vec![],
            top: crate::app::state::TrackList::new("Donna The Buffalo", "", None),
            albums: vec![].into(),
            tab: crate::app::state::ArtistTab::Albums,
            loading: false,
            error: None,
        });
        st.push_view();
        st.main = album_state().main;

        let lines = render(&mut st, 90, 22);
        // The ancestor elides at `ANCESTOR_W` while the head keeps `HEAD_W`:
        // the page you are on is what the row is about, a step behind it only
        // has to be recognizable enough to aim at.
        assert!(
            lines[PATH].contains("DONNA THE BUF…  ›  DANCE IN THE STREET"),
            "{:?}",
            lines[PATH]
        );
        // The ancestor leads somewhere, at the depth it was pushed to. Home
        // sits below it at depth 0 and draws nothing, and the page itself is
        // a title rather than a control, so neither gets a rect.
        assert_eq!(st.hit.crumbs.len(), 1);
        assert_eq!(st.hit.crumbs[0].1, CrumbTarget::Depth(1));
    }

    /// A path too long for the row loses its *middle*, not its front.
    ///
    /// Both ends earn their place: the head is the page you are on, and the
    /// root is where the path starts. Shedding from the front — which is what
    /// this did first — took `HOME` off the row exactly when the path was long
    /// enough to want it.
    #[test]
    fn a_long_path_sheds_its_middle_and_keeps_both_ends() {
        let mut st = tracks_state(vec![track("One", "Donna")]);
        let page = st.main.clone();
        for name in ["home", "one", "two", "three"] {
            let mut list = crate::app::state::TrackList::new(name, "", None);
            // Distinct identities, or `push_view` would collapse them.
            list.cache_key = Some(crate::app::state::playlist_key(name));
            st.main = MainView::Tracks(list);
            st.push_view();
        }
        st.main = page;
        let lines = render(&mut st, 90, 22);
        assert!(
            lines[PATH].contains("…  ›  TWO  ›  THREE  ›  MY LIST"),
            "{:?}",
            lines[PATH]
        );
        assert!(!lines[PATH].contains("ONE  ›"), "{:?}", lines[PATH]);
        // The root is a crumb like any other; the ellipsis between it and the
        // rest stands for what was shed and leads nowhere.
        assert_eq!(st.hit.crumbs.len(), 3);
        assert_eq!(st.hit.crumbs[0].1, CrumbTarget::Depth(0));
        assert_eq!(st.hit.crumbs[1].1, CrumbTarget::Depth(2));

        // The narrow ancestors earn their keep here: at the head's width this
        // row would hold two steps, and it holds three.
        let lines = render(&mut st, 80, 22);
        assert!(
            lines[PATH].contains("…  ›  TWO  ›  THREE  ›  MY LIST"),
            "{:?}",
            lines[PATH]
        );
    }

    /// The rail's contents, at full width: one line per playlist with the
    /// count as a column, and the Owner cell blank for the ones you own.
    #[test]
    fn the_playlists_page_lists_them_one_to_a_row() {
        let mut st = AppState::new();
        st.main = MainView::Playlists;
        st.me_id = Some("dm".into());
        st.set_playlists(vec![
            Playlist {
                id: "p1".into(),
                name: "trendy".into(),
                track_count: 18,
                owner: "Dalton M".into(),
                owner_id: "dm".into(),
                snapshot_id: "s".into(),
                cover_url: None,
                public: None,
                collaborative: false,
            },
            Playlist {
                id: "p2".into(),
                name: "New Music Friday".into(),
                track_count: 38,
                owner: "NPR Music".into(),
                owner_id: "npr".into(),
                snapshot_id: "s".into(),
                cover_url: None,
                public: None,
                collaborative: false,
            },
        ]);
        let lines = render(&mut st, 90, 22);
        assert!(lines[PATH].contains("PLAYLISTS"), "{:?}", lines[PATH]);
        assert!(lines[PATH].contains("2 playlists"), "{:?}", lines[PATH]);
        assert!(lines[4].contains("Title") && lines[4].contains("Owner"));
        // Playlists only: Liked Songs is a Home row, and is not one of these.
        assert!(!lines.iter().any(|l| l.contains("Liked Songs")));
        assert!(lines[6].contains("trendy") && lines[6].contains("18"));
        assert!(
            !lines[6].contains("Dalton M"),
            "your own name is not information: {:?}",
            lines[6]
        );
        assert!(lines[7].contains("NPR Music"), "{:?}", lines[7]);
    }

    /// Nothing pushed means nothing to go back to — except on an album page,
    /// which can always go *up* to the artist its header credits. An `up` and
    /// a `back` are the same shape in a trail, which is half the reason for
    /// drawing one: a single `← <name>` pill spells both and cannot say which
    /// it means.
    #[test]
    fn an_album_page_with_no_history_offers_its_artist() {
        let mut st = album_state();
        let lines = render(&mut st, 90, 22);
        // Capped at `ANCESTOR_W`, which is what every ancestor crumb gets.
        assert!(
            lines[PATH].contains("DONNA THE BUF…  ›  DANCE IN THE STREET"),
            "{:?}",
            lines[PATH]
        );
        assert_eq!(
            st.hit.crumbs[0].1,
            CrumbTarget::Artist {
                id: "r1".into(),
                name: "Donna The Buffalo".into()
            }
        );

        // Without an artist id there is nowhere to go, so the page stands
        // alone: one crumb, its own name, and nothing to click.
        let mut st = album_state();
        if let MainView::Tracks(list) = &mut st.main {
            list.header.credits[0].id = None;
        }
        let lines = render(&mut st, 90, 22);
        assert!(st.hit.crumbs.is_empty());
        assert!(
            lines[PATH].contains("DANCE IN THE STREET"),
            "{:?}",
            lines[PATH]
        );
        assert!(!lines[PATH].contains('›'));
    }

    /// A pane too narrow for the whole path keeps the page's own name and
    /// sheds the rest, rather than letting the trail run off the row. The
    /// crumbs that went take their hit rects with them, like every other
    /// control this UI would otherwise clip.
    #[test]
    fn a_narrow_pane_sheds_the_trail_but_keeps_the_page() {
        let mut st = album_state();
        from_home(&mut st);
        let lines = render(&mut st, 34, 22);
        assert!(st.hit.crumbs.is_empty(), "{:?}", st.hit.crumbs);
        assert!(!lines[PATH].contains("HOME"), "{:?}", lines[PATH]);
        assert!(lines[PATH].contains("DANCE"), "{:?}", lines[PATH]);
    }

    /// The decoded sleeve carries the URL it came from, and the band checks it.
    ///
    /// `pop_view` restores a header without issuing a fetch, so without this a
    /// page you navigate *back* to wears whatever artwork the page you left
    /// had put in the single slot.
    #[test]
    fn an_album_page_refuses_a_sleeve_from_a_different_record() {
        let sleeve = |url: &str| {
            std::sync::Arc::new(crate::cover::Cover {
                url: url.into(),
                px: vec![[200, 40, 40]; crate::cover::COVER_PX * crate::cover::COVER_PX],
                size: crate::cover::COVER_PX,
                accent: None,
                ramp: None,
            })
        };

        // Matching URLs: the decoded cover is drawn.
        let mut st = album_state();
        st.view_cover = Some(sleeve("https://i.scdn.co/image/abc"));
        let mut terminal = Terminal::new(TestBackend::new(90, 22)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let art_fg = buf.cell(Position { x: 0, y: 4 }).unwrap().fg;
        assert_eq!(art_fg, ratatui::style::Color::Rgb(200, 40, 40));

        // The slot still holds the *other* album's sleeve: the band falls back
        // to the placeholder rather than hanging it on this record.
        let mut st = album_state();
        st.view_cover = Some(sleeve("https://i.scdn.co/image/SOMETHING-ELSE"));
        let mut terminal = Terminal::new(TestBackend::new(90, 22)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let lines: Vec<String> = (0..20)
            .map(|y| {
                (0..90)
                    .filter_map(|x| buf.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect();
        // Still a block, so the layout does not jump — just not that cover.
        assert!(
            lines[4].starts_with('▀'),
            "the band lost its block: {:?}",
            lines[4]
        );
        assert_ne!(
            buf.cell(Position { x: 0, y: 4 }).unwrap().fg,
            ratatui::style::Color::Rgb(200, 40, 40),
            "a different album's sleeve was drawn"
        );
    }

    /// A page with no cover loses the sleeve and nothing else. A playlist that
    /// Spotify only made a mosaic for is one of these — `playlist_cover` drops
    /// the mosaic, so the page arrives with no cover rather than with one the
    /// band has to refuse — and so is an album whose art it never gave us.
    ///
    /// The point is that the two read as one page with less to say, not as two
    /// different pages. Every line sits where it sits beside a sleeve.
    #[test]
    fn a_coverless_page_keeps_the_layout_and_drops_only_the_sleeve() {
        for mut st in [
            {
                let mut st = tracks_state(vec![track("A", "B")]);
                if let MainView::Tracks(list) = &mut st.main {
                    list.header.subtitle = "by me".into();
                }
                st
            },
            {
                let mut st = album_state();
                if let MainView::Tracks(list) = &mut st.main {
                    list.header.cover_url = None;
                }
                st
            },
        ] {
            let lines = render(&mut st, 90, 22);
            assert!(
                !lines.iter().any(|l| l.contains('▀')),
                "a sleeve appeared: {lines:#?}"
            );
            assert!(lines[6].contains("tracks"), "{lines:#?}");
            assert!(lines[8].contains("▶ play"), "{lines:#?}");
        }
    }

    /// The same page, with and without a sleeve, puts every line of the header
    /// on the same row. The sleeve is six rows tall, so the table below it
    /// starts one row lower — that is the art taking up room, not the layout
    /// rearranging itself around what the page happens to have.
    #[test]
    fn a_sleeve_moves_nothing_but_the_table_under_it() {
        let rows_of = |cover: Option<&str>| {
            let mut st = playlist_state();
            if let MainView::Tracks(list) = &mut st.main {
                list.header.description = "Fresh music every Monday".into();
                list.header.cover_url = cover.map(str::to_string);
            }
            let lines = render(&mut st, 90, 22);
            let at = |needle: &str| {
                lines
                    .iter()
                    .position(|l| l.contains(needle))
                    .unwrap_or_else(|| panic!("no {needle:?} in {lines:#?}"))
            };
            [
                at("Discover Weekly"),
                at("by Spotify"),
                at("Fresh music"),
                at("tracks ·"),
                at("▶ play"),
            ]
        };
        assert_eq!(
            rows_of(Some("https://i.scdn.co/image/dw")),
            rows_of(None),
            "the header rearranged itself around the sleeve"
        );
    }

    /// A playlist with a cover of its own is an album page in every respect
    /// that matters: the sleeve, the name beside it, the control below.
    #[test]
    fn a_playlist_with_a_cover_draws_the_sleeve() {
        let mut st = playlist_state();
        let lines = render(&mut st, 90, 22);
        assert!(
            lines.iter().any(|l| l.contains('▀')),
            "no sleeve: {lines:#?}"
        );
        assert!(lines[4].contains("Discover Weekly"));
        assert!(lines[5].contains("by Spotify"));
        assert!(lines[6].contains("tracks"));
        assert!(lines[8].contains("▶ play"));
    }

    /// The blurb takes a row of its own, and the controls move down for it
    /// rather than sitting on top of it.
    #[test]
    fn a_blurb_spends_a_row_and_moves_the_controls() {
        let mut st = playlist_state();
        if let MainView::Tracks(list) = &mut st.main {
            list.header.description = "Your weekly mixtape of fresh music".into();
        }
        let lines = render(&mut st, 90, 22);
        assert!(lines[5].contains("by Spotify"));
        assert!(lines[6].contains("weekly mixtape"));
        assert!(lines[7].contains("tracks"));
        assert!(
            lines[9].contains("▶ play"),
            "controls moved wrong: {lines:#?}"
        );
    }

    /// Without a sleeve the band grows by the blurb's row rather than drawing
    /// it over the column header below.
    #[test]
    fn a_blurb_without_a_sleeve_pushes_the_table_down() {
        let mut st = tracks_state(vec![track("A", "B")]);
        if let MainView::Tracks(list) = &mut st.main {
            list.header.subtitle = "by me".into();
            list.header.description = "Deep cuts chosen for you".into();
        }
        let lines = render(&mut st, 90, 22);
        assert!(lines[5].contains("by me"), "{lines:#?}");
        assert!(lines[6].contains("Deep cuts"), "{lines:#?}");
        assert!(lines[7].contains("tracks"), "{lines:#?}");
        assert!(
            lines[9].contains("▶ play"),
            "controls moved wrong: {lines:#?}"
        );
        assert!(lines[11].contains("Title"), "{lines:#?}");
    }

    /// A pane too short to seat the stacked band falls back to the one-row
    /// one, blurb and all — that is a degradation, and prose is the first
    /// thing it can afford to lose.
    #[test]
    fn a_short_pane_falls_back_to_the_compact_band() {
        let mut st = tracks_state(vec![track("A", "B")]);
        if let MainView::Tracks(list) = &mut st.main {
            list.header.subtitle = "by me".into();
            list.header.description = "Deep cuts chosen for you".into();
        }
        let lines = render(&mut st, 90, 15);
        assert!(lines[4].contains("by me"), "{lines:#?}");
        assert!(lines[4].contains("tracks"), "not one row: {lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("Deep cuts")));
        assert!(lines[5].contains("▶ play"), "{lines:#?}");
    }

    /// The save control is for a playlist someone else made. Your own carries
    /// `edit` instead: Spotify has no unfollow for an owned playlist that is
    /// not a delete, and that is not what a pill beside ▶ play should mean.
    #[test]
    fn the_playlist_controls_follow_who_owns_it() {
        let mut st = playlist_state();
        st.me_id = Some("me".into());
        st.saved_playlists.insert("dw".into(), true);
        let theirs = render(&mut st, 90, 22);
        assert!(theirs[8].contains("★ saved"), "{theirs:#?}");
        assert!(!theirs[8].contains("edit"));

        if let MainView::Tracks(list) = &mut st.main {
            list.header.owner_id = "me".into();
        }
        let mine = render(&mut st, 90, 22);
        assert!(mine[8].contains("edit"), "{mine:#?}");
        assert!(!mine[8].contains("save"));
    }

    /// A playlist nothing has answered about yet draws no save control. A
    /// dim `☆ save` on a playlist already in the library would be a lie until
    /// the check lands, and the check is one round trip behind the page.
    #[test]
    fn an_unchecked_playlist_draws_no_save_control() {
        let mut st = playlist_state();
        st.me_id = Some("me".into());
        let lines = render(&mut st, 90, 22);
        assert!(!lines[8].contains("save"), "{lines:#?}");
    }

    /// The sleeve is shed before the text is, the same order the player sheds
    /// its own cover in.
    #[test]
    fn a_narrow_or_short_album_page_drops_the_sleeve_first() {
        for (w, h) in [(50u16, 20u16), (90, 12)] {
            let mut st = album_state();
            let lines = render(&mut st, w, h);
            assert!(
                !lines.iter().any(|l| l.contains('▀')),
                "the sleeve survived at {w}x{h}: {lines:#?}"
            );
            assert!(
                lines.iter().any(|l| l.contains("Dance In The Street")),
                "the name went before the sleeve did at {w}x{h}"
            );
        }
    }

    /// Nothing may paint text without choosing a colour. `Style::default()`
    /// leaves the raw terminal foreground, the one unthemed colour on the
    /// screen, which makes the browse pages read harsher than the player even
    /// though they share a palette.
    ///
    /// `Color::Reset` on a *background* is fine and expected: only the cover
    /// art and the hover pills set one.
    #[test]
    fn no_row_paints_text_in_the_raw_terminal_foreground() {
        use ratatui::style::Color;

        let mut states = vec![
            tracks_state(vec![track("Alpha", "Ann"), track("Beta", "Bob")]),
            search_state(),
            artist_state(),
            AppState::new(),
        ];
        // Album, artist, playlist and station tabs draw their own row builders.
        for tab in [
            SearchTab::Albums,
            SearchTab::Artists,
            SearchTab::Playlists,
            SearchTab::Stations,
        ] {
            let mut st = search_state();
            st.search_tab = tab;
            states.push(st);
        }
        for st in &mut states {
            let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
            terminal.draw(|f| screen(st, f)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            for y in 0..16 {
                for x in 0..100 {
                    let cell = buffer.cell(Position { x, y }).unwrap();
                    if cell.symbol().trim().is_empty() {
                        continue;
                    }
                    assert!(
                        matches!(cell.fg, Color::Rgb(..)),
                        "{:?} at {x},{y} is unthemed ({:?})",
                        cell.symbol(),
                        cell.fg
                    );
                }
            }
        }
    }

    #[test]
    fn track_table_renders_columns_and_markers() {
        let mut st = tracks_state(vec![
            track("Alpha", "Ann"),
            track("Beta", "Bob"),
            track("Gamma", "Cyd"),
        ]);
        play_uri(&mut st, "spotify:track:Beta");
        let lines = render(&mut st, 90, 14);
        // Trail + blank, then the band (summary, ▶ play, spacer),
        // then the column header + spacer, then the rows.
        assert!(lines[PATH].contains("MY LIST"));
        assert!(lines[4].contains("My List"));
        assert!(lines[4].contains("3 tracks · 4 min"));
        assert!(lines[5].contains("▶ play"));
        assert!(lines[5].contains("shuffle"));
        assert!(!st.hit.header_play_btn.is_empty());
        assert!(!st.hit.header_shuffle_btn.is_empty());
        assert!(lines[7].contains("Title"));
        assert!(lines[7].contains("Artist"));
        assert!(lines[7].contains("Album"));
        // Time and the `★ ⧉ +` run head themselves.
        assert!(!lines[7].contains("Time"));
        assert!(lines[9].contains("Alpha"));
        // No frame, so rows start at column 0.
        assert!(lines[10].starts_with("▶ ") && lines[10].contains("Beta"));
        // Only the playing row is marked; there is no next-up arrow.
        assert!(lines[11].starts_with("  ") && lines[11].contains("Gamma"));
        assert!(!lines.iter().any(|l| l.contains("→")));
        assert!(lines[9].contains("1:23"));
        assert!(!st.hit.main_list.is_empty());
        // Not one box-drawing character anywhere on the pane.
        assert!(
            !lines.iter().any(|l| l.contains('│') || l.contains('╭')),
            "a border survived: {lines:#?}"
        );
    }

    #[test]
    fn wide_glyphs_keep_duration_column_aligned() {
        let mut st = tracks_state(vec![
            track("Plain Ascii Name", "Someone"),
            track("残酷な天使のテーゼ、とても長いタイトル", "高橋洋子"),
            track("emoji 🎵🎵🎵🎵 name", "🎤 artist"),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        // Label, blank, band (summary / ▶ play / spacer), column header,
        // spacer, then the rows at y7..=9.
        // The duration "1:23" must sit at the same screen column in every row.
        let colon_x = |y: u16| {
            (0..60u16)
                .rev()
                .find(|&x| buffer.cell(Position { x, y }).unwrap().symbol() == ":")
                .expect("no duration on this row")
        };
        let x0 = colon_x(9);
        assert_eq!(colon_x(10), x0);
        assert_eq!(colon_x(11), x0);
    }

    #[test]
    fn selection_persists_when_offset_scrolls_away() {
        let tracks: Vec<Track> = (0..30).map(|i| track(&format!("T{i}"), "A")).collect();
        let mut st = tracks_state(tracks);
        st.main_index = 1;
        *st.main_list.offset_mut() = 10;
        render(&mut st, 80, 16);
        // Drawing must not reset the wheel-scrolled offset.
        assert_eq!(st.main_list.offset(), 10);

        // Scroll back: the selected row still carries the highlight.
        *st.main_list.offset_mut() = 0;
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        // border + pad + band(5, no subtitle on this fixture) + header +
        // spacer + row 0; selected is index 1
        let row_y = 12;
        assert!((1..79u16).any(|x| {
            let cell = buffer.cell(Position { x, y: row_y }).unwrap();
            cell.fg == theme::BRIGHT && cell.modifier.contains(Modifier::BOLD)
        }));
    }

    /// The number belongs to the track, not to the row it lands on: a sorted
    /// list reads 3, 2, 1 down the side rather than renumbering itself.
    #[test]
    fn the_number_column_stays_with_its_track_across_a_sort() {
        let mut st = tracks_state(vec![
            track("Zebra", "A1"),
            track("Mango", "A2"),
            track("Apple", "A3"),
        ]);
        let lines = render(&mut st, 90, 14);
        let no = |line: &str| line.trim_start().split(' ').next().unwrap().to_string();
        assert_eq!(
            [no(&lines[9]), no(&lines[10]), no(&lines[11])],
            ["1", "2", "3"]
        );

        if let MainView::Tracks(list) = &mut st.main {
            list.sort = Sort {
                key: ColKey::Title,
                ascending: true,
            };
            list.rebuild();
        }
        let lines = render(&mut st, 90, 14);
        assert!(lines[9].contains("Apple") && lines[11].contains("Zebra"));
        assert_eq!(
            [no(&lines[9]), no(&lines[10]), no(&lines[11])],
            ["3", "2", "1"]
        );
    }

    #[test]
    fn sorted_view_reorders_rows_and_suppresses_next_marker() {
        let mut st = tracks_state(vec![
            track("Zebra", "A1"),
            track("Apple", "A2"),
            track("Mango", "A3"),
        ]);
        play_uri(&mut st, "spotify:track:Zebra");
        if let MainView::Tracks(list) = &mut st.main {
            list.sort = Sort {
                key: ColKey::Title,
                ascending: true,
            };
            list.rebuild();
        }
        let lines = render(&mut st, 90, 14);
        // The header marks the column it is ordered by, and nothing else on
        // the page repeats that.
        assert!(lines[7].contains("Title ▲"));
        assert!(!lines.iter().any(|l| l.contains("sort:")));
        assert!(lines[9].contains("Apple"));
        assert!(lines[10].contains("Mango"));
        assert!(lines[11].starts_with("▶ ") && lines[11].contains("Zebra"));
        // Playback follows context order, so the next-up guess is hidden.
        assert!(!lines.iter().any(|l| l.contains("→ ")));
    }

    #[test]
    fn artist_and_album_columns_record_clickable_rects() {
        let mut st = tracks_state(vec![track("Alpha", "Ann"), track("Beta", "Bob")]);
        let mut terminal = Terminal::new(TestBackend::new(90, 14)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        let col = &st.hit.main_album_col;
        assert!(!col.rect.is_empty());
        // Clipped to the two filled rows, inside the list area.
        assert_eq!(col.rect.height, 2);
        assert!(st.hit.main_list.contains(Position {
            x: col.rect.x,
            y: col.rect.y
        }));
        // One artist per row, each on the row it belongs to.
        assert_eq!(st.hit.main_artist_links.len(), 2);
        for (i, (rect, credit)) in st.hit.main_artist_links.iter().enumerate() {
            assert_eq!(rect.y, st.hit.main_list.y + i as u16);
            assert_eq!(credit.id.as_deref(), Some(["id-Ann", "id-Bob"][i]));
        }

        // The recorded targets line up with the rendered cells.
        let text_at = |rect: Rect, w: usize| -> String {
            (rect.x..rect.x + w as u16)
                .filter_map(|x| buffer.cell(Position { x, y: rect.y }).map(|c| c.symbol()))
                .collect()
        };
        assert_eq!(text_at(st.hit.main_artist_links[0].0, 3), "Ann");
        assert_eq!(text_at(st.hit.main_album_col.rect, 5), "Album");
    }

    /// The target is the name as printed, not the column it is padded out to:
    /// the pill lights the text alone, and a click in the empty tail of a
    /// short name would open a page nothing on screen offered.
    #[test]
    fn a_link_column_takes_clicks_on_its_text_and_not_its_padding() {
        let mut st = tracks_state(vec![track("Alpha", "Ann"), track("Beta", "Bob")]);
        render(&mut st, 90, 14);

        let col = &st.hit.main_album_col;
        let printed = "Album Name".len() as u16;
        assert!(
            col.rect.width > printed,
            "the column is not padded, so this proves nothing"
        );
        assert_eq!(col.widths, vec![printed, printed]);
        let at = |dx: u16| Position {
            x: col.rect.x + dx,
            y: col.rect.y,
        };
        assert!(col.hit(at(0)), "the first cell of the name misses");
        assert!(col.hit(at(printed - 1)), "the last cell of the name misses");
        assert!(!col.hit(at(printed)), "the padding after the name hits");
        assert!(!col.hit(at(col.rect.width - 1)), "the far edge hits");

        // The artist column is padded the same way, and its target is the name
        // rather than the cell — it is recorded as the name and nothing else.
        let (rect, _) = st.hit.main_artist_links[0];
        assert_eq!(rect.width, "Ann".len() as u16);
    }

    /// A record credited to several artists prints one line and records one
    /// target per name, each covering that name and neither of its neighbours.
    #[test]
    fn every_credited_artist_is_its_own_target() {
        let mut st = tracks_state(vec![track("Clarity", "Zedd, Alessia Cara")]);
        render(&mut st, 110, 14);

        let links = &st.hit.main_artist_links;
        assert_eq!(links.len(), 2, "one target per name");
        assert_eq!(links[0].1.name, "Zedd");
        assert_eq!(links[1].1.name, "Alessia Cara");
        assert_eq!(links[0].0.width, "Zedd".len() as u16);
        assert_eq!(links[1].0.width, "Alessia Cara".len() as u16);
        assert_eq!(
            links[1].0.x,
            links[0].0.right() + 2,
            "the `, ` between them belongs to neither"
        );
    }

    /// A name Spotify identified by name only is drawn and inert, the way a
    /// station's country is when the directory gave no code.
    #[test]
    fn a_credit_without_an_id_is_printed_but_not_a_target() {
        let mut anonymous = track("Clarity", "Zedd");
        anonymous.credits[0].id = None;
        let mut st = tracks_state(vec![anonymous]);
        let lines = render(&mut st, 110, 14);

        assert!(
            lines.iter().any(|l| l.contains("Zedd")),
            "the name went missing"
        );
        assert!(st.hit.main_artist_links.is_empty());
    }

    /// A row with nothing in the cell prints nothing, so there is nothing to
    /// click — the whole padded column used to open an album the row had not
    /// got.
    #[test]
    fn an_empty_cell_is_no_target_at_all() {
        let mut blank = track("Alpha", "Ann");
        blank.album = String::new();
        let mut st = tracks_state(vec![blank]);
        render(&mut st, 90, 14);

        let col = &st.hit.main_album_col;
        assert_eq!(col.widths, vec![0]);
        for dx in 0..col.rect.width {
            assert!(
                !col.hit(Position {
                    x: col.rect.x + dx,
                    y: col.rect.y
                }),
                "an empty cell took a click at {dx}"
            );
        }
    }

    /// Every row wears the run, and the star reads its state as colour: the
    /// accent when saved, dim when not. The glyph itself never changes, so a
    /// row that is still being checked looks like one that came back unsaved
    /// rather than like a third thing.
    #[test]
    fn the_star_colours_saved_rows_and_dims_the_rest() {
        let mut st = tracks_state(vec![
            track("Alpha", "A"),
            track("Beta", "B"),
            track("Gamma", "C"),
        ]);
        st.liked.insert("spotify:track:Alpha".into(), true);
        st.liked.insert("spotify:track:Beta".into(), false);
        let mut terminal = Terminal::new(TestBackend::new(90, 13)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        let col = st.hit.main_like_col;
        // The mark sits one cell into its padded target.
        let star = |row: u16| {
            buffer
                .cell(Position {
                    x: col.x + 1,
                    y: col.y + row,
                })
                .unwrap()
        };
        for row in 0..3 {
            assert_eq!(star(row).symbol(), super::super::table::LIKED_MARK);
        }
        assert_eq!(star(0).fg, theme::accent_color());
        // Unsaved and still-unchecked are the same dim mark.
        assert_eq!(star(1).fg, theme::DIM);
        assert_eq!(star(2).fg, theme::DIM);
    }

    /// A page you can link to offers the control, right of ▶ shuffle and left
    /// of the playlist controls after it.
    #[test]
    fn a_page_with_a_link_offers_the_share_control() {
        use crate::app::state::TrackListKind;
        for (kind, key) in [
            (TrackListKind::Playlist, "playlist:p1"),
            (TrackListKind::Album, "album:a1"),
        ] {
            let mut st = page_state(kind, key);
            let lines = render(&mut st, 90, 14);
            assert!(lines[5].contains("⧉ share"), "{kind:?} {:?}", lines[5]);
            assert!(!st.hit.header_share_btn.is_empty(), "{kind:?}");
            assert!(
                st.hit.header_share_btn.x > st.hit.header_shuffle_btn.right(),
                "{kind:?}"
            );
        }
    }

    /// The artist page carries it too: an artist page is opened by id, so there
    /// is always a link.
    #[test]
    fn the_artist_page_offers_the_share_control() {
        let mut st = artist_state();
        let lines = render(&mut st, 90, 20);
        assert!(lines.iter().any(|l| l.contains("⧉ share")), "{lines:#?}");
        assert!(!st.hit.header_share_btn.is_empty());
        assert!(st.hit.header_share_btn.x > st.hit.header_shuffle_btn.right());
    }

    /// Liked Songs has no link that means the same thing to whoever opens it,
    /// so it is offered no control rather than one that shares the wrong page.
    #[test]
    fn liked_songs_offers_no_share_control() {
        let mut st = page_state(crate::app::state::TrackListKind::LikedSongs, "liked");
        let lines = render(&mut st, 90, 14);
        assert!(!lines[5].contains("share"), "{:?}", lines[5]);
        assert!(st.hit.header_share_btn.is_empty());
        assert!(!st.hit.header_shuffle_btn.is_empty(), "shuffle went too");
    }

    /// Hovering any one control lights that control alone — its whole padded
    /// cell, so the lit run says how big the target is.
    #[test]
    fn hovering_lights_one_control_of_the_pair() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        // Draw once to find out where the run landed.
        render(&mut st, 90, 12);
        let (like, share, add) = (
            st.hit.main_like_col,
            st.hit.main_share_col,
            st.hit.main_add_col,
        );
        assert!(!like.is_empty() && !share.is_empty() && !add.is_empty());

        let bg_at = |st: &mut AppState, x: u16, y: u16| {
            let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
            terminal.draw(|f| screen(st, f)).unwrap();
            terminal
                .backend()
                .buffer()
                .cell(Position { x, y })
                .unwrap()
                .bg
        };

        // The padding is part of the control, so the pointer catches it a cell
        // off the mark and the pill covers the whole cell.
        st.mouse_pos = Some(Position { x: add.x, y: add.y });
        for x in add.x..add.right() {
            assert_eq!(bg_at(&mut st, x, add.y), theme::DIM, "the + drew no pill");
        }
        for x in like.x..share.right() {
            assert_ne!(
                bg_at(&mut st, x, like.y),
                theme::DIM,
                "hovering the + lit a neighbour too"
            );
        }

        // The control between them lights alone in the same way.
        st.mouse_pos = Some(Position {
            x: share.x,
            y: share.y,
        });
        for x in share.x..share.right() {
            assert_eq!(bg_at(&mut st, x, share.y), theme::DIM, "the ⧉ drew no pill");
        }
        for x in [like.x, add.x] {
            assert_ne!(
                bg_at(&mut st, x, like.y),
                theme::DIM,
                "hovering the ⧉ lit a neighbour too"
            );
        }
    }

    /// The row under the pointer wears a faint wash across its whole width,
    /// and its neighbours stay as the terminal left them. The wash says which
    /// row a click will land on, which no single lit cell can.
    #[test]
    fn hovering_washes_the_whole_row_and_only_that_row() {
        use ratatui::style::Color;
        let mut st = tracks_state(vec![
            track("Alpha", "A"),
            track("Beta", "B"),
            track("Gamma", "C"),
        ]);
        render(&mut st, 90, 12);
        let rows = st.hit.main_list;
        let second = rows.y + 1;
        st.mouse_pos = Some(Position {
            x: rows.x + 4,
            y: second,
        });

        let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let bg = |x: u16, y: u16| buffer.cell(Position { x, y }).unwrap().bg;

        for x in rows.x..rows.right() {
            assert_eq!(bg(x, second), theme::FIELD, "the row is not washed at {x}");
            assert_eq!(bg(x, second - 1), Color::Reset, "the row above is washed");
            assert_eq!(bg(x, second + 1), Color::Reset, "the row below is washed");
        }
        // The gutter belongs to the scrollbar, not to the row.
        assert_eq!(bg(rows.right(), second), Color::Reset);
    }

    /// The wash sits behind the row rather than replacing what the row already
    /// lights: a control under the pointer keeps its own pill on top of it.
    #[test]
    fn a_washed_row_keeps_its_hovered_control_lit() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        render(&mut st, 90, 12);
        let add = st.hit.main_add_col;
        st.mouse_pos = Some(Position { x: add.x, y: add.y });

        let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let bg = |x: u16| buffer.cell(Position { x, y: add.y }).unwrap().bg;

        for x in add.x..add.right() {
            assert_eq!(bg(x), theme::DIM, "the wash swallowed the + pill");
        }
        assert_eq!(bg(add.x - 1), theme::FIELD, "the row is not washed");
    }

    /// The artist page's list is not all rows. Only a top track is a table
    /// row; a heading, the column header and the blanks around it are not.
    #[test]
    fn only_the_artist_pages_track_lines_wash() {
        let mut st = artist_state();
        render(&mut st, 90, 32);
        let body = st.hit.main_list;

        let bg_of = |st: &mut AppState, line: u16| {
            st.mouse_pos = Some(Position {
                x: body.x + 4,
                y: body.y + line,
            });
            let mut terminal = Terminal::new(TestBackend::new(90, 32)).unwrap();
            terminal.draw(|f| screen(st, f)).unwrap();
            terminal
                .backend()
                .buffer()
                .cell(Position {
                    x: body.x + 4,
                    y: body.y + line,
                })
                .unwrap()
                .bg
        };

        // Heading, blank, column header, blank, then the one top track. The
        // header lights its own sort label under the pointer, so what is
        // checked is that none of them takes the row wash.
        for line in 0..4 {
            assert_ne!(bg_of(&mut st, line), theme::FIELD, "line {line} washed");
        }
        assert_eq!(bg_of(&mut st, 4), theme::FIELD, "the track did not wash");
    }

    /// Each control is a padded click target of its own, the three flush
    /// against each other, at the end of the row before the time.
    #[test]
    fn the_pair_records_two_adjacent_clickable_rects() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        st.liked.insert("spotify:track:Alpha".into(), true);
        let mut terminal = Terminal::new(TestBackend::new(90, 14)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        let (like, share, add) = (
            st.hit.main_like_col,
            st.hit.main_share_col,
            st.hit.main_add_col,
        );
        for col in [like, share, add] {
            assert_eq!(col.width, 3, "the control lost its padding");
            assert_eq!(col.height, 2, "the column outran the filled rows");
            assert!(st.hit.main_list.contains(Position { x: col.x, y: col.y }));
        }
        // Flush, in the order the deck wears them: no cell between them
        // belongs to neither control.
        assert_eq!(share.x, like.right());
        assert_eq!(add.x, share.right());
        let symbol = |x: u16| {
            buffer
                .cell(Position { x, y: like.y })
                .unwrap()
                .symbol()
                .to_string()
        };
        // Each mark is centred in its own cell.
        assert_eq!(symbol(like.x + 1), super::super::table::LIKED_MARK);
        assert_eq!(symbol(share.x + 1), super::super::table::SHARE_MARK);
        assert_eq!(symbol(add.x + 1), super::super::table::ADD_MARK);
        // And the run comes after the data columns it follows.
        assert!(like.x > st.hit.main_album_col.rect.right());
    }

    /// The pair is the last thing a narrowing pane gives up: the year, the
    /// album and the number all go first.
    #[test]
    fn the_pair_outlives_the_data_columns_on_a_narrow_pane() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        let lines = render(&mut st, 38, 12);
        let header = lines.iter().find(|l| l.contains("Title")).unwrap();
        assert!(!header.contains("Year"), "{header:?}");
        assert!(!header.contains("Album"), "{header:?}");
        // The pair heads itself, so the rects are what say it is still there.
        assert!(!st.hit.main_like_col.is_empty());
        assert!(!st.hit.main_add_col.is_empty());
    }

    #[test]
    fn header_band_absent_on_short_panes() {
        let mut st = tracks_state(vec![track("A", "B")]);
        // Body height 6, under the band's minimum of 8.
        let lines = render(&mut st, 80, 10);
        assert!(!lines.iter().any(|l| l.contains("▶ play")));
        assert!(!lines.iter().any(|l| l.contains("shuffle")));
        assert!(st.hit.header_play_btn.is_empty());
        assert!(st.hit.header_shuffle_btn.is_empty());
    }

    #[test]
    fn empty_playlist_shows_hint() {
        let mut st = tracks_state(Vec::new());
        let lines = render(&mut st, 60, 12);
        assert!(lines.iter().any(|l| l.contains("this playlist is empty")));
    }

    #[test]
    fn loading_title_shows_progress_and_suppresses_empty_hint() {
        let mut st = tracks_state(Vec::new());
        if let MainView::Tracks(list) = &mut st.main {
            list.loading = true;
            list.total = Some(200);
        }
        let lines = render(&mut st, 70, 14);
        assert!(lines[PATH].contains("MY LIST (LOADING…)"));
        assert!(lines[4].contains("0 of 200 tracks"));
        assert!(!lines.iter().any(|l| l.contains("this playlist is empty")));
    }

    /// The reason a page is blank, said on the page.
    ///
    /// The whole point of the control: a refused load used to draw the very
    /// line an empty one draws, so a playlist you know has tracks in it read
    /// as a playlist with none.
    #[test]
    fn a_refused_page_says_so_instead_of_claiming_to_be_empty() {
        let mut st = tracks_state(Vec::new());
        if let MainView::Tracks(list) = &mut st.main {
            list.error = Some(crate::app::state::LoadError::new(
                "429 Too Many Requests",
                crate::app::command::AppCommand::LoadPlaylistTracks {
                    playlist_id: "p1".into(),
                },
            ));
        }
        let lines = render(&mut st, 60, 14);
        assert!(lines.iter().any(|l| l.contains("429 Too Many Requests")));
        assert!(
            !lines.iter().any(|l| l.contains("this playlist is empty")),
            "a page that failed claimed to be empty"
        );
        assert!(lines.iter().any(|l| l.contains(RETRY_PILL)));
        assert!(!st.hit.retry_btn.is_empty(), "the control took no clicks");
    }

    /// A refusal that comes straight back leaves the spinner up for less than
    /// a frame, so the count is what tells you the press did anything.
    #[test]
    fn a_repeated_refusal_says_how_many_times_it_was_asked() {
        let mut st = tracks_state(Vec::new());
        if let MainView::Tracks(list) = &mut st.main {
            list.error = Some(crate::app::state::LoadError::new(
                "429 Too Many Requests",
                crate::app::command::AppCommand::LoadLikedSongs,
            ));
        }
        let has_tally = |lines: &[String]| lines.iter().any(|l| l.contains("asked again"));
        assert!(!has_tally(&render(&mut st, 60, 16)), "counted a first ask");

        st.retries = 1;
        // What `ui::draw` does at the top of every frame; this helper draws
        // the pane alone, so the reset has to be spelled here.
        st.hit = crate::app::state::HitAreas::default();
        let lines = render(&mut st, 60, 16);
        assert!(lines.iter().any(|l| l.contains("asked again once")));
        assert!(lines.iter().any(|l| l.contains(RETRY_PILL)));
        assert!(
            !st.hit.retry_btn.is_empty(),
            "the tally crowded the control"
        );

        st.retries = 3;
        assert!(
            render(&mut st, 60, 16)
                .iter()
                .any(|l| l.contains("asked again 3 times"))
        );
    }

    /// A directory page has nothing above its tab strip but the trail, so the
    /// word on the row and the spinner under it were the same news twice.
    #[test]
    fn a_loading_radio_page_says_so_in_the_pane_and_not_on_the_trail() {
        let mut st = AppState::new();
        st.main = MainView::Radio(crate::app::state::RadioView::new(
            crate::app::state::RadioScope::Popular,
            1,
        ));
        let lines = render(&mut st, 76, 16);
        assert!(!lines[PATH].contains("LOADING"), "{:?}", lines[PATH]);
        assert!(lines.iter().any(|l| l.contains("loading stations…")));
        // The trail going quiet must not take the fast tick with it, or the
        // spinner steps instead of turning.
        assert!(super::super::is_animating(&st));
    }

    /// The control is only ever recorded where it was drawn.
    #[test]
    fn a_page_that_did_not_fail_records_no_retry() {
        let mut st = tracks_state(Vec::new());
        let lines = render(&mut st, 60, 14);
        assert!(lines.iter().any(|l| l.contains("this playlist is empty")));
        assert!(st.hit.retry_btn.is_empty());
    }

    /// Too short for the pill, so it is not drawn — and therefore not
    /// recorded either, or a click on the row it would have taken would
    /// retry a page that never offered it.
    #[test]
    fn a_short_pane_drops_the_control_rather_than_recording_it_offscreen() {
        let mut st = tracks_state(Vec::new());
        if let MainView::Tracks(list) = &mut st.main {
            list.error = Some(crate::app::state::LoadError::new(
                "no",
                crate::app::command::AppCommand::LoadLikedSongs,
            ));
        }
        render(&mut st, 60, HEAD as u16 + 2);
        assert!(st.hit.retry_btn.is_empty());
    }

    /// A page still being fetched turns a spinner, and the frame loop holds
    /// the fast tick over it so it turns rather than steps.
    #[test]
    fn a_loading_page_turns_a_spinner() {
        let mut st = tracks_state(Vec::new());
        if let MainView::Tracks(list) = &mut st.main {
            list.loading = true;
        }
        let lines = render(&mut st, 60, 14);
        assert!(
            lines
                .iter()
                .any(|l| super::super::table::SPINNER.iter().any(|f| l.contains(f))),
            "no spinner frame on a loading page"
        );
        assert!(super::super::is_animating(&st));
    }

    /// The reason a search came back blank belongs to the half that was
    /// refused: the directory being unreachable is not Spotify refusing.
    #[test]
    fn each_search_half_reports_its_own_refusal() {
        let mut st = AppState::new();
        st.main = MainView::Search(crate::app::state::SearchResults {
            query: "muse".into(),
            stations_error: Some(crate::app::state::LoadError::new(
                "directory unreachable",
                crate::app::command::AppCommand::Search("muse".into()),
            )),
            ..Default::default()
        });
        st.search_tab = SearchTab::Stations;
        let lines = render(&mut st, 70, 16);
        assert!(lines.iter().any(|l| l.contains("directory unreachable")));

        // What `ui::draw` does at the top of every frame; this helper draws
        // the pane alone, so the reset has to be spelled here.
        st.hit = crate::app::state::HitAreas::default();
        st.search_tab = SearchTab::Tracks;
        let lines = render(&mut st, 70, 16);
        assert!(
            lines.iter().any(|l| l.contains("no tracks results")),
            "the Spotify tabs wore the directory's failure"
        );
        assert!(st.hit.retry_btn.is_empty());
    }

    /// The row names the page, not the page's kind.
    ///
    /// The header band under it already tells you the kind, with a sleeve and
    /// a year or with an owner, so the row spends itself on the path instead —
    /// which nothing else says.
    #[test]
    fn the_trail_names_the_page_whatever_kind_it_is() {
        let mut st = tracks_state(vec![track("Alpha", "Ann")]);
        for kind in [
            crate::app::state::TrackListKind::Album,
            crate::app::state::TrackListKind::LikedSongs,
            crate::app::state::TrackListKind::Playlist,
        ] {
            if let MainView::Tracks(list) = &mut st.main {
                list.kind = kind;
            }
            let lines = render(&mut st, 70, 12);
            assert!(
                lines[PATH].contains("MY LIST"),
                "{kind:?}: {:?}",
                lines[PATH]
            );
        }
    }

    fn search_state() -> AppState {
        let mut st = AppState::new();
        // All five tabs: four of them are Spotify's, and they are on the strip
        // only for a signed-in Premium account.
        st.spotify = crate::app::state::SpotifyState::Ready;
        st.main = MainView::Search(SearchResults {
            query: "muse".into(),
            tracks: vec![track("Starlight", "Muse")].into(),
            albums: vec![crate::app::state::AlbumItem {
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
                cover_url: None,
            }]
            .into(),
            artists: vec![crate::app::state::ArtistItem {
                id: "r1".into(),
                uri: "spotify:artist:r1".into(),
                name: "Muse".into(),
            }]
            .into(),
            playlists: vec![Playlist {
                id: "p1".into(),
                name: "Muse Mix".into(),
                track_count: 42,
                owner: "someone".into(),
                owner_id: "someone".into(),
                snapshot_id: "s1".into(),
                cover_url: None,
                public: None,
                collaborative: false,
            }]
            .into(),
            stations: vec![test_station("st1", "Radio Paradise")].into(),
            ..Default::default()
        });
        st
    }

    /// A station with enough on it to fill every column of the table.
    fn test_station(uuid: &str, name: &str) -> crate::app::state::Station {
        crate::app::state::Station {
            uuid: uuid.into(),
            name: name.into(),
            url: format!("http://example.test/{uuid}"),
            homepage: String::new(),
            tags: "eclectic,rock".into(),
            country: "United States".into(),
            countrycode: "US".into(),
            language: "english".into(),
            codec: "MP3".into(),
            bitrate: 128,
            votes: 900,
            hls: false,
        }
    }

    /// The Stations tab is the directory's own table, not a fifth layout: the
    /// same columns a radio page draws, so a station reads the same wherever
    /// you found it.
    #[test]
    fn the_stations_tab_draws_the_directorys_own_table() {
        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        let joined = render(&mut st, 90, 18).join("\n");
        for column in ["Station", "Tags", "Where", "Stream"] {
            assert!(joined.contains(column), "missing {column:?} in {joined}");
        }
        assert!(joined.contains("Radio Paradise"), "{joined}");
        assert!(joined.contains("MP3 128k"), "{joined}");
        assert!(joined.contains("1 results"), "{joined}");
    }

    /// The saved star and the playing note reach search results too, which is
    /// only true if `radio_favorites` and the playing station are threaded
    /// through `draw` to this tab.
    #[test]
    fn a_saved_or_playing_station_is_marked_in_search_results() {
        let station = test_station("st1", "Radio Paradise");

        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        st.radio_favorites = vec![station.clone()];
        let lines = render(&mut st, 90, 18);
        assert!(lines[7].contains("Saved"), "{:?}", lines[7]);
        let row = lines.iter().find(|l| l.contains("Radio Paradise")).unwrap();
        // The star has a column of its own now, off to the right of the name.
        let star = row.find(super::super::table::LIKED_MARK).unwrap();
        assert!(star > row.find("Radio Paradise").unwrap(), "{row:?}");

        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        st.radio = Some(crate::app::state::RadioPlayback {
            station,
            is_playing: true,
            started_at: std::time::Instant::now(),
            title: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            channels: Default::default(),
            volume_percent: 50,
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
        });
        let playing = render(&mut st, 90, 18).join("\n");
        assert!(playing.contains("♫ Radio Paradise"), "{playing}");
    }

    /// The saved page, with what each station is announcing.
    fn saved_state(stations: Vec<crate::app::state::Station>) -> AppState {
        use crate::app::state::{RadioRow, RadioScope, RadioView};

        let mut st = AppState::new();
        st.spotify = crate::app::state::SpotifyState::Ready;
        let mut view = RadioView::new(RadioScope::Favorites, 0);
        view.loading = false;
        view.rows = stations
            .iter()
            .cloned()
            .map(RadioRow::Station)
            .collect::<Vec<_>>()
            .into();
        st.radio_favorites = stations;
        st.main = MainView::Radio(view);
        st
    }

    #[test]
    fn the_saved_page_says_what_each_station_is_playing() {
        use crate::app::state::{NowStatus, StationNow};

        let mut st = saved_state(vec![
            test_station("st1", "Radio Paradise"),
            test_station("st2", "Silent FM"),
        ]);
        st.radio_now.insert(
            "st1".into(),
            StationNow::new(NowStatus::Title("Muse - Hysteria".into())),
        );
        st.radio_now
            .insert("st2".into(), StationNow::new(NowStatus::Unreachable));

        let lines = render(&mut st, 120, 18);
        let joined = lines.join("\n");
        assert!(joined.contains("Now Playing"), "{joined}");
        let paradise = lines.iter().find(|l| l.contains("Radio Paradise")).unwrap();
        assert!(paradise.contains("Muse - Hysteria"), "{paradise:?}");
        let silent = lines.iter().find(|l| l.contains("Silent FM")).unwrap();
        assert!(silent.contains("—"), "{silent:?}");
    }

    /// The station on the deck reads what the deck reads. Anything else would
    /// mean a second connection to a stream this machine already holds open.
    #[test]
    fn the_playing_station_reads_its_own_announcement() {
        let station = test_station("st1", "Radio Paradise");
        let mut st = saved_state(vec![station.clone()]);
        st.radio = Some(crate::app::state::RadioPlayback {
            station,
            is_playing: true,
            started_at: std::time::Instant::now(),
            title: std::sync::Arc::new(parking_lot::Mutex::new(Some(
                "Alela Diane - Take Us Back".into(),
            ))),
            channels: Default::default(),
            volume_percent: 50,
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
        });

        let joined = render(&mut st, 120, 18).join("\n");
        assert!(joined.contains("Alela Diane - Take Us Back"), "{joined}");
    }

    /// The column costs one connection per row, so it must not reach a page
    /// that is up to `radio::api::STATION_LIMIT` rows deep.
    #[test]
    fn only_the_saved_page_carries_the_now_playing_column() {
        use crate::app::state::{RadioScope, RadioView};

        let mut st = saved_state(vec![test_station("st1", "Radio Paradise")]);
        if let MainView::Radio(view) = &mut st.main {
            let rows = view.rows.clone();
            *view = RadioView::new(RadioScope::Popular, 0);
            view.loading = false;
            view.rows = rows;
        }
        let joined = render(&mut st, 120, 18).join("\n");
        assert!(!joined.contains("Now Playing"), "{joined}");

        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        let joined = render(&mut st, 120, 18).join("\n");
        assert!(!joined.contains("Now Playing"), "{joined}");
    }

    /// A directory that has not answered yet is not a directory that answered
    /// "nothing". The count band says which, so an empty tab mid-flight cannot
    /// be read as a failed search.
    #[test]
    fn a_pending_stations_tab_says_it_is_searching() {
        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        if let MainView::Search(r) = &mut st.main {
            r.stations = crate::app::state::SortedList::new();
            r.stations_loading = true;
        }
        let joined = render(&mut st, 90, 18).join("\n");
        assert!(joined.contains("searching…"), "{joined}");
        assert!(!joined.contains("0 results"), "{joined}");
        assert!(!joined.contains("no stations for"), "{joined}");
    }

    /// …and once it has answered, an empty tab says so plainly. Not "no
    /// stations results for", which the generic template would have produced.
    #[test]
    fn an_answered_empty_stations_tab_says_there_are_none() {
        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        if let MainView::Search(r) = &mut st.main {
            r.stations = crate::app::state::SortedList::new();
            r.stations_loading = false;
        }
        let joined = render(&mut st, 90, 18).join("\n");
        assert!(joined.contains("no stations for \"muse\""), "{joined}");
    }

    /// The tab strip must not move while the slower half lands: its labels are
    /// hit rects, and a label that grew or shrank mid-search would slide the
    /// other tabs out from under the pointer.
    #[test]
    fn the_tab_strip_does_not_move_while_the_stations_land() {
        let mut st = search_state();
        st.search_tab = SearchTab::Tracks;
        if let MainView::Search(r) = &mut st.main {
            r.stations = crate::app::state::SortedList::new();
            r.stations_loading = true;
        }
        render(&mut st, 90, 18);
        let pending = st.hit.search_tabs.clone();

        let mut st = search_state();
        st.search_tab = SearchTab::Tracks;
        render(&mut st, 90, 18);
        assert_eq!(pending, st.hit.search_tabs);
    }

    #[test]
    fn search_tab_hit_rects_match_rendered_labels() {
        let mut st = search_state();
        let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_eq!(st.hit.search_tabs.len(), SearchTab::ALL.len());
        for (rect, tab) in &st.hit.search_tabs {
            assert!(!rect.is_empty());
            let text: String = (rect.x..rect.right())
                .filter_map(|x| buffer.cell(Position { x, y: rect.y }).map(|c| c.symbol()))
                .collect();
            assert!(
                text.contains(tab.title()),
                "tab {tab:?} rect {rect:?} shows {text:?}"
            );
        }
    }

    #[test]
    fn album_tab_renders_columns() {
        let mut st = search_state();
        st.search_tab = SearchTab::Albums;
        let lines = render(&mut st, 90, 16);
        assert!(lines[7].contains("Album"));
        assert!(lines[7].contains("Artist"));
        assert!(lines[7].contains("Year"));
        assert!(lines[9].contains("Black Holes"));
        assert!(lines[9].contains("2006"));
        // The Artist cell is a link here too, one target per credited name.
        assert_eq!(st.hit.album_artist_links.len(), 1);
        let (rect, credit) = &st.hit.album_artist_links[0];
        assert_eq!(credit.id.as_deref(), Some("r1"));
        assert_eq!(rect.width, "Muse".len() as u16);
        assert_eq!(rect.y, 9, "not on the row that credits it");
    }

    /// Every table heads its columns, spaces the header off its rows, and
    /// keeps the same marker gutter clear at the left — which is what makes
    /// the pages line up with each other.
    #[test]
    fn every_browse_table_heads_its_columns_over_the_same_gutter() {
        let cases: [(SearchTab, &str, &str); 4] = [
            (SearchTab::Tracks, "Title", "Starlight"),
            (SearchTab::Albums, "Album", "Black Holes"),
            (SearchTab::Artists, "Artist", "Muse"),
            (SearchTab::Playlists, "Title", "Muse Mix"),
        ];
        let gutter = crate::ui::columns::PREFIX_W;
        for (tab, head, first) in cases {
            let mut st = search_state();
            st.search_tab = tab;
            let lines = render(&mut st, 90, 16);
            assert!(
                lines[7][..gutter].trim().is_empty() && lines[7].contains(head),
                "{tab:?} header: {:?}",
                lines[7]
            );
            assert!(lines[8].trim().is_empty(), "{tab:?} lost its spacer");
            assert!(
                lines[9][..gutter].trim().is_empty() && lines[9].contains(first),
                "{tab:?} first row: {:?}",
                lines[9]
            );
        }
    }

    /// The bug the old per-table offset chains existed to prevent: a click
    /// rect that disagrees with the glyphs it covers.
    ///
    /// Checked at several widths, because a rect and a label drift apart at
    /// the width where a column drops out from under one of them.
    #[test]
    fn a_header_hit_rect_covers_exactly_its_own_label() {
        for width in [34, 45, 60, 80, 120] {
            let mut st = tracks_state(vec![track("Starlight", "Muse")]);
            let lines = render(&mut st, width, 16);
            assert!(
                !st.hit.column_headers.is_empty(),
                "no headers recorded at width {width}"
            );
            let layout = Layout::resolve(
                &crate::ui::columns::tracks(false),
                (width - GUTTER) as usize,
                1,
            );
            for (rect, key) in &st.hit.column_headers {
                let printed: String = lines[rect.y as usize]
                    .chars()
                    .skip(rect.x as usize)
                    .take(rect.width as usize)
                    .collect();
                // The active column carries a ▲, which is part of the label
                // and so part of the target. A narrow column clips its label,
                // and the target clips with it.
                let marker = match *key == ColKey::No {
                    true => " ▲",
                    false => "",
                };
                let cell = layout.cell(*key).expect("a rect for a dropped column");
                if key.head().is_empty() {
                    // A column that heads itself with nothing takes its whole
                    // cell as the target — there is no label to aim at.
                    assert_eq!(rect.width as usize, cell.width, "{key:?} at width {width}");
                    continue;
                }
                let expected = fit(&format!("{}{marker}", key.head()), cell.width)
                    .trim_end()
                    .to_string();
                assert_eq!(printed, expected, "{key:?} at width {width}");
                assert!(!printed.is_empty() && !printed.starts_with(' '));
            }
        }
    }

    /// An album row's *name* is a link, the way the Album column of a track
    /// table is: one click opens it. A double-click or Enter is a way in that
    /// nothing on screen says.
    #[test]
    fn an_album_row_registers_its_name_as_a_click_target() {
        let mut st = search_state();
        st.search_tab = SearchTab::Albums;
        let lines = render(&mut st, 90, 16);
        let col = st.hit.main_album_col.clone();
        assert!(!col.rect.is_empty());
        // Covers the name column, past the marker gutter every table carries.
        assert_eq!(
            col.rect.x,
            st.hit.main_list.x + crate::ui::columns::PREFIX_W as u16
        );
        assert_eq!(col.rect.y, st.hit.main_list.y);
        let layout = Layout::resolve(&crate::ui::columns::albums(), 90 - GUTTER as usize, 0);
        assert_eq!(col.rect.width, layout.width_of(ColKey::Album) as u16);
        // Clipped to the one row that actually has an album on it.
        assert_eq!(col.rect.height, 1);
        let name: String = lines[col.rect.y as usize]
            .chars()
            .skip(col.rect.x as usize)
            .take(col.rect.width as usize)
            .collect();
        assert!(name.starts_with("Black Holes"), "{name:?}");
        // And the click stops with the name, not with the column.
        let at = |dx: u16| Position {
            x: col.rect.x + dx,
            y: col.rect.y,
        };
        let printed = "Black Holes".len() as u16;
        assert_eq!(col.widths, vec![printed]);
        assert!(col.hit(at(printed - 1)));
        assert!(!col.hit(at(printed)), "the padding after the name hits");
    }

    #[test]
    fn empty_search_tab_shows_message() {
        let mut st = search_state();
        if let MainView::Search(r) = &mut st.main {
            r.playlists = crate::app::state::SortedList::new();
        }
        st.search_tab = SearchTab::Playlists;
        let lines = render(&mut st, 60, 14);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("no playlists results for \"muse\""))
        );
    }

    fn album_item(name: &str, year: &str, cover: Option<&str>) -> crate::app::state::AlbumItem {
        grouped_item(name, year, cover, "album")
    }

    fn grouped_item(
        name: &str,
        year: &str,
        cover: Option<&str>,
        group: &str,
    ) -> crate::app::state::AlbumItem {
        crate::app::state::AlbumItem {
            id: format!("id-{name}"),
            name: name.into(),
            artists: "Muse".into(),
            credits: vec![Credit {
                name: "Muse".into(),
                id: Some("r1".into()),
            }],
            release_year: year.into(),
            album_type: group.into(),
            album_group: group.into(),
            track_count: 12,
            cover_url: cover.map(Into::into),
        }
    }

    fn artist_state() -> AppState {
        artist_state_with(vec![album_item("Black Holes", "2006", None)])
    }

    fn artist_state_with(albums: Vec<crate::app::state::AlbumItem>) -> AppState {
        let mut st = AppState::new();
        let mut top = crate::app::state::TrackList::new("Muse", "top tracks", None);
        top.append(vec![track("Uprising", "Muse")]);
        let mut v = crate::app::state::ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Muse".into(),
            image_url: Some("https://i.scdn.co/image/artist".into()),
            genres: vec!["alt rock".into(), "space rock".into()],
            top,
            albums: albums.into(),
            tab: crate::app::state::ArtistTab::Albums,
            loading: false,
            error: None,
        };
        v.retab();
        st.main = MainView::Artist(v);
        st
    }

    /// The artist page wears the album page's header band — portrait where the
    /// sleeve goes, name at the top of the column beside it, ▶ play at the
    /// bottom of it — and puts both sections in one scrolling body under it.
    #[test]
    fn artist_page_stacks_a_portrait_band_over_tracks_then_cards() {
        let mut st = artist_state();
        let lines = render(&mut st, 90, 30);
        assert!(lines[PATH].contains("MUSE"));
        // The photo occupies the left 20 cells of the ten band rows (a
        // placeholder swatch here: nothing is decoded in the test).
        let w = super::super::table::art_w(ART_H) as usize;
        for row in lines.iter().take(14).skip(4) {
            let block: String = row.chars().take(w).collect();
            assert!(
                block.chars().all(|c| c == '▀' || c == '♫'),
                "not a portrait row: {row:?}"
            );
        }
        assert!(lines[4].contains("Muse"));
        assert!(lines[5].contains("alt rock · space rock"));
        // The catalogue is counted; the top tracks are not — the list below
        // numbers itself.
        assert!(lines[6].contains("1 record"));
        assert!(!lines[6].contains("top track"));
        assert!(lines[8].contains("▶ play"));
        assert!(lines[8].contains("shuffle"));
        assert!(!st.hit.header_play_btn.is_empty());
        assert!(!st.hit.header_shuffle_btn.is_empty());

        // Body: the two sections, in order. Both headings keep a blank row
        // under them, and this catalogue is one group deep, so the album
        // strip stays away — see `the_album_strip_names_only_the_groups_the_artist_has`.
        assert!(lines[15].contains("Top Tracks"));
        assert!(lines[16].trim().is_empty());
        assert!(lines[17].contains("Title"));
        assert!(lines[18].trim().is_empty());
        assert!(lines[19].contains("Uprising"));
        assert!(lines[21].contains("Albums"));
        assert!(lines[22].trim().is_empty());
        assert!(lines[23].contains("Black Holes"));
        assert!(lines[24].contains("2006 · 12 tracks"));
        assert!(lines[25].contains("▶ play"));
        assert!(lines[25].contains("shuffle"));
        assert_eq!(st.hit.card_play.len(), 1);
        assert_eq!(st.hit.card_shuffle.len(), 1);
    }

    /// The top tracks read as a table: a blank under the heading, the column
    /// header, then the blank every other table on the browse screen keeps
    /// between its header and its rows.
    #[test]
    fn the_top_tracks_header_keeps_a_blank_above_the_rows() {
        let mut st = artist_state();
        let lines = render(&mut st, 90, 32);
        assert!(lines[17].contains("Title"), "{:?}", lines[17]);
        assert!(lines[18].trim().is_empty(), "{:?}", lines[18]);
        assert!(lines[19].contains("Uprising"), "{:?}", lines[19]);
    }

    /// A pane too narrow to seat both pills keeps ▶ play and drops shuffle —
    /// in the drawing and the hit rects alike.
    #[test]
    fn narrow_cards_drop_the_shuffle_pill() {
        let mut st = artist_state();
        let lines = render(&mut st, 14, 26);
        assert!(!st.hit.card_play.is_empty());
        assert!(st.hit.card_shuffle.is_empty());
        assert!(!lines.iter().any(|l| l.contains("shuffle")));
    }

    /// Cards are five lines apiece, so the pane keeps a line model and every
    /// line of a card resolves back to the same row. Without it a click lands
    /// several rows past whatever it was aimed at.
    #[test]
    fn album_cards_map_every_line_back_to_one_row() {
        let mut st = artist_state_with(vec![
            album_item("One", "2001", None),
            album_item("Two", "2002", None),
        ]);
        render(&mut st, 90, 32);
        let rows: Vec<Option<usize>> = st.hit.main_lines.clone();
        // Heading, blank, column header, blank, one track, blank, heading,
        // blank, then the cards: four lines each plus a blank.
        assert_eq!(
            &rows[..8],
            &[None, None, None, None, Some(0), None, None, None]
        );
        assert_eq!(&rows[8..13], &[Some(1), Some(1), Some(1), Some(1), None]);
        assert_eq!(&rows[13..18], &[Some(2), Some(2), Some(2), Some(2), None]);
        assert_eq!(st.hit.album_names.len(), 2);
        assert_eq!(st.hit.card_play.len(), 2);
        assert_eq!(st.hit.card_shuffle.len(), 2);
    }

    /// A catalogue in several groups gets a strip under the heading, and the
    /// strip offers only the groups the artist has records in — an empty tab
    /// is a dead end you can still walk into.
    #[test]
    fn the_album_strip_names_only_the_groups_the_artist_has() {
        use crate::app::state::ArtistTab;
        let mut st = artist_state_with(vec![
            grouped_item("Origin Of Symmetry", "2001", None, "album"),
            grouped_item("Hysteria", "2003", None, "single"),
            grouped_item("Live At Rome", "2013", None, "appears_on"),
        ]);
        let lines = render(&mut st, 90, 32);
        assert!(lines[21].contains("Albums"));
        assert!(lines[22].trim().is_empty());
        let strip = &lines[23];
        assert!(strip.contains("Albums"), "{strip:?}");
        assert!(strip.contains("Singles"), "{strip:?}");
        assert!(strip.contains("Appears On"), "{strip:?}");
        assert!(!strip.contains("Compilations"), "{strip:?}");
        assert!(lines[24].trim().is_empty());
        // Only the open group's cards, one blank row below the strip.
        assert!(lines[25].contains("Origin Of Symmetry"));
        assert!(!lines.iter().any(|l| l.contains("Hysteria")));
        assert_eq!(st.hit.card_play.len(), 1);
        assert_eq!(st.hit.card_shuffle.len(), 1);
        assert_eq!(
            st.hit
                .artist_tabs
                .iter()
                .map(|(_, t)| *t)
                .collect::<Vec<_>>(),
            vec![ArtistTab::Albums, ArtistTab::Singles, ArtistTab::AppearsOn]
        );
    }

    /// Every recorded tab rect sits over the label it belongs to, the way the
    /// search strip's do — a strip you cannot aim at is decoration.
    #[test]
    fn artist_tab_hit_rects_match_rendered_labels() {
        let mut st = artist_state_with(vec![
            album_item("Origin Of Symmetry", "2001", None),
            grouped_item("Hysteria", "2003", None, "single"),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(90, 32)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_eq!(st.hit.artist_tabs.len(), 2);
        for (rect, tab) in &st.hit.artist_tabs {
            assert!(!rect.is_empty());
            let text: String = (rect.x..rect.right())
                .filter_map(|x| buffer.cell(Position { x, y: rect.y }).map(|c| c.symbol()))
                .collect();
            assert!(
                text.contains(tab.title()),
                "tab {tab:?} rect {rect:?} shows {text:?}"
            );
        }
    }

    /// Switching the tab re-cuts the cards and the row model with them, so a
    /// click on the second tab's first card cannot land on the first tab's.
    #[test]
    fn switching_the_album_tab_re_cuts_the_cards() {
        use crate::app::state::{ArtistRow, ArtistTab};
        let mut st = artist_state_with(vec![
            album_item("Origin Of Symmetry", "2001", None),
            grouped_item("Hysteria", "2003", None, "single"),
        ]);
        if let MainView::Artist(v) = &mut st.main {
            v.set_tab(ArtistTab::Singles);
        }
        let lines = render(&mut st, 90, 32);
        assert!(lines.iter().any(|l| l.contains("Hysteria")));
        assert!(!lines.iter().any(|l| l.contains("Origin Of Symmetry")));
        let MainView::Artist(v) = &st.main else {
            unreachable!()
        };
        // One top track, so the first card is row 1 whichever tab is open.
        assert_eq!(v.len(), 2);
        let Some(ArtistRow::Album(a)) = v.row(1) else {
            panic!("row 1 is not a card")
        };
        assert_eq!(a.name, "Hysteria");
    }

    /// The sleeve is shed before the text is, on a card as on a header band.
    #[test]
    fn narrow_cards_drop_their_sleeve_before_their_name() {
        let mut st = artist_state();
        let lines = render(&mut st, 30, 26);
        assert!(lines.iter().any(|l| l.contains("Black Holes")));
        assert!(
            !lines.iter().any(|l| l.contains('▀')),
            "art survived a narrow pane: {lines:#?}"
        );
    }

    /// Hovering a link lights the run itself and nothing else. Every link on
    /// these pages is a cell padded out to its column width, so an underline
    /// would draw a rule from an album card's name clear across the pane.
    #[test]
    fn hovering_a_link_lights_the_text_and_not_its_padding() {
        use ratatui::style::Color;

        let mut st = artist_state();
        // Draw once to learn where the first card's name landed.
        render(&mut st, 90, 26);
        let (rect, row) = st.hit.album_names[0];
        assert_eq!(row, 1, "the card is the row after the one top track");
        assert_eq!(rect.width, 11, "the link is \"Black Holes\" and no wider");
        let name = Position {
            x: rect.x,
            y: rect.y,
        };
        st.mouse_pos = Some(name);
        let mut terminal = Terminal::new(TestBackend::new(90, 26)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let lit = |x: u16| {
            buf.cell(Position { x, y: name.y }).unwrap().bg == Color::Rgb(0x58, 0x61, 0x66)
        };
        // "Black Holes" is 11 cells; the pill covers exactly those.
        assert!(lit(name.x), "the name is not lit");
        assert!(lit(name.x + 10), "the pill stops short of the name");
        assert!(
            !lit(name.x + 11),
            "the pill ran past the name into its padding"
        );
        // And nothing is underlined any more, here or anywhere else.
        for y in 0..24 {
            for x in 0..90 {
                let cell = buf.cell(Position { x, y }).unwrap();
                assert!(
                    !cell.modifier.contains(Modifier::UNDERLINED),
                    "{:?} at {x},{y} is underlined",
                    cell.symbol()
                );
            }
        }
    }

    /// Hover styling is resolved against rects recorded during the same
    /// frame, and the artist page's are the fiddliest on the screen: a
    /// pointer anywhere — including above the pane, on a heading, between
    /// cards — has to resolve or miss, never underflow a row index.
    #[test]
    fn a_pointer_anywhere_on_the_artist_page_resolves_or_misses() {
        for y in 0..24u16 {
            for x in 0..90u16 {
                let mut st = artist_state();
                if let MainView::Artist(v) = &mut st.main {
                    v.albums = (0..4)
                        .map(|i| album_item(&format!("Record {i}"), "2006", None))
                        .collect();
                }
                st.mouse_pos = Some(Position { x, y });
                render(&mut st, 90, 26);
            }
        }
    }

    #[test]
    fn short_pane_degrades_without_panicking() {
        for height in 0..12 {
            let mut st = tracks_state(vec![track("A", "B")]);
            render(&mut st, 80, height);
            let mut st = search_state();
            render(&mut st, 80, height);
            let mut st = tracks_state(Vec::new());
            render(&mut st, 80, height);
            let mut st = artist_state();
            render(&mut st, 80, height);
        }
    }
}
