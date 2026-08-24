use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::theme;
use crate::app::state::{
    AppState, Crumb, CrumbTarget, HitAreas, HomeItem, LoadError, MainView, SearchTab, SortKey,
    Track, TrackSort, format_duration,
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
    // The station playing, if one is, so a radio page can mark its row the way
    // a track table marks the playing track.
    let playing_station = state.radio.as_ref().map(|r| r.station.url.clone());
    // Home's rows and their tails, resolved before the split borrow below —
    // both read `playlists`, which the borrow takes.
    let home: Vec<(HomeItem, String, &'static str)> = state
        .home_items()
        .into_iter()
        .map(|item| (item, state.home_count(item), state.home_blurb(item)))
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
        radio_favorites,
        main_list,
        hit,
        liked,
        view_cover,
        page_art,
        ..
    } = state;
    let liked = &*liked;
    let radio_favorites = &*radio_favorites;
    let page_art = &*page_art;
    let playlists = &*playlists;
    let playlists_error = &*playlists_error;
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
            frame, list_area, list, view_cover, loading, main_index, main_list, retries, hit,
            &marks, liked, mouse,
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
#[allow(clippy::too_many_arguments)]
fn draw_home(
    frame: &mut Frame,
    area: Rect,
    items: &[(HomeItem, String, &'static str)],
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

    let cols = PlaylistCols::new(inner.width as usize);
    let mut rows_area = inner;
    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new(playlist_header(&cols)),
            Rect { height: 1, ..inner },
        );
        let skip = if inner.height >= 3 { 2 } else { 1 };
        rows_area = Rect {
            y: inner.y + skip,
            height: inner.height - skip,
            ..inner
        };
    }
    hit.main_list = rows_area;

    // Playlists only. Liked Songs is a Home row of its own: it is not a
    // playlist, so it does not belong under a heading that says it is.
    let items: Vec<ListItem> = playlists
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let playing =
                marks.context.as_deref() == Some(crate::app::state::playlist_key(&p.id).as_str());
            playlist_row(p, &cols, me_id, playing, i == main_index)
        })
        .collect();
    let count = items.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// Width of the station table's saved column: the mark, then a space.
const STATION_SAVE_W: usize = 2;
/// Width of the right-aligned quality cell ("AAC+ 128k", "HLS").
const STATION_QUALITY_W: usize = 10;
/// Width of the facet list's right-aligned station count.
const FACET_COUNT_W: usize = 7;

/// Column widths for the station table, from the pane's inner width. Tags are
/// the first thing dropped: they are the only column a station reads fine
/// without.
struct StationCols {
    name: usize,
    tags: usize,
    country: usize,
}

impl StationCols {
    fn new(width: usize) -> Self {
        let fixed = STATION_SAVE_W + STATION_QUALITY_W + 3 * COL_GAP.len();
        let flex = width.saturating_sub(fixed);
        // Below this the tag list is a few clipped letters saying nothing, so
        // the name takes the space instead.
        if flex < 44 {
            return Self {
                name: flex.saturating_sub(6),
                tags: 0,
                country: 6.min(flex),
            };
        }
        let name = flex * 5 / 10;
        let country = 6;
        Self {
            name,
            tags: flex.saturating_sub(name + country),
            country,
        }
    }
}

fn station_header(cols: &StationCols) -> Line<'static> {
    let mut text = format!(
        "{}{}{COL_GAP}",
        " ".repeat(STATION_SAVE_W),
        fit("Station", cols.name)
    );
    if cols.tags > 0 {
        text.push_str(&fit("Tags", cols.tags));
        text.push_str(COL_GAP);
    }
    text.push_str(&fit("Where", cols.country));
    text.push_str(COL_GAP);
    text.push_str(&format!("{:>STATION_QUALITY_W$}", "Stream"));
    Line::styled(text, theme::dim())
}

