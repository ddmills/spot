//! Shared building blocks for the list/table panes: display-width-aware
//! column fitting, selection styling, hoverable segments, scrollbars, cover
//! art, and the transport controls the player view and the bottom bar share.

use std::sync::OnceLock;
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthChar;

use super::play_state::PlayState;
use super::theme;
use crate::app::state::{Credit, HitAreas};
use crate::cover::Cover;

/// Truncate with an ellipsis or pad with spaces to exactly `w` display
/// columns. Width is measured in terminal cells, so CJK and emoji stay
/// aligned; a truncation that lands mid-wide-char pads with a space.
pub fn fit(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    let total: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= w {
        let mut out = String::with_capacity(s.len() + (w - total));
        out.push_str(s);
        out.extend(std::iter::repeat_n(' ', w - total));
        return out;
    }
    let target = w - 1;
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > target {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    used += 1;
    out.extend(std::iter::repeat_n(' ', w - used));
    out
}

/// Display width of `s` in terminal cells.
///
/// The same measure [`fit`] truncates by, so a caller that checks whether a
/// run will fit and then lays it out gets one answer rather than two. Counting
/// `chars` instead reads a CJK title as half its true width.
pub fn width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Byte index of the first character at or after display column `col`.
///
/// Columns rather than bytes or chars, because that is what [`fit`] and
/// [`width`] measure and what a hit rect is in.
fn byte_at(s: &str, col: usize) -> usize {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        if used >= col {
            return i;
        }
        used += c.width().unwrap_or(0);
    }
    s.len()
}

/// Where one credited artist's name is printed inside a cell: the offset from
/// the cell's left edge and the cells the name takes, both in display columns,
/// with the artist it opens.
///
/// A run whose credit has no id is drawn and not clickable — the rule [`link`]
/// follows for a single run, applied to each name of several.
///
/// The whole credit rides along rather than the id alone, so a click knows the
/// name it landed on as well as the page it opens — the crumb the artist page
/// hangs under is that name, and it is already in hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditRun {
    pub dx: u16,
    pub width: u16,
    pub credit: Credit,
}

/// The credit line as a `w`-cell cell, with the run each name occupies.
///
/// One rule for both halves of the job: the pane records hit rects from the
/// runs and prints the string, and a second rule for either would put a click
/// on a name the pointer was not over. The string is exactly what a cell of
/// this width has always held — [`fit`] of the joined line — so nothing about
/// the printed row changes.
///
/// A name the cell ran out of room for gets no run and cannot be clicked. A
/// name only *part* of which fits keeps a run as wide as what was printed: the
/// ellipsis stands for the rest of that name, and belongs with it.
pub fn credit_line(credits: &[Credit], w: usize) -> (String, Vec<CreditRun>) {
    let cell = fit(&crate::app::state::artists_line(credits), w);
    let inked = width(cell.trim_end());
    let mut runs = Vec::new();
    let mut dx = 0usize;
    for (i, credit) in credits.iter().enumerate() {
        if i > 0 {
            dx += width(crate::app::state::CREDIT_SEP);
        }
        if dx >= inked {
            break;
        }
        let name_w = width(&credit.name);
        let shown = name_w.min(inked - dx);
        if shown > 0 {
            runs.push(CreditRun {
                dx: dx as u16,
                width: shown as u16,
                credit: credit.clone(),
            });
        }
        dx += name_w;
    }
    (cell, runs)
}

/// Spans for a [`credit_line`], with the `hovered` run lit.
///
/// The light goes on one name, not on the whole cell: the names either side of
/// it lead somewhere else, and a pill across all of them would say they lead
/// to the same place.
pub fn credit_spans(
    cell: &str,
    runs: &[CreditRun],
    style: Style,
    hovered: Option<usize>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut cut = 0usize;
    let mut push = |text: &str, style: Style| {
        if !text.is_empty() {
            spans.push(Span::styled(text.to_string(), style));
        }
    };
    for (i, run) in runs.iter().enumerate() {
        let start = byte_at(cell, run.dx as usize);
        let end = byte_at(cell, (run.dx + run.width) as usize);
        push(&cell[cut..start], style);
        push(
            &cell[start..end],
            match hovered == Some(i) {
                true => hover_style(style),
                false => style,
            },
        );
        cut = end;
    }
    push(&cell[cut..], style);
    spans
}

