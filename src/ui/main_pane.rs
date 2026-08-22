use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::theme;
use crate::app::state::{
    AppState, Crumb, CrumbTarget, HitAreas, HomeItem, MainView, SearchTab, SortKey, Track,
    TrackSort, format_duration,
};

/// Playback context needed to mark the playing row, copied out of
/// `AppState.playback` before the draw split-borrow.
struct PlayMarks {
    /// URI of the playing track, if any.
    uri: Option<String>,
    /// URI of the playing context, for marking the row of the playlist it
    /// came out of.
    context: Option<String>,
}

pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // The search input used to live here, as a bordered box that appeared only
    // while you were typing and pushed the whole pane down three rows to make
    // room. It is now a permanent row above this one; see `super::top_row`.
    let list_area = area;

    let loading = state.loading;
    let search_tab = state.search_tab;
    let main_index = state.main_index;
    let mouse = state.mouse_pos;
    let marks = PlayMarks {
        uri: state.playback.as_ref().and_then(|p| p.track_uri.clone()),
        context: state.playback.as_ref().and_then(|p| p.context_uri.clone()),
    };
    let me_id = state.me_id.clone();
    // The station playing, if one is, so a radio page can mark its row the way
    // a track table marks the playing track.
    let playing_station = state.radio.as_ref().map(|r| r.station.url.clone());
    // Home's rows and their tails, resolved before the split borrow below —
    // both read `playlists`, which the borrow takes.
    let home: Vec<(HomeItem, String)> = state
        .home_items()
        .into_iter()
        .map(|item| (item, state.home_count(item)))
        .collect();
    // Split borrows: the view data is read while the list state and hit
    // areas are written.
    let AppState {
        main,
        playlists,
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
            me_id.as_deref(),
            main_index,
            main_list,
            hit,
            &marks,
        ),
        MainView::Tracks(list) => draw_tracks(
            frame, list_area, list, view_cover, loading, main_index, main_list, hit, &marks, liked,
            mouse,
        ),
        MainView::Search(results) => draw_search(
            frame, list_area, results, loading, search_tab, main_index, main_list, hit, &marks,
            mouse, liked,
        ),
        MainView::Artist(v) => draw_artist(
            frame, list_area, v, page_art, main_index, main_list, hit, &marks, mouse, liked,
        ),
        MainView::Radio(v) => draw_radio(
            frame,
            list_area,
            v,
            radio_favorites,
            playing_station.as_deref(),
            main_index,
            main_list,
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
    items: &[(HomeItem, String)],
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
    for (i, (item, count)) in items.iter().enumerate() {
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
            format!("{indent}{}", fit(item.blurb(), name_w)),
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

/// Everything the left rail used to hold: Liked Songs, then the playlists.
///
/// The rail spent two lines on each of them — a name over "56 tracks" —
/// because it was 30 cells wide. At full width the count is a column, so the
/// page holds twice as many rows and can afford to say who owns them.
#[allow(clippy::too_many_arguments)]
fn draw_playlists(
    frame: &mut Frame,
    area: Rect,
    playlists: &[crate::app::state::Playlist],
    me_id: Option<&str>,
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    marks: &PlayMarks,
) {
    let inner = body_area(area);
    if inner.width == 0 || inner.height == 0 {
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

    // Playlists only. Liked Songs used to head this list because the rail had
    // nowhere else to put it; it is a Home row of its own now, and it is not a
    // playlist, so it does not belong under a heading that says it is.
    let items: Vec<ListItem> = playlists
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let playing = marks.context.as_deref() == Some(p.uri.as_str());
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
        empty_message(frame, body, radio_empty_hint(view));
        return;
    }

    let cols = StationCols::new(body.width as usize);
    let mut rows_area = body;
    // Facet lists get no header row: "Country / Stations" over a list of
    // countries says only what the tab above it already said.
    if !facets && body.height >= 2 {
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

    let items: Vec<ListItem> = view
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            RadioRow::Facet { label, count, .. } => {
                facet_row(label, *count, rows_area.width as usize, i == main_index)
            }
            RadioRow::Station(s) => station_row(
                s,
                &cols,
                favorites.iter().any(|f| f.uuid == s.uuid),
                playing_url == Some(s.url.as_str()),
                i == main_index,
            ),
        })
        .collect();
    let count = items.len();
    super::clamp_offset(list_state, count, rows_area.height as usize);
    frame.render_stateful_widget(List::new(items), rows_area, list_state);
    super::table::draw_scrollbar(frame, scroll_col(rows_area), count, list_state.offset());
}

/// What to say on a radio page with nothing on it.
fn radio_empty_hint(view: &crate::app::state::RadioView) -> &'static str {
    use crate::app::state::RadioScope;
    if view.loading {
        return "loading stations…";
    }
    match view.scope {
        RadioScope::Favorites => "no saved stations yet — press L on one to keep it",
        RadioScope::Search(_) => "no stations by that name",
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
const PLAY_PILL: &str = " ▶ play ";

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
    /// One row of an album card: `row` counts from the top of its sleeve.
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
    hit: &mut HitAreas,
    marks: &PlayMarks,
    mouse: Option<Position>,
    liked: &std::collections::HashMap<String, bool>,
) {
    let inner = body_area(area);
    let body = artist_band(frame, inner, v, page_art, hit, mouse);
    if v.len() == 0 {
        hit.main_list = body;
        empty_message(
            frame,
            body,
            if v.loading {
                "loading…"
            } else {
                "nothing to show for this artist"
            },
        );
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
/// page and used to look like two different products.
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
    frame.render_widget(Paragraph::new(Line::from(spans)), play_area);

    let used = if stacked { ART_BAND_H } else { TEXT_BAND_H };
    Rect {
        y: inner.y + used,
        height: inner.height - used,
        ..inner
    }
}
/// "30 albums", or nothing until the catalogue lands.
///
/// The top tracks are not counted here. They are a numbered list a few rows
/// below, under a heading that names them — a band reading "10 top tracks"
/// over a list running 1 to 10 is reading the screen back to you.
fn artist_counts(v: &crate::app::state::ArtistView) -> String {
    match v.albums.len() {
        0 => String::new(),
        1 => "1 album".to_string(),
        n => format!("{n} albums"),
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
    if !v.albums.is_empty() {
        plan.push(ArtistLine::Heading("Albums"));
        plan.push(ArtistLine::Blank);
        for album in 0..v.albums.len() {
            plan.extend((0..CARD_ART_H).map(|row| ArtistLine::Card { album, row }));
            plan.push(ArtistLine::Blank);
        }
    }
    let cards_from = cards_at + 2;

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

    // The two controls on each visible card — its name and its ▶ play —
    // recorded before the rows are built, so hovering one can light it.
    hit.card_play.clear();
    hit.album_names.clear();
    for (album, a) in v.albums.iter().enumerate() {
        let first = cards_from + album * CARD_H;
        let push = |hits: &mut Vec<(Rect, usize)>, row: u16, w: u16| {
            let Some(y) = screen_y(first + row as usize) else {
                return;
            };
            let rect = Rect {
                x: body.x + indent,
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
        push(&mut hit.album_names, 0, name_w);
        push(
            &mut hit.card_play,
            CARD_PLAY_ROW,
            super::table::width(PLAY_PILL) as u16,
        );
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
            if cols.like {
                hit.main_like_col = rect(cols.like_offset(), LIKE_W);
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
            ArtistLine::Card { album, row } => card_line(
                &v.albums[album],
                row,
                indent as usize,
                card_text_w,
                split + album == main_index,
                hover_album == Some(album),
                hover_play == Some(album),
            ),
            ArtistLine::Blank => ListItem::new(Line::default()),
        })
        .collect();
    frame.render_stateful_widget(List::new(items), body, list_state);

    // The sleeves go on last: they are painted cells, not list rows, so they
    // are drawn over the block the rows left blank for them, clipped to the
    // body so a card scrolling off the top slides under it.
    if art_w > 0 {
        for (album, a) in v.albums.iter().enumerate() {
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
fn card_line(
    a: &crate::app::state::AlbumItem,
    row: u16,
    indent: usize,
    text_w: usize,
    selected: bool,
    hovered: bool,
    play_hover: bool,
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

/// "2006 · Single · 10 tracks" — every part Spotify actually reported.
///
/// "Album" is left off: on an artist's page a record is an album unless it
/// says otherwise, and printing the word on nine cards in ten says nothing.
fn album_meta(a: &crate::app::state::AlbumItem) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !a.release_year.is_empty() {
        parts.push(a.release_year.clone());
    }
    if !a.album_type.is_empty() && !a.album_type.eq_ignore_ascii_case("album") {
        let mut kind = a.album_type.clone();
        kind[..1].make_ascii_uppercase();
        parts.push(kind);
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
        vec![Span::styled(" ▶ play ", accent)],
    );
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
        let rect = super::table::segment(
            spans,
            x,
            row,
            mouse,
            vec![Span::styled(format!(" {} ", title(tab)), style)],
        );
        hits.push((rect, tab));
    }
}

/// Reserved at the right of the pane: a blank column, then the scrollbar. The
/// border used to carry the bar; with no border it needs columns of its own,
/// kept outside the content rect so a click on it cannot resolve to a row.
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

/// One crumb's text: capped, and uppercased to sit in the row the section
/// label used to own. `fit` pads to width, which a crumb must not do.
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
            // page's own name and no path — a clipped root would name a
            // destination the crumb no longer leads to.
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
            loading: v.loading && v.rows.is_empty(),
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
/// This is all that is left of the old `section_body`, and before that of
/// `theme::pane_block` and the section label between them. The label named the
/// page's kind (`ALBUM`) and carried a `← <name>` pill after it; the path that
/// replaced it lived on the pane's own first row. Both are now in the header
/// two rows above, drawn once for both views — see [`super::top_row`].
fn body_area(area: Rect) -> Rect {
    Rect {
        width: area.width.saturating_sub(GUTTER),
        ..area
    }
}

/// Centered dim hint for a view with nothing to list.
fn empty_message(frame: &mut Frame, inner: Rect, text: &str) {
    if inner.height == 0 {
        return;
    }
    let row = Rect {
        y: inner.y + inner.height / 2,
        height: 1,
        ..inner
    };
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(theme::dim()),
        row,
    );
}

const DUR_W: usize = 5;
const YEAR_W: usize = 4;
const COL_GAP: &str = "   ";
/// Leading marker column: "▶ " playing, "→ " next up.
const PREFIX_W: usize = 2;
/// Liked column: the mark when saved, blank when not — it comes back under
/// the pointer. See [`track_row`] and [`super::table::LIKED_MARK`].
const LIKE_W: usize = 2;
/// Minimum width of the track-number column, right-aligned.
const NO_W: usize = 3;

/// Column widths for the track table, derived from the pane's inner width.
/// Narrow panes drop the year first, then the album, then the liked mark.
struct TrackCols {
    name: usize,
    artist: usize,
    /// 0 = hidden.
    album: usize,
    year: bool,
    like: bool,
    track_no: bool,
    /// Width of the number column, grown to fit the largest number.
    no_w: usize,
}

impl TrackCols {
    fn new(width: usize, max_no: u32) -> Self {
        let year = width >= 70;
        let show_album = width >= 50;
        let like = width >= 60;
        let track_no = width >= 40;
        let no_w = max_no.to_string().len().max(NO_W);
        let mut flex = width.saturating_sub(PREFIX_W + DUR_W + COL_GAP.len());
        if like {
            flex = flex.saturating_sub(LIKE_W);
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
                like,
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
                like,
                track_no,
                no_w,
            }
        }
    }

    /// Column offset (from the row start) of the liked cell, shown or not.
    fn like_offset(&self) -> usize {
        let mut x = PREFIX_W;
        if self.track_no {
            x += self.no_w + COL_GAP.len();
        }
        x
    }

    /// Column offset (from the row start) of the artist cell.
    ///
    /// Built on [`Self::like_offset`] rather than repeating its arithmetic:
    /// the two columns are adjacent, and a click rect that disagrees with the
    /// glyph it covers is the bug this exists to prevent.
    fn artist_offset(&self) -> usize {
        let mut x = self.like_offset();
        if self.like {
            x += LIKE_W;
        }
        x + self.name + COL_GAP.len()
    }

    /// Column offset of the album cell, when the column is shown.
    fn album_offset(&self) -> Option<usize> {
        (self.album > 0).then(|| self.artist_offset() + self.artist + COL_GAP.len())
    }
}

/// Which clickable cell of a track row the mouse is over.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverCol {
    Like,
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
    if cols.like {
        text = format!("{text}{}", fit(super::table::LIKED_MARK, LIKE_W));
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
    // everything supporting it at DIM, and the playing row in accent. The
    // title used to be `Style::default()` — the raw terminal foreground, and
    // the one unthemed colour on the page.
    let name_style = if mark == RowMark::Playing {
        accent_bold
    } else {
        theme::text()
    };
    let mut spans = vec![prefix];
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
    if cols.like {
        // The mark or nothing — never the same shape at two brightnesses. A
        // column of hollow glyphs beside a column of solid ones reads as one
        // mark lit two ways, which is exactly what it must not be mistaken
        // for, and it leaves the page speckled with a glyph that means "no".
        //
        // So an unsaved track shows nothing, and hovering the cell brings the
        // mark up as an offer. That reveal is also the only thing that says
        // the column can be clicked at all: without it a blank cell has
        // nothing to invite the pointer.
        let hovered = hover == Some(HoverCol::Like);
        let (glyph, style) = match liked {
            Some(true) => (super::table::LIKED_MARK, theme::accent()),
            // An unchecked row offers the same mark: `library_add` on a track
            // you already have is a no-op, so the worst a premature press can
            // do is confirm what was already true.
            _ if hovered => (super::table::LIKED_MARK, dim),
            _ => ("", dim),
        };
        let style = if hovered {
            super::table::hover_style(style)
        } else {
            style
        };
        spans.push(Span::styled(fit(glyph, LIKE_W), style));
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
    spans.push(Span::raw(COL_GAP));
    spans.push(Span::styled(
        format!("{:>DUR_W$}", format_duration(t.duration_ms)),
        dim,
    ));
    let mut line = Line::from(spans);
    if selected {
        super::table::apply_selection(&mut line);
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
///
/// It used to carry a mark on Liked Songs too, back when that was a row of
/// this list rather than an entry on Home.
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
    if cols.like {
        hit.main_like_col = cell_col(cols.like_offset(), LIKE_W);
    }
    hit.main_artist_col = cell_col(cols.artist_offset(), cols.artist);
    if let Some(off) = cols.album_offset() {
        hit.main_album_col = cell_col(off, cols.album);
    }
    let hover_cell: Option<(usize, HoverCol)> = mouse.and_then(|m| {
        let row = |y: u16| list_state.offset() + (y - rows_area.y) as usize;
        if hit.main_like_col.contains(m) {
            Some((row(m.y), HoverCol::Like))
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
    hit: &mut HitAreas,
    marks: &PlayMarks,
    liked: &std::collections::HashMap<String, bool>,
    mouse: Option<Position>,
) {
    let loading = list.loading || global_loading;
    let inner = body_area(area);
    let body = header_band(frame, inner, list, cover, loading, hit, mouse);
    if list.display.is_empty() && !loading {
        hit.main_list = body;
        empty_message(frame, body, "this playlist is empty");
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
    main_index: usize,
    list_state: &mut ListState,
    hit: &mut HitAreas,
    marks: &PlayMarks,
    mouse: Option<Position>,
    liked: &std::collections::HashMap<String, bool>,
) {
    let inner = body_area(area);

    let tab_len = match search_tab {
        SearchTab::Tracks => results.tracks.len(),
        SearchTab::Albums => results.albums.len(),
        SearchTab::Artists => results.artists.len(),
        SearchTab::Playlists => results.playlists.len(),
    };

    // Header band: query bold with the result count right-aligned, then the
    // tab strip. On short panes the tabs keep a single row.
    let mut body = inner;
    if inner.height >= 8 {
        let info_area = Rect { height: 1, ..inner };
        let totals = Span::styled(format!("{tab_len} results"), theme::dim());
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
            &SearchTab::ALL,
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
            &SearchTab::ALL,
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

    if tab_len == 0 && !loading {
        hit.main_list = body;
        let text = format!(
            "no {} results for \"{}\"",
            search_tab.title().to_lowercase(),
            results.query
        );
        empty_message(frame, body, &text);
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
        SearchTab::Tracks | SearchTab::Albums => unreachable!(),
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
                    let playing = marks.context.as_deref() == Some(p.uri.as_str());
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
    use crate::app::state::{AppState, PlaybackSnapshot, Playlist, RepeatMode, SearchResults};

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
        let mut list = crate::app::state::TrackList::new("My List", "", None, None);
        list.append(tracks);
        st.main = MainView::Tracks(list);
        st
    }

    fn playing(uri: &str) -> PlaybackSnapshot {
        PlaybackSnapshot {
            is_playing: true,
            progress_ms: 0,
            duration_ms: 100_000,
            track_uri: Some(uri.into()),
            context_uri: None,
            artist_id: None,
            album_id: None,
            track_name: "x".into(),
            artists: "x".into(),
            album: "x".into(),
            release_year: "2020".into(),
            cover_url: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume_percent: 50,
            device_name: "dev".into(),
            is_local_device: true,
            fetched_at: std::time::Instant::now(),
        }
    }

    /// Rows the header takes above the pane. The path a page draws is on it,
    /// so these tests draw it too — the pane no longer owns that row, but
    /// [`fit_trail`] and [`draw_trail`] are still what puts a path on it.
    const HEAD: usize = super::super::HEAD_H as usize;

    /// Draw the header and the pane under it, as [`super::super::draw`] lays
    /// them out. Row 0 is the mark and the path, row 2 the search prompt, row
    /// [`HEAD`] the first row of the pane.
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

    /// There is one album page, not two. It looked like two because the
    /// header band only draws the art layout when the view has a sleeve, and
    /// a track row used to open an album with `cover_url: None` — so albums
    /// reached from a track list got the cramped text band instead. Every
    /// route now supplies the sleeve, so every route renders the same page.
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
        assert!(lines[0].contains("DANCE IN THE STREET"));
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
        assert!(!st.hit.header_play_btn.is_empty());
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
    /// them spells the path that got it there. The playlist page used to be
    /// the exception, because it was opened from a rail that never went away.
    ///
    /// The trail is anchored right after the mark, which is the point of it:
    /// the `← <name>` pill this replaced sat three cells after a section label
    /// whose width was the page's kind, so the one control that means "go
    /// back" landed in a different column on every page.
    ///
    /// Home contributes no crumb at either end — the mark beside the path is
    /// already the way there.
    #[test]
    fn pages_spell_the_path_that_reached_them() {
        // The column every path starts in: after the mark and its gap.
        let x0 = super::super::table::BRAND_W + super::super::top_row::MARK_GAP;

        let mut st = album_state();
        from_home(&mut st);
        st.main_index = 0;
        let lines = render(&mut st, 90, 22);
        assert!(
            st.hit.crumbs.is_empty(),
            "Home is the only step behind it, and it draws none: {:?}",
            st.hit.crumbs
        );
        assert!(lines[0].contains("DANCE IN THE STREET"), "{:?}", lines[0]);
        assert!(!lines[0].contains('›'), "{:?}", lines[0]);

        // And the head starts in the same column whatever the page is called
        // — which the pill, drawn after a variable-width label, never did.
        // Columns, not bytes: the `♫` in the mark is three bytes wide and one
        // cell.
        let col =
            |line: &str, needle: &str| line.find(needle).map(|b| line[..b].chars().count() as u16);
        assert_eq!(col(&lines[0], "DANCE"), Some(x0));

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
            assert_eq!(col(&lines[0], name), Some(x0), "{name}: {:?}", lines[0]);
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
            top: crate::app::state::TrackList::new("Donna The Buffalo", "", None, None),
            albums: vec![],
            loading: false,
        });
        st.push_view();
        st.main = album_state().main;

        let lines = render(&mut st, 90, 22);
        // The ancestor elides at `ANCESTOR_W` while the head keeps `HEAD_W`:
        // the page you are on is what the row is about, a step behind it only
        // has to be recognizable enough to aim at.
        assert!(
            lines[0].contains("DONNA THE BUF…  ›  DANCE IN THE STREET"),
            "{:?}",
            lines[0]
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
            let mut list = crate::app::state::TrackList::new(name, "", None, None);
            // Distinct identities, or `push_view` would collapse them.
            list.cache_key = Some(crate::app::state::playlist_key(name));
            st.main = MainView::Tracks(list);
            st.push_view();
        }
        st.main = page;
        let lines = render(&mut st, 90, 22);
        assert!(
            lines[0].contains("…  ›  TWO  ›  THREE  ›  MY LIST"),
            "{:?}",
            lines[0]
        );
        assert!(!lines[0].contains("ONE  ›"), "{:?}", lines[0]);
        // The root is a crumb like any other; the ellipsis between it and the
        // rest stands for what was shed and leads nowhere.
        assert_eq!(st.hit.crumbs.len(), 3);
        assert_eq!(st.hit.crumbs[0].1, CrumbTarget::Depth(0));
        assert_eq!(st.hit.crumbs[1].1, CrumbTarget::Depth(2));

        // The narrow ancestors earn their keep here: at the head's width this
        // row would hold two steps, and it holds three.
        let lines = render(&mut st, 80, 22);
        assert!(
            lines[0].contains("…  ›  TWO  ›  THREE  ›  MY LIST"),
            "{:?}",
            lines[0]
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
                uri: "spotify:playlist:p1".into(),
                name: "trendy".into(),
                track_count: 18,
                owner: "Dalton M".into(),
                owner_id: "dm".into(),
                snapshot_id: "s".into(),
            },
            Playlist {
                id: "p2".into(),
                uri: "spotify:playlist:p2".into(),
                name: "New Music Friday".into(),
                track_count: 38,
                owner: "NPR Music".into(),
                owner_id: "npr".into(),
                snapshot_id: "s".into(),
            },
        ];
        let lines = render(&mut st, 90, 22);
        assert!(lines[0].contains("PLAYLISTS"), "{:?}", lines[0]);
        assert!(lines[0].contains("2 playlists"), "{:?}", lines[0]);
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
    /// drawing one: the old pill spelled both `← <name>` and could not say
    /// which it meant.
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
            lines[0].contains("DONNA  ›  DANCE IN THE STREET"),
            "{:?}",
            lines[0]
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
        assert!(lines[0].contains("DANCE IN THE STREET"), "{:?}", lines[0]);
        assert!(!lines[0].contains('›'));
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
        assert!(!lines[0].contains("HOME"), "{:?}", lines[0]);
        assert!(lines[0].contains("DANCE"), "{:?}", lines[0]);
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

    /// Every name on these pages used to be drawn with `Style::default()` —
    /// the raw terminal foreground, and the one unthemed colour on the screen.
    /// It is why the browse pages read harsher than the player even though
    /// they share a palette. Nothing may paint text without choosing a colour.
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
        // Album and artist search tabs draw their own row builders.
        for tab in [SearchTab::Albums, SearchTab::Artists, SearchTab::Playlists] {
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
        st.playback = Some(playing("spotify:track:Beta"));
        let lines = render(&mut st, 90, 14);
        // Trail + blank, then the band (summary, ▶ play, spacer),
        // then the column header + spacer, then the rows.
        assert!(lines[0].contains("MY LIST"));
        assert!(lines[4].contains("My List"));
        assert!(lines[4].contains("3 tracks · 4 min"));
        assert!(lines[5].contains("▶ play"));
        assert!(!st.hit.header_play_btn.is_empty());
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
        st.playback = Some(playing("spotify:track:Zebra"));
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

    /// The mark or nothing. An unsaved row is blank rather than wearing a
    /// hollow twin: one shape at two brightnesses reads as one thing lit two
    /// ways, and the page ends up speckled with a glyph that means "no".
    #[test]
    fn the_liked_column_marks_saved_rows_and_leaves_the_rest_blank() {
        let mut st = tracks_state(vec![
            track("Alpha", "A"),
            track("Beta", "B"),
            track("Gamma", "C"),
        ]);
        st.liked.insert("spotify:track:Alpha".into(), true);
        st.liked.insert("spotify:track:Beta".into(), false);
        // Label, blank, band(3), header, spacer, then the rows from y7.
        let lines = render(&mut st, 90, 13);
        let mark = super::super::table::LIKED_MARK;
        assert!(lines[9].contains(mark), "{:?}", lines[9]);
        // Unsaved and unchecked look the same, and neither wears a twin.
        for row in [10, 11] {
            assert!(!lines[row].contains(mark), "row {row}: {:?}", lines[row]);
        }
    }

    /// The blank cell has to invite the pointer somehow, so hovering it brings
    /// the mark up — the same one a saved row wears, on the same hover pill
    /// every other clickable cell gets.
    #[test]
    fn hovering_the_liked_column_offers_the_mark() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        // Draw once to find out where the column landed.
        render(&mut st, 90, 12);
        let col = st.hit.main_like_col;
        assert!(!col.is_empty());
        st.mouse_pos = Some(Position {
            x: col.x,
            y: col.y + 1,
        });

        let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let cell = buffer
            .cell(Position {
                x: col.x,
                y: col.y + 1,
            })
            .unwrap();
        assert_eq!(
            cell.symbol(),
            super::super::table::LIKED_MARK,
            "the hover offered no mark"
        );
        assert_eq!(cell.bg, theme::DIM, "the hover drew no pill");
        // The row the pointer is not on stays blank.
        assert_eq!(
            buffer
                .cell(Position { x: col.x, y: col.y })
                .unwrap()
                .symbol(),
            " "
        );
    }

    /// The column is a click target, so its rect has to sit over the glyphs it
    /// covers — the whole two cells, not just the one the mark is drawn in.
    #[test]
    fn the_liked_column_records_a_clickable_rect() {
        let mut st = tracks_state(vec![track("Alpha", "A"), track("Beta", "B")]);
        st.liked.insert("spotify:track:Alpha".into(), true);
        let mut terminal = Terminal::new(TestBackend::new(90, 14)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        let col = st.hit.main_like_col;
        assert_eq!(col.width, 2);
        assert_eq!(col.height, 2, "the column outran the filled rows");
        assert!(st.hit.main_list.contains(Position { x: col.x, y: col.y }));
        assert_eq!(
            buffer
                .cell(Position { x: col.x, y: col.y })
                .unwrap()
                .symbol(),
            super::super::table::LIKED_MARK
        );
        // And it stops where the title starts.
        assert!(col.right() <= st.hit.main_artist_col.x);
    }

    #[test]
    fn header_band_absent_on_short_panes() {
        let mut st = tracks_state(vec![track("A", "B")]);
        let lines = render(&mut st, 80, 10); // body height 6 < 8
        assert!(!lines.iter().any(|l| l.contains("▶ play")));
        assert!(st.hit.header_play_btn.is_empty());
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
        assert!(lines[0].contains("MY LIST (LOADING…)"));
        assert!(lines[4].contains("0 of 200 tracks"));
        assert!(!lines.iter().any(|l| l.contains("this playlist is empty")));
    }

    /// The row names the page, not the page's kind.
    ///
    /// It used to say `ALBUM` or `LIKED SONGS` — the kind — and hang a back
    /// pill off the end of that. The kind is what the header band under it
    /// already tells you, with a sleeve and a year or with an owner, so the
    /// row spends itself on the path instead, which nothing else was saying.
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
            assert!(lines[0].contains("MY LIST"), "{kind:?}: {:?}", lines[0]);
        }
    }

    fn search_state() -> AppState {
        let mut st = AppState::new();
        st.main = MainView::Search(SearchResults {
            query: "muse".into(),
            tracks: vec![track("Starlight", "Muse")],
            albums: vec![crate::app::state::AlbumItem {
                id: "a1".into(),
                name: "Black Holes".into(),
                artists: "Muse".into(),
                release_year: "2006".into(),
                album_type: "album".into(),
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
                uri: "spotify:playlist:p1".into(),
                name: "Muse Mix".into(),
                track_count: 42,
                owner: "someone".into(),
                owner_id: "someone".into(),
                snapshot_id: "s1".into(),
            }],
        });
        st
    }

    #[test]
    fn search_tab_hit_rects_match_rendered_labels() {
        let mut st = search_state();
        let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
        terminal.draw(|f| screen(&mut st, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_eq!(st.hit.search_tabs.len(), 4);
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
    /// table is: one click opens it. Before this the only way in was a
    /// double-click or Enter, which nothing on screen said.
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
        crate::app::state::AlbumItem {
            id: format!("id-{name}"),
            name: name.into(),
            artists: "Muse".into(),
            release_year: year.into(),
            album_type: "album".into(),
            track_count: 12,
            cover_url: cover.map(Into::into),
        }
    }

    fn artist_state() -> AppState {
        let mut st = AppState::new();
        let mut top = crate::app::state::TrackList::new("Muse", "top tracks", None, None);
        top.append(vec![track("Uprising", "Muse")]);
        st.main = MainView::Artist(crate::app::state::ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Muse".into(),
            image_url: Some("https://i.scdn.co/image/artist".into()),
            genres: vec!["alt rock".into(), "space rock".into()],
            top,
            albums: vec![album_item("Black Holes", "2006", None)],
            loading: false,
        });
        st
    }

    /// The artist page wears the album page's header band — portrait where the
    /// sleeve goes, name at the top of the column beside it, ▶ play at the
    /// bottom of it — and puts both sections in one scrolling body under it.
    #[test]
    fn artist_page_stacks_a_portrait_band_over_tracks_then_cards() {
        let mut st = artist_state();
        let lines = render(&mut st, 90, 26);
        assert!(lines[0].contains("MUSE"));
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
        assert!(lines[6].contains("1 album"));
        assert!(!lines[6].contains("top track"));
        assert!(lines[8].contains("▶ play"));
        assert!(!st.hit.header_play_btn.is_empty());

        // Body: the two sections, in order, with no tab strip anywhere. Both
        // headings keep a blank row under them.
        assert!(lines[11].contains("Top Tracks"));
        assert!(lines[12].trim().is_empty());
        assert!(lines[13].contains("Title"));
        assert!(lines[14].contains("Uprising"));
        assert!(lines[16].contains("Albums"));
        assert!(lines[17].trim().is_empty());
        assert!(lines[18].contains("Black Holes"));
        assert!(lines[19].contains("2006 · 12 tracks"));
        assert!(lines[20].contains("▶ play"));
        assert_eq!(st.hit.card_play.len(), 1);
    }

    /// Cards are five lines apiece, so the pane keeps a line model and every
    /// line of a card resolves back to the same row. Without it a click lands
    /// several rows past whatever it was aimed at.
    #[test]
    fn album_cards_map_every_line_back_to_one_row() {
        let mut st = artist_state();
        if let MainView::Artist(v) = &mut st.main {
            v.albums = vec![
                album_item("One", "2001", None),
                album_item("Two", "2002", None),
            ];
        }
        render(&mut st, 90, 32);
        let rows: Vec<Option<usize>> = st.hit.main_lines.clone();
        // Heading, blank, column header, one track, blank, heading, blank,
        // then the cards: four lines each plus a blank.
        assert_eq!(&rows[..7], &[None, None, None, Some(0), None, None, None]);
        assert_eq!(&rows[7..12], &[Some(1), Some(1), Some(1), Some(1), None]);
        assert_eq!(&rows[12..17], &[Some(2), Some(2), Some(2), Some(2), None]);
        assert_eq!(st.hit.album_names.len(), 2);
        assert_eq!(st.hit.card_play.len(), 2);
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

    /// Hovering a link lights the run itself and nothing else. It used to
    /// underline, and every link on these pages is a cell padded out to its
    /// column width — so hovering an album card drew a rule from its name
    /// clear across the pane.
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
