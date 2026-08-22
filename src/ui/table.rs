//! Shared building blocks for the list/table panes: display-width-aware
//! column fitting, selection styling, hoverable segments, scrollbars, cover
//! art, and the transport controls the player view and the bottom bar share.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthChar;

use super::theme;
use crate::app::state::HitAreas;
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
/// The one hover mark on the screen. Links used to underline instead, which
/// looked fine on a bare run and wrong everywhere else: a name padded out to
/// its column width underlines the padding too, so hovering a card drew a rule
/// clear across the pane. A background says the same thing and can only cover
/// the cells the run actually occupies.
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

/// Lay hoverable segments out flush with `row`'s right edge and draw them as
/// one line, returning each group's hit rect. A row too narrow to hold them
/// draws nothing and returns empty rects, which can never be hit — so a
/// control pinned this way is either fully usable or absent, never clipped
/// into a slider whose click mapping has silently lost its far end.
pub fn right_row(
    frame: &mut Frame,
    row: Rect,
    mouse: Option<Position>,
    groups: Vec<Vec<Span<'static>>>,
) -> Vec<Rect> {
    let total: usize = groups.iter().flatten().map(|s| s.width()).sum();
    let total = total as u16;
    if total > row.width {
        return vec![Rect::default(); groups.len()];
    }
    let start = row.right() - total;
    let mut spans = Vec::new();
    let mut x = start;
    let rects: Vec<Rect> = groups
        .into_iter()
        .map(|g| segment(&mut spans, &mut x, row, mouse, g))
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

/// Period of the header status dot's pulse.
const PULSE: Duration = Duration::from_millis(1800);

/// Style for the header's status dot at a given moment. It breathes so the
/// header shows liveness at a glance even when the visualizer has nothing to
/// say — a silent passage, or audio coming from another device. Brightness
/// rides a cosine so the turn at each end is soft; a linear ramp reads as a
/// blink.
///
/// The player view already redraws at ~20 fps for the visualizer, so this
/// costs no extra wakeups. The transport's own buttons sit still: they are
/// controls to click, not indicators, and movement under the cursor is noise.
pub(super) fn pulse_style(now: Instant) -> Style {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let elapsed = now.saturating_duration_since(*ORIGIN.get_or_init(|| now));
    pulse_at(elapsed.as_secs_f32() / PULSE.as_secs_f32())
}

/// The pulse itself, as a fraction of a period. Split out from the clock so
/// the shape can be checked without one.
fn pulse_at(phase: f32) -> Style {
    let t = (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0;
    theme::accent_at(0.45 + 0.55 * t)
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
pub fn state_spans(is_playing: bool) -> Vec<Span<'static>> {
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
    vec![
        Span::styled(glyph, style),
        Span::styled(format!(" {:<5}", word), style),
    ]
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
    use super::{fit, pulse_at};
    use crate::ui::theme;

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

    /// The pulse rides a cosine, so it turns softly at each end rather than
    /// blinking, and it comes all the way back around.
    #[test]
    fn playing_dot_breathes_over_its_period() {
        let at = |phase: f32| pulse_at(phase).fg.unwrap();
        assert_ne!(at(0.0), at(0.5), "the dot never changes brightness");
        assert_eq!(at(0.0), at(1.0), "the pulse does not loop");
        // Symmetric about the peak: rising and falling pass the same values.
        assert_eq!(at(0.25), at(0.75));
        // And it is a breath, not a blink — the dimmest state is still lit.
        assert_ne!(at(0.0), theme::accent_at(0.0).fg.unwrap());
    }
}