/// Which run of a [`credit_line`] the pointer is on, as an index into `runs`.
///
/// `cell` is where the line starts on screen. Only a run that leads somewhere
/// answers: hovering a name with no page behind it must not light it.
pub fn hovered_credit(cell: Rect, runs: &[CreditRun], mouse: Option<Position>) -> Option<usize> {
    let at = mouse.filter(|m| m.y == cell.y)?;
    runs.iter()
        .position(|run| run.credit.id.is_some() && credit_rect(cell, run).contains(at))
}

/// Where a run lands on screen, clipped to the cell it is printed in.
pub fn credit_rect(cell: Rect, run: &CreditRun) -> Rect {
    Rect {
        x: cell.x.saturating_add(run.dx),
        y: cell.y,
        width: run.width,
        height: 1,
    }
    .intersection(cell)
}

/// Record every clickable name of a [`credit_line`] drawn at `cell`.
///
/// A name Spotify identified by name only gets no entry at all, rather than an
/// entry that leads nowhere — an absent target is how the rest of the app
/// spells inert.
pub fn credit_links(cell: Rect, runs: &[CreditRun], out: &mut Vec<(Rect, Credit)>) {
    for run in runs {
        if run.credit.id.is_none() {
            continue;
        }
        let rect = credit_rect(cell, run);
        if !rect.is_empty() {
            out.push((rect, run.credit.clone()));
        }
    }
}

/// The style the selected row is painted with. Weight and brightness, not a
/// filled bar: a solid accent row would drown out the accent-colored marker
/// and title the playing track already carries.
pub fn selection_style(style: Style) -> Style {
    style.fg(theme::BRIGHT).add_modifier(Modifier::BOLD)
}

/// Restyle every span of a row as selected (bright, bold).
pub fn apply_selection(line: &mut Line<'static>) {
    for span in &mut line.spans {
        span.style = selection_style(span.style);
    }
}

/// The row the pointer is on, as an index into a list's items.
///
/// `rows` is the rect the list renders into and `offset` its first visible
/// item, so the answer follows the scroll. A pointer past the last row of an
/// underfilled list is over no row at all.
pub fn hovered_row(
    rows: Rect,
    offset: usize,
    len: usize,
    mouse: Option<Position>,
) -> Option<usize> {
    let at = mouse.filter(|m| rows.contains(*m))?;
    Some(offset + (at.y - rows.y) as usize).filter(|&row| row < len)
}

