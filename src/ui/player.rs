//! Full-screen player view (cliamp-style): current track header, live
//! spectrum visualizer, and the playing context's queue.
//!
//! The view is self-contained — it draws its own play state, progress bar,
//! volume and transport — so [`super::now_playing`] is not drawn beneath it.
//! It draws them from the same code the bar does: this is [`super::deck`]
//! with a spectrum wedged in beside the cover and the queue listed
//! underneath. What lives here is the part that is only ever the player's —
//! the row budget, the visualizer, and the queue table.
//!
//! Every control records itself into the same [`HitAreas`] field the bottom
//! bar would, so `event.rs` resolves clicks without knowing which pane drew
//! them.
//!
//! [`HitAreas`]: crate::app::state::HitAreas

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};

use super::deck;
use super::main_pane;
use super::play_state;
use super::table::{apply_selection, art_w, draw_scrollbar, fit};
use super::theme;
use crate::app::state::{AppState, HitAreas, PlaybackSnapshot, TrackList, format_duration};
use crate::cover::Cover;
use crate::viz::VizState;

/// Samples older than this mean audio is paused or playing elsewhere.
const FRESH_WITHIN: Duration = Duration::from_millis(500);
/// Band-count bounds. Bars are one cell wide on a two-cell stride, so a band
/// costs twice its bar (see [`BAND_STRIDE`]), and the upper bound is what stops
/// a very wide pane from slicing the spectrum so finely that it reads as a
/// horizon line rather than as a spectrum.
const MIN_BANDS: u16 = 4;
const MAX_BANDS: u16 = 80;
/// Cells one band occupies: a one-cell bar and the blank column after it. The
/// gap separates the bars horizontally the way the dark half of every `▄`
/// separates the LEDs vertically (see [`viz_cell`]).
const BAND_STRIDE: u16 = 2;

const DUR_W: usize = 5;
const COL_GAP: &str = "   ";
/// Leading marker column: "▶ " on the playing row, blank elsewhere.
const PREFIX_W: usize = 2;

/// Row heights for the sections above the queue, each including the blank
/// spacer rows around it. The progress band leads with two of them: the bar
/// belongs to the visualizer, but sitting directly under the field it reads
/// as one more row of it.
const HEADER_H: u16 = deck::MASTHEAD_H + 1;
const PROGRESS_H: u16 = 3;
/// Previous, the play state and next, then a blank. They sit directly under
/// the progress bar rather than sharing the list header's row: pushed to
/// opposite edges under the track with the state centred between them, the
/// row says "back", "what it is doing" and "forward" without a label.
const TRANSPORT_H: u16 = 2;
const LIST_HEAD_H: u16 = 2;
/// Rows the visualizer gets when the pane can afford them.
const VIZ_H: u16 = 9;
/// Blank rows the progress band puts above its bar.
const PROGRESS_PAD: u16 = 2;

/// The header's identity band — the mark and the path — under the name this
/// view knows it by. Both bands come from [`super`] rather than being restated
/// here: the masthead starts on the line a browse page's list does, so
/// toggling `v` moves nothing.
const BRAND_H: u16 = super::NAV_H;

/// Cover-art block heights, in rows. A terminal cell is about twice as tall as
/// it is wide and a `▀` cell stacks two pixels (see [`deck::sleeve`]), so an R-row
/// block is 2R cells wide and 2R x 2R pixels — square on screen. See
/// [`art_w`], which the bottom bar draws its own smaller sleeve with.
const ART_TALL_H: u16 = 12;
const ART_SHORT_H: u16 = 8;
/// Cells between the cover and the column beside it.
const ART_GAP: u16 = 3;
/// Narrowest field worth splitting off next to the cover — under this the
/// spectrum reads as a handful of stray bars rather than as a shape. The
/// metadata does not figure into it: that lives in the masthead above, which
/// spans the pane whatever the cover is doing.
const MIN_FIELD_W: u16 = 32;
/// The art tier's progress band: a blank row, then the bar. Narrower than
/// [`PROGRESS_H`] because the bar no longer has to separate itself from a
/// field directly above it.
const ART_PROGRESS_H: u16 = 2;

/// Vertical budget for one pane. The masthead (`header`) is drawn in both
/// tiers; `art > 0` puts the cover under it with the visualizer beside the
/// sleeve, so `viz` is the *stacked* tier's field and is zero whenever there
/// is a cover.
///
/// The cover is the first thing shed, because the stacked fallback still has a
/// spectrum, a progress bar and a title. After that, short panes shed the
/// visualizer, then the list header, then the transport, then the progress
/// row, then the header — the track title survives longest because it is what
/// the view is about.
struct Rows {
    header: u16,
    art: u16,
    viz: u16,
    progress: u16,
    transport: u16,
    list_head: u16,
    queue: u16,
}

impl Rows {
    /// Everything but the visualizer needs this much to show at all.
    const FIXED: u16 = HEADER_H + PROGRESS_H + TRANSPORT_H + LIST_HEAD_H;
    /// Rows kept for the queue before the visualizer is allowed to grow.
    const MIN_QUEUE: u16 = 3;
    /// Rows the queue must still get for a cover to be worth drawing. Higher
    /// than [`MIN_QUEUE`]: a three-row queue under a twelve-row sleeve is a
    /// worse screen than no sleeve at all.
    const MIN_ART_QUEUE: u16 = 5;

    fn new(width: u16, height: u16) -> Self {
        let zero = Self {
            art: 0,
            header: 0,
            viz: 0,
            progress: 0,
            transport: 0,
            list_head: 0,
            queue: 0,
        };

        // Art tiers, tallest first. A tier needs its own rows plus the
        // masthead, the progress band, the list header and a usable queue; the
        // wider cover also needs room beside it for the field. The visualizer
        // costs nothing here — it rides in the block's own rows, beside the
        // sleeve.
        for art in [ART_TALL_H, ART_SHORT_H] {
            let used = HEADER_H + art + ART_PROGRESS_H + TRANSPORT_H + LIST_HEAD_H;
            if width >= art_w(art) + ART_GAP + MIN_FIELD_W && height >= used + Self::MIN_ART_QUEUE {
                return Self {
                    header: HEADER_H,
                    art,
                    progress: ART_PROGRESS_H,
                    transport: TRANSPORT_H,
                    list_head: LIST_HEAD_H,
                    queue: height - used,
                    ..zero
                };
            }
        }

        if height >= Self::FIXED + Self::MIN_QUEUE {
            let viz = (height - Self::FIXED - Self::MIN_QUEUE).min(VIZ_H);
            Self {
                header: HEADER_H,
                viz,
                progress: PROGRESS_H,
                transport: TRANSPORT_H,
                list_head: LIST_HEAD_H,
                queue: height - Self::FIXED - viz,
                ..zero
            }
        } else if height >= HEADER_H + PROGRESS_H + TRANSPORT_H {
            // The list header goes before the transport does: the queue's
            // name is repeated by the rows under it, and prev/next are not
            // repeated anywhere.
            let used = HEADER_H + PROGRESS_H + TRANSPORT_H;
            Self {
                header: HEADER_H,
                progress: PROGRESS_H,
                transport: TRANSPORT_H,
                queue: height - used,
                ..zero
            }
        } else if height >= HEADER_H + PROGRESS_H {
            let queue = height - HEADER_H - PROGRESS_H;
            Self {
                header: HEADER_H,
                progress: PROGRESS_H,
                queue,
                ..zero
            }
        } else if height >= HEADER_H {
            Self {
                header: HEADER_H,
                queue: height - HEADER_H,
                ..zero
            }
        } else {
            Self {
                header: height,
                ..zero
            }
        }
    }
}

