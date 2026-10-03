//! The overlay `?` opens: every key the TUI takes, grouped by where it
//! works. It's drawn from [`KEYS`], one table, so that what it says and
//! what the README says can be checked against each other.

use super::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// Where a key works, which is the heading it's listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Sidebar,
    Pane,
    Question,
    Mouse,
}

impl Section {
    fn heading(self) -> &'static str {
        match self {
            Section::Sidebar => "In the sidebar",
            Section::Pane => "In a pane",
            Section::Question => "Answering a question",
            Section::Mouse => "With the mouse",
        }
    }
}

/// A key, or a few that go together, and what it does.
pub struct Key {
    /// How the key is written, with `/` between keys that go together:
    /// `j/k ↓/↑`.
    pub label: &'static str,
    pub does: &'static str,
    pub section: Section,
}

/// Every key the TUI takes, in the order the overlay lists them.
pub const KEYS: &[Key] = &[
    sidebar("j/k ↓/↑", "select a session"),
    sidebar("Enter", "type into it, or rerun"),
    sidebar("Tab/Shift+Tab", "next / previous pane"),
    sidebar("s", "split off, or unsplit"),
    sidebar("PageUp/PageDown", "page its history"),
    sidebar("n", "start a new session"),
    sidebar("w", "start one in a worktree"),
    sidebar("W", "remove its worktree"),
    sidebar("r", "rename it"),
    sidebar("x", "kill it"),
    sidebar("u", "next one that needs you"),
    sidebar("/", "find a session"),
    sidebar("o", "open its pull request"),
    sidebar("i", "its project's issues"),
    sidebar("d", "diff of its worktree"),
    sidebar("p", "find a file to edit"),
    sidebar("?", "show these keys"),
    sidebar("q", "quit; sessions keep on"),
    in_pane("Ctrl+\\", "back to the sidebar"),
    in_pane("Shift+PgUp", "page back"),
    in_pane("Shift+PgDn", "page forward"),
    in_pane("other keys", "go to the program"),
    question("y", "yes; any other key, no"),
    question("Enter", "answer"),
    question("Esc", "cancel"),
    question("Ctrl+U", "clear the answer"),
    mouse("click", "select; focus a pane"),
    mouse("wheel", "move, or scroll a pane"),
    mouse("Shift+drag", "select text"),
];

const fn sidebar(label: &'static str, does: &'static str) -> Key {
    Key {
        label,
        does,
        section: Section::Sidebar,
    }
}

const fn in_pane(label: &'static str, does: &'static str) -> Key {
    Key {
        label,
        does,
        section: Section::Pane,
    }
}

const fn question(label: &'static str, does: &'static str) -> Key {
    Key {
        label,
        does,
        section: Section::Question,
    }
}

const fn mouse(label: &'static str, does: &'static str) -> Key {
    Key {
        label,
        does,
        section: Section::Mouse,
    }
}

/// The overlay's left column: the sidebar's keys.
const LEFT: &[Section] = &[Section::Sidebar];

/// The overlay's right column: everything else.
const RIGHT: &[Section] = &[Section::Pane, Section::Question, Section::Mouse];

/// Space between the two columns.
const GAP: u16 = 2;

/// Columns kept clear on each side of the overlay, inside its edge.
const SIDE: u16 = 2;

/// Draws the overlay over the middle of `area`: a panel of the theme's
/// own, or, where the theme paints nothing, a thin frame.
pub fn draw(frame: &mut Frame, theme: &Theme, area: Rect) {
    let left = column(LEFT, theme);
    let right = column(RIGHT, theme);
    let (width, height) = size(&left, &right);
    let overlay = centered(area, width, height);

    frame.render_widget(Clear, overlay);
    let framed = theme.panel == Color::Reset;
    let block = if framed {
        Block::bordered().border_style(Style::new().fg(theme.rule))
    } else {
        Block::new()
    };
    let title = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let block = block
        .style(Style::new().bg(theme.panel).fg(theme.text))
        .title(Line::styled(" keys ", title))
        .title_bottom(Line::styled(
            " any key closes this ",
            Style::new().fg(theme.muted),
        ));
    // The title rows are kept either way; a frame takes a column a side of
    // the room around the columns.
    let margin = if framed { SIDE - 1 } else { SIDE };
    let inside = block.inner(overlay).inner(Margin::new(margin, 0));
    frame.render_widget(block, overlay);

    let [left_area, _, right_area] = Layout::horizontal([
        Constraint::Length(widest(&left)),
        Constraint::Length(GAP),
        Constraint::Fill(1),
    ])
    .areas(inside);
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
}