/// Wash a row under the pointer with [`theme::row_hover`].
///
/// A row-wide mark, where [`hover_style`] is a word-wide one. The style covers
/// the row's whole width before its spans are drawn, so a cell that lights its
/// own run — one mark of the `★ ⧉ +` run, a clickable artist — keeps its pill on
/// top of the wash rather than being flattened into it.
pub fn hover_row(item: ListItem<'static>, hovered: bool) -> ListItem<'static> {
    match hovered {
        true => item.style(theme::row_hover()),
        false => item,
    }
}

/// Draw a vertical scrollbar in `bar` (a 1-wide column, usually over a pane's
/// right border) when the content overflows its viewport.
pub fn draw_scrollbar(frame: &mut Frame, bar: Rect, len: usize, offset: usize) {
    let viewport = bar.height as usize;
    if viewport == 0 || len <= viewport {
        return;
    }
    let mut state = ScrollbarState::new(len - viewport)
        .position(offset)
        .viewport_content_length(viewport);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("█")
        .track_style(theme::dim())
        .thumb_style(theme::text());
    frame.render_stateful_widget(scrollbar, bar, &mut state);
}

/// Restyle a run as hovered: a dim "pill" behind it, with dim text brightened
/// so it stays readable on top.
///
/// The one hover mark on the screen. A background can only cover the cells the
/// run actually occupies; an underline would also take the padding a name
/// carries out to its column width, drawing a rule clear across the pane.
pub fn hover_style(style: Style) -> Style {
    let mut style = style;
    if style.fg == Some(theme::DIM) {
        style.fg = Some(theme::TEXT);
    }
    style.bg(theme::DIM)
}

/// Append one hoverable segment to a row of spans and return its screen rect.
/// When the mouse is inside the segment, paint a DarkGray "pill" behind it
/// (dim text brightens so it stays readable on the pill).
pub fn segment(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    area: Rect,
    mouse: Option<Position>,
    parts: Vec<Span<'static>>,
) -> Rect {
    let width: usize = parts.iter().map(|p| p.width()).sum();
    let rect = Rect {
        x: *x,
        y: area.y,
        width: width as u16,
        height: 1,
    };
    let hover = mouse.is_some_and(|m| rect.contains(m)) && rect.right() <= area.right();
    for part in parts {
        let style = if hover {
            hover_style(part.style)
        } else {
            part.style
        };
        spans.push(Span::styled(part.content, style));
    }
    *x = x.saturating_add(width as u16);
    rect.intersection(area)
}

/// Append one clickable text run to a row of spans and return its screen rect,
/// advancing `x` past it. Hovering lights the run — see [`hover_style`].
///
/// A run that is not `clickable` returns an empty rect, which can never be
/// hit — so a name whose id the API did not give us is drawn but inert.
pub fn link(
    spans: &mut Vec<Span<'static>>,
    x: &mut u16,
    area: Rect,
    mouse: Option<Position>,
    text: String,
    style: Style,
    clickable: bool,
) -> Rect {
    let span = Span::styled(text, style);
    let rect = Rect {
        x: *x,
        y: area.y,
        width: span.width() as u16,
        height: 1,
    }
    .intersection(area);
    let hover = clickable && mouse.is_some_and(|m| rect.contains(m));
    *x = x.saturating_add(rect.width);
    spans.push(if hover {
        span.style(hover_style(style))
    } else {
        span
    });
    if clickable { rect } else { Rect::default() }
}

/// The wordmark, drawn at the left of `row`, and the Home button it doubles
/// as. Returns its hit rect, which is the mark's own cells and nothing more —
/// a control the width of the screen would swallow clicks meant for whatever
/// sits beside it.
///
/// Two-tone rather than one flat colour: the note takes the accent, which
/// [`theme::accent_color`] re-derives from the playing sleeve, and the word
/// takes [`theme::BRIGHT`], which does not. So the mark drifts with the record
/// on screen without the name of the app ever going with it.
///
/// Both screens draw it from here so they cannot drift apart: it is the same
/// glyphs in the same column whichever view is up, which is what lets `v`
/// toggle between them without the mark appearing to move.
pub fn brand(frame: &mut Frame, row: Rect, mouse: Option<Position>) -> Rect {
    if row.width == 0 || row.height == 0 {
        return Rect::default();
    }
    let mut spans = Vec::new();
    let mut x = row.x;
    let rect = segment(
        &mut spans,
        &mut x,
        row,
        mouse,
        vec![
            Span::styled("♫ ", theme::accent()),
            Span::styled("spot", theme::bright().add_modifier(Modifier::BOLD)),
        ],
    );
    // All or nothing: half a wordmark reads as a rendering fault rather than
    // as a truncation.
    if rect.width < BRAND_W {
        return Rect::default();
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    rect
}

/// Cells the mark occupies: `♫`, a space, and four letters.
pub const BRAND_W: u16 = 6;

/// The mark a saved ("liked") track wears — in the track table's column and
/// on the deck's control, so the two cannot drift.
///
/// A star rather than a heart. `♥` U+2665 is nominally the *black* heart
/// suit, but plenty of terminal fonts draw it as an outline — including the
/// one this was built against — which leaves it indistinguishable from `♡`
/// and turns the saved state into something you cannot read. `★` is solid
/// where `☆` is not, and the app says "liked" in words everywhere it
/// matters anyway.
pub const LIKED_MARK: &str = "★";

/// The share mark, wearing the `⧉` the deck's `⧉ share` wears.
///
/// The copy mark rather than an arrow: the control puts a link on the
/// clipboard, and an arrow out of the app promises to open something.
pub const SHARE_MARK: &str = "⧉";

/// The add-to-playlist mark, wearing the `+` the deck's `+ add` wears.
pub const ADD_MARK: &str = "+";

/// Liked cell of a track row's action run: the mark with a space either
/// side, so the control is a target rather than a single cell to hit.
pub const LIKE_W: usize = 3;
/// Share cell of the run, padded to match.
pub const SHARE_W: usize = 3;
/// Add-to-playlist cell of the run, padded to match.
pub const ADD_W: usize = 3;
/// The `★ ⧉ +` run at the end of a track row. The cells sit flush: each
/// already carries its own padding, and a separator between them would put a
/// gap belonging to no control back in between.
pub const ACTIONS_W: usize = LIKE_W + SHARE_W + ADD_W;
/// Narrowest list that still carries the run. The last thing a narrowing
/// list drops: these are controls, and a row you cannot act on is worth less
/// than a row missing its year.
pub const ACTIONS_MIN: usize = 33;

/// Which control of a track row's action run the pointer is on.
///
/// One enum for the browse table and the player's queue, so the two lists
/// cannot come to disagree about what the run holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    Like,
    Share,
    Add,
}

/// The `★ ⧉ +` run for one row, in [`ACTIONS_W`] cells.
///
/// The star is always drawn — dim when the track is unsaved or its state is
/// still unknown, accent when saved. It is one of three now, and a control
/// that appeared only under the pointer would leave a hole beside its
/// neighbours and shift what the row looks like as the mouse crosses it.
///
/// Each mark carries a space either side, and the hover pill takes that
/// padding with it: the lit run is what says how big the target is, and a
/// pill hugging a single glyph would understate a control you can hit from a
/// cell away. Unlike [`super::main_pane`]'s text cells, which light the text
/// and leave their padding bare — there the padding is a column's leftover,
/// here it is the control.
///
/// Both views draw the run from here so the queue and the browse table
/// cannot drift into two spellings of the same controls.
pub fn action_spans(liked: Option<bool>, hover: Option<RowAction>) -> Vec<Span<'static>> {
    let lit = |style: Style, action: RowAction| match hover == Some(action) {
        true => hover_style(style),
        false => style,
    };
    let like_style = if liked == Some(true) {
        theme::accent()
    } else {
        theme::dim()
    };
    vec![
        Span::styled(format!(" {LIKED_MARK} "), lit(like_style, RowAction::Like)),
        Span::styled(
            format!(" {SHARE_MARK} "),
            lit(theme::dim(), RowAction::Share),
        ),
        Span::styled(format!(" {ADD_MARK} "), lit(theme::dim(), RowAction::Add)),
    ]
}

