use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::theme;

/// The keymap, grouped for display. This is the single source of truth the
/// README's key table must mirror (see the drift test below).
pub const KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "Playback",
        &[
            ("Space", "play / pause"),
            ("n / p", "next / previous track"),
            ("h / l", "seek -5s / +5s"),
            ("- / =", "volume down / up"),
            ("s", "toggle shuffle"),
        ],
    ),
    (
        "Navigation",
        &[
            ("j / k", "move down / up"),
            ("g / G", "jump to top / bottom"),
            ("Ctrl-d/u", "half page down / up"),
            ("H", "go home"),
            ("v", "toggle player view (Esc closes)"),
            ("← / →", "switch search or radio tab"),
            ("Backspace", "back to previous view"),
            ("Esc", "back / close overlay"),
        ],
    ),
    (
        "Browse & play",
        &[
            ("Enter", "open item / play selection"),
            ("x", "play without opening"),
            ("a", "add selected track to queue"),
            ("L", "like the track, or save the station"),
            ("b / B", "open track's album / artist"),
            ("o / O", "cycle sort / flip direction"),
            ("/", "search Spotify, or radio stations"),
            ("R", "refresh view & playlists"),
        ],
    ),
    (
        "Other",
        &[("?", "toggle this help"), ("q", "quit"), ("Ctrl-c", "quit")],
    ),
];

const MOUSE_HINTS: &[&str] = &[
    "click ♫ spot, top left of either view, to go home",
    "on Home, one click anywhere on a row opens it",
    "click rows to select / open · double-click plays",
    "click the ★ column, or the ★ on the bar, to like a track",
    "on a radio page, click a tab to change what it lists",
    "click an artist or album name to open its page",
    "on an album card: the name opens it, ▶ play starts it",
    "click a step of the path above a page to go there",
    "opening a page already on the path walks back to it",
    "click tabs, transport, ▶ play, sliders",
    "click the search row at the top to start searching",
    "click the visualizer to play / pause; the cover opens its album",
    "scroll lists; scroll over the bottom bar = volume",
];

pub fn draw(frame: &mut Frame) {
    let mut lines: Vec<Line> = Vec::new();
    let group_style = theme::accent().add_modifier(Modifier::BOLD);
    for (i, (group, entries)) in KEYS.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::styled(format!("  {group}"), group_style));
        for (key, action) in entries.iter() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<11}"), theme::text()),
                Span::raw(*action),
            ]));
        }
    }
    lines.push(Line::default());
    lines.push(Line::styled("  Mouse", group_style));
    for hint in MOUSE_HINTS {
        lines.push(Line::styled(format!("  {hint}"), theme::text()));
    }

    let height = lines.len() as u16 + 2;
    let area = centered(frame.area(), 56, height);
    frame.render_widget(Clear, area);
    let para = Paragraph::new(lines).block(
        Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::accent()),
    );
    frame.render_widget(para, area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
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

#[cfg(test)]
mod tests {
    use super::KEYS;

    /// The README key table must carry every binding the help overlay
    /// shows. Comparison ignores backticks and spaces so markdown styling
    /// doesn't matter.
    #[test]
    fn readme_documents_every_key() {
        let readme: String = include_str!("../../README.md")
            .chars()
            .filter(|c| *c != '`' && *c != ' ')
            .collect();
        for (_, entries) in KEYS {
            for (key, action) in entries.iter() {
                let needle: String = key.chars().filter(|c| *c != ' ').collect();
                assert!(
                    readme.contains(&needle),
                    "README key table is missing `{key}` ({action})"
                );
            }
        }
    }
}
