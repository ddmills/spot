//! The one column engine every list pane resolves through.
//!
//! A table declares its columns as data; the header, the rows and the click
//! rects are then all read back off the same resolved [`Layout`], so none of
//! the three can drift from the other two. Before this, five structs each
//! carried their own width policy and their own hand-written offset chain.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::table::{ACTIONS_MIN, ACTIONS_W, fit, segment};
use super::theme;
use crate::app::state::{HitAreas, Sort};

/// What separates two columns. Three cells, so a truncated cell still reads as
/// ending before the next one starts.
pub const COL_GAP: &str = "   ";
/// Right-aligned duration cell: "12:34".
pub const DUR_W: usize = 5;
/// Right-aligned "how long ago" cell. The width of the longest reading it
/// prints, "23h ago", which everything shorter is padded out to.
pub const AGO_W: usize = 7;
/// Narrowest pane the `Ago` column is worth its ten cells on. Above the width
/// the artist column needs, so a tight row loses the reading before it loses
/// the name.
pub const AGO_MIN: usize = 60;
/// Four-digit release year.
pub const YEAR_W: usize = 4;
/// Leading marker column: "▶ " playing, "♫ " the playing context, "★ " saved.
/// Every table carries one, so rows line up across pages.
pub const PREFIX_W: usize = 2;
/// Narrowest the track-number column goes before the data widens it.
pub const NO_W: usize = 3;
/// "compilation" is the longest album type Spotify prints.
pub const TYPE_W: usize = 11;
/// Right-aligned track count.
pub const COUNT_W: usize = 6;
/// Right-aligned station count on a facet row, wide enough for the heading
/// over it.
pub const FACET_COUNT_W: usize = 8;
/// ISO 3166-1 alpha-2 code on a country row, wide enough for the heading and
/// its sort mark.
pub const FACET_CODE_W: usize = 6;
/// Right-aligned stream quality ("AAC+ 128k", "HLS").
pub const QUALITY_W: usize = 10;
/// Country code, or the name where the directory reported no code.
pub const WHERE_W: usize = 6;
/// The saved mark on a station row, under the word that names it.
pub const SAVED_W: usize = 5;

/// Reserved at the right of every list pane: a blank column, then the
/// scrollbar. With no border to hang the bar on it needs columns of its own,
/// kept outside the content rect so a click on it cannot resolve to a row.
///
/// Two rather than one because the last column of most tables is a
/// right-aligned number, and a duration flush against the scrollbar reads as
/// one mark rather than two.
pub const GUTTER: u16 = 2;

/// The scrollbar column for a content rect: the far side of the gutter.
pub fn scroll_col(body: Rect) -> Rect {
    Rect {
        x: body.right() + GUTTER - 1,
        y: body.y,
        width: 1,
        height: body.height,
    }
}

/// Every column identity in the app, and the sort key with it.
///
/// One enum for both: sorting by a column and naming a column are the same
/// act, and two enums for it would let a header mark a column the sort does
/// not touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColKey {
    /// Fetch order. The `#` column *is* the order the source sent, which is
    /// the only order playback follows.
    No,
    Title,
    Artist,
    Album,
    Year,
    Time,
    Type,
    Tracks,
    Owner,
    Station,
    /// What a station is announcing. Not sortable: it is a reading of a live
    /// stream, and a sort over it would reorder the page under you every time
    /// one of the records changed.
    Now,
    Tags,
    Where,
    Stream,
    Name,
    Stations,
    /// The country code a facet row queries by.
    Code,
    /// Whether a station is one you keep. Not sortable: the favourites are a file
    /// of spot's own and are not on the row itself.
    Saved,
    /// How long ago a station announced a record, on the list of what it has
    /// played. A reading that changes under the reader rather than a value, so
    /// nothing sorts by it.
    Ago,
    /// The `★ ⧉ +` run. Controls rather than values, so it heads nothing
    /// sortable.
    Actions,
    /// The leading marker column.
    Mark,
}