/// The spinner's frames, in the order they are shown: a weight travelling
/// round a braille cell.
///
/// Every frame is one cell wide, so a row carrying one does not move as it
/// turns. Braille needs a font that has the U+28xx block — most terminal fonts
/// do, but Consolas does not, and there it will come out blank.
pub const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// How long each frame of [`SPINNER`] holds. Eight frames at this rate is
/// about a turn a second, which reads as motion without becoming a flicker.
const SPINNER_FRAME: u128 = 110;

/// The spinner frame for right now.
///
/// Phase comes from one process-wide start, so every spinner on screen turns
/// together — two that had each started their own clock would wobble against
/// each other.
///
/// The frame loop has to be told to keep drawing for this to turn: see
/// [`super::is_animating`].
pub fn spinner() -> &'static str {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed().as_millis();
    SPINNER[(elapsed / SPINNER_FRAME) as usize % SPINNER.len()]
}

/// Whether `line` ends in the nav row's spinning `LOADING`, on whichever
/// frame the spinner happens to be.
///
/// The frame comes from the clock, so a test cannot pin it and must ask this
/// instead. Shared, because both the nav row's own tests and the whole-screen
/// ones look for the same row.
#[cfg(test)]
pub fn ends_with_loading(line: &str) -> bool {
    let line = line.trim_end();
    SPINNER
        .iter()
        .any(|frame| line.ends_with(&format!("{frame} LOADING")))
}

/// A box of `width` × `height` in the middle of `area`, clamped to fit.
///
/// Where the overlays go: the help box and the add-to-playlist box are both
/// laid out this way, so the two land in the same place and read as one kind
/// of thing.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(height.min(area.height)),
            Constraint::Min(0),
        ])
        .split(area);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(width.min(area.width)),
            Constraint::Min(0),
        ])
        .split(v[1]);
    h[1]
}

/// Lay hoverable segments out flush with `row`'s right edge and draw them as
/// one line, returning each group's hit rect. A row too narrow to hold them
/// draws nothing and returns empty rects, which can never be hit — so a
/// control pinned this way is either fully usable or absent, never clipped
/// into a slider whose click mapping has silently lost its far end.
///
/// Neighbouring groups sit one blank cell apart. That cell belongs to no
/// group, so it stays unlit as the pointer crosses it and no label has to
/// carry padding that its own hover pill would then cover.
pub fn right_row(
    frame: &mut Frame,
    row: Rect,
    mouse: Option<Position>,
    groups: Vec<Vec<Span<'static>>>,
) -> Vec<Rect> {
    let gaps = groups.len().saturating_sub(1);
    let total: usize = groups.iter().flatten().map(|s| s.width()).sum::<usize>() + gaps;
    let total = total as u16;
    if total > row.width {
        return vec![Rect::default(); groups.len()];
    }
    let start = row.right() - total;
    let mut spans = Vec::new();
    let mut x = start;
    let rects: Vec<Rect> = groups
        .into_iter()
        .enumerate()
        .map(|(i, g)| {
            let rect = segment(&mut spans, &mut x, row, mouse, g);
            if i < gaps {
                spans.push(Span::raw(" "));
                x = x.saturating_add(1);
            }
            rect
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x: start,
            width: total,
            ..row
        },
    );
    rects
}