pub fn draw(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // No border and no title: the view fills the terminal, and a pane frame
    // around the only thing on screen is just noise. One cell of side padding
    // keeps text off the edge, matching what `pane_block` gave us.
    //
    // Everything below lays itself out across this column, and the column is
    // the whole terminal: masthead, progress bar, transport and queue all
    // stretch with it, so a wide screen gets a wide player rather than a
    // fixed-width one floating in the middle of one.
    let mut inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // The mark and the path, claimed before the row budget below gets to
    // spend anything — but never at the cost of the masthead, since a screen
    // showing nothing but a wordmark is worse than one showing the track.
    //
    // Just the one band: the header's other two rows are the search prompt,
    // which this view does not draw (see `top_row::draw`), so the rows go to
    // the queue rather than sitting blank. That is the one place the two
    // views' layouts part company — everything below here starts two rows
    // higher than a browse page's content does.
    //
    // The row itself is the browse screen's own, drawn from the browse
    // screen's own code — the path included, which here is the path of the
    // page waiting underneath. Sharing the function rather than restating it
    // is what stops the two drifting.
    //
    // No count: the player's list is the queue, and the page underneath's
    // total is not a fact about what is on screen.
    //
    // Before the split borrow below, which takes the state apart.
    let head = match inner.height >= BRAND_H + deck::MASTHEAD_H {
        true => BRAND_H,
        false => 0,
    };
    if head > 0 {
        let band = Rect {
            height: head,
            ..inner
        };
        super::top_row::draw(frame, band, state, main_pane::PageHeader::default());
    }
    inner = Rect {
        y: inner.y + head,
        height: inner.height - head,
        ..inner
    };

    // What the corner of the header is saying, read before the split borrow
    // below so the pill under the progress track can only agree with it.
    let play = play_state::or_paused(play_state::status(state));

    // Split borrows: queue/playback are read while list state, hit areas
    // and the visualizer's smoothing state are written.
    let AppState {
        playback,
        radio,
        radio_favorites,
        queue,
        queue_index,
        queue_list,
        hit,
        viz,
        audio_tap,
        mouse_pos,
        cover: state_cover,
        liked,
        ..
    } = state;
    let mouse = *mouse_pos;
    let state_cover = state_cover.as_deref();

    let rows = Rows::new(inner.width, inner.height);
    let mut y = inner.y;
    let mut band = |h: u16| {
        let r = Rect {
            y,
            height: h,
            ..inner
        };
        y += h;
        r
    };
    let header_area = band(rows.header);
    let art_area = band(rows.art);
    // Zero-height in the art tier: there the field lives beside the cover, and
    // the progress bar lands under the pair of them.
    let viz_band = band(rows.viz);
    let progress_area = band(rows.progress);
    let transport_area = band(rows.transport);
    let list_head_area = band(rows.list_head);
    let queue_area = band(rows.queue);

    // A station uses the same bands in the same order, minus the two things a
    // broadcast has not got: a sleeve and a queue. The spectrum takes the rows
    // the cover would have had, which is the one place the radio player looks
    // better than the Spotify one.
    if let Some(r) = radio.as_ref() {
        if rows.header > 0 {
            let like = r.matched_track().and_then(|t| liked.get(&t.uri).copied());
            deck::radio_masthead(frame, header_area, r, like, mouse, hit);
            hit.now_playing = Rect {
                height: header_area.height.min(deck::MASTHEAD_H),
                ..header_area
            };
        }
        let tall = Rect {
            y: art_area.y,
            height: art_area.height + viz_band.height,
            ..inner
        };
        let used = band_layout(inner.width).used;
        let field = match used <= inner.width {
            true => Rect {
                x: inner.x + (inner.width - used) / 2,
                width: used,
                ..tall
            },
            false => Rect::default(),
        };
        if field.height >= 2 && field.width >= 5 {
            draw_visualizer(frame, field, audio_tap, viz);
            hit.viz = field;
        }
        if rows.progress > 0 {
            let pad = if rows.art > 0 { 1 } else { PROGRESS_PAD };
            deck::radio_status(
                frame,
                Rect {
                    y: progress_area.y + pad,
                    height: 1,
                    ..progress_area
                },
                r,
                hit,
            );
        }
        if rows.transport > 0 {
            deck::radio_transport(
                frame,
                Rect {
                    height: 1,
                    ..transport_area
                },
                play,
                mouse,
                hit,
            );
        }
        if rows.list_head > 0 {
            let saved = radio_favorites.iter().any(|f| f.uuid == r.station.uuid);
            deck::radio_station_row(
                frame,
                Rect {
                    height: 1,
                    ..list_head_area
                },
                r,
                saved,
                mouse,
                hit,
            );
        }
        return;
    }

    let Some(pb) = playback.as_ref() else {
        deck::no_playback_hint(frame, header_area);
        draw_queue(frame, queue_area, None, queue.as_ref(), 0, queue_list, hit);
        return;
    };

    if rows.header > 0 {
        let like = pb
            .track_uri
            .as_ref()
            .and_then(|uri| liked.get(uri).copied());
        deck::masthead(frame, header_area, pb, like, mouse, hit);

        // Only the two written rows are the volume wheel's target: a wheel on
        // the blank one below is a scroll. (The bottom bar claims its whole
        // height instead — see `super::now_playing`.)
        hit.now_playing = Rect {
            height: header_area.height.min(deck::MASTHEAD_H),
            ..header_area
        };
    }

    // The field: beside the cover when there is one, its own band when there
    // is not.
    let field = if rows.art > 0 {
        draw_block(frame, art_area, pb, state_cover, hit)
    } else {
        // The stacked field spans the pane; centre the odd cell its stride
        // cannot fill rather than banking it on one side.
        // `band_layout` has a minimum band count, so a very narrow pane asks
        // for more cells than it has, and the field is simply not drawn.
        let used = band_layout(inner.width).used;
        match used <= inner.width {
            true => Rect {
                x: inner.x + (inner.width - used) / 2,
                width: used,
                ..viz_band
            },
            false => Rect::default(),
        }
    };
    if field.height >= 2 && field.width >= 5 {
        draw_visualizer(frame, field, audio_tap, viz);
        hit.viz = field;
    }
    if rows.progress > 0 {
        // One blank row above the bar in the art tier, two when stacked.
        let pad = if rows.art > 0 { 1 } else { PROGRESS_PAD };
        let bar = Rect {
            y: progress_area.y + pad,
            height: 1,
            ..progress_area
        };
        deck::progress(frame, bar, pb, hit);
    }
    if rows.transport > 0 {
        deck::transport(
            frame,
            Rect {
                height: 1,
                ..transport_area
            },
            play,
            mouse,
            hit,
        );
    }
    if rows.list_head > 0 {
        deck::context_row(
            frame,
            Rect {
                height: 1,
                ..list_head_area
            },
            pb,
            queue.as_ref(),
            mouse,
            hit,
        );
    }
    draw_queue(
        frame,
        queue_area,
        Some(pb),
        queue.as_ref(),
        *queue_index,
        queue_list,
        hit,
    );
}

/// The now-playing block: cover art on the left, spectrum field on the right.
///
/// The two share both a top and a bottom edge, which is what makes them read
/// as one object rather than as a picture with a chart next to it. The
/// metadata that used to sit in this column is now in the masthead above (see
/// [`deck::masthead`]), so the field gets the sleeve's full height.
///
/// Returns the field's rect for the caller to paint, so the visualizer is
/// drawn in one place whether or not there is a cover.
fn draw_block(
    frame: &mut Frame,
    area: Rect,
    pb: &PlaybackSnapshot,
    cover: Option<&Cover>,
    hit: &mut HitAreas,
) -> Rect {
    let art = deck::sleeve(frame, area, pb, cover, hit);
    Rect {
        x: art.right() + ART_GAP,
        width: area.width.saturating_sub(art.width + ART_GAP),
        ..area
    }
}

/// How the spectrum divides a rect: how many bands, how wide their bars, and
/// the width the field ends up occupying.
struct Bands {
    n: usize,
    /// Cells the narrow bars take. The first [`wide`](Self::wide) bands get one
    /// more, which is how the field fills a rect the stride does not divide.
    bar: u16,
    wide: u16,
    used: u16,
}

impl Bands {
    /// Band `b`'s left edge, as an offset into the field.
    fn x(&self, b: usize) -> u16 {
        b as u16 * (self.bar + 1) + (b as u16).min(self.wide)
    }

    fn width(&self, b: usize) -> u16 {
        self.bar + u16::from((b as u16) < self.wide)
    }
}