impl ColKey {
    /// The column heading.
    pub fn head(self) -> &'static str {
        match self {
            ColKey::No => "#",
            ColKey::Title => "Title",
            ColKey::Artist => "Artist",
            ColKey::Album => "Album",
            ColKey::Year => "Year",
            ColKey::Type => "Type",
            ColKey::Tracks => "Tracks",
            ColKey::Owner => "Owner",
            ColKey::Station => "Station",
            ColKey::Now => "Now Playing",
            ColKey::Tags => "Tags",
            ColKey::Where => "Where",
            ColKey::Stream => "Stream",
            ColKey::Name => "Name",
            ColKey::Stations => "Stations",
            ColKey::Code => "Code",
            ColKey::Saved => "Saved",
            // Headed, unlike the two below it: "6:59" says what it is and
            // "42m ago" only says what it is once you know what it counts.
            ColKey::Ago => "Ago",
            // The `★ ⧉ +` run and the duration head themselves: a row of marks
            // and a column of times both say what they are, and a label over
            // either is a word spent on nothing.
            ColKey::Time | ColKey::Actions | ColKey::Mark => "",
        }
    }
}

/// How a column claims its cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    Fixed(usize),
    /// A share of what the fixed columns leave, by weight.
    Flex(u16),
    /// Fixed, but never narrower than the largest number the list prints in
    /// it.
    Grow(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// One declared column.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    pub key: ColKey,
    pub label: &'static str,
    pub width: Width,
    pub align: Align,
    /// Pane width under which the column is dropped; 0 = never.
    pub drop_below: usize,
    pub sortable: bool,
}

impl Column {
    pub fn new(key: ColKey, width: Width) -> Self {
        Self {
            key,
            label: key.head(),
            width,
            align: Align::Left,
            drop_below: 0,
            sortable: false,
        }
    }

    pub fn labelled(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }

    pub fn right(mut self) -> Self {
        self.align = Align::Right;
        self
    }

    pub fn drop_below(mut self, width: usize) -> Self {
        self.drop_below = width;
        self
    }

    pub fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }
}

/// A column the pane was wide enough to keep, with the cells it holds.
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub key: ColKey,
    pub label: &'static str,
    pub width: usize,
    pub align: Align,
    /// Cells from the row start. This replaces what used to be a
    /// hand-written offset chain in each of two table renderers.
    pub x: usize,
    pub sortable: bool,
}

/// The columns of one table at one pane width.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    cells: Vec<Cell>,
}

