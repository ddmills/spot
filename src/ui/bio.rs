//! The article about an artist, open over the page it is about.
//!
//! An overlay rather than a page, for the reason the add-to-playlist box is
//! one: it is about the artist you are standing on, and walking away to read
//! it would lose the thing being read about. The header band wraps the opening
//! of this into the rows it has; the rest of it lives here.
//!
//! Text and nothing else. The artist's portrait is on the page a keypress
//! behind the box, and a copy of it here would cost a third of the reading
//! column to say what the page already says.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::table::{centered, draw_scrollbar, fit, width, wrap};
use super::theme;
use crate::app::state::{AppState, MainView};

/// The widest the box gets. Wider than the help box, because this is prose,
/// and a measure much past seventy columns stops being readable.
const BOX_W: u16 = 76;
const BOX_H: u16 = 26;

const CONTROLS: &str = "j / k  scroll · Esc  close";

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let Some(popup) = state.bio.as_ref() else {
        return;
    };
    // The box is about one artist. A page swapped under it — a link opened
    // from outside the app is the way that happens — takes it with them,
    // rather than leaving one artist's history under another's name.
    if !matches!(&state.main, MainView::Artist(v) if v.id == popup.artist_id) {
        state.bio = None;
        return;
    }

    let screen = frame.area();
    let box_w = BOX_W.min(screen.width.saturating_sub(4));
    if box_w < 3 {
        return;
    }
    // The prose column is a function of the width alone, so it can be measured
    // before the height is chosen — and the height then follows the text, the
    // way the add-to-playlist box follows its rows. A short article in a box of
    // empty rows reads as a page that failed to load. The last cell of the
    // column belongs to the scrollbar.
    let prose_w = (box_w - 2).saturating_sub(1);
    if prose_w == 0 {
        return;
    }
    let wrapped = wrap(&popup.bio.text, prose_w as usize);
    // Chrome: two border rows, one blank, one footer.
    let box_h = (wrapped.len() as u16 + 4)
        .min(BOX_H)
        .min(screen.height.saturating_sub(2));
    let area = centered(screen, box_w, box_h);
    if area.width < 3 || area.height < 3 {
        return;
    }
    // Trimmed, not padded: `fit` pads to its width, and a padded title would
    // draw the artist's name across the whole of the top border.
    let title = format!(
        " {} ",
        fit(&popup.name, area.width.saturating_sub(4) as usize).trim_end()
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::accent()),
        area,
    );
    state.hit.bio_box = area;

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };
    // One blank row and one footer row under the prose.
    let body = Rect {
        height: inner.height.saturating_sub(2),
        ..inner
    };

    // The whole width, less the cell the scrollbar sits in. The portrait is a
    // keypress away on the page behind this box, and repeating it here would
    // spend a third of a reading column saying what the page already says.
    let prose = Rect {
        width: body.width.saturating_sub(1),
        ..body
    };
    if prose.width == 0 {
        return;
    }

    let popup = state.bio.as_mut().expect("checked above");
    // There is no resize event to rebuild the wrap on — `event::handle_event`
    // reads keys and mouse and drops the rest — so the frame that notices the
    // width has changed is the one that fixes it.
    if popup.wrapped_w != prose.width {
        popup.lines = wrap(&popup.bio.text, prose.width as usize);
        popup.wrapped_w = prose.width;
    }
    popup.offset = popup
        .offset
        .min(popup.lines.len().saturating_sub(prose.height as usize));

    let lines: Vec<Line> = popup
        .lines
        .iter()
        .skip(popup.offset)
        .take(prose.height as usize)
        .map(|l| Line::styled(l.clone(), theme::text()))
        .collect();
    frame.render_widget(Paragraph::new(lines), prose);
    state.hit.bio_body = prose;

    draw_scrollbar(
        frame,
        Rect {
            x: inner.right(),
            width: 1,
            y: prose.y,
            height: prose.height,
        },
        popup.lines.len(),
        popup.offset,
    );

    let credit = credit(&popup.bio.source_url);
    draw_footer(frame, inner, &credit);
}

/// The article's address without its scheme, which is how a reader would say
/// it and how a browser would show it.
fn credit(source_url: &str) -> String {
    source_url
        .strip_prefix("https://")
        .unwrap_or(source_url)
        .to_string()
}

