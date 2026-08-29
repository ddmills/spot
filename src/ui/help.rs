use ratatui::Frame;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::table::centered;
use super::theme;
use crate::app::state::{AppState, UpdateState};

/// The keymap, grouped for display. This is the single source of truth the
/// README's key table must mirror (see the drift test below).
pub const KEYS: &[(&str, &[(&str, &str)])] = &[
    (
        "Playback",
        &[
            ("Space", "play / pause"),
            ("n / p", "next / previous track or station"),
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
            ("V", "cycle the visualizer (in the player view)"),
            ("← / →", "switch search, artist or radio tab"),
            ("Backspace", "back to previous view"),
            ("Esc", "back / close overlay"),
        ],
    ),
    (
        "Browse & play",
        &[
            ("Enter", "open item / play selection"),
            ("x", "play without opening"),
            ("a", "play the selected track next"),
            ("L", "like the track, or save the station"),
            ("F", "save the playlist you are on"),
            ("E", "edit a playlist of your own"),
            ("b / B", "open track's album / artist"),
            ("o / O", "cycle sort / flip direction"),
            ("/", "search Spotify and radio"),
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
    "every track row ends in ★ ⧉ +: like it, copy its link, or put it on a playlist",
    "the same three sit on the bar and in the player, for what is playing",
    "⧉ share beside ▶ shuffle copies the link to the page you are on",
    "in that box, click a playlist to put the track on it or take it off",
    "+ new playlist in that box makes one and puts the track on it",
    "on a radio or artist page, click a tab to change what it lists",
    "click an artist or album name to open its page",
    "where a record credits several artists, each name opens its own",
    "on a station, spot looks the announced track up on Spotify",
    "on a station, ◂◂ and ▸▸ walk what you have listened to",
    "on an album card: the name opens it, ▶ play starts it, ▶ shuffle mixes it",
    "on a playlist page, ☆ save keeps it; edit renames one of your own",
    "click a step of the path, beside the mark in either view, to go there",
    "opening a page already on the path walks back to it",
    "click tabs, transport, ▶ play, ▶ shuffle, sliders",
    "click vol beside the slider to mute; mut puts the level back",
    "when a page will not load, ↻ try again asks for it again; so does Enter",
    "on a browse page, click the search row under the path to search",
    "click the visualizer to play / pause",
    "click any cover to see the whole sleeve; click again or Esc closes it",
    "scroll lists; scroll over the bottom bar = volume",
];

pub fn draw(frame: &mut Frame, state: &AppState) {
    let mut lines: Vec<Line> = Vec::new();
    let group_style = theme::accent().add_modifier(Modifier::BOLD);
    // First, not last: the box is taller than the keymap deserves and a short
    // terminal clips its bottom, so a version line under the mouse hints would
    // be invisible on exactly the windows spot is usually run in.
    lines.push(Line::styled(format!("  {}", version(state)), theme::dim()));
    for (group, entries) in KEYS.iter() {
        lines.push(Line::default());
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

/// The build being run, and what Home is offering if it is offering anything.
///
/// The only place the version appears in the UI. It belongs here rather than
/// beside the mark, where it would push the two views' shared header apart.
fn version(state: &AppState) -> String {
    const RUNNING: &str = concat!("spot v", env!("CARGO_PKG_VERSION"));
    match &state.update {
        Some(UpdateState::Available(release)) => format!("{RUNNING} · {} available", release.tag),
        Some(UpdateState::Installing) => format!("{RUNNING} · downloading an update"),
        Some(UpdateState::Installed) => format!("{RUNNING} · restart to finish the update"),
        Some(UpdateState::Failed) | None => RUNNING.to_string(),
    }
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