impl Layout {
    /// Settle `cols` into a pane `width` cells wide.
    ///
    /// `grow` is the largest number the list prints in a [`Width::Grow`]
    /// column, which is what widens it.
    pub fn resolve(cols: &[Column], width: usize, grow: usize) -> Self {
        let digits = grow.to_string().len();
        let kept: Vec<&Column> = cols
            .iter()
            .filter(|c| c.drop_below == 0 || width >= c.drop_below)
            .collect();
        let base = |c: &Column| match c.width {
            Width::Fixed(n) => n,
            Width::Grow(min) => min.max(digits),
            Width::Flex(_) => 0,
        };
        // The marker column runs straight into the one beside it: it holds a
        // glyph and a space, and that space is the gap.
        let gap = |prev: ColKey| {
            if prev == ColKey::Mark {
                0
            } else {
                COL_GAP.len()
            }
        };
        let mut fixed = 0;
        let mut gaps = 0;
        for (i, c) in kept.iter().enumerate() {
            fixed += base(c);
            if i > 0 {
                gaps += gap(kept[i - 1].key);
            }
        }
        let leftover = width.saturating_sub(fixed + gaps);
        let weight: usize = kept
            .iter()
            .map(|c| match c.width {
                Width::Flex(w) => usize::from(w),
                _ => 0,
            })
            .sum();
        let last_flex = kept.iter().rposition(|c| matches!(c.width, Width::Flex(_)));

        let mut cells = Vec::with_capacity(kept.len());
        let mut spent = 0;
        let mut x = 0;
        for (i, c) in kept.iter().enumerate() {
            if i > 0 {
                x += gap(kept[i - 1].key);
            }
            let cell_w = match c.width {
                // The last flex column takes the remainder, so rounding
                // leaves no cell of the pane unclaimed.
                Width::Flex(_) if weight > 0 && Some(i) == last_flex => leftover - spent,
                Width::Flex(w) if weight > 0 => {
                    let share = leftover * usize::from(w) / weight;
                    spent += share;
                    share
                }
                _ => base(c),
            };
            cells.push(Cell {
                key: c.key,
                label: c.label,
                width: cell_w,
                align: c.align,
                x,
                sortable: c.sortable,
            });
            x += cell_w;
        }
        Self { cells }
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn cell(&self, key: ColKey) -> Option<&Cell> {
        self.cells.iter().find(|c| c.key == key)
    }

    /// Width of a column; 0 when the pane was too narrow to keep it.
    pub fn width_of(&self, key: ColKey) -> usize {
        self.cell(key).map_or(0, |c| c.width)
    }

    /// The sortable columns this pane is showing, so `o` can never name a
    /// column that is not on it.
    pub fn sort_keys(&self) -> Vec<ColKey> {
        self.cells
            .iter()
            .filter(|c| c.sortable)
            .map(|c| c.key)
            .collect()
    }

    /// The header spans, with each sortable label hit rect recorded.
    ///
    /// `row` is the screen row the header lands on, which the artist page
    /// moves as its body scrolls: off screen [`segment`] clips every rect to
    /// nothing, so a label cannot be clicked where it is not drawn.
    pub fn header_line(
        &self,
        sort: Option<Sort>,
        row: Rect,
        mouse: Option<Position>,
        hit: &mut HitAreas,
    ) -> Line<'static> {
        hit.column_headers.clear();
        hit.sort_keys = self.sort_keys();
        let dim = theme::dim();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut x = row.x;
        for (i, cell) in self.cells.iter().enumerate() {
            if i > 0 && self.cells[i - 1].key != ColKey::Mark {
                spans.push(Span::raw(COL_GAP));
                x += COL_GAP.len() as u16;
            }
            // Trimmed back off the padding `fit` adds, so a recorded rect
            // covers the glyphs of the label and not the leftover of the
            // column.
            let text = fit(&sort_label(cell.label, cell.key, sort), cell.width)
                .trim_end()
                .to_string();
            let pad = cell.width.saturating_sub(super::table::width(&text));
            // A column that heads itself with nothing still sorts where it is
            // drawn — the whole cell is the target, there being no label to
            // aim at, and it lights under the pointer to say so.
            if cell.sortable && text.is_empty() {
                let blank = vec![Span::raw(" ".repeat(cell.width))];
                let rect = segment(&mut spans, &mut x, row, mouse, blank);
                hit.column_headers.push((rect, cell.key));
                continue;
            }
            if cell.align == Align::Right && pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
                x += pad as u16;
            }
            if cell.sortable {
                let rect = segment(
                    &mut spans,
                    &mut x,
                    row,
                    mouse,
                    vec![Span::styled(text, dim)],
                );
                hit.column_headers.push((rect, cell.key));
            } else {
                x += super::table::width(&text) as u16;
                spans.push(Span::styled(text, dim));
            }
            if cell.align == Align::Left && pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
                x += pad as u16;
            }
        }
        Line::from(spans)
    }

    /// Draw the header and the blank spacer under it, and return the rows
    /// rect below them.
    ///
    /// The spacer goes in only where there is room for it: on a two-row pane a
    /// header and one row beat a header and a blank.
    pub fn draw_header(
        &self,
        frame: &mut Frame,
        inner: Rect,
        sort: Option<Sort>,
        mouse: Option<Position>,
        hit: &mut HitAreas,
    ) -> Rect {
        if inner.height < 2 {
            return inner;
        }
        let row = Rect { height: 1, ..inner };
        let line = self.header_line(sort, row, mouse, hit);
        frame.render_widget(Paragraph::new(line), row);
        let skip = if inner.height >= 3 { 2 } else { 1 };
        Rect {
            y: inner.y + skip,
            height: inner.height - skip,
            ..inner
        }
    }
}

/// Build a row through `layout`: `cell` is called once per kept column and
/// appends that column's spans, and the gap between them is put in here.
///
/// A row and its header walk the same cells in the same order, which is what
/// stops the two coming to disagree about where a column starts.
pub fn row_spans(
    layout: &Layout,
    mut cell: impl FnMut(&Cell, &mut Vec<Span<'static>>),
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, c) in layout.cells().iter().enumerate() {
        if i > 0 && layout.cells()[i - 1].key != ColKey::Mark {
            spans.push(Span::raw(COL_GAP));
        }
        cell(c, &mut spans);
    }
    spans
}

/// `text` in exactly `w` cells, flush right.
///
/// [`fit`] pads on the right, so it would left-align a cell under a
/// right-aligned label.
pub fn right(text: &str, w: usize) -> String {
    let cell = fit(text, w);
    let text = cell.trim_end();
    let pad = w.saturating_sub(super::table::width(text));
    format!("{}{text}", " ".repeat(pad))
}