/// How a rect `width` cells wide divides into bands.
///
/// A band is a bar and the gap column after it, less the trailing gap nothing
/// follows. Bars are one cell wide until the band count reaches [`MAX_BANDS`];
/// past that the bands stop multiplying and start widening, so a very wide
/// pane gets a coarser spectrum that still fills it rather than a fine one
/// stopping short of the edge. Below [`MIN_BANDS`] it asks for more cells than
/// the rect has, and the caller declines to draw.
fn band_layout(width: u16) -> Bands {
    let n = width.div_ceil(BAND_STRIDE).clamp(MIN_BANDS, MAX_BANDS);
    // The stride that fits `n` bands in the rect, and the cells left over
    // after it — the `+ 1` on both pays for the gap the last bar does not
    // need. The leftovers go one apiece to the leading bands rather than into
    // a gap at the field's right edge. Only a capped field has any to share
    // out: below the cap the band count itself grew to take them, leaving at
    // most the one cell a two-cell stride cannot fill.
    let capped = n == MAX_BANDS;
    let stride = match capped {
        true => ((width + 1) / n).max(BAND_STRIDE),
        false => BAND_STRIDE,
    };
    let wide = match capped {
        true => (width + 1).saturating_sub(n * stride),
        false => 0,
    };
    Bands {
        n: n as usize,
        bar: stride - 1,
        wide,
        used: n * stride - 1 + wide,
    }
}

/// One band's animation state, in rows rather than 0..=1.
struct Column {
    bar: f32,
    /// Where the bar was recently. Everything between `bar` and `glow` is
    /// drawn as cooling afterglow, so the fade chases the falling bar.
    glow: f32,
}

/// Glyph and style for one visualizer cell. `slot` counts height slots up
/// from 1 at the floor; `rows` is the pane height.
///
/// Cells are `▄`, not `█`: leaving the top half of every character cell dark
/// separates the rows into discrete LEDs. The *lower* half is the lit one so
/// the bottom LED sits on the floor of the field, level with the bottom edge of
/// the cover beside it — an upper half-block lights half a cell higher, and the
/// field then reads as floating above the sleeve. Partial states are carried by
/// brightness rather than by a different glyph — see [`theme::Led`] for why a
/// shading character can't be used here.
fn viz_cell(col: &Column, slot: u16, rows: u16) -> (char, Style) {
    let slot = slot as f32;
    let pos = (slot - 0.5) / rows as f32;
    let led = |led| ('▄', Style::default().fg(theme::viz_color(pos, led)));

    if col.bar >= slot {
        led(theme::Led::Lit)
    } else if col.bar > slot - 0.5 {
        led(theme::Led::Half)
    } else if col.glow >= slot - 0.5 {
        led(theme::Led::Trail)
    } else {
        (' ', Style::default())
    }
}

fn draw_visualizer(
    frame: &mut Frame,
    area: Rect,
    tap: &crate::audio_tap::AudioTap,
    viz: &mut VizState,
) {
    let bands = band_layout(area.width);
    // The band count is clamped, so a very narrow rect asks for more cells than
    // it has; drawing anyway would spill the field past its own right edge.
    if bands.used > area.width || area.height == 0 {
        return;
    }
    let fresh = tap.is_fresh(FRESH_WITHIN);
    viz.update(tap, bands.n, fresh, Instant::now());

    let rows = area.height;
    let h = rows as f32;
    // Every cell carries its own color, so this paints the buffer directly;
    // going through `Paragraph` would mean one `Span` per cell.
    let buf = frame.buffer_mut();
    for b in 0..bands.n {
        let col = Column {
            bar: viz.bars()[b] * h,
            glow: viz.glow()[b] * h,
        };
        // The gap column after each bar is simply never written.
        let x = area.x + bands.x(b);
        let bar_w = bands.width(b);
        for row in 0..rows {
            let (ch, style) = viz_cell(&col, rows - row, rows);
            // A stale tap means paused or playing elsewhere: keep the shape,
            // drop the color.
            let style = if fresh { style } else { theme::dim() };
            for x in x..x + bar_w {
                if let Some(cell) = buf.cell_mut((x, area.y + row)) {
                    cell.set_char(ch).set_style(style);
                }
            }
        }
    }
}

/// Column widths for the queue table: marker + number + title + artist + time.
struct QueueCols {
    /// Right-aligned row number, wide enough for the longest one.
    num: usize,
    name: usize,
    /// 0 = hidden (narrow panes).
    artist: usize,
}

impl QueueCols {
    fn new(width: usize, len: usize) -> Self {
        let num = len.to_string().len().max(2);
        let fixed = PREFIX_W + num + COL_GAP.len() + DUR_W + COL_GAP.len();
        let flex = width.saturating_sub(fixed);
        if width >= 40 + num {
            let flex = flex.saturating_sub(COL_GAP.len());
            // Titles are what a reader scans for, and they run longer than
            // artist names — especially the CJK ones, which cost two cells a
            // character. Give them the larger share.
            let name = flex * 60 / 100;
            Self {
                num,
                name,
                artist: flex - name,
            }
        } else {
            Self {
                num,
                name: flex,
                artist: 0,
            }
        }
    }
}

/// Columns the queue list keeps clear on its right: a blank one, then the
/// scrollbar. The same two the browse pane reserves, for the same reason — a
/// duration flush against the bar reads as one mark rather than two.
const QUEUE_GUTTER: u16 = 2;