/// Spans for a slider track: elapsed `━` (accent) then remainder `─` (dim).
///
/// With `knob`, the boundary cell becomes a `●` whose position matches the
/// click mapping `x_offset / (width - 1)` used in the event layer, and `hover`
/// brightens it as a "you can click" hint. The volume slider draws one; the
/// progress track does not, in either view — it is a readout of where the
/// record is, and a handle on a line that moves by itself reads as one more
/// control rather than as a position.
pub fn meter(ratio: f64, width: u16, knob: bool, hover: bool) -> Vec<Span<'static>> {
    let ratio = ratio.clamp(0.0, 1.0);
    if !knob {
        // Half-cell resolution: `╸` is the left half of `━`, so the boundary
        // cell can show a half step. Rounding to whole cells makes a
        // five-minute track advance in visible ~6-second jumps.
        let exact = ratio * width as f64;
        let filled = exact.floor() as u16;
        let half = exact - filled as f64 >= 0.5 && filled < width;
        let rest = width.saturating_sub(filled + half as u16);
        return vec![
            Span::styled("━".repeat(filled as usize), theme::accent()),
            Span::styled(if half { "╸" } else { "" }, theme::accent()),
            Span::styled("─".repeat(rest as usize), theme::dim()),
        ];
    }
    let at = (ratio * width.saturating_sub(1) as f64).round() as u16;
    let knob_style = if hover {
        Style::default()
            .fg(theme::accent_bright())
            .add_modifier(Modifier::BOLD)
    } else {
        theme::accent()
    };
    vec![
        Span::styled("━".repeat(at as usize), theme::accent()),
        Span::styled("●", knob_style),
        Span::styled(
            "─".repeat(width.saturating_sub(at + 1) as usize),
            theme::dim(),
        ),
    ]
}

/// Cells an `rows`-row cover-art block occupies horizontally.
///
/// A terminal cell is about twice as tall as it is wide, and [`draw_art`]
/// stacks two pixels per cell, so an R-row block `2R` cells wide is square on
/// screen and square in pixels.
pub const fn art_w(rows: u16) -> u16 {
    rows * 2
}

/// Paint `area` with `cover`, or with a placeholder swatch when there is none.
///
/// Cells are `▀`, the foreground carrying the upper pixel and the background
/// the lower one, so each cell holds two vertically stacked pixels. See
/// [`art_w`] for why that makes the block square.
///
/// Every cell must set its background: an unset one lets the terminal's own
/// paint through the bottom half of the glyph, and the art reads as stripes.
///
/// `seed` picks the placeholder's swatch, and should be the album id so a given
/// record always gets the same one.
pub fn draw_art(frame: &mut Frame, area: Rect, cover: Option<&Cover>, seed: &str) {
    draw_art_clipped(frame, area, area, cover, seed);
}

/// [`draw_art`], but painting only the cells of `area` that fall inside
/// `clip`.
///
/// The image is still resampled for the whole of `area`, so a block scrolling
/// past the edge of a list slides under it rather than squashing into what is
/// left — the same picture, with part of it off screen.
pub fn draw_art_clipped(
    frame: &mut Frame,
    area: Rect,
    clip: Rect,
    cover: Option<&Cover>,
    seed: &str,
) {
    if area.width == 0 || area.height == 0 || clip.is_empty() {
        return;
    }
    let (cols, rows) = (area.width as usize, area.height as usize);
    let px = match cover {
        Some(c) => c.block(cols, rows),
        None => placeholder(seed, cols, rows),
    };
    let rgb = |p: [u8; 3]| Color::Rgb(p[0], p[1], p[2]);
    let buf = frame.buffer_mut();
    for row in 0..rows {
        for col in 0..cols {
            let at = Position {
                x: area.x + col as u16,
                y: area.y + row as u16,
            };
            if !clip.contains(at) {
                continue;
            }
            let (top, bottom) = (px[2 * row * cols + col], px[(2 * row + 1) * cols + col]);
            if let Some(cell) = buf.cell_mut(at) {
                cell.set_char('▀')
                    .set_style(Style::default().fg(rgb(top)).bg(rgb(bottom)));
            }
        }
    }
    if cover.is_none() {
        // A note in the middle, so a block with no art reads as artwork that
        // has not arrived rather than as a coloured hole. It keeps the swatch
        // behind it as its own background instead of punching through.
        let (cx, cy) = (cols / 2, rows / 2);
        let at = Position {
            x: area.x + cx as u16,
            y: area.y + cy as u16,
        };
        if let Some(cell) = clip.contains(at).then(|| buf.cell_mut(at)).flatten() {
            cell.set_char('♫').set_style(
                Style::default()
                    .fg(theme::BRIGHT)
                    .bg(rgb(px[(2 * cy + 1) * cols + cx])),
            );
        }
    }
}