/// Column label with a ▲/▼ marker when it is the active sort column.
///
/// A space before the mark, so it reads as something said about the column
/// rather than as the last letter of its name.
fn sort_label(base: &str, key: ColKey, sort: Option<Sort>) -> String {
    let Some(s) = sort.filter(|s| s.key == key) else {
        return base.to_string();
    };
    let mark = if s.ascending { "▲" } else { "▼" };
    // A column with no heading still shows the mark, which is the whole of
    // what it has to say.
    match base.is_empty() {
        true => mark.to_string(),
        false => format!("{base} {mark}"),
    }
}

/// Track lists, artist top tracks, and the Tracks tab of a search.
///
/// Narrow panes drop the year first, then the album, then the track number;
/// the action pair outlives all three.
///
/// An album page drops the Album column outright, whatever the width: every
/// row of it would name the record you are already on, which is a column
/// spent saying the title in the band above.
pub fn tracks(album: bool) -> Vec<Column> {
    let mut cols = vec![
        Column::new(ColKey::Mark, Width::Fixed(PREFIX_W)),
        Column::new(ColKey::No, Width::Grow(NO_W))
            .right()
            .drop_below(40)
            .sortable(),
        Column::new(ColKey::Title, Width::Flex(4)).sortable(),
        Column::new(ColKey::Artist, Width::Flex(3)).sortable(),
    ];
    if !album {
        cols.push(
            Column::new(ColKey::Album, Width::Flex(3))
                .drop_below(50)
                .sortable(),
        );
    }
    cols.extend([
        Column::new(ColKey::Year, Width::Fixed(YEAR_W))
            .drop_below(70)
            .sortable(),
        Column::new(ColKey::Actions, Width::Fixed(ACTIONS_W)).drop_below(ACTIONS_MIN),
        Column::new(ColKey::Time, Width::Fixed(DUR_W))
            .right()
            .sortable(),
    ]);
    cols
}

/// The Albums tab of a search. The records on an artist page keep their card
/// form.
pub fn albums() -> Vec<Column> {
    vec![
        Column::new(ColKey::Mark, Width::Fixed(PREFIX_W)),
        Column::new(ColKey::Album, Width::Flex(1)).sortable(),
        Column::new(ColKey::Artist, Width::Flex(1)).sortable(),
        Column::new(ColKey::Year, Width::Fixed(YEAR_W))
            .drop_below(55)
            .sortable(),
        Column::new(ColKey::Type, Width::Fixed(TYPE_W))
            .drop_below(55)
            .sortable(),
        Column::new(ColKey::Tracks, Width::Fixed(COUNT_W))
            .right()
            .drop_below(70)
            .sortable(),
    ]
}

/// The Playlists page, and the Playlists tab of a search.
pub fn playlists() -> Vec<Column> {
    vec![
        Column::new(ColKey::Mark, Width::Fixed(PREFIX_W)),
        Column::new(ColKey::Title, Width::Flex(6)).sortable(),
        Column::new(ColKey::Owner, Width::Flex(4)).sortable(),
        Column::new(ColKey::Tracks, Width::Fixed(COUNT_W))
            .right()
            .sortable(),
    ]
}

/// The Artists tab of a search. One name to a row, and it keeps the marker
/// column so the rows line up with every other table.
pub fn artists() -> Vec<Column> {
    vec![
        Column::new(ColKey::Mark, Width::Fixed(PREFIX_W)),
        Column::new(ColKey::Artist, Width::Flex(1)).sortable(),
    ]
}

