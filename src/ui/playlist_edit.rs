//! The edit-playlist box: the name and the blurb of a playlist you own.
//!
//! An overlay rather than a page, for the reason the add-to-playlist box is
//! one — the edit is about the playlist behind it, and walking away to type
//! would lose the thing being edited.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::table::{centered, fit, segment};
use super::theme;
use crate::app::state::{AppState, EditField, EditTarget, PlaylistEdit};

const BOX_W: u16 = 52;
/// Rows the blurb field spends. Spotify allows three hundred characters,
/// which is more than one row of this box holds and less than anyone types.
const DESCRIPTION_H: u16 = 3;
const SAVE_PILL: &str = "save";
const CREATE_PILL: &str = "create";
const NAME_LABEL: &str = "name";
const DESCRIPTION_LABEL: &str = "about";
/// Widest label, so the two fields start at the same cell.
const LABEL_W: u16 = 6;

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let Some(edit) = state.edit.clone() else {
        return;
    };
    let mouse = state.mouse_pos;
    let status = status_line(&edit);

    // Name row, blurb rows, a blank, the control row, and the status when
    // there is one — plus the border.
    let height = 1 + DESCRIPTION_H + 1 + 1 + u16::from(status.is_some()) + 2;
    let making = matches!(edit.target, EditTarget::New { .. });
    let area = centered(frame.area(), BOX_W, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(match making {
                true => " New playlist ",
                false => " Edit playlist ",
            })
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::accent()),
        area,
    );
    if area.width < 3 || area.height < 3 {
        return;
    }
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    };

    let name = Rect { height: 1, ..inner };
    draw_field(frame, name, NAME_LABEL, &edit, EditField::Name, mouse);
    state.hit.edit_name = name;

    let description = Rect {
        y: name.y + 1,
        height: DESCRIPTION_H,
        ..inner
    };
    draw_field(
        frame,
        description,
        DESCRIPTION_LABEL,
        &edit,
        EditField::Description,
        mouse,
    );
    state.hit.edit_description = description;

    let controls = Rect {
        y: description.y + DESCRIPTION_H + 1,
        height: 1,
        ..inner
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut x = controls.x;
    // Inert while a change is in flight, and it says so rather than looking
    // like a control that ignored you.
    let (label, style) = match (edit.pending, making) {
        (true, true) => ("creating…", theme::dim()),
        (true, false) => ("saving…", theme::dim()),
        (false, true) => (CREATE_PILL, theme::accent()),
        (false, false) => (SAVE_PILL, theme::accent()),
    };
    let rect = segment(
        &mut spans,
        &mut x,
        controls,
        mouse,
        vec![Span::styled(label, style)],
    );
    state.hit.edit_save = if edit.pending { Rect::default() } else { rect };
    spans.push(Span::styled("   tab switches · esc cancels", theme::dim()));
    frame.render_widget(Paragraph::new(Line::from(spans)), controls);

    if let Some(line) = status {
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                y: controls.y + 1,
                height: 1,
                ..inner
            },
        );
    }
}

/// One field: a label, then the fill that makes it read as a box you can type
/// in. The caret marks the field the keys go to, so the two never both look
/// live.
fn draw_field(
    frame: &mut Frame,
    area: Rect,
    label: &'static str,
    edit: &PlaylistEdit,
    field: EditField,
    mouse: Option<Position>,
) {
    let hover = mouse.is_some_and(|m| area.contains(m));
    frame.render_widget(Paragraph::new("").style(theme::field(hover)), area);
    let focused = edit.field == field && !edit.pending;
    let value = match field {
        EditField::Name => &edit.name,
        EditField::Description => &edit.description,
    };
    let text = match focused {
        true => format!("{value}▏"),
        false => value.clone(),
    };
    let style = match value.is_empty() && !focused {
        true => theme::dim(),
        false => theme::text(),
    };
    let body_w = area.width.saturating_sub(LABEL_W) as usize;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(fit(label, LABEL_W as usize), theme::dim()),
            Span::styled(text, style),
        ]))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .style(theme::field(hover)),
        Rect {
            width: (LABEL_W as usize + body_w) as u16,
            ..area
        },
    );
}

/// What the box says under its controls: a refusal, and nothing otherwise.
fn status_line(edit: &PlaylistEdit) -> Option<Line<'static>> {
    edit.error
        .as_ref()
        .map(|e| Line::styled(format!(" {e}"), theme::warn()))
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn open(target: EditTarget) -> AppState {
        let mut st = AppState::new();
        st.edit = Some(PlaylistEdit {
            target,
            name: "Roadtrip".into(),
            description: String::new(),
            field: EditField::Name,
            pending: false,
            error: None,
            seq: 1,
        });
        st
    }

    fn screen(state: &mut AppState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..24)
            .map(|y| {
                (0..80)
                    .filter_map(|x| buffer.cell(Position { x, y }).map(|c| c.symbol()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A box about a playlist that exists says what it does to it.
    #[test]
    fn an_edit_box_saves() {
        let mut st = open(EditTarget::Existing("p1".into()));
        let text = screen(&mut st);
        assert!(text.contains("Edit playlist"), "{text}");
        assert!(text.contains(SAVE_PILL), "{text}");
    }

    /// A box about a playlist that does not exist yet says the other thing —
    /// a control marked `save` on a playlist there is nothing to save would
    /// name a change to something that is not there.
    #[test]
    fn a_new_box_creates() {
        let mut st = open(EditTarget::New {
            uri: "spotify:track:x".into(),
        });
        let text = screen(&mut st);
        assert!(text.contains("New playlist"), "{text}");
        assert!(text.contains(CREATE_PILL), "{text}");
        assert!(!text.contains(SAVE_PILL), "{text}");
    }

    /// A create in flight holds the box inert and says so.
    #[test]
    fn a_pending_new_box_says_it_is_creating() {
        let mut st = open(EditTarget::New {
            uri: "spotify:track:x".into(),
        });
        st.edit.as_mut().unwrap().pending = true;
        let text = screen(&mut st);
        assert!(text.contains("creating…"), "{text}");
        assert!(st.hit.edit_save.is_empty());
    }
}