/// One station row.
///
/// The saved mark reuses the track table's `★`, and for the same reason: this
/// is the same gesture on the same key, and a second glyph for it would say
/// there were two kinds of keeping.
fn station_row(
    s: &crate::app::state::Station,
    cols: &StationCols,
    saved: bool,
    playing: bool,
    selected: bool,
) -> ListItem<'static> {
    let mark = if playing {
        Span::styled("♫ ", theme::accent())
    } else if saved {
        Span::styled(format!("{} ", super::table::LIKED_MARK), theme::accent())
    } else {
        Span::raw("  ")
    };
    // An HLS station is listed but cannot be played, so it is drawn as
    // something that is there rather than something that is offered. Hiding
    // them would quietly remove the BBC and most national broadcasters from
    // the directory, which is a worse lie than a dim row.
    let name_style = if s.hls { theme::dim() } else { theme::text() };

    let mut spans = vec![mark, Span::styled(fit(&s.name, cols.name), name_style)];
    spans.push(Span::raw(COL_GAP));
    if cols.tags > 0 {
        spans.push(Span::styled(fit(&s.tags, cols.tags), theme::dim()));
        spans.push(Span::raw(COL_GAP));
    }
    let where_ = if s.countrycode.is_empty() {
        s.country.as_str()
    } else {
        s.countrycode.as_str()
    };
    spans.push(Span::styled(fit(where_, cols.country), theme::dim()));
    spans.push(Span::raw(COL_GAP));
    // Right-aligned against the header, which is too — `fit` pads on the
    // right, so it would left-align the cell under a right-aligned label.
    let quality = fit(&s.quality(), STATION_QUALITY_W).trim_end().to_string();
    spans.push(Span::styled(
        format!("{quality:>STATION_QUALITY_W$}"),
        theme::dim(),
    ));

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
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
) {
    let cols = StationCols::new(body.width as usize);
    let mut rows_area = body;
    if body.height >= 2 {
        frame.render_widget(
            Paragraph::new(station_header(&cols)),
            Rect { height: 1, ..body },
        );
        let skip = if body.height >= 3 { 2 } else { 1 };
        rows_area = Rect {
            y: body.y + skip,
            height: body.height - skip,
            ..body
        };
    }
    hit.main_list = rows_area;

    let items: Vec<ListItem> = stations
        .iter()
        .enumerate()
        .map(|(i, s)| {
            station_row(
                s,
                &cols,
                favorites.iter().any(|f| f.uuid == s.uuid),
                playing_url == Some(s.url.as_str()),
                i == main_index,
            )
        })
        .collect();
    let count = items.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// One country or genre row: a name and how many stations are behind it.
fn facet_row(label: &str, count: u32, width: usize, selected: bool) -> ListItem<'static> {
    let name_w = width.saturating_sub(STATION_SAVE_W + FACET_COUNT_W + COL_GAP.len());
    let mut line = Line::from(vec![
        Span::raw(" ".repeat(STATION_SAVE_W)),
        Span::styled(fit(label, name_w), theme::text()),
        Span::raw(COL_GAP),
        Span::styled(format!("{count:>FACET_COUNT_W$}"), theme::dim()),
    ]);
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
    main_index: usize,
    list_state: &mut ListState,
    retries: u32,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) {
    use crate::app::state::{RadioRow, RadioTab};

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
    // it goes through the same builder. Facet lists get no header row:
    // "Country / Stations" over a list of countries says only what the tab
    // above it already said.
    if !facets {
        let stations: Vec<&crate::app::state::Station> = view
            .rows
            .iter()
            .filter_map(|r| match r {
                RadioRow::Station(s) => Some(s),
                RadioRow::Facet { .. } => None,
            })
            .collect();
        render_station_table(
            frame,
            body,
            &stations,
            favorites,
            playing_url,
            main_index,
            list_state,
            hit,
        );
        return;
    }

    hit.main_list = body;
    let items: Vec<ListItem> = view
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            RadioRow::Facet { label, count, .. } => {
                facet_row(label, *count, body.width as usize, i == main_index)
            }
            // Unreachable: `facets` is read off the first row, and the
            // directory never mixes the two kinds in one answer.
            RadioRow::Station(s) => facet_row(&s.name, 0, body.width as usize, i == main_index),
        })
        .collect();
    let count = items.len();
    super::clamp_offset(list_state, count, body.height as usize);
    frame.render_stateful_widget(List::new(items), body, list_state);
    super::table::draw_scrollbar(frame, scroll_col(body), count, list_state.offset());
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
/// The shuffle control beside every ▶ play. The bare word, no glyph: the
/// deck already names the mode in words, and the common shuffle glyphs are
/// emoji- or ambiguous-width, which would drift the recorded hit rect.
const SHUFFLE_PILL: &str = "shuffle";