/// Radio pages and the Stations tab of a search.
///
/// Tags are the first thing dropped: they are the only column a station reads
/// fine without.
pub fn stations(now: bool) -> Vec<Column> {
    // The announcement is only ever asked for on the saved page, so only that
    // page has a column for it. Everywhere else this table is up to
    // `radio::api::STATION_LIMIT` rows deep, and a column there would promise a
    // reading of every one of them.
    let mut cols = vec![
        Column::new(ColKey::Mark, Width::Fixed(PREFIX_W)),
        Column::new(ColKey::Station, Width::Flex(5)).sortable(),
    ];
    if now {
        cols.push(Column::new(ColKey::Now, Width::Flex(6)).drop_below(60));
    }
    // Tags go before the announcement does: what a station played last year is
    // worth less than what it is playing now.
    let tags_min = match now {
        true => 100,
        false => 65,
    };
    cols.extend([
        Column::new(ColKey::Tags, Width::Flex(5))
            .drop_below(tags_min)
            .sortable(),
        Column::new(ColKey::Where, Width::Fixed(WHERE_W)).sortable(),
        // Its own column rather than a second meaning for the marker at the
        // left: whether you keep a station and whether it is playing are two
        // different answers, and one cell cannot give both.
        Column::new(ColKey::Saved, Width::Fixed(SAVED_W)),
        Column::new(ColKey::Stream, Width::Fixed(QUALITY_W))
            .right()
            .sortable(),
    ]);
    cols
}

/// The Countries and Genres pages of the radio directory. `label` is what the
/// rows are, which is the one thing the two lists do not share. `code` heads
/// the row with the code the directory queries a country by; a genre is its
/// own key, so a code column there would say the name twice.
pub fn facets(label: &'static str, code: bool) -> Vec<Column> {
    let mut cols = vec![Column::new(ColKey::Mark, Width::Fixed(PREFIX_W))];
    if code {
        cols.push(Column::new(ColKey::Code, Width::Fixed(FACET_CODE_W)).sortable());
    }
    cols.extend([
        Column::new(ColKey::Name, Width::Flex(1))
            .labelled(label)
            .sortable(),
        Column::new(ColKey::Stations, Width::Fixed(FACET_COUNT_W))
            .right()
            .sortable(),
    ]);
    cols
}

/// The queue in the player view.
///
/// Nothing here is sortable: row `i` plays at position `i`, and a sorted queue
/// would either lie about play order or silently reorder playback. The header
/// says what the columns are and lines its rows up with the browse tables.
pub fn queue(len: usize) -> Vec<Column> {
    let num = len.to_string().len().max(2);
    vec![
        Column::new(ColKey::Mark, Width::Fixed(PREFIX_W)),
        Column::new(ColKey::No, Width::Grow(2)).right(),
        Column::new(ColKey::Title, Width::Flex(60)),
        // Data-dependent, through the width of the number column beside it.
        Column::new(ColKey::Artist, Width::Flex(40)).drop_below(40 + num),
        Column::new(ColKey::Actions, Width::Fixed(ACTIONS_W)).drop_below(ACTIONS_MIN),
        Column::new(ColKey::Time, Width::Fixed(DUR_W)).right(),
    ]
}

