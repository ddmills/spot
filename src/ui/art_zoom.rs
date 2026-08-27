//! The cover-art block a click expanded, filling the screen.
//!
//! One picture and nothing else: no controls, no chrome, no layout budget. It
//! is what the sleeve looks like when the terminal is the frame.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Clear;

use super::table;
use crate::app::state::AppState;

/// Columns of daylight either side of the block when the width is what bounds
/// it. One, so the picture is as large as the terminal allows and still reads
/// as a picture rather than as a screen that has been painted over.
const PAD: u16 = 1;

/// Paint the whole sleeve, as large as the screen can seat it, over everything
/// on it.
///
/// The block is square, so the shorter side of the terminal is what bounds it:
/// the rows there are, or the rows the width can square, whichever is fewer.
/// The picture is therefore always whole — nothing is cropped and there is
/// nothing to scroll — and it is centred in whatever the longer side has left
/// over.
pub fn draw(frame: &mut Frame, state: &AppState) {
    let screen = frame.area();
    let Some(zoom) = state.art_zoom.as_ref() else {
        return;
    };
    if screen.width <= 2 * PAD {
        return;
    }
    let rows = screen.height.min(table::art_rows(screen.width - 2 * PAD));
    if rows == 0 {
        return;
    }
    let cols = table::art_w(rows);
    let cover = state.art_cover(&zoom.source);
    let seed = zoom.seed.as_str();

    let block = Rect {
        x: screen.x + (screen.width - cols) / 2,
        y: screen.y + (screen.height - rows) / 2,
        width: cols,
        height: rows,
    };
    frame.render_widget(Clear, screen);
    table::draw_art(frame, block, cover.as_deref(), seed);

    // The toast goes back on top. The bottom bar draws its own, under this, and
    // the transport keys still work while the picture is up — several of them
    // report through a toast and nothing else, so without this they would act
    // silently.
    if let Some((msg, _)) = &state.toast {
        let row = Rect {
            y: screen.bottom().saturating_sub(1),
            height: 1,
            ..screen
        };
        super::now_playing::draw_toast(frame, row, Some(msg.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;
    use crate::app::state::{ArtSource, ArtZoom};
    use crate::cover::{COVER_PX, Cover};

    /// A cover split across a horizontal seam: red above, blue below, so the
    /// block can say which part of the picture it is showing.
    fn split_cover(seam: usize) -> Cover {
        let px = (0..COVER_PX)
            .flat_map(|y| {
                let c = if y < seam { [255, 0, 0] } else { [0, 0, 255] };
                (0..COVER_PX).map(move |_| c)
            })
            .collect();
        Cover {
            url: "https://i.scdn.co/image/zoom".into(),
            px,
            size: COVER_PX,
            accent: None,
            ramp: None,
        }
    }

    fn zoomed(cover: Option<Cover>, w: u16, h: u16) -> (AppState, ratatui::buffer::Buffer) {
        let mut st = AppState::new();
        let source = match &cover {
            Some(c) => ArtSource::Page(Some(c.url.clone())),
            None => ArtSource::Page(None),
        };
        if let Some(c) = cover {
            st.view_cover = Some(std::sync::Arc::new(c));
        }
        st.art_zoom = Some(ArtZoom {
            source,
            seed: "seed".into(),
        });
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, &st)).unwrap();
        let buf = term.backend().buffer().clone();
        (st, buf)
    }

    /// The bounds of what was painted, as `(x, y, width, height)`.
    fn painted(buf: &ratatui::buffer::Buffer) -> (u16, u16, u16, u16) {
        let area = buf.area();
        let cells: Vec<(u16, u16)> = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter(|(x, y)| buf.cell((*x, *y)).unwrap().symbol() != " ")
            .collect();
        let xs = cells.iter().map(|c| c.0);
        let ys = cells.iter().map(|c| c.1);
        let (x0, x1) = (xs.clone().min().unwrap(), xs.max().unwrap());
        let (y0, y1) = (ys.clone().min().unwrap(), ys.max().unwrap());
        (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
    }

    /// A terminal wider than it is tall: the rows are what run out, so the
    /// block takes all of them and the width keeps what it does not need.
    #[test]
    fn a_wide_screen_is_bounded_by_its_rows() {
        let (_, buf) = zoomed(Some(split_cover(COVER_PX / 2)), 80, 24);
        // 24 rows square to 48 cells, well inside the 78 the width offers.
        assert_eq!(painted(&buf), (16, 0, 48, 24));
    }

    /// A terminal taller than it is wide: the width is what runs out, so the
    /// block takes it less a column each side and the rows keep the rest.
    #[test]
    fn a_tall_screen_is_bounded_by_its_width() {
        let (_, buf) = zoomed(Some(split_cover(COVER_PX / 2)), 40, 60);
        // 38 cells of width square to 19 rows.
        assert_eq!(painted(&buf), (1, 20, 38, 19));
    }

    /// Every cell of the block carries both halves of its pixel pair, or the
    /// picture reads as stripes.
    #[test]
    fn the_block_paints_both_halves_of_every_cell() {
        let (_, buf) = zoomed(Some(split_cover(COVER_PX / 2)), 80, 24);
        let (x, y, w, h) = painted(&buf);
        for cy in y..y + h {
            for cx in x..x + w {
                let cell = buf.cell((cx, cy)).unwrap();
                assert_eq!(cell.symbol(), "\u{2580}", "at {cx},{cy}");
                assert!(matches!(cell.fg, Color::Rgb(..)), "fg at {cx},{cy}");
                assert!(matches!(cell.bg, Color::Rgb(..)), "bg at {cx},{cy}");
            }
        }
    }

    /// The whole sleeve, not a band of it: the seam the cover carries lands
    /// halfway down the block, with the picture's own top and bottom at the
    /// block's.
    #[test]
    fn the_whole_cover_is_shown() {
        let (_, buf) = zoomed(Some(split_cover(COVER_PX / 2)), 80, 24);
        let (x, y, w, h) = painted(&buf);
        let mid = x + w / 2;
        assert_eq!(buf.cell((mid, y)).unwrap().fg, Color::Rgb(255, 0, 0));
        assert_eq!(
            buf.cell((mid, y + h - 1)).unwrap().bg,
            Color::Rgb(0, 0, 255)
        );
        let reds = (y..y + h)
            .filter(|cy| buf.cell((mid, *cy)).unwrap().fg == Color::Rgb(255, 0, 0))
            .count();
        assert_eq!(reds, h as usize / 2, "the seam is not halfway down");
    }

    #[test]
    fn the_placeholder_fills_the_expanded_block() {
        let (_, buf) = zoomed(None, 80, 24);
        let (x, y, w, h) = painted(&buf);
        let mut notes = 0;
        for cy in y..y + h {
            for cx in x..x + w {
                let cell = buf.cell((cx, cy)).unwrap();
                assert!(matches!(cell.bg, Color::Rgb(..)), "bg at {cx},{cy}");
                if cell.symbol() == "\u{266b}" {
                    notes += 1;
                }
            }
        }
        // One note, in the middle of the block it belongs to.
        assert_eq!(notes, 1);
        assert_eq!(
            buf.cell((x + w / 2, y + h / 2)).unwrap().symbol(),
            "\u{266b}"
        );
    }

    /// A terminal with no room for a block draws nothing rather than panicking
    /// on the arithmetic.
    #[test]
    fn a_screen_too_small_for_a_block_draws_nothing() {
        for (w, h) in [(2, 10), (1, 1), (10, 1), (3, 4), (4, 2)] {
            let mut st = AppState::new();
            st.art_zoom = Some(ArtZoom {
                source: ArtSource::Page(None),
                seed: "seed".into(),
            });
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &st)).unwrap();
        }
    }
}