/// What the box can be done to, and where what is in it came from.
///
/// The credit is not decoration: the prose is CC BY-SA, and this is the only
/// place spot says whose it is. It is the first thing dropped when the row is
/// too narrow for both, because a half-printed address credits nobody.
fn draw_footer(frame: &mut Frame, inner: Rect, credit: &str) {
    let footer = Rect {
        y: inner.bottom().saturating_sub(1),
        height: 1,
        ..inner
    };
    let dim = theme::dim();
    frame.render_widget(Paragraph::new(Line::styled(CONTROLS, dim)), footer);
    let credit_w = width(credit) as u16;
    if footer.width >= width(CONTROLS) as u16 + credit_w + 2 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                credit.to_string(),
                dim.add_modifier(Modifier::ITALIC),
            )),
            Rect {
                x: footer.right() - credit_w,
                width: credit_w,
                ..footer
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;

    use super::*;
    use crate::app::state::{ArtistBio, ArtistTab, ArtistView, BioPopup, BioState, TrackList};

    const PROSE: &str = "Muse are an English rock band from Teignmouth, Devon, formed in 1994. The band consists of Matt Bellamy, Chris Wolstenholme and Dominic Howard.\n\nThey released Showbiz in 1999.";

    fn opened(text: &str, photo: Option<&str>) -> AppState {
        let mut st = AppState::new();
        let mut view = ArtistView {
            id: "r1".into(),
            uri: "spotify:artist:r1".into(),
            name: "Muse".into(),
            image_url: photo.map(Into::into),
            genres: Vec::new(),
            bio: BioState::default(),
            top: TrackList::new("Muse", "top tracks", None),
            albums: Vec::new().into(),
            tab: ArtistTab::Albums,
            loading: false,
            error: None,
        };
        let bio = Arc::new(ArtistBio {
            text: text.into(),
            image_url: None,
            source_url: "https://en.wikipedia.org/wiki/Muse_(band)".into(),
        });
        view.bio = BioState::Ready(Arc::clone(&bio));
        st.main = MainView::Artist(view);
        st.bio = Some(BioPopup::new("r1".into(), "Muse".into(), bio));
        st
    }

    fn render(st: &mut AppState, w: u16, h: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, st)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .filter_map(|x| buf.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_box_wears_the_artists_name_and_opens_at_the_first_line() {
        let mut st = opened(PROSE, None);
        let lines = render(&mut st, 100, 30);
        let all = lines.join("\n");
        assert!(all.contains("Muse"), "the title names nobody");
        assert!(all.contains("Teignmouth"), "the lead is not on screen");
        assert!(
            all.contains("en.wikipedia.org/wiki/Muse_(band)"),
            "no credit"
        );
        assert!(all.contains("Esc"), "the box says nothing about closing");
        assert!(!st.hit.bio_box.is_empty());
        assert!(!st.hit.bio_body.is_empty());
    }

    /// The wrap belongs to the width the frame is drawing into, and there is no
    /// resize event to rebuild it on — so the frame that notices is the frame
    /// that fixes it.
    #[test]
    fn a_narrower_terminal_re_wraps_the_prose() {
        let mut st = opened(PROSE, None);
        render(&mut st, 100, 30);
        let wide = st.bio.as_ref().unwrap().lines.len();
        render(&mut st, 46, 30);
        let narrow = st.bio.as_ref().unwrap();
        assert!(narrow.lines.len() > wide, "the prose did not re-wrap");
        assert!(
            narrow
                .lines
                .iter()
                .all(|l| width(l) <= narrow.wrapped_w as usize)
        );
    }

    /// The prose starts at the inner edge and runs to the scrollbar. The
    /// artist's portrait is on the page a keypress behind this box and is not
    /// repeated in it, whatever the box has room for.
    #[test]
    fn the_prose_owns_the_whole_width() {
        for w in [100, 60, 44] {
            let mut st = opened(PROSE, Some("https://i.scdn.co/image/artist"));
            render(&mut st, w, 30);
            let (body, boxed) = (st.hit.bio_body, st.hit.bio_box);
            assert_eq!(body.x, boxed.x + 1, "a column was kept for a picture");
            assert_eq!(body.width, boxed.width - 3, "the prose is not full width");
        }
    }

    /// An offset left past the end of a shorter wrap comes back rather than
    /// showing a box of nothing.
    #[test]
    fn an_offset_past_the_end_is_pulled_back() {
        let mut st = opened(PROSE, None);
        render(&mut st, 100, 30);
        st.bio.as_mut().unwrap().offset = 9_000;
        render(&mut st, 100, 30);
        let popup = st.bio.as_ref().unwrap();
        assert!(popup.offset <= popup.lines.len());
        assert!(popup.offset < 9_000);
    }

    /// A page swapped under the box takes it with them: one artist's history
    /// must never sit under another's name.
    #[test]
    fn a_page_swapped_underneath_closes_the_box() {
        let mut st = opened(PROSE, None);
        st.main = MainView::Home;
        render(&mut st, 100, 30);
        assert!(st.bio.is_none());
    }

    /// A terminal with no room for a box draws nothing rather than panicking on
    /// the arithmetic.
    #[test]
    fn a_screen_too_small_for_a_box_draws_nothing() {
        for (w, h) in [(2, 10), (1, 1), (10, 1), (3, 4), (4, 2), (6, 6)] {
            let mut st = opened(PROSE, None);
            render(&mut st, w, h);
        }
    }
}