/// A stable two-tone diagonal gradient for `seed`, at the block's own
/// resolution. Cheap enough to recompute every frame, so it needs no cache
/// and lands in exactly the place and size the real cover will.
fn placeholder(seed: &str, cols: usize, rows: usize) -> Vec<[u8; 3]> {
    // FNV-1a: a hash that is stable across runs, unlike `DefaultHasher`.
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325u64, |acc, b| {
        (acc ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let (from, to) = theme::PLACEHOLDER[hash as usize % theme::PLACEHOLDER.len()];
    let (w, h) = (cols, rows * 2);
    let span = (w + h).saturating_sub(2).max(1) as f32;
    (0..h)
        .flat_map(|y| {
            (0..w).map(move |x| {
                let t = (x + y) as f32 / span;
                let ch =
                    |i: usize| (from[i] as f32 + (to[i] as f32 - from[i] as f32) * t).round() as u8;
                [ch(0), ch(1), ch(2)]
            })
        })
        .collect()
}

/// The play-state pill. The word is padded out to a fixed width so the run
/// cannot shift under the cursor when it toggles; the run itself carries no
/// padding, so the hover pill covers the text and nothing else.
///
/// Glyph and word both name what a click does rather than the state it is in:
/// `■ pause` while audio is running, `▶ play` when it is not — the same `▶`
/// the album and artist pages put on their own play pills, and the square
/// transports have used for stop since tape decks.
///
/// Both states take their colour from the accent in force — at full strength
/// while running, held back when stopped — so the transport sits in whatever
/// colour the playing sleeve has put on the rest of the screen. Neither state
/// moves: the header's dot is where liveness is shown, and a button that
/// breathes under the pointer is just something to flinch at.
///
/// `None` on [`PlayState::Loading`], which is the pill's third face and the
/// only one that is not a control: the header corner already says `LOADING`,
/// in the same colour, in the place the eye goes for that. There is no state
/// left to report and nothing a click could do, so the caller leaves
/// `hit.play_btn` empty to match.
pub fn state_spans(play: PlayState) -> Option<Vec<Span<'static>>> {
    let is_playing = match play {
        PlayState::Playing => true,
        PlayState::Paused => false,
        PlayState::Loading => return None,
    };
    let (glyph, word) = if is_playing {
        ("■", "pause")
    } else {
        ("▶", "play")
    };
    let style = if is_playing {
        theme::accent()
    } else {
        theme::stopped_dim()
    };
    Some(vec![
        Span::styled(glyph, style),
        Span::styled(format!(" {:<5}", word), style),
    ])
}

/// Width of the volume-slider track, in cells. The player view and the bottom
/// bar draw the same control, so they share the width as well as the code.
pub const VOL_TRACK_W: u16 = 16;

/// The volume slider, flush with `row`'s right edge. Records
/// `hit.volume_slider` over the track alone — the label and the readout do
/// not map to a percent — and returns the whole segment's rect.
///
/// It always carries the `●` handle, whose position matches the click mapping
/// the event layer applies to `hit.volume_slider`: both views make the track
/// clickable, so both show what there is to grab. The progress track
/// deliberately does not (see [`meter`]).
pub fn draw_volume(
    frame: &mut Frame,
    row: Rect,
    volume_percent: u8,
    mouse: Option<Position>,
    hit: &mut HitAreas,
) -> Rect {
    let dim = theme::dim();
    let label = Span::styled("vol ", dim);
    let pct = Span::styled(format!(" {volume_percent:>3}%"), dim);
    let label_w = label.width() as u16;
    let seg_w = label_w + VOL_TRACK_W + pct.width() as u16;
    let hover =
        mouse.is_some_and(|m| m.y == row.y && seg_w <= row.width && m.x >= row.right() - seg_w);
    let mut parts = vec![label];
    parts.extend(meter(
        f64::from(volume_percent) / 100.0,
        VOL_TRACK_W,
        true,
        hover,
    ));
    parts.push(pct);
    let seg = right_row(frame, row, mouse, vec![parts])[0];
    hit.volume_slider = if seg.is_empty() {
        Rect::default()
    } else {
        Rect {
            x: seg.x + label_w,
            width: VOL_TRACK_W,
            ..seg
        }
        .intersection(row)
    };
    seg
}

#[cfg(test)]
mod tests {
    use ratatui::layout::{Position, Rect};
    use ratatui::style::Style;

    use super::{
        ACTIONS_W, ADD_MARK, Credit, LIKED_MARK, RowAction, SHARE_MARK, SPINNER, SPINNER_FRAME,
        action_spans, credit_line, credit_links, credit_spans, fit, hovered_credit, hovered_row,
        spinner, width,
    };

    fn credits(names: &[(&str, Option<&str>)]) -> Vec<Credit> {
        names
            .iter()
            .map(|(name, id)| Credit {
                name: (*name).into(),
                id: id.map(Into::into),
            })
            .collect()
    }

    /// A four-row list at (10, 5), 20 wide.
    fn rows() -> Rect {
        Rect {
            x: 10,
            y: 5,
            width: 20,
            height: 4,
        }
    }

    fn at(x: u16, y: u16) -> Option<Position> {
        Some(Position { x, y })
    }

    /// The cell prints exactly what one link ever printed — the joined line,
    /// fitted — and the runs land on the names inside it.
    #[test]
    fn a_credit_line_splits_the_cell_it_prints() {
        let c = credits(&[("Zedd", Some("z")), ("Alessia Cara", Some("a"))]);
        let (cell, runs) = credit_line(&c, 20);
        assert_eq!(cell, "Zedd, Alessia Cara  ");
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].dx, runs[0].width), (0, 4));
        // Past the two cells of `, `, which belong to neither name.
        assert_eq!((runs[1].dx, runs[1].width), (6, 12));
    }

    /// A name only part of which fits keeps a run as wide as what was printed:
    /// the ellipsis stands for the rest of that name and belongs with it. A
    /// name there was no room for at all gets no run.
    #[test]
    fn a_clipped_credit_line_stops_at_what_it_printed() {
        let c = credits(&[("Zedd", Some("z")), ("Alessia Cara", Some("a"))]);
        let (cell, runs) = credit_line(&c, 10);
        assert_eq!(cell, "Zedd, Ale…");
        assert_eq!(runs.len(), 2);
        // Four cells: `Ale` and the `…` that stands for the rest of the name.
        assert_eq!((runs[1].dx, runs[1].width), (6, 4));

        let (cell, runs) = credit_line(&c, 4);
        assert_eq!(cell, "Zed…");
        assert_eq!(runs.len(), 1, "a name with no room is not a target");
    }

    /// Only a name with an id is a target; one Spotify identified by name
    /// alone is drawn and inert.
    #[test]
    fn a_credit_without_an_id_is_not_a_target() {
        let c = credits(&[("Zedd", None), ("Alessia Cara", Some("a"))]);
        let cell = Rect::new(10, 5, 20, 1);
        let (_, runs) = credit_line(&c, 20);
        let mut links = Vec::new();
        credit_links(cell, &runs, &mut links);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1.name, "Alessia Cara");
        assert_eq!(links[0].0, Rect::new(16, 5, 12, 1));
        // Nor does it light under the pointer.
        assert_eq!(hovered_credit(cell, &runs, at(11, 5)), None);
        assert_eq!(hovered_credit(cell, &runs, at(17, 5)), Some(1));
        // A row above or below the line is not on it at all.
        assert_eq!(hovered_credit(cell, &runs, at(17, 6)), None);
    }

    /// The spans put back together exactly what [`credit_line`] printed, so
    /// splitting the cell to light one name cannot change the row.
    #[test]
    fn credit_spans_rebuild_the_cell_they_split() {
        let c = credits(&[("Zedd", Some("z")), ("Alessia Cara", Some("a"))]);
        for w in [0, 1, 4, 7, 10, 18, 20, 40] {
            let (cell, runs) = credit_line(&c, w);
            for lit in [None, Some(0), Some(1)] {
                let joined: String = credit_spans(&cell, &runs, Style::default(), lit)
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                assert_eq!(joined, cell, "width {w}, lit {lit:?}");
            }
        }
    }

    #[test]
    fn a_hovered_row_is_the_one_under_the_pointer() {
        assert_eq!(hovered_row(rows(), 0, 4, at(12, 5)), Some(0));
        assert_eq!(hovered_row(rows(), 0, 4, at(29, 8)), Some(3));
    }

    /// The offset is added, so the answer names the item the pointer is on
    /// rather than the screen row it landed on.
    #[test]
    fn a_scrolled_list_hovers_the_item_it_shows() {
        assert_eq!(hovered_row(rows(), 7, 20, at(12, 5)), Some(7));
    }

    #[test]
    fn a_pointer_off_the_list_hovers_nothing() {
        assert_eq!(hovered_row(rows(), 0, 4, at(9, 5)), None);
        assert_eq!(hovered_row(rows(), 0, 4, at(12, 4)), None);
        assert_eq!(hovered_row(rows(), 0, 4, at(12, 9)), None);
        assert_eq!(hovered_row(rows(), 0, 4, None), None);
    }

    /// A list with fewer items than rows leaves blank cells at the bottom, and
    /// a pointer resting on one of them is over no row at all.
    #[test]
    fn a_pointer_past_the_last_row_hovers_nothing() {
        assert_eq!(hovered_row(rows(), 0, 2, at(12, 6)), Some(1));
        assert_eq!(hovered_row(rows(), 0, 2, at(12, 7)), None);
    }

    #[test]
    fn fit_pads_short_strings_to_width() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("", 3), "   ");
    }

    #[test]
    fn fit_truncates_with_ellipsis() {
        assert_eq!(fit("abcdef", 4), "abc…");
    }

    #[test]
    fn fit_measures_wide_glyphs_in_cells() {
        // Each CJK char is 2 cells; "残酷" = 4 cells.
        assert_eq!(fit("残酷", 4), "残酷");
        assert_eq!(fit("残酷", 5), "残酷 ");
        // Truncating "残酷な" (6 cells) to 5: "残酷" (4) + "…" = 5.
        assert_eq!(fit("残酷な", 5), "残酷…");
        // Truncating to 4: only "残" (2) fits before the ellipsis; pad 1.
        assert_eq!(fit("残酷な", 4), "残… ");
    }

    #[test]
    fn fit_handles_zero_and_tiny_widths() {
        assert_eq!(fit("abc", 0), "");
        assert_eq!(fit("abc", 1), "…");
        assert_eq!(fit("残", 1), "…");
    }

    /// Every frame is one cell, so a row carrying the spinner keeps its width
    /// as it turns and nothing beside it steps sideways.
    #[test]
    fn every_spinner_frame_is_one_cell() {
        for frame in SPINNER {
            assert_eq!(width(frame), 1, "{frame:?}");
        }
    }

    /// Whatever the clock says, the glyph is one of the frames.
    #[test]
    fn the_spinner_reads_off_its_own_frames() {
        assert!(SPINNER.contains(&spinner()), "{:?}", spinner());
    }

    /// The frame is a function of elapsed time, so the phase advances rather
    /// than sitting on whichever frame the process started on.
    #[test]
    fn the_spinner_turns() {
        let first = spinner();
        std::thread::sleep(std::time::Duration::from_millis(SPINNER_FRAME as u64 + 20));
        assert_ne!(first, spinner());
    }

    /// The run reads `★ ⧉ +` and fills exactly the cells the column reserves,
    /// so a table laid out from [`ACTIONS_W`] cannot come up short.
    #[test]
    fn the_action_run_fills_its_column_in_order() {
        let spans = action_spans(Some(true), None);
        let printed: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(printed, format!(" {LIKED_MARK}  {SHARE_MARK}  {ADD_MARK} "));
        assert_eq!(width(&printed), ACTIONS_W);
    }

    /// Only the hovered control takes the pill; a run where more than one lit
    /// would say the pointer is on all of them.
    #[test]
    fn one_hover_lights_one_control() {
        for (i, action) in [RowAction::Like, RowAction::Share, RowAction::Add]
            .into_iter()
            .enumerate()
        {
            let spans = action_spans(None, Some(action));
            let lit: Vec<usize> = spans
                .iter()
                .enumerate()
                .filter(|(_, s)| s.style.bg.is_some())
                .map(|(i, _)| i)
                .collect();
            assert_eq!(lit, vec![i], "{action:?}");
        }
    }
}