/// The lines of one column: each section's heading, then its keys, with
/// the keys lined up and a blank line between sections.
fn column(sections: &[Section], theme: &Theme) -> Vec<Line<'static>> {
    let keys: Vec<&Key> = KEYS
        .iter()
        .filter(|key| sections.contains(&key.section))
        .collect();
    let label_width = keys.iter().map(|key| width(key.label)).max().unwrap_or(0);

    let mut lines = Vec::new();
    for &section in sections {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        let heading = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
        lines.push(Line::styled(section.heading(), heading));
        for key in keys.iter().filter(|key| key.section == section) {
            let padding = " ".repeat(label_width - width(key.label) + 2);
            lines.push(Line::from(vec![
                Span::styled(key.label, Style::new().fg(theme.accent)),
                Span::raw(padding),
                Span::styled(key.does, Style::new().fg(theme.text)),
            ]));
        }
    }
    lines
}

/// The overlay's size: both columns side by side, the room on each side,
/// and a row above and below for its title and how to close it.
fn size(left: &[Line], right: &[Line]) -> (u16, u16) {
    let width = SIDE * 2 + widest(left) + GAP + widest(right);
    let height = 2 + left.len().max(right.len()) as u16;
    (width, height)
}

fn widest(lines: &[Line]) -> u16 {
    lines
        .iter()
        .map(|line| line.width() as u16)
        .max()
        .unwrap_or(0)
}

/// How many columns `text` takes on screen. Every key label is one column
/// a character, arrows included.
fn width(text: &str) -> usize {
    text.chars().count()
}

/// A `width` by `height` rectangle in the middle of `area`, cut down to
/// fit it.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;

    fn theme() -> Theme {
        Theme::new(ThemeName::Dark, false)
    }

    /// The keys a label stands for: `j/k ↓/↑` is j, k, ↓ and ↑. A label
    /// that's only `/` is that key.
    fn keys_in(label: &str) -> Vec<String> {
        if label == "/" {
            return vec![label.to_string()];
        }
        label
            .split(['/', ' '])
            .filter(|key| !key.is_empty())
            .map(String::from)
            .collect()
    }

    /// The keys in each row of the README's table of sidebar keys: what's
    /// between backticks in its first column.
    fn readme_sidebar_keys() -> Vec<Vec<String>> {
        let readme = include_str!("../../README.md");
        let table = readme
            .split("| Key | In the sidebar |")
            .nth(1)
            .expect("the README has a table of sidebar keys");
        table
            .lines()
            .skip(2)
            .take_while(|line| line.starts_with('|'))
            .map(|row| {
                let first_column = row.split(" | ").next().unwrap();
                first_column
                    .split('`')
                    .skip(1)
                    .step_by(2)
                    .map(String::from)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_overlay_and_the_readme_list_the_same_sidebar_keys() {
        let mut overlay: Vec<Vec<String>> = KEYS
            .iter()
            .filter(|key| key.section == Section::Sidebar)
            .map(|key| keys_in(key.label))
            .collect();
        let mut readme = readme_sidebar_keys();
        overlay.sort();
        readme.sort();
        assert_eq!(overlay, readme);
    }

    #[test]
    fn the_overlay_fits_an_80_by_24_terminal() {
        let (width, height) = size(&column(LEFT, &theme()), &column(RIGHT, &theme()));
        assert!(width <= 80, "the overlay is {width} columns wide");
        // The footer keeps the bottom row.
        assert!(height <= 23, "the overlay is {height} rows high");
    }

    #[test]
    fn every_section_is_in_a_column() {
        for key in KEYS {
            assert!(
                LEFT.contains(&key.section) || RIGHT.contains(&key.section),
                "{} isn't shown",
                key.label
            );
        }
    }

    #[test]
    fn a_key_label_lines_up_with_the_others() {
        let lines = column(LEFT, &theme());
        // Heading first, then `j/k ↓/↑`, padded out to `PageUp/PageDown`.
        let first: String = lines[1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let padded = format!("{:<17}select a session", "j/k ↓/↑");
        assert_eq!(first, padded);
    }
}