/// A station's own list in the player view: what it has played, oldest first.
///
/// The queue's columns and one more. A history is about *when* you heard a
/// record as much as what it was, so the last cell says how long ago the
/// station announced it. Declared beside [`queue`] rather than added to it,
/// because the Spotify queue is a play order and has no answer to give.
///
/// The extra column is the first thing a narrow pane gives up — before the
/// artist, which is what tells two records with the same title apart.
pub fn heard(len: usize) -> Vec<Column> {
    let mut cols = queue(len);
    cols.push(
        Column::new(ColKey::Ago, Width::Fixed(AGO_W))
            .right()
            .drop_below(AGO_MIN),
    );
    cols
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths(layout: &Layout) -> Vec<(ColKey, usize, usize)> {
        layout
            .cells()
            .iter()
            .map(|c| (c.key, c.width, c.x))
            .collect()
    }

    #[test]
    fn flex_weights_split_what_the_fixed_columns_leave() {
        let layout = Layout::resolve(&tracks(false), 100, 12);
        // 100 less the marker, the number, the year, the run, the time and
        // six gaps.
        let flex: usize = layout
            .cells()
            .iter()
            .filter(|c| matches!(c.key, ColKey::Title | ColKey::Artist | ColKey::Album))
            .map(|c| c.width)
            .sum();
        assert_eq!(flex, 100 - (PREFIX_W + 3 + YEAR_W + ACTIONS_W + DUR_W) - 18);
        assert_eq!(layout.width_of(ColKey::Title), flex * 4 / 10);
        assert_eq!(layout.width_of(ColKey::Artist), flex * 3 / 10);
    }

    #[test]
    fn every_cell_starts_where_the_one_before_it_ends() {
        for width in [30, 45, 60, 80, 120] {
            let layout = Layout::resolve(&tracks(false), width, 999);
            let mut x = 0;
            for (i, cell) in layout.cells().iter().enumerate() {
                if i > 0 && layout.cells()[i - 1].key != ColKey::Mark {
                    x += COL_GAP.len();
                }
                assert_eq!(cell.x, x, "column {:?} at width {width}", cell.key);
                x += cell.width;
            }
            assert!(x <= width, "row overflows at width {width}");
        }
    }

    #[test]
    fn narrow_panes_drop_columns_in_order() {
        let at = |w: usize| {
            let layout = Layout::resolve(&tracks(false), w, 9);
            (
                layout.cell(ColKey::Year).is_some(),
                layout.cell(ColKey::Album).is_some(),
                layout.cell(ColKey::No).is_some(),
                layout.cell(ColKey::Actions).is_some(),
            )
        };
        assert_eq!(at(80), (true, true, true, true));
        assert_eq!(at(69), (false, true, true, true));
        assert_eq!(at(49), (false, false, true, true));
        assert_eq!(at(39), (false, false, false, true));
        assert_eq!(at(29), (false, false, false, false));
    }

    #[test]
    fn the_number_column_widens_to_fit_the_largest_number() {
        assert_eq!(
            Layout::resolve(&tracks(false), 100, 9).width_of(ColKey::No),
            NO_W
        );
        assert_eq!(
            Layout::resolve(&tracks(false), 100, 1234).width_of(ColKey::No),
            4
        );
    }

    #[test]
    fn the_queue_drops_its_artist_against_the_number_beside_it() {
        // The threshold moves with the digits of the largest row number.
        assert!(
            Layout::resolve(&queue(9), 42, 9)
                .cell(ColKey::Artist)
                .is_some()
        );
        assert!(
            !Layout::resolve(&queue(9), 41, 9)
                .cell(ColKey::Artist)
                .is_some()
        );
        assert!(
            !Layout::resolve(&queue(1000), 43, 1000)
                .cell(ColKey::Artist)
                .is_some()
        );
        assert!(
            Layout::resolve(&queue(1000), 44, 1000)
                .cell(ColKey::Artist)
                .is_some()
        );
    }

    /// A station's list is the queue's table with one cell more on the end,
    /// and it is the first cell a tight row gives up — before the artist.
    #[test]
    fn a_stations_list_is_the_queue_plus_a_reading_it_sheds_first() {
        let keys = |cols: &[Column], width: usize| -> Vec<ColKey> {
            Layout::resolve(cols, width, 9)
                .cells()
                .iter()
                .map(|c| c.key)
                .collect()
        };
        let mut expected = keys(&queue(9), 90);
        expected.push(ColKey::Ago);
        assert_eq!(keys(&heard(9), 90), expected);

        assert!(keys(&heard(9), AGO_MIN).contains(&ColKey::Ago));
        let tight = keys(&heard(9), AGO_MIN - 1);
        assert!(!tight.contains(&ColKey::Ago));
        assert!(tight.contains(&ColKey::Artist), "the artist outlives it");
    }

    #[test]
    fn a_dropped_column_gives_its_cells_and_its_gap_to_the_flex() {
        let wide = Layout::resolve(&stations(false), 65, 0);
        let narrow = Layout::resolve(&stations(false), 64, 0);
        assert!(wide.cell(ColKey::Tags).is_some());
        assert!(!narrow.cell(ColKey::Tags).is_some());
        assert_eq!(
            narrow.width_of(ColKey::Station),
            wide.width_of(ColKey::Station) + wide.width_of(ColKey::Tags) + COL_GAP.len() - 1
        );
    }

    #[test]
    fn only_the_columns_on_screen_are_sortable() {
        let wide = Layout::resolve(&tracks(false), 100, 9);
        assert!(wide.sort_keys().contains(&ColKey::Year));
        let narrow = Layout::resolve(&tracks(false), 45, 9);
        assert!(!narrow.sort_keys().contains(&ColKey::Year));
        assert!(!narrow.sort_keys().contains(&ColKey::Album));
        assert!(narrow.sort_keys().contains(&ColKey::Title));
    }

    #[test]
    fn the_marker_column_runs_into_the_one_beside_it() {
        let layout = Layout::resolve(&playlists(), 80, 0);
        let cells = widths(&layout);
        assert_eq!(cells[0].0, ColKey::Mark);
        assert_eq!(cells[1].2, PREFIX_W);
    }
}