/// Narrowest text column that seats both card pills with their gap; below it
/// the card keeps ▶ play alone.
fn card_pills_min_w() -> usize {
    super::table::width(PLAY_PILL) + 2 + super::table::width(SHUFFLE_PILL)
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
    spans.push(Span::raw("  "));
    *x = x.saturating_add(2);
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
    let split = v.top.display.len();
    let cols = TrackCols::new(width, split as u32);

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
        plan.extend((0..split).map(ArtistLine::Track));
        plan.push(ArtistLine::Blank);
    }
    let tracks_at = (split > 0).then_some(3usize);
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
        for album in 0..v.display.len() {
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
    for (album, a) in v.display.iter().map(|&i| &v.albums[i]).enumerate() {
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
                (super::table::width(PLAY_PILL) + 2) as u16,
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
            let rect = |off: usize, w: usize| {
                Rect {
                    x: body.x.saturating_add(off as u16),
                    y: body.y + (visible.0 - offset) as u16,
                    width: w as u16,
                    height: (visible.1 - visible.0) as u16,
                }
                .intersection(body)
            };
            if cols.actions {
                hit.main_like_col = rect(cols.like_offset(), LIKE_W);
                hit.main_add_col = rect(cols.add_offset(), ADD_W);
            }
            hit.main_artist_col = rect(cols.artist_offset(), cols.artist);
            if let Some(off) = cols.album_offset() {
                hit.main_album_col = rect(off, cols.album);
            }
            // The column, then the row: both rects sit inside the track
            // block, so the row arithmetic is only safe once one of them has
            // claimed the pointer.
            hover_cell = mouse
                .and_then(|m| {
                    if hit.main_like_col.contains(m) {
                        Some((m, HoverCol::Like))
                    } else if hit.main_add_col.contains(m) {
                        Some((m, HoverCol::Add))
                    } else if hit.main_artist_col.contains(m) {
                        Some((m, HoverCol::Artist))
                    } else if hit.main_album_col.contains(m) {
                        Some((m, HoverCol::Album))
                    } else {
                        None
                    }
                })
                .map(|(m, col)| (offset + (m.y - body.y) as usize - first, col));
        }
    }

    // Row positions are in display order; the playing marker resolves by URI.
    let playing = marks.uri.as_deref().and_then(|uri| {
        v.top
            .display
            .iter()
            .position(|&ti| v.top.tracks[ti].uri == uri)
    });
    let items: Vec<ListItem> = plan
        .iter()
        .map(|line| match *line {
            ArtistLine::Heading(text) => ListItem::new(Line::styled(
                text.to_string(),
                theme::text().add_modifier(Modifier::BOLD),
            )),
            ArtistLine::TrackHeader => ListItem::new(track_header(&cols, None)),
            ArtistLine::Track(i) => {
                let ti = v.top.display[i];
                track_row(
                    &v.top.tracks[ti],
                    &cols,
                    i as u32 + 1,
                    if Some(i) == playing {
                        RowMark::Playing
                    } else {
                        RowMark::None
                    },
                    i == main_index,
                    liked.get(&v.top.tracks[ti].uri).copied(),
                    hover_cell.and_then(|(row, col)| (row == i).then_some(col)),
                )
            }
            ArtistLine::Tabs => ListItem::new(Line::from(tab_spans.clone())),
            ArtistLine::Card { album, row } => card_line(
                &v.albums[v.display[album]],
                row,
                indent as usize,
                card_text_w,
                split + album == main_index,
                hover_album == Some(album),
                hover_play == Some(album),
                hover_shuffle == Some(album),
            ),
            ArtistLine::Blank => ListItem::new(Line::default()),
        })
        .collect();
    frame.render_stateful_widget(List::new(items), body, list_state);

    // The sleeves go on last: they are painted cells, not list rows, so they
    // are drawn over the block the rows left blank for them, clipped to the
    // body so a card scrolling off the top slides under it.
    if art_w > 0 {
        for (album, a) in v.display.iter().map(|&i| &v.albums[i]).enumerate() {
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
                spans.push(Span::raw("  "));
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

/// Rows an album's sleeve spans in the header band, and the narrowest text
/// column worth keeping it for. Wider than the bottom bar's thumbnail because
/// this is the one view that is *about* a record rather than merely reporting
/// which one is playing.
const ART_H: u16 = 6;
const ART_GAP: u16 = 3;
const MIN_META_W: u16 = 40;
/// Rows the band spends: the sleeve, then a blank spacer under it. Without a
/// cover the text alone is three rows, as it always was.
const ART_BAND_H: u16 = ART_H + 1;
const TEXT_BAND_H: u16 = 3;
/// Rows the track table must still get for a sleeve to be worth drawing: its
/// column header, a spacer, and enough rows to be a list. A seven-row band
/// over a three-row table is a worse screen than no sleeve at all — the same
/// judgement `player::Rows::MIN_ART_QUEUE` makes about its queue.
const MIN_TABLE_H: u16 = 6;

/// Summary band above a track table: the record's own sleeve when it has one,
/// then name + subtitle + totals, a clickable ▶ play for the whole context,
/// and a passive sort indicator. Returns the area left for the table.
///
/// Skipped entirely on short panes, and the sleeve is shed before the text is
/// — the same order the player sheds its own cover in.
fn header_band(
    frame: &mut Frame,
    inner: Rect,
    list: &crate::app::state::TrackList,
    cover: Option<&crate::cover::Cover>,
    loading: bool,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) -> Rect {
    if inner.height < 8 {
        return inner;
    }
    let gray = theme::text();
    let dim = theme::dim();
    let accent = theme::accent();

    // The sleeve, when this is an album, it has one, and there are rows and
    // cells to spare. A playlist's mosaic is not a record cover, so playlists
    // and Liked Songs never take this branch.
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

    // Row 1: name + subtitle on the left, totals right-aligned.
    let info_area = Rect { height: 1, ..text };
    let total_ms: u64 = list.tracks.iter().map(|t| t.duration_ms).sum();
    let count = list.tracks.len();
    let dur = format_total_duration(total_ms);
    let totals = Span::styled(
        match (loading, list.total) {
            (true, Some(total)) => format!("{count} of {total} tracks · {dur}"),
            (true, None) => format!("{count} tracks · {dur}+"),
            (false, _) => format!("{count} tracks · {dur}"),
        },
        dim,
    );
    // Beside a sleeve the totals get their own row: the column is narrower
    // there, and a name squeezed against a right-aligned count reads as two
    // things fighting rather than as a heading.
    let stacked = art.is_some();
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
    if stacked {
        frame.render_widget(
            Paragraph::new(Line::styled(list.header.subtitle.clone(), gray)),
            Rect {
                y: text.y + 1,
                height: 1,
                ..text
            },
        );
        frame.render_widget(
            Paragraph::new(Line::from(totals)),
            Rect {
                y: text.y + 2,
                height: 1,
                ..text
            },
        );
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

    // The ▶ play pill, with the active sort as a passive hint opposite it.
    // Beside a sleeve it sits low in the block, so the metadata above it and
    // the control below read as two groups rather than one list.
    let play_area = Rect {
        y: text.y + if stacked { 4 } else { 1 },
        height: 1,
        ..text
    };
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
    frame.render_widget(Paragraph::new(Line::from(spans)), play_area);
    if list.sort.key != SortKey::Position {
        let hint = Span::styled(
            format!(
                "sort: {} {}  (o/O)",
                list.sort.key.label(),
                if list.sort.ascending { "▲" } else { "▼" }
            ),
            dim,
        );
        let w = (hint.width() as u16).min(play_area.width);
        let hint_area = Rect {
            x: play_area.right().saturating_sub(w),
            width: w,
            ..play_area
        };
        frame.render_widget(Paragraph::new(Line::from(hint)), hint_area);
    }

    let used = if stacked { ART_BAND_H } else { TEXT_BAND_H };
    Rect {
        y: inner.y + used,
        height: inner.height - used,
        ..inner
    }
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

/// Reserved at the right of the pane: a blank column, then the scrollbar. With
/// no border to hang the bar on it needs columns of its own, kept outside the
/// content rect so a click on it cannot resolve to a row.
///
/// Two rather than one because the last column of a track row is the
/// right-aligned Time, and a duration flush against the scrollbar reads as one
/// mark rather than two.
const GUTTER: u16 = 2;

/// The scrollbar's column for a content rect: the far side of the gutter.
fn scroll_col(body: Rect) -> Rect {
    Rect {
        x: body.right() + GUTTER - 1,
        y: body.y,
        width: 1,
        height: body.height,
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

const DUR_W: usize = 5;
const YEAR_W: usize = 4;
const COL_GAP: &str = "   ";
/// Leading marker column: "▶ " playing, "→ " next up.
const PREFIX_W: usize = 2;
use super::table::{ACTIONS_MIN, ACTIONS_W, ADD_W, LIKE_W, action_spans, actions_label};

/// Minimum width of the track-number column, right-aligned.
const NO_W: usize = 3;

/// Column widths for the track table, derived from the pane's inner width.
/// Narrow panes drop the year first, then the album, then the track number;
/// the action pair outlives all three.
struct TrackCols {
    name: usize,
    artist: usize,
    /// 0 = hidden.
    album: usize,
    year: bool,
    /// The `★ +` pair at the end of the row.
    actions: bool,
    track_no: bool,
    /// Width of the number column, grown to fit the largest number.
    no_w: usize,
}

impl TrackCols {
    fn new(width: usize, max_no: u32) -> Self {
        let year = width >= 70;
        let show_album = width >= 50;
        let actions = width >= ACTIONS_MIN;
        let track_no = width >= 40;
        let no_w = max_no.to_string().len().max(NO_W);
        let mut flex = width.saturating_sub(PREFIX_W + DUR_W + COL_GAP.len());
        if actions {
            flex = flex.saturating_sub(ACTIONS_W + COL_GAP.len());
        }
        if track_no {
            flex = flex.saturating_sub(no_w + COL_GAP.len());
        }
        if year {
            flex = flex.saturating_sub(YEAR_W + COL_GAP.len());
        }
        if show_album {
            let flex = flex.saturating_sub(2 * COL_GAP.len());
            let name = flex * 4 / 10;
            let artist = flex * 3 / 10;
            Self {
                name,
                artist,
                album: flex - name - artist,
                year,
                actions,
                track_no,
                no_w,
            }
        } else {
            let flex = flex.saturating_sub(COL_GAP.len());
            let name = flex * 6 / 10;
            Self {
                name,
                artist: flex - name,
                album: 0,
                year,
                actions,
                track_no,
                no_w,
            }
        }
    }

    /// Column offset (from the row start) of the title cell, and the head of
    /// the offset chain below.
    ///
    /// Each offset is built on the one before it rather than repeating its
    /// arithmetic: a click rect that disagrees with the glyph it covers is the
    /// bug this chain exists to prevent.
    fn title_offset(&self) -> usize {
        let mut x = PREFIX_W;
        if self.track_no {
            x += self.no_w + COL_GAP.len();
        }
        x
    }

    /// Column offset (from the row start) of the artist cell.
    fn artist_offset(&self) -> usize {
        self.title_offset() + self.name + COL_GAP.len()
    }

    /// Column offset of the album cell, when the column is shown.
    fn album_offset(&self) -> Option<usize> {
        (self.album > 0).then(|| self.artist_offset() + self.artist + COL_GAP.len())
    }

    /// Column offset of the `★ +` pair, shown or not: past whichever of album
    /// and artist ends the data columns, then past the year.
    fn actions_offset(&self) -> usize {
        let mut x = self
            .album_offset()
            .map_or(self.artist_offset() + self.artist, |off| off + self.album);
        if self.year {
            x += COL_GAP.len() + YEAR_W;
        }
        x + COL_GAP.len()
    }

    /// Column offset of the liked cell, the first of the pair.
    fn like_offset(&self) -> usize {
        self.actions_offset()
    }

    /// Column offset of the add cell, flush against the liked one: each cell
    /// carries its own padding, so the two targets meet.
    fn add_offset(&self) -> usize {
        self.actions_offset() + LIKE_W
    }
}

/// Which clickable cell of a track row the mouse is over.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverCol {
    Like,
    Add,
    Artist,
    Album,
}

use super::table::fit;

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

/// Column label with a ▲/▼ marker when it is the active sort column.
fn sort_label(base: &str, key: SortKey, sort: Option<TrackSort>) -> String {
    match sort {
        Some(s) if s.key == key => {
            format!("{base}{}", if s.ascending { "▲" } else { "▼" })
        }
        _ => base.to_string(),
    }
}

fn track_header(cols: &TrackCols, sort: Option<TrackSort>) -> Line<'static> {
    let mut text = " ".repeat(PREFIX_W);
    if cols.track_no {
        text = format!("{text}{:>w$}{COL_GAP}", "#", w = cols.no_w);
    }
    text = format!(
        "{text}{}{COL_GAP}{}",
        fit(&sort_label("Title", SortKey::Title, sort), cols.name),
        fit(&sort_label("Artist", SortKey::Artist, sort), cols.artist)
    );
    if cols.album > 0 {
        text = format!(
            "{text}{COL_GAP}{}",
            fit(&sort_label("Album", SortKey::Album, sort), cols.album)
        );
    }
    if cols.year {
        text = format!("{text}{COL_GAP}{}", fit("Year", YEAR_W));
    }
    if cols.actions {
        text = format!("{text}{COL_GAP}{}", actions_label());
    }
    text = format!(
        "{text}{COL_GAP}{:>DUR_W$}",
        sort_label("Time", SortKey::Duration, sort)
    );
    Line::styled(text, theme::dim())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowMark {
    None,
    Playing,
}

fn track_row(
    t: &Track,
    cols: &TrackCols,
    no: u32,
    mark: RowMark,
    selected: bool,
    liked: Option<bool>,
    hover: Option<HoverCol>,
) -> ListItem<'static> {
    let dim = theme::dim();
    let accent_bold = theme::accent().add_modifier(Modifier::BOLD);
    let prefix = match mark {
        RowMark::None => Span::raw(" ".repeat(PREFIX_W)),
        RowMark::Playing => Span::styled("▶ ", accent_bold),
    };
    // Three weights, the way the player queue does it: the title at TEXT,
    // everything supporting it at DIM, and the playing row in accent.
    // `Style::default()` here would leak the raw terminal foreground, the one
    // unthemed colour on the page.
    let name_style = if mark == RowMark::Playing {
        accent_bold
    } else {
        theme::text()
    };
    let mut spans = vec![prefix];
    // Where the star lands, so the selection restyle below can put its accent
    // back.
    let mut star_at = None;
    if cols.track_no {
        let no = if no > 0 {
            no.to_string()
        } else {
            String::new()
        };
        spans.push(Span::styled(
            format!("{no:>w$}{COL_GAP}", w = cols.no_w),
            dim,
        ));
    }
    spans.push(Span::styled(fit(&t.name, cols.name), name_style));
    spans.push(Span::raw(COL_GAP));
    // Artist and album cells are clickable; hovering lights them.
    spans.extend(cell_spans(
        &t.artists,
        cols.artist,
        dim,
        hover == Some(HoverCol::Artist),
    ));
    if cols.album > 0 {
        spans.push(Span::raw(COL_GAP));
        spans.extend(cell_spans(
            &t.album,
            cols.album,
            dim,
            hover == Some(HoverCol::Album),
        ));
    }
    if cols.year {
        spans.push(Span::raw(COL_GAP));
        spans.push(Span::styled(fit(&t.release_year, YEAR_W), dim));
    }
    if cols.actions {
        spans.push(Span::raw(COL_GAP));
        star_at = Some(spans.len());
        spans.extend(action_spans(
            liked,
            hover == Some(HoverCol::Like),
            hover == Some(HoverCol::Add),
        ));
    }
    spans.push(Span::raw(COL_GAP));
    spans.push(Span::styled(
        format!("{:>DUR_W$}", format_duration(t.duration_ms)),
        dim,
    ));
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

const TYPE_W: usize = 11;
const COUNT_W: usize = 6;

/// Column widths for the search Albums tab.
struct AlbumCols {
    name: usize,
    artist: usize,
    /// Show the year + type columns.
    meta: bool,
}

impl AlbumCols {
    fn new(width: usize) -> Self {
        let meta = width >= 55;
        let mut flex = width.saturating_sub(COL_GAP.len());
        if meta {
            flex = flex.saturating_sub(YEAR_W + TYPE_W + 2 * COL_GAP.len());
        }
        let name = flex / 2;
        Self {
            name,
            artist: flex.saturating_sub(name),
            meta,
        }
    }
}

fn album_header(cols: &AlbumCols) -> Line<'static> {
    let mut text = format!(
        "{}{COL_GAP}{}",
        fit("Album", cols.name),
        fit("Artist", cols.artist)
    );
    if cols.meta {
        text = format!(
            "{text}{COL_GAP}{}{COL_GAP}{}",
            fit("Year", YEAR_W),
            fit("Type", TYPE_W)
        );
    }
    Line::styled(text, theme::dim())
}

fn album_row(
    a: &crate::app::state::AlbumItem,
    cols: &AlbumCols,
    selected: bool,
    hovered: bool,
) -> ListItem<'static> {
    let dim = theme::dim();
    // The name is the link, so it lights under the pointer — the same
    // affordance `track_row` gives the album cell of a track table.
    let mut spans = cell_spans(&a.name, cols.name, theme::text(), hovered);
    spans.push(Span::raw(COL_GAP));
    spans.push(Span::styled(fit(&a.artists, cols.artist), dim));
    if cols.meta {
        spans.push(Span::raw(COL_GAP));
        spans.push(Span::styled(fit(&a.release_year, YEAR_W), dim));
        spans.push(Span::raw(COL_GAP));
        spans.push(Span::styled(fit(&a.album_type, TYPE_W), dim));
    }
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// Column widths for a playlist table: the Playlists page, and the search
/// view's Playlists tab.
struct PlaylistCols {
    name: usize,
    owner: usize,
}

/// Leading marker column: `♫ ` on the playing context, blank otherwise.
const PL_MARK_W: usize = 2;

impl PlaylistCols {
    fn new(width: usize) -> Self {
        let flex = width.saturating_sub(PL_MARK_W + COUNT_W + 2 * COL_GAP.len());
        let name = flex * 6 / 10;
        Self {
            name,
            owner: flex.saturating_sub(name),
        }
    }
}

fn playlist_header(cols: &PlaylistCols) -> Line<'static> {
    let text = format!(
        "{}{}{COL_GAP}{}{COL_GAP}{:>COUNT_W$}",
        " ".repeat(PL_MARK_W),
        fit("Title", cols.name),
        fit("Owner", cols.owner),
        "Tracks"
    );
    Line::styled(text, theme::dim())
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
    cols: &PlaylistCols,
    me_id: Option<&str>,
    playing: bool,
    selected: bool,
) -> ListItem<'static> {
    let owner = match me_id {
        Some(me) if me == p.owner_id => "",
        _ => p.owner.as_str(),
    };
    let mut line = Line::from(vec![
        Span::styled(if playing { "♫ " } else { "  " }, theme::accent()),
        Span::styled(fit(&p.name, cols.name), theme::text()),
        Span::raw(COL_GAP),
        Span::styled(fit(owner, cols.owner), theme::dim()),
        Span::raw(COL_GAP),
        Span::styled(format!("{:>COUNT_W$}", p.track_count), theme::dim()),
    ]);
    if selected {
        super::table::apply_selection(&mut line);
    }
    ListItem::new(line)
}

/// Render the album table (header + scrollable rows) inside `inner`.
fn render_album_table(
    frame: &mut Frame,
    inner: Rect,
    albums: &[crate::app::state::AlbumItem],
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    mouse: Option<Position>,
) {
    let cols = AlbumCols::new(inner.width as usize);
    let mut rows_area = inner;
    if inner.height >= 2 && !albums.is_empty() {
        frame.render_widget(
            Paragraph::new(album_header(&cols)),
            Rect { height: 1, ..inner },
        );
        // Blank spacer row under the header when there is room for it.
        let skip = if inner.height >= 3 { 2 } else { 1 };
        rows_area = Rect {
            y: inner.y + skip,
            height: inner.height - skip,
            ..inner
        };
    }
    hit.main_list = rows_area;

    // The album name is a link here the same way the Album *column* is one in
    // a track table: single click opens the album. The rect covers the name
    // column only, clipped to the rows actually filled.
    let filled_rows =
        (albums.len().saturating_sub(list_state.offset()) as u16).min(rows_area.height);
    hit.main_album_col = Rect {
        x: rows_area.x,
        y: rows_area.y,
        width: cols.name as u16,
        height: filled_rows,
    }
    .intersection(rows_area);
    let hover_row = mouse
        .filter(|m| hit.main_album_col.contains(*m))
        .map(|m| list_state.offset() + (m.y - rows_area.y) as usize);

    let items: Vec<ListItem> = albums
        .iter()
        .enumerate()
        .map(|(i, a)| album_row(a, &cols, i == main_index, hover_row == Some(i)))
        .collect();
    let count = items.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// Render the track table (header row + scrollable rows) inside `inner`,
/// the area within a pane's borders.
#[allow(clippy::too_many_arguments)]
fn render_track_table(
    frame: &mut Frame,
    inner: Rect,
    tracks: &[Track],
    display: &[usize],
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    sort: Option<TrackSort>,
    liked: &std::collections::HashMap<String, bool>,
    show_track_no: bool,
    mouse: Option<Position>,
) {
    let cols = TrackCols::new(inner.width as usize, display.len() as u32);
    let mut rows_area = inner;
    if inner.height >= 2 && !display.is_empty() {
        let header_area = Rect { height: 1, ..inner };
        // Blank spacer row under the header when there is room for it.
        let skip = if inner.height >= 3 { 2 } else { 1 };
        rows_area = Rect {
            y: inner.y + skip,
            height: inner.height - skip,
            ..inner
        };
        frame.render_widget(Paragraph::new(track_header(&cols, sort)), header_area);
    }
    hit.main_list = rows_area;
    super::clamp_offset(list_state, display.len(), rows_area.height as usize);

    // Clickable cells: the artist and album columns, clipped to actual rows.
    let filled_rows =
        (display.len().saturating_sub(list_state.offset()) as u16).min(rows_area.height);
    let cell_col = |off: usize, width: usize| {
        Rect {
            x: rows_area.x.saturating_add(off as u16),
            y: rows_area.y,
            width: width as u16,
            height: filled_rows,
        }
        .intersection(rows_area)
    };
    if cols.actions {
        hit.main_like_col = cell_col(cols.like_offset(), LIKE_W);
        hit.main_add_col = cell_col(cols.add_offset(), ADD_W);
    }
    hit.main_artist_col = cell_col(cols.artist_offset(), cols.artist);
    if let Some(off) = cols.album_offset() {
        hit.main_album_col = cell_col(off, cols.album);
    }
    let hover_cell: Option<(usize, HoverCol)> = mouse.and_then(|m| {
        let row = |y: u16| list_state.offset() + (y - rows_area.y) as usize;
        if hit.main_like_col.contains(m) {
            Some((row(m.y), HoverCol::Like))
        } else if hit.main_add_col.contains(m) {
            Some((row(m.y), HoverCol::Add))
        } else if hit.main_artist_col.contains(m) {
            Some((row(m.y), HoverCol::Artist))
        } else if hit.main_album_col.contains(m) {
            Some((row(m.y), HoverCol::Album))
        } else {
            None
        }
    });
    // Row positions are in display order; the playing marker resolves by URI.
    let playing = marks
        .uri
        .as_deref()
        .and_then(|uri| display.iter().position(|&ti| tracks[ti].uri == uri));
    let items: Vec<ListItem> = display
        .iter()
        .enumerate()
        .map(|(i, &ti)| {
            let mark = if Some(i) == playing {
                RowMark::Playing
            } else {
                RowMark::None
            };
            // Album views number by the track's own position on the album;
            // everything else numbers by position in the current view.
            let no = if show_track_no {
                tracks[ti].track_number
            } else {
                i as u32 + 1
            };
            track_row(
                &tracks[ti],
                &cols,
                no,
                mark,
                i == main_index,
                liked.get(&tracks[ti].uri).copied(),
                hover_cell.and_then(|(row, col)| (row == i).then_some(col)),
            )
        })
        .collect();
    let count = items.len();
    let list = List::new(items);
    frame.render_stateful_widget(list, rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

#[allow(clippy::too_many_arguments)]
fn draw_tracks(
    frame: &mut Frame,
    area: Rect,
    list: &crate::app::state::TrackList,
    cover: Option<&crate::cover::Cover>,
    global_loading: bool,
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
    let body = header_band(frame, inner, list, cover, loading, hit, mouse);
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
        &list.tracks,
        &list.display,
        main_index,
        list_state,
        hit,
        marks,
        Some(list.sort),
        liked,
        list.show_track_no(),
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
        let stations: Vec<&crate::app::state::Station> = results.stations.iter().collect();
        render_station_table(
            frame,
            body,
            &stations,
            favorites,
            playing_url,
            main_index,
            list_state,
            hit,
        );
        return;
    }

    if search_tab == SearchTab::Tracks {
        let display: Vec<usize> = (0..results.tracks.len()).collect();
        render_track_table(
            frame,
            body,
            &results.tracks,
            &display,
            main_index,
            list_state,
            hit,
            marks,
            None,
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
            mouse,
        );
        return;
    }

    let width = body.width as usize;
    let header: Option<Line> = match search_tab {
        SearchTab::Playlists => Some(playlist_header(&PlaylistCols::new(width))),
        _ => None,
    };
    let mut rows_area = body;
    if let Some(h) = header
        && body.height >= 2
    {
        frame.render_widget(Paragraph::new(h), Rect { height: 1, ..body });
        // Blank spacer row under the header when there is room for it.
        let skip = if body.height >= 3 { 2 } else { 1 };
        rows_area = Rect {
            y: body.y + skip,
            height: body.height - skip,
            ..body
        };
    }
    hit.main_list = rows_area;

    let items: Vec<ListItem> = match search_tab {
        SearchTab::Tracks | SearchTab::Albums | SearchTab::Stations => unreachable!(),
        SearchTab::Artists => results
            .artists
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let mut line = Line::styled(fit(&a.name, width), theme::text());
                if i == main_index {
                    super::table::apply_selection(&mut line);
                }
                ListItem::new(line)
            })
            .collect(),
        SearchTab::Playlists => {
            let cols = PlaylistCols::new(width);
            results
                .playlists
                .iter()
                .enumerate()
                // Search results name every owner: one of them being yours is
                // the exception here, not the rule. The playing marker still
                // applies — a result can be the queue you are listening to.
                .map(|(i, p)| {
                    let playing = marks.context.as_deref()
                        == Some(crate::app::state::playlist_key(&p.id).as_str());
                    playlist_row(p, &cols, None, playing, i == main_index)
                })
                .collect()
        }
    };
    let count = items.len();
    let list = List::new(items);
    super::clamp_offset(list_state, count, rows_area.height as usize);
    frame.render_stateful_widget(list, rows_area, list_state);
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
            artist_id: None,
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

    fn album_state() -> AppState {
        let mut st = tracks_state(vec![track("One", "Donna"), track("Two", "Donna")]);
        if let MainView::Tracks(list) = &mut st.main {
            list.kind = crate::app::state::TrackListKind::Album;
            list.header.name = "Dance In The Street".into();
            list.header.subtitle = "Donna The Buffalo · 2018".into();
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
            let lines = render(&mut out, 90, 20);
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
        assert!(with_art[11].contains("Title"));
        // Without one it collapses onto three rows and loses the artwork.
        assert!(!no_art[4].chars().take(w).all(|c| c == '▀' || c == '♫'));
    }

    /// The album page is the one browse view that is *about* a record, so it
    /// is the one that gets a sleeve. It is 6 rows and therefore 12 cells, and
    /// the metadata stacks beside it rather than sharing a row with the count.
    #[test]
    fn an_album_page_draws_its_sleeve_beside_stacked_metadata() {
        let mut st = album_state();
        let lines = render(&mut st, 90, 22);
        assert!(lines[PATH].contains("DANCE IN THE STREET"));
        // Sleeve occupies the left 12 cells of the six band rows.
        // No cover is decoded in the test, so this is the placeholder swatch:
        // half-blocks with a single ♫ in the middle.
        let w = super::super::table::art_w(ART_H) as usize;
        for row in lines.iter().take(10).skip(4) {
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
        assert!(lines[11].contains("Title"));
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
            albums: vec![],
            display: Vec::new(),
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
        st.playlists = vec![
            Playlist {
                id: "p1".into(),
                name: "trendy".into(),
                track_count: 18,
                owner: "Dalton M".into(),
                owner_id: "dm".into(),
                snapshot_id: "s".into(),
            },
            Playlist {
                id: "p2".into(),
                name: "New Music Friday".into(),
                track_count: 38,
                owner: "NPR Music".into(),
                owner_id: "npr".into(),
                snapshot_id: "s".into(),
            },
        ];
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
    /// which can always go *up* to the artist its tracks credit. An `up` and
    /// a `back` are the same shape in a trail, which is half the reason for
    /// drawing one: a single `← <name>` pill spells both and cannot say which
    /// it means.
    #[test]
    fn an_album_page_with_no_history_offers_its_artist() {
        let mut st = album_state();
        if let MainView::Tracks(list) = &mut st.main {
            for t in &mut list.tracks {
                t.artist_id = Some("r1".into());
            }
        }
        let lines = render(&mut st, 90, 22);
        assert!(
            lines[PATH].contains("DONNA  ›  DANCE IN THE STREET"),
            "{:?}",
            lines[PATH]
        );
        assert_eq!(
            st.hit.crumbs[0].1,
            CrumbTarget::Artist {
                id: "r1".into(),
                name: "Donna".into()
            }
        );

        // Without an artist id there is nowhere to go, so the page stands
        // alone: one crumb, its own name, and nothing to click.
        let mut st = album_state();
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

    /// A playlist's mosaic is not a record sleeve, so playlists keep the
    /// text-only band — and so does an album whose art Spotify never gave us.
    #[test]
    fn playlists_and_coverless_albums_keep_the_text_band() {
        for mut st in [tracks_state(vec![track("A", "B")]), {
            let mut st = album_state();
            if let MainView::Tracks(list) = &mut st.main {
                list.header.cover_url = None;
            }
            st
        }] {
            let lines = render(&mut st, 90, 22);
            assert!(
                !lines.iter().any(|l| l.contains('▀')),
                "a sleeve appeared: {lines:#?}"
            );
            // Name and totals share row 2, as they always did.
            assert!(lines[4].contains("tracks"));
            assert!(lines[5].contains("▶ play"));
        }
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
        assert!(lines[7].contains("Time"));
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
        // border + pad + band(3) + header + spacer + row 0; selected is index 1
        let row_y = 10;
        assert!((1..79u16).any(|x| {
            let cell = buffer.cell(Position { x, y: row_y }).unwrap();
            cell.fg == theme::BRIGHT && cell.modifier.contains(Modifier::BOLD)
        }));
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
            list.sort = crate::app::state::TrackSort {
                key: SortKey::Title,
                ascending: true,
            };
            list.rebuild_display();
        }
        let lines = render(&mut st, 90, 14);
        assert!(lines[5].contains("sort: title ▲"));
        assert!(lines[7].contains("Title▲"));
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

        for rect in [st.hit.main_artist_col, st.hit.main_album_col] {
            assert!(!rect.is_empty());
            // Clipped to the two filled rows, inside the list area.
            assert_eq!(rect.height, 2);
            assert!(st.hit.main_list.contains(Position {
                x: rect.x,
                y: rect.y
            }));
        }
        // The recorded columns line up with the rendered cells.
        let text_at = |rect: Rect, w: usize| -> String {
            (rect.x..rect.x + w as u16)
                .filter_map(|x| buffer.cell(Position { x, y: rect.y }).map(|c| c.symbol()))
                .collect()
        };
        assert_eq!(text_at(st.hit.main_artist_col, 3), "Ann");
        assert_eq!(text_at(st.hit.main_album_col, 5), "Album");
    }

    /// Every row wears the pair, and the star reads its state as colour: the
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

    /// Hovering either control lights that control alone — its whole padded
    /// cell, so the lit run says how big the target is.
    #[test]
    fn hovering_lights_one_control_of_the_pair() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        // Draw once to find out where the pair landed.
        render(&mut st, 90, 12);
        let (like, add) = (st.hit.main_like_col, st.hit.main_add_col);
        assert!(!like.is_empty() && !add.is_empty());

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
        for x in like.x..like.right() {
            assert_ne!(
                bg_at(&mut st, x, like.y),
                theme::DIM,
                "hovering the + lit the ★ too"
            );
        }
    }

    /// Each control is a padded click target of its own, the two flush against
    /// each other, at the end of the row before the time.
    #[test]
    fn the_pair_records_two_adjacent_clickable_rects() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        st.liked.insert("spotify:track:Alpha".into(), true);
        let mut terminal = Terminal::new(TestBackend::new(90, 14)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        let (like, add) = (st.hit.main_like_col, st.hit.main_add_col);
        for col in [like, add] {
            assert_eq!(col.width, 3, "the control lost its padding");
            assert_eq!(col.height, 2, "the column outran the filled rows");
            assert!(st.hit.main_list.contains(Position { x: col.x, y: col.y }));
        }
        // Flush, in the order the deck wears them: no cell between them
        // belongs to neither control.
        assert_eq!(add.x, like.right());
        let symbol = |x: u16| {
            buffer
                .cell(Position { x, y: like.y })
                .unwrap()
                .symbol()
                .to_string()
        };
        // Each mark is centred in its own cell.
        assert_eq!(symbol(like.x + 1), super::super::table::LIKED_MARK);
        assert_eq!(symbol(add.x + 1), super::super::table::ADD_MARK);
        // And the pair comes after the data columns it follows.
        assert!(like.x > st.hit.main_album_col.right());
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
        assert!(
            header.contains(&super::super::table::actions_label()),
            "{header:?}"
        );
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
            tracks: vec![track("Starlight", "Muse")],
            albums: vec![crate::app::state::AlbumItem {
                id: "a1".into(),
                name: "Black Holes".into(),
                artists: "Muse".into(),
                release_year: "2006".into(),
                album_type: "album".into(),
                album_group: "album".into(),
                track_count: 12,
                cover_url: None,
            }],
            artists: vec![crate::app::state::ArtistItem {
                id: "r1".into(),
                uri: "spotify:artist:r1".into(),
                name: "Muse".into(),
            }],
            playlists: vec![Playlist {
                id: "p1".into(),
                name: "Muse Mix".into(),
                track_count: 42,
                owner: "someone".into(),
                owner_id: "someone".into(),
                snapshot_id: "s1".into(),
            }],
            stations: vec![test_station("st1", "Radio Paradise")],
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
        let saved = render(&mut st, 90, 18).join("\n");
        assert!(
            saved.contains(&format!(
                "{} Radio Paradise",
                super::super::table::LIKED_MARK
            )),
            "{saved}"
        );

        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        st.radio = Some(crate::app::state::RadioPlayback {
            station,
            is_playing: true,
            started_at: std::time::Instant::now(),
            title: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            volume_percent: 50,
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
        });
        let playing = render(&mut st, 90, 18).join("\n");
        assert!(playing.contains("♫ Radio Paradise"), "{playing}");
    }

    /// A directory that has not answered yet is not a directory that answered
    /// "nothing". The count band says which, so an empty tab mid-flight cannot
    /// be read as a failed search.
    #[test]
    fn a_pending_stations_tab_says_it_is_searching() {
        let mut st = search_state();
        st.search_tab = SearchTab::Stations;
        if let MainView::Search(r) = &mut st.main {
            r.stations.clear();
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
            r.stations.clear();
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
            r.stations.clear();
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
    }

    /// An album row's *name* is a link, the way the Album column of a track
    /// table is: one click opens it. A double-click or Enter is a way in that
    /// nothing on screen says.
    #[test]
    fn an_album_row_registers_its_name_as_a_click_target() {
        let mut st = search_state();
        st.search_tab = SearchTab::Albums;
        let lines = render(&mut st, 90, 16);
        let col = st.hit.main_album_col;
        assert!(!col.is_empty());
        // Starts at the left edge of the rows and covers the name column.
        assert_eq!(col.x, st.hit.main_list.x);
        assert_eq!(col.y, st.hit.main_list.y);
        assert_eq!(col.width, AlbumCols::new(90 - GUTTER as usize).name as u16);
        // Clipped to the one row that actually has an album on it.
        assert_eq!(col.height, 1);
        let name: String = lines[col.y as usize]
            .chars()
            .take(col.width as usize)
            .collect();
        assert!(name.starts_with("Black Holes"), "{name:?}");
    }

    #[test]
    fn empty_search_tab_shows_message() {
        let mut st = search_state();
        if let MainView::Search(r) = &mut st.main {
            r.playlists.clear();
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
            albums,
            display: Vec::new(),
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
        let lines = render(&mut st, 90, 26);
        assert!(lines[PATH].contains("MUSE"));
        // The photo occupies the left 12 cells of the six band rows (a
        // placeholder swatch here: nothing is decoded in the test).
        let w = super::super::table::art_w(ART_H) as usize;
        for row in lines.iter().take(10).skip(4) {
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
        assert!(lines[11].contains("Top Tracks"));
        assert!(lines[12].trim().is_empty());
        assert!(lines[13].contains("Title"));
        assert!(lines[14].contains("Uprising"));
        assert!(lines[16].contains("Albums"));
        assert!(lines[17].trim().is_empty());
        assert!(lines[18].contains("Black Holes"));
        assert!(lines[19].contains("2006 · 12 tracks"));
        assert!(lines[20].contains("▶ play"));
        assert!(lines[20].contains("shuffle"));
        assert_eq!(st.hit.card_play.len(), 1);
        assert_eq!(st.hit.card_shuffle.len(), 1);
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
        // Heading, blank, column header, one track, blank, heading, blank,
        // then the cards: four lines each plus a blank.
        assert_eq!(&rows[..7], &[None, None, None, Some(0), None, None, None]);
        assert_eq!(&rows[7..12], &[Some(1), Some(1), Some(1), Some(1), None]);
        assert_eq!(&rows[12..17], &[Some(2), Some(2), Some(2), Some(2), None]);
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
        assert!(lines[16].contains("Albums"));
        assert!(lines[17].trim().is_empty());
        let strip = &lines[18];
        assert!(strip.contains("Albums"), "{strip:?}");
        assert!(strip.contains("Singles"), "{strip:?}");
        assert!(strip.contains("Appears On"), "{strip:?}");
        assert!(!strip.contains("Compilations"), "{strip:?}");
        assert!(lines[19].trim().is_empty());
        // Only the open group's cards, one blank row below the strip.
        assert!(lines[20].contains("Origin Of Symmetry"));
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