#[allow(clippy::too_many_arguments)]
fn draw_queue(
    frame: &mut Frame,
    area: Rect,
    playback: Option<&crate::app::state::PlaybackSnapshot>,
    queue: Option<&TrackList>,
    queue_index: usize,
    queue_list: &mut ratatui::widgets::ListState,
    hit: &mut crate::app::state::HitAreas,
) {
    if area.height == 0 {
        return;
    }
    let Some(q) = queue else {
        let row = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new("no queue — play a playlist or album to fill this")
                .alignment(ratatui::layout::Alignment::Center)
                .style(theme::dim()),
            row,
        );
        return;
    };

    // The name and count now head the list from `draw_list_header`, so the
    // rows start at the top of this band. Its right edge is the scrollbar's
    // gutter — see [`QUEUE_GUTTER`].
    let rows_area = Rect {
        width: area.width.saturating_sub(QUEUE_GUTTER),
        ..area
    };
    hit.player_queue = rows_area;
    super::clamp_offset(queue_list, q.display.len(), rows_area.height as usize);

    // The playing marker resolves by URI each frame, like the main pane.
    let playing_uri = playback.and_then(|p| p.track_uri.as_deref());
    let playing =
        playing_uri.and_then(|uri| q.display.iter().position(|&ti| q.tracks[ti].uri == uri));

    let cols = QueueCols::new(rows_area.width as usize, q.display.len());
    let accent_bold = theme::accent().add_modifier(Modifier::BOLD);
    let items: Vec<ListItem> = q
        .display
        .iter()
        .enumerate()
        .map(|(i, &ti)| {
            let t = &q.tracks[ti];
            let prefix = if Some(i) == playing {
                Span::styled("▶ ", accent_bold)
            } else {
                Span::raw(" ".repeat(PREFIX_W))
            };
            // Three weights, so the playing row actually stands out: the
            // title at TEXT, everything supporting it at DIM, and the playing
            // row in accent. The title used to be `Style::default()` — the
            // raw terminal foreground, the one unthemed colour in the view.
            let name_style = if Some(i) == playing {
                accent_bold
            } else {
                theme::text()
            };
            // Display position, not `Track::track_number`: the queue is a
            // permutation, so the number has to follow the rows on screen.
            let mut spans = vec![
                prefix,
                Span::styled(
                    format!("{:>w$}", i + 1, w = cols.num),
                    if Some(i) == playing {
                        accent_bold
                    } else {
                        theme::dim()
                    },
                ),
                Span::raw(COL_GAP),
                Span::styled(fit(&t.name, cols.name), name_style),
            ];
            if cols.artist > 0 {
                spans.push(Span::raw(COL_GAP));
                spans.push(Span::styled(fit(&t.artists, cols.artist), theme::dim()));
            }
            spans.push(Span::raw(COL_GAP));
            spans.push(Span::styled(
                format!("{:>DUR_W$}", format_duration(t.duration_ms)),
                theme::dim(),
            ));
            let mut line = Line::from(spans);
            if i == queue_index {
                apply_selection(&mut line);
                // Selection restyles every span, which would leave a row that
                // is *also* playing with nothing but the `▶` to say so. Put
                // the marker and its number back in accent.
                if Some(i) == playing {
                    for span in line.spans.iter_mut().take(2) {
                        span.style = accent_bold;
                    }
                }
            }
            ListItem::new(line)
        })
        .collect();
    let count = items.len();
    frame.render_stateful_widget(List::new(items), rows_area, queue_list);
    draw_scrollbar(
        frame,
        Rect {
            x: rows_area.right() + QUEUE_GUTTER - 1,
            y: rows_area.y,
            width: 1,
            height: rows_area.height,
        },
        count,
        queue_list.offset(),
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::style::Color;

    use super::super::table::VOL_TRACK_W;
    use super::*;
    use crate::app::state::{
        CrumbTarget, MainView, PlaybackSnapshot, RepeatMode, Track, TrackListKind,
    };

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

    fn playing_state() -> AppState {
        let mut st = AppState::new();
        st.show_player = true;
        st.playback = Some(PlaybackSnapshot {
            is_playing: true,
            progress_ms: 10_000,
            duration_ms: 83_000,
            track_uri: Some("spotify:track:Beta".into()),
            context_uri: Some("spotify:playlist:p1".into()),
            artist_id: None,
            album_id: None,
            track_name: "Beta".into(),
            artists: "Bob".into(),
            album: "Album Name".into(),
            release_year: "2020".into(),
            cover_url: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume_percent: 50,
            device_name: "dev".into(),
            is_local_device: true,
            fetched_at: std::time::Instant::now(),
        });
        let mut q = TrackList::new(
            "My Mix",
            "by me",
            Some("spotify:playlist:p1".to_string()),
            Some(3),
        );
        q.kind = TrackListKind::Playlist;
        q.append(vec![
            track("Alpha", "Ann"),
            track("Beta", "Bob"),
            track("Gamma", "Cyd"),
        ]);
        st.queue = Some(q);
        // Samples arriving, which is what "playing" means to the header and
        // the transport alike: a snapshot claiming to play with a silent tap
        // is a track still loading. See [`super::super::play_state`].
        st.audio_tap.push(&[0.0; 2048], 1.0);
        st
    }

    /// Render `height` rows *of the player itself*, with the header's rows
    /// above them dropped.
    ///
    /// The terminal is made that much taller so the view's own row budget is
    /// the `height` asked for, and every row index below is measured from
    /// under the header — so the layout these tests describe is the same one
    /// they described before the header went in. [`render_raw`] sees the whole
    /// screen, header included.
    fn render(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
        render_raw(state, width, height + BRAND_H).split_off(BRAND_H as usize)
    }

    /// A page to sit behind the player, so its borrowed path has a step on it
    /// that is not the page itself. Home draws no crumb, so a path one deep
    /// from Home is a head and nothing else.
    fn artist(name: &str) -> crate::app::state::ArtistView {
        crate::app::state::ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: name.into(),
            image_url: None,
            genres: vec![],
            top: TrackList::new(name, "", None, None),
            albums: vec![],
            display: Vec::new(),
            tab: crate::app::state::ArtistTab::Albums,
            loading: false,
        }
    }

    fn render_raw(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
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

    /// The header is the browse screen's top row: the mark, then the path.
    /// The player is an overlay rather than a page, so it borrows the path of
    /// what is behind it — and its head is the way out, unlike the browse
    /// screen's, where the head is the page you are already on and leads
    /// nowhere.
    ///
    /// And it is only that row. The prompt the browse screen carries two rows
    /// under it is not drawn here, so the masthead starts where the blank
    /// under the path ends.
    #[test]
    fn the_header_carries_the_browse_screens_own_row_and_path() {
        let mut st = playing_state();
        st.main_index = 2;
        st.push_view();
        st.main = MainView::Artist(artist("Muse"));
        st.push_view();
        st.main = MainView::Tracks(TrackList::new("Black Holes", "", None, None));

        let lines = render_raw(&mut st, 80, 26);
        assert!(
            lines[0].starts_with(" ♫ spot   MUSE  ›  BLACK HOLES"),
            "the mark does not lead the path: {:?}",
            lines[0]
        );
        // And the playback status is opposite it, the same as on the browse
        // screen — the player draws this row from the same function.
        assert!(
            lines[0].trim_end().ends_with("● STREAMING"),
            "{:?}",
            lines[0]
        );
        assert!(lines[1].trim().is_empty(), "{:?}", lines[1]);
        assert!(!lines[2].contains("/  search"), "{:?}", lines[2]);
        assert!(lines[2].contains("Beta"), "the masthead took the row");
        assert!(st.hit.search_box.is_empty());

        // The head closes the view; the step before it is a crumb like any
        // other, and clicking it closes the view *and* goes there.
        assert!(!st.hit.close_player.is_empty());
        assert_eq!(st.hit.crumbs.len(), 1);
        assert_eq!(st.hit.crumbs[0].1, CrumbTarget::Depth(1));
        // Left to right, and clear of the mark rather than crowding it.
        assert_eq!(st.hit.crumbs[0].0.y, 0);
        assert!(st.hit.crumbs[0].0.x > st.hit.home_btn.right());
        assert!(st.hit.crumbs[0].0.right() < st.hit.close_player.x);
    }

    /// A row too narrow for the whole path sheds its ancestors and keeps the
    /// page you are on — the browse screen's behaviour, and here also the
    /// labelled way out, since the head is what closes the view.
    #[test]
    fn a_narrow_header_sheds_the_path_but_keeps_the_page() {
        let mut st = playing_state();
        st.push_view();
        st.main = MainView::Artist(artist("Muse"));
        st.push_view();
        st.main = MainView::Tracks(TrackList::new("Black Holes", "", None, None));
        let lines = render_raw(&mut st, 30, 26);
        assert!(lines[0].starts_with(" ♫ spot"), "{:?}", lines[0]);
        assert!(!lines[0].contains('›'), "{:?}", lines[0]);
        assert!(lines[0].contains("BLACK HOLES"), "{:?}", lines[0]);
        assert!(st.hit.crumbs.is_empty(), "no ancestor survived the shed");
        assert!(!st.hit.close_player.is_empty(), "no way out of the view");
        assert!(!st.hit.home_btn.is_empty());
    }

    /// The shortest view that still gets a header: the masthead has to fit
    /// under it, because a screen showing nothing but a wordmark is worse than
    /// one showing the track. A row shorter than that and the header goes.
    #[test]
    fn the_header_survives_down_to_the_masthead_and_no_further() {
        let short = |h: u16| {
            let mut st = playing_state();
            st.push_view();
            st.main = MainView::Tracks(TrackList::new("Black Holes", "", None, None));
            let lines = render_raw(&mut st, 80, h);
            (st, lines)
        };

        let (st, lines) = short(BRAND_H + deck::MASTHEAD_H);
        assert!(lines[0].starts_with(" ♫ spot"), "{:?}", lines[0]);
        assert!(lines[0].contains("BLACK HOLES"), "{:?}", lines[0]);
        assert!(!st.hit.home_btn.is_empty());
        assert!(!st.hit.close_player.is_empty());
        // The masthead starts right under it — no prompt, and no gap where
        // one would have been on the browse screen.
        assert!(
            lines[BRAND_H as usize].contains("Beta"),
            "{:?}",
            lines[BRAND_H as usize]
        );

        let (st, lines) = short(BRAND_H + deck::MASTHEAD_H - 1);
        assert!(!lines[0].contains("spot"), "{:?}", lines[0]);
        assert!(st.hit.home_btn.is_empty());
        assert!(st.hit.close_player.is_empty());
    }

    #[test]
    fn renders_block_viz_and_queue_with_markers() {
        let mut st = playing_state();
        let lines = render(&mut st, 80, 26);
        // No pane frame: the view starts at the very first row.
        assert!(!lines[0].contains(" Player "));
        // The masthead spans the pane above the cover: the title, then
        // artists · album · year with the volume opposite. The play state is
        // not up here — it is centred under the progress bar.
        // No note on the title: the `♫ spot` mark two rows above owns that
        // glyph and that column.
        assert!(lines[0].trim_start().starts_with("Beta"), "{:?}", lines[0]);
        assert!(!lines[0].contains("♫"), "{:?}", lines[0]);
        assert!(!lines[0].contains("playing"), "{:?}", lines[0]);
        assert!(
            lines[1].contains("Bob · Album Name · 2020"),
            "{:?}",
            lines[1]
        );
        assert!(lines[1].contains("50%"), "{:?}", lines[1]);
        assert!(lines[2].trim().is_empty(), "{:?}", lines[2]);
        assert!(!lines.iter().any(|l| l.contains("playing from")));
        // Stale tap and no signal: the field rests dark. There is no baseline
        // row, so the floor paints no LEDs — only the sleeve's own `▀` cells
        // are on this line.
        assert!(
            !lines[VIZ_BOTTOM as usize].contains('▄'),
            "{:?}",
            lines[VIZ_BOTTOM as usize]
        );
        // A blank row above the progress bar, and blanks either side of the
        // row naming the queue.
        assert!(lines[15].trim().is_empty(), "{:?}", lines[15]);
        // Remaining is interpolated in real time, so only its prefix is stable.
        assert!(
            lines[16].contains("0:10 ━") && lines[16].contains(" -1:1"),
            "{:?}",
            lines[16]
        );
        // The transport sits directly under the bar, at both edges of it —
        // inside the pane's one-cell inset — with the play state centred
        // between the two buttons.
        assert!(
            lines[17].trim_start().starts_with("◂◂ previous"),
            "{:?}",
            lines[17]
        );
        assert!(lines[17].contains("■ pause"), "{:?}", lines[17]);
        assert!(lines[17].trim_end().ends_with("next ▸▸"), "{:?}", lines[17]);
        assert!(lines[18].trim().is_empty() && lines[20].trim().is_empty());
        // Then one row heads the list: name on the left, shuffle opposite.
        assert!(lines[19].contains("My Mix · 3 tracks"), "{:?}", lines[19]);
        assert!(lines[19].contains("shuffle off"), "{:?}", lines[19]);
        assert!(!lines.iter().any(|l| l.contains("repeat")));
        // Queue rows: numbered, and only the playing one is marked.
        assert!(lines[21].contains(" 1   Alpha"), "{:?}", lines[21]);
        assert!(lines[22].contains("▶  2   Beta"), "{:?}", lines[22]);
        assert!(lines[23].contains(" 3   Gamma"), "{:?}", lines[23]);
        assert!(!lines.iter().any(|l| l.contains("→")));
        assert!(!st.hit.player_queue.is_empty());
        assert_eq!(st.hit.player_queue.y, BRAND_H + 21);
    }

    /// A cell of padding on each side of the player view.
    const PANE_INSET: u16 = 1;
    /// At 80x26 the tall art tier applies: a three-row masthead, then a
    /// twelve-row cover with twelve LED rows beside it.
    const VIZ_TOP: u16 = HEADER_H;
    const VIZ_BOTTOM: u16 = HEADER_H + ART_TALL_H - 1;

    /// A loud 440 Hz sine, interleaved stereo, long enough to fill the tap's
    /// analysis window.
    fn loud_sine() -> Vec<f64> {
        (0..4096)
            .map(|i| {
                let t = i as f32 / 44_100.0;
                (0.9 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()) as f64
            })
            .flat_map(|s| [s, s])
            .collect()
    }

    /// Every cell of the visualizer field of a freshly-drawn 80x26 player view.
    fn viz_cells(st: &mut AppState) -> Vec<ratatui::buffer::Cell> {
        let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let field = st.hit.viz;
        (field.y..field.bottom())
            .flat_map(|y| (field.x..field.right()).map(move |x| Position { x, y }))
            .map(|p| buffer.cell(p).unwrap().clone())
            .collect()
    }

    /// The field matches the sleeve top and bottom, so the block reads as one
    /// object rather than as a picture with a chart next to it.
    #[test]
    fn the_field_fills_the_column_beside_the_cover() {
        let mut st = playing_state();
        let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let (art, field) = (st.hit.art, st.hit.viz);
        assert_eq!(field.y, BRAND_H + VIZ_TOP);
        assert_eq!(field.bottom(), BRAND_H + VIZ_BOTTOM + 1);
        assert_eq!(field.y, art.y, "field and sleeve are out of line");
        assert_eq!(
            field.bottom(),
            art.bottom(),
            "field and sleeve are out of line"
        );
        assert_eq!(field.height, ART_TALL_H, "the field lost rows to something");
        assert_eq!(field.x, art.right() + ART_GAP);
        assert_eq!(
            field.right(),
            80 - PANE_INSET,
            "field stops short of the pane edge"
        );
    }

    /// The masthead spans the pane above the cover, so a long title has the
    /// whole width to be read in rather than a column beside the sleeve.
    #[test]
    fn the_masthead_spans_the_pane_above_the_cover() {
        let mut st = playing_state();
        st.playback.as_mut().unwrap().track_name =
            "A Title Long Enough To Have Been Clipped By The Old Column".into();
        let lines = render(&mut st, 80, 26);
        assert!(
            lines[0].contains("Been Clipped By The Old Column"),
            "{:?}",
            lines[0]
        );
        // The title has row 0 to itself; metadata and volume share row 1, both
        // above the art.
        let (title, meta) = (&lines[0], &lines[1]);
        assert!(title.trim_end().ends_with("Old Column"), "{title:?}");
        assert!(meta.trim_end().ends_with('%'), "{meta:?}");
        assert!(meta.contains("Bob · Album Name · 2020"), "{meta:?}");
        // The metadata starts flush with the title, not indented past it.
        // Cell columns, not byte offsets — the `·` separators are multi-byte.
        let col = |s: &str, c: char| s.chars().position(|x| x == c);
        assert_eq!(col(meta, 'B'), col(title, 'A'), "{meta:?} / {title:?}");
        // Row 2 separates the masthead from the block; the art starts on row 3.
        assert!(lines[2].trim().is_empty(), "{:?}", lines[2]);
        assert_eq!(st.hit.art.y, BRAND_H + HEADER_H);
    }

    #[test]
    fn fresh_audio_lights_bars_across_the_flame_ramp() {
        let mut st = playing_state();
        st.audio_tap.push(&loud_sine(), 1.0);
        let cells = viz_cells(&mut st);
        assert!(
            cells.iter().any(|c| c.symbol() == "▄"),
            "expected lit LED cells in the viz area"
        );
        // The flame ramp is continuous, so a tall bar paints its cells in
        // several distinct colors rather than one flat fill.
        let hues: std::collections::HashSet<_> = cells
            .iter()
            .filter(|c| c.symbol() == "▄")
            .map(|c| format!("{:?}", c.fg))
            .collect();
        assert!(hues.len() >= 3, "gradient is flat: {hues:?}");
        assert!(
            cells.iter().all(|c| !matches!(
                c.fg,
                ratatui::style::Color::Green | ratatui::style::Color::Yellow
            )),
            "the old LED palette is still in use"
        );
    }

    /// Broadband content, so every band has something to show. A single tone
    /// leaves the bands above it dark, and with no resting baseline left to
    /// paint the floor there would be nothing to tell a gap column from a
    /// silent bar.
    fn broadband() -> Vec<f64> {
        crate::viz::pink_noise(4096)
            .iter()
            .flat_map(|&s| [s as f64, s as f64])
            .collect()
    }

    /// The field's rect, and the x positions painted on its bottom row. Every
    /// band is lit, so the painted columns are exactly the bar columns — and
    /// the unpainted ones exactly the gaps.
    fn painted_bottom_row(width: u16) -> (Rect, Vec<u16>) {
        let mut st = playing_state();
        st.audio_tap.push(&broadband(), 1.0);
        let mut terminal = Terminal::new(TestBackend::new(width, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let field = st.hit.viz;
        let y = field.bottom() - 1;
        let painted = (field.x..field.right())
            .filter(|&x| buffer.cell(Position { x, y }).unwrap().symbol() != " ")
            .collect();
        (field, painted)
    }

    /// Bars are separated by a blank column and the run ends on a bar rather
    /// than a trailing gap, at every width the layout produces.
    #[test]
    fn bars_are_gapped_and_fill_the_field_at_every_width() {
        for width in [40u16, 80, 94, 200, 400] {
            let (field, painted) = painted_bottom_row(width);
            let bands = band_layout(field.width);
            assert!(bands.n <= MAX_BANDS as usize, "{width}: {} bands", bands.n);
            assert!(
                bands.used <= field.width,
                "{width}: the field overflows its rect"
            );
            let expected: Vec<u16> = (0..bands.n)
                .flat_map(|b| {
                    let x = field.x + bands.x(b);
                    x..x + bands.width(b)
                })
                .collect();
            assert_eq!(painted, expected, "{width}: bars are not gapped");
            assert_eq!(
                expected.last().unwrap() - field.x + 1,
                bands.used,
                "{width}: the run does not end on a bar"
            );
        }
    }

    /// Past the cap the bands stop multiplying and start widening, so a very
    /// wide pane fills with a coarser spectrum rather than a horizon line —
    /// and rather than a fine one stopping short of the pane's edge.
    #[test]
    fn a_very_wide_pane_widens_the_bars_at_the_band_cap() {
        let bands = band_layout(500);
        assert_eq!(bands.n, MAX_BANDS as usize, "{} bands", bands.n);
        assert!(bands.bar > 1, "the bars did not widen");
        assert_eq!(bands.used, 500, "the field stopped short of the pane");
    }

    /// The view spans the terminal: the block starts at the screen's own
    /// margin and the progress bar and queue run to the far side of it, so a
    /// wide terminal gets a wide player rather than a fixed column floating in
    /// the middle of one.
    #[test]
    fn a_wide_terminal_gets_the_whole_width() {
        let width = 200;
        let mut st = playing_state();
        let mut terminal = Terminal::new(TestBackend::new(width, 30 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let inner_w = width - 2 * PANE_INSET;

        for (name, r) in [
            ("art", st.hit.art),
            ("viz", st.hit.viz),
            ("gauge", st.hit.gauge),
            ("next", st.hit.next_btn),
        ] {
            assert!(!r.is_empty(), "{name} was not drawn");
        }
        assert_eq!(
            st.hit.art.x, PANE_INSET,
            "the block does not start at the screen margin"
        );
        // The progress bar and the queue are what have to grow — the sleeve is
        // square and the spectrum stops at its band cap.
        assert_eq!(
            st.hit.gauge.width,
            inner_w - deck::TIME_W,
            "the progress bar did not take the width"
        );
        assert_eq!(
            st.hit.player_queue.width,
            inner_w - QUEUE_GUTTER,
            "the queue did not take the width"
        );
    }

    #[test]
    fn viz_cell_lights_leds_under_a_fading_trail() {
        let rows = 8;
        // Bar at 3 rows, glow still up at 6 from where the bar just fell.
        let col = Column {
            bar: 3.0,
            glow: 6.0,
        };
        let glyphs: Vec<char> = (1..=rows)
            .rev()
            .map(|slot| viz_cell(&col, slot, rows).0)
            .collect();
        assert_eq!(glyphs, [' ', ' ', '▄', '▄', '▄', '▄', '▄', '▄']);
        // Every cell is the same half-height glyph, so nothing can hang outside
        // the LED row it belongs to; lit, half-lit and trail differ by color.
        let fg = |slot| viz_cell(&col, slot, rows).1.fg.unwrap();
        assert_ne!(fg(3), fg(5), "trail is not dimmer than the bar");
        assert_ne!(
            fg(4),
            viz_cell(
                &Column {
                    bar: 4.0,
                    glow: 4.0
                },
                4,
                rows
            )
            .1
            .fg
            .unwrap(),
            "trail matches a fully lit LED at the same height"
        );

        // A cell the bar only reaches into lights at half brightness; less
        // than half a row of signal leaves it dark.
        assert_ne!(
            viz_cell(
                &Column {
                    bar: 2.7,
                    glow: 2.7
                },
                3,
                rows
            )
            .1
            .fg,
            viz_cell(
                &Column {
                    bar: 3.0,
                    glow: 3.0
                },
                3,
                rows
            )
            .1
            .fg
        );
        assert_eq!(
            viz_cell(
                &Column {
                    bar: 2.2,
                    glow: 2.2
                },
                3,
                rows
            )
            .0,
            ' '
        );
    }

    /// Silence leaves the field empty — there is no dim baseline row along the
    /// floor for the bars to sit on.
    #[test]
    fn viz_cell_paints_nothing_when_silent() {
        let col = Column {
            bar: 0.0,
            glow: 0.0,
        };
        assert_eq!(viz_cell(&col, 1, 8), (' ', Style::default()));
        assert_eq!(viz_cell(&col, 2, 8).0, ' ');
    }

    /// Color comes from height alone, so a given row is the same color in
    /// every bar — the field reads as banded green/yellow/red, not confetti.
    #[test]
    fn bar_color_depends_only_on_height() {
        let color = |bar: f32, slot| viz_cell(&Column { bar, glow: bar }, slot, 8).1.fg;
        assert_eq!(color(8.0, 4), color(4.0, 4), "same row, different color");
        assert_ne!(color(8.0, 1), color(8.0, 8), "no gradient up the bar");
    }

    #[test]
    fn no_queue_shows_hint() {
        let mut st = playing_state();
        st.queue = None;
        let lines = render(&mut st, 80, 26);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("no queue — play a playlist or album"))
        );
        assert!(st.hit.player_queue.is_empty());
    }

    #[test]
    fn nothing_playing_shows_hint() {
        let mut st = playing_state();
        st.playback = None;
        let lines = render(&mut st, 80, 26);
        assert!(lines[0].contains("nothing playing"));
        // Nothing to control, so no control records a hit rect.
        for rect in [
            st.hit.play_btn,
            st.hit.volume_slider,
            st.hit.gauge,
            st.hit.prev_btn,
        ] {
            assert!(rect.is_empty(), "{rect:?}");
        }
    }

    #[test]
    fn list_header_names_the_queue() {
        let mut st = playing_state();
        let mut q = TrackList::new("Search results", "", None, None);
        q.append(vec![track("Alpha", "Ann")]);
        st.queue = Some(q);
        let lines = render(&mut st, 80, 26);
        assert!(
            lines[19].contains("Search results · 1 tracks"),
            "{:?}",
            lines[19]
        );
        assert!(!lines.iter().any(|l| l.contains("Up Next")));
    }

    /// Selection is weight and brightness now, not a filled accent bar, so it
    /// never outshouts the accent-colored playing row.
    #[test]
    fn selection_brightens_the_selected_row() {
        let mut st = playing_state();
        st.queue_index = 0;
        let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let y = st.hit.player_queue.y;
        let row: Vec<_> = (1..79u16)
            .map(|x| buffer.cell(Position { x, y }).unwrap())
            .collect();
        assert!(
            row.iter().all(|c| c.bg != theme::ACCENT),
            "selection still paints a bar"
        );
        assert!(
            row.iter()
                .any(|c| c.fg == theme::BRIGHT && c.modifier.contains(Modifier::BOLD))
        );
    }

    /// Every control the bottom bar used to own now records the same hit rect
    /// from the player view, so `event.rs` needs no idea which pane drew it.
    #[test]
    fn controls_record_hits_on_their_rows() {
        let mut st = playing_state();
        render(&mut st, 100, 26);
        // The masthead owns rows 0..2 — the title, metadata and volume, blank
        // — then the cover and the field beside it, then the progress bar, the
        // transport, the list header and the queue.
        const BLOCK: u16 = HEADER_H;
        const BAR: u16 = BLOCK + ART_TALL_H + ART_PROGRESS_H - 1;
        for (rect, y) in [
            (st.hit.volume_slider, 1),
            (st.hit.art, BLOCK),
            (st.hit.viz, BLOCK),
            (st.hit.gauge, BAR),
            (st.hit.prev_btn, BAR + 1),
            (st.hit.play_btn, BAR + 1),
            (st.hit.next_btn, BAR + 1),
            (st.hit.shuffle_btn, BAR + 1 + TRANSPORT_H),
            (st.hit.queue_name, BAR + 1 + TRANSPORT_H),
        ] {
            assert!(!rect.is_empty(), "{rect:?}");
            assert_eq!(rect.y, BRAND_H + y, "{rect:?}");
        }
        assert_eq!(st.hit.volume_slider.width, VOL_TRACK_W);
        // Wheel-over-metadata adjusts volume, the way the bottom bar did — but
        // only over the two written rows, not the blank one below them.
        assert_eq!(st.hit.now_playing.height, 2);
        assert_eq!(
            st.hit.viz.bottom(),
            st.hit.art.bottom(),
            "field and sleeve are out of line"
        );
    }

    /// The progress row runs the pane's full inner width, on the same grid as
    /// the block above it and the queue below.
    #[test]
    fn progress_row_spans_the_pane() {
        let mut st = playing_state();
        render(&mut st, 100, 26);
        let inner_w = 100 - 2 * PANE_INSET;
        assert_eq!(st.hit.gauge.x, PANE_INSET + 5, "elapsed label is 5 cells");
        assert_eq!(st.hit.gauge.width, inner_w - deck::TIME_W);
    }

    /// A still `■ pause` in the accent while audio runs, a held-back `▶ play`
    /// when it does not — and the same width either way, so the word does not
    /// shift under the cursor. It sits on the transport row, centred under the
    /// progress bar.
    #[test]
    fn play_state_marker_reports_running_or_stopped() {
        const STATE_ROW: usize = (HEADER_H + ART_TALL_H + ART_PROGRESS_H) as usize;
        let mut playing = playing_state();
        let lines = render(&mut playing, 80, 26);
        assert!(
            lines[STATE_ROW].contains("■ pause"),
            "{:?}",
            lines[STATE_ROW]
        );

        let mut paused = playing_state();
        paused.playback.as_mut().unwrap().is_playing = false;
        let lines = render(&mut paused, 80, 26);
        assert!(
            lines[STATE_ROW].contains("▶ play"),
            "{:?}",
            lines[STATE_ROW]
        );
        assert_eq!(playing.hit.play_btn, paused.hit.play_btn);

        let marker = |is_playing: bool| {
            let mut st = playing_state();
            st.playback.as_mut().unwrap().is_playing = is_playing;
            let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
            terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
            // The pill has no padding, so the glyph is its first cell.
            let (x, y) = (st.hit.play_btn.x, st.hit.play_btn.y);
            let cell = terminal
                .backend()
                .buffer()
                .cell(Position { x, y })
                .unwrap()
                .clone();
            (cell.symbol().to_string(), cell.fg)
        };
        let paused_fg = theme::stopped_dim().fg.unwrap();
        assert_eq!(marker(false), ("▶".to_string(), paused_fg));
        // The theme accent, held back rather than a colour of its own: a
        // resting state that still belongs to the sleeve on screen.
        let Color::Rgb(r, g, b) = paused_fg else {
            panic!("{paused_fg:?} is not truecolor")
        };
        let Color::Rgb(ar, ag, ab) = theme::accent_color() else {
            panic!("accent is not truecolor")
        };
        assert!(r <= ar && g <= ag && b <= ab, "{paused_fg:?} is not faded");
        assert!(
            (r, g, b) != (0, 0, 0) && (r, g, b) != (ar, ag, ab),
            "{paused_fg:?} is not a held-back accent"
        );
        assert_eq!(marker(true).0, "■");
        assert_ne!(marker(true).1, paused_fg);
    }

    /// The number is the row's position on screen. `display` is a permutation
    /// of `tracks`, so `Track::track_number` would drift out of step with it.
    #[test]
    fn queue_numbers_follow_display_order() {
        let mut st = playing_state();
        st.queue.as_mut().unwrap().display.reverse();
        let lines = render(&mut st, 80, 26);
        assert!(lines[21].contains(" 1   Gamma"), "{:?}", lines[21]);
        assert!(lines[23].contains(" 3   Alpha"), "{:?}", lines[23]);
    }

    #[test]
    fn short_pane_degrades_without_panicking() {
        // Widths sweep the `< 5` early return, the remainder distribution,
        // and well past the width cap.
        for width in [8u16, 9, 11, 40, 41, 79, 80, 101, 102, 160, 240] {
            for height in 0..27 {
                let mut st = playing_state();
                st.audio_tap.push(&loud_sine(), 1.0);
                render(&mut st, width, height);
                let mut st = playing_state();
                st.queue = None;
                st.playback = None;
                render(&mut st, width, height);
            }
        }
    }

    /// A cover with a known pattern, so a test can tell fg from bg and left
    /// from right without decoding anything.
    fn synthetic_cover(f: impl Fn(u32, u32) -> [u8; 3]) -> std::sync::Arc<Cover> {
        let size = crate::cover::COVER_PX;
        std::sync::Arc::new(Cover {
            url: "https://i.scdn.co/image/test".into(),
            px: (0..size as u32)
                .flat_map(|y| (0..size as u32).map(move |x| (x, y)))
                .map(|(x, y)| f(x, y))
                .collect(),
            ramp: None,
            size,
            accent: None,
        })
    }

    /// Every art cell must carry *both* a foreground and a background: an
    /// unset background lets the terminal's own paint through the bottom half
    /// of `▀`, and the cover renders as stripes.
    #[test]
    fn art_paints_half_blocks_with_fg_and_bg() {
        let mut st = playing_state();
        st.cover = Some(synthetic_cover(|x, y| [x as u8, y as u8, 128]));
        let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let art = st.hit.art;
        assert!(!art.is_empty());
        for y in art.y..art.bottom() {
            for x in art.x..art.right() {
                let cell = buffer.cell(Position { x, y }).unwrap();
                assert_eq!(cell.symbol(), "▀", "at {x},{y}");
                assert!(matches!(cell.fg, Color::Rgb(..)), "no fg at {x},{y}");
                assert!(matches!(cell.bg, Color::Rgb(..)), "no bg at {x},{y}");
            }
        }
    }

    /// Two vertically stacked pixels per cell is the whole point of the
    /// half-block trick, and nothing else exercises it.
    #[test]
    fn art_top_and_bottom_pixels_land_in_one_cell() {
        let mut st = playing_state();
        render(&mut st, 80, 26);
        let art = st.hit.art;

        // Put the colour change on an *odd* pixel row, so one cell straddles
        // it: an even row would leave both halves of every cell the same
        // colour and the test would pass on a renderer that ignored `bg`.
        let px_rows = art.height as u32 * 2;
        let split_row = (px_rows / 2) | 1;
        let boundary = split_row * crate::cover::COVER_PX as u32 / px_rows;
        st.cover = Some(synthetic_cover(move |_, y| {
            if y < boundary {
                [220, 0, 0]
            } else {
                [0, 0, 220]
            }
        }));

        let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let y = art.y + (split_row / 2) as u16;
        let cell = buffer.cell(Position { x: art.x, y }).unwrap();
        let (Color::Rgb(fr, _, fb), Color::Rgb(br, _, bb)) = (cell.fg, cell.bg) else {
            panic!("art cell is not truecolor: {cell:?}");
        };
        assert!(fr > fb, "upper pixel is not red: {:?}", cell.fg);
        assert!(bb > br, "lower pixel is not blue: {:?}", cell.bg);
    }

    /// Catches transposed or mirrored indexing, which a solid-colour cover
    /// would sail straight past.
    #[test]
    fn art_maps_source_left_to_pane_left() {
        let half = crate::cover::COVER_PX as u32 / 2;
        let mut st = playing_state();
        st.cover = Some(synthetic_cover(|x, _| {
            if x < half { [220, 0, 0] } else { [0, 0, 220] }
        }));
        let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let art = st.hit.art;
        let fg_at = |frac: u16| {
            buffer
                .cell(Position {
                    x: art.x + art.width * frac / 4,
                    y: art.y,
                })
                .unwrap()
                .fg
        };
        let Color::Rgb(lr, _, lb) = fg_at(1) else {
            panic!("not truecolor")
        };
        let Color::Rgb(rr, _, rb) = fg_at(3) else {
            panic!("not truecolor")
        };
        assert!(lr > lb, "left quarter is not red");
        assert!(rb > rr, "right quarter is not blue");
    }

    /// Half-block pixels are square only when the block is twice as wide as
    /// it is tall, so this is what keeps a square sleeve looking square.
    #[test]
    fn art_block_is_square_in_cells() {
        for (w, h) in [(80u16, 26u16), (94, 36), (100, 30)] {
            let mut st = playing_state();
            render(&mut st, w, h);
            let art = st.hit.art;
            assert!(!art.is_empty(), "{w}x{h}: no art");
            assert_eq!(art.width, 2 * art.height, "{w}x{h}: {art:?}");
        }
    }

    /// The decoded cover is held at a fixed resolution and resampled to the
    /// pane every frame, so a resize re-fits it without a re-fetch.
    #[test]
    fn art_resamples_to_the_pane() {
        let cover = synthetic_cover(|x, y| [x as u8, y as u8, 128]);
        let rect_at = |w, h| {
            let mut st = playing_state();
            st.cover = Some(std::sync::Arc::clone(&cover));
            render(&mut st, w, h);
            st.hit.art
        };
        let small = rect_at(55, 24);
        let large = rect_at(120, 40);
        assert_eq!(small.height, ART_SHORT_H);
        assert_eq!(large.height, ART_TALL_H);
        assert_ne!(small, large);
    }

    /// With no art the block keeps its exact footprint, so nothing reflows
    /// when the real cover lands — and it reads as artwork pending rather
    /// than as a hole.
    #[test]
    fn placeholder_fills_the_block_when_there_is_no_cover() {
        let mut st = playing_state();
        st.cover = None;
        let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let art = st.hit.art;
        let mut note = 0;
        for y in art.y..art.bottom() {
            for x in art.x..art.right() {
                let cell = buffer.cell(Position { x, y }).unwrap();
                assert!(matches!(cell.bg, Color::Rgb(..)), "no bg at {x},{y}");
                if cell.symbol() == "♫" {
                    note += 1;
                }
            }
        }
        assert_eq!(note, 1, "expected exactly one note glyph");
    }

    /// Seeded on the album, so a record's swatch does not reshuffle between
    /// its own tracks — or between frames.
    #[test]
    fn placeholder_is_stable_for_an_album() {
        let swatch = |album_id: Option<&str>| {
            let mut st = playing_state();
            st.cover = None;
            st.playback.as_mut().unwrap().album_id = album_id.map(Into::into);
            let mut terminal = Terminal::new(TestBackend::new(80, 26 + BRAND_H)).unwrap();
            terminal.draw(|f| draw(f, f.area(), &mut st)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            let art = st.hit.art;
            (art.y..art.bottom())
                .flat_map(|y| (art.x..art.right()).map(move |x| Position { x, y }))
                .map(|p| buffer.cell(p).unwrap().bg)
                .collect::<Vec<_>>()
        };
        assert_eq!(swatch(Some("alb1")), swatch(Some("alb1")));
        // Six palette entries, so a couple of ids could collide; two that do
        // not are enough to show the seed is actually used.
        assert!(
            (0..8).any(|i| swatch(Some("alb1")) != swatch(Some(&format!("other{i}")))),
            "every album got the same swatch"
        );
    }

    /// The row budget, pinned per tier. Every band's height is a constant, so
    /// a change to one of them should fail here rather than quietly eat the
    /// queue.
    #[test]
    fn the_row_budget_leaves_the_queue_usable() {
        // With a cover the field rides beside the sleeve, so `viz` — the
        // *stacked* tier's band — is zero in the art tiers.
        for (w, h, art, viz, queue) in [
            (94u16, 36u16, ART_TALL_H, 0u16, 15u16),
            (100, 30, ART_TALL_H, 0, 9),
            (80, 26, ART_TALL_H, 0, 5),
            (55, 24, ART_SHORT_H, 0, 7),
            (44, 30, 0, VIZ_H, 11),
        ] {
            let rows = Rows::new(w - 2 * PANE_INSET, h);
            assert_eq!(rows.art, art, "{w}x{h} cover");
            assert_eq!(rows.viz, viz, "{w}x{h} field");
            assert_eq!(rows.queue, queue, "{w}x{h} queue");
            assert_eq!(
                rows.art
                    + rows.header
                    + rows.progress
                    + rows.viz
                    + rows.transport
                    + rows.list_head
                    + rows.queue,
                h,
                "{w}x{h} does not add up"
            );
        }
    }

    /// Art is the first thing shed: the stacked fallback still has a
    /// spectrum, a progress bar and a title, so it costs the least.
    #[test]
    fn a_narrow_pane_drops_art_before_the_visualizer() {
        let mut st = playing_state();
        render(&mut st, 44, 30);
        assert!(st.hit.art.is_empty(), "{:?}", st.hit.art);
        assert!(!st.hit.viz.is_empty(), "the visualizer went first");
        assert_eq!(st.hit.viz.x, PANE_INSET);
    }

    #[test]
    fn a_short_pane_drops_art() {
        let mut st = playing_state();
        render(&mut st, 100, 17);
        assert!(st.hit.art.is_empty(), "{:?}", st.hit.art);
        assert!(!st.hit.viz.is_empty());
    }

    /// The artist and album lines are click targets, the way the bottom bar's
    /// are — but only when the API gave us an id to open.
    #[test]
    fn metadata_lines_are_links_when_their_ids_are_known() {
        let mut st = playing_state();
        render(&mut st, 100, 26);
        assert!(st.hit.now_artist.is_empty(), "linked without an artist id");
        assert!(st.hit.now_album.is_empty(), "linked without an album id");

        let mut st = playing_state();
        let pb = st.playback.as_mut().unwrap();
        pb.artist_id = Some("art1".into());
        pb.album_id = Some("alb1".into());
        render(&mut st, 100, 26);
        // Both sit on the one metadata row, the album after the separator.
        assert_eq!(st.hit.now_artist.y, BRAND_H + 1);
        assert_eq!(st.hit.now_album.y, BRAND_H + 1);
        // The rects stop at the text rather than running to the pane edge.
        assert_eq!(st.hit.now_artist.width, "Bob".len() as u16);
        assert_eq!(
            st.hit.now_album.x,
            st.hit.now_artist.right() + 3,
            "separator is 3 cells"
        );
        assert_eq!(st.hit.now_album.width, "Album Name".len() as u16);
    }

    /// A self-titled release renders as "Beta · Beta": both names are links,
    /// to the artist and to the record, so neither is dropped for repeating
    /// the other.
    #[test]
    fn the_album_line_keeps_a_name_that_repeats_the_track() {
        let mut st = playing_state();
        let pb = st.playback.as_mut().unwrap();
        pb.album = "Beta".into();
        pb.album_id = Some("alb1".into());
        let lines = render(&mut st, 80, 26);
        assert!(lines[1].contains("Bob · Beta · 2020"), "{:?}", lines[1]);
        assert!(!st.hit.now_album.is_empty(), "the album lost its link");
    }

    /// The metadata row runs out at the volume slider. A segment that will not
    /// fit is dropped whole, rather than clipped to a dangling separator.
    #[test]
    fn the_metadata_row_drops_segments_it_cannot_fit() {
        let mut st = playing_state();
        let lines = render(&mut st, 50, 30);
        assert!(lines[1].contains("Bob · Album Name"), "{:?}", lines[1]);
        assert!(
            !lines[1].contains("Album Name ·"),
            "dangling separator: {:?}",
            lines[1]
        );
        assert!(!lines[1].contains("2020"), "{:?}", lines[1]);
        // Wider, and the year comes back.
        let lines = render(&mut st, 80, 26);
        assert!(
            lines[1].contains("Bob · Album Name · 2020"),
            "{:?}",
            lines[1]
        );
    }

    /// Dumps the rendered view so a human can look at it:
    /// `cargo test --quiet dump_player -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn dump_player() {
        let mut st = playing_state();
        st.cover = Some(synthetic_cover(|x, y| {
            let (fx, fy) = (x as f32 / 64.0, y as f32 / 64.0);
            [
                (255.0 * (0.35 + 0.5 * fx)) as u8,
                (255.0 * (0.15 + 0.35 * fy)) as u8,
                (255.0 * (0.55 - 0.4 * fx * fy)) as u8,
            ]
        }));
        st.audio_tap.push(&loud_sine(), 1.0);
        for (w, h) in [(94u16, 36u16), (80, 26), (50, 30)] {
            println!("--- {w}x{h} ---");
            for line in render(&mut st, w, h) {
                println!("|{line}|");
            }
        }
        st.cover = None;
        for (w, h) in [(94u16, 36u16), (140, 34)] {
            println!("--- {w}x{h}, no cover ---");
            for line in render(&mut st, w, h) {
                println!("|{line}|");
            }
        }
    }

    #[test]
    fn band_layout_fills_its_rect_up_to_the_cap() {
        for width in 5..=500u16 {
            let b = band_layout(width);
            let n = b.n as u16;
            assert!(
                (MIN_BANDS..=MAX_BANDS).contains(&n),
                "width {width} gave {n} bands"
            );
            // Every band is a bar and the gap after it, less the gap after the
            // last one — whatever the bars are individually worth.
            let used: u16 = (0..b.n).map(|i| b.width(i) + 1).sum::<u16>() - 1;
            assert_eq!(used, b.used, "width {width}");
            assert_eq!(b.x(b.n - 1) + b.width(b.n - 1), b.used, "width {width}");
            // It never overruns the rect it was given, except at the minimum
            // band count where the pane is simply too narrow to draw at all.
            assert!(
                b.used <= width || n == MIN_BANDS,
                "width {width} overrun by {}",
                b.used
            );
            // And it leaves nothing unused — at most the one odd cell a
            // two-cell stride cannot fill, since past the cap the bars widen
            // to take the rest.
            assert!(
                width.saturating_sub(b.used) <= 1,
                "width {width} only used {}",
                b.used
            );
        }
    }
}
