//! The overlay `?` opens: every key the TUI takes, grouped by where it
//! works. It's drawn from [`KEYS`], one table, so that what it says and
//! what the README says can be checked against each other. A key that
//! belongs to one of crystal's plugins is only listed while that plugin is
//! on, and the keys installed plugins' actions took are listed after the
//! sidebar's own.

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
    /// Keys that run installed plugins' actions.
    Plugins,
    Pane,
    Question,
    NewSession,
    Mouse,
}

impl Section {
    fn heading(self) -> &'static str {
        match self {
            Section::Sidebar => "In the sidebar",
            Section::Plugins => "Your plugins",
            Section::Pane => "In a pane",
            Section::Question => "Answering a question",
            Section::NewSession => "Starting a session",
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
    /// The plugin the key is part of, which has to be on for it to work.
    pub plugin: Option<&'static str>,
}

/// Every key the TUI takes, in the order the overlay lists them.
pub const KEYS: &[Key] = &[
    sidebar("j/k ↓/↑", "select a session"),
    sidebar("Enter", "type into it, or rerun"),
    sidebar("Tab/Shift+Tab", "next / previous pane"),
    sidebar("s/z/v", "split / zoom / copy"),
    sidebar("F H/L S", "float / move / layouts"),
    sidebar("PgUp/PgDn e", "page / edit its history"),
    sidebar("t/T/&", "tab: new / name / close"),
    sidebar("[/] 1-9 >", "switch tabs / move it"),
    sidebar("n/w", "new, or in a worktree"),
    sidebar("W", "remove its worktree"),
    sidebar("r/x", "rename / kill it"),
    of_plugin("tasks", "c", "close its task"),
    of_plugin("flows", "g/f", "flow: go on / send back"),
    sidebar("u /", "next needing you / find"),
    of_plugin("github", "o/i", "pull request / issues"),
    sidebar("d/p", "diff / find a file"),
    of_plugin("backlog", "b", "the project's backlog"),
    of_plugin("memory", "m", "what it has remembered"),
    of_plugin("profiles", "P", "your agent profiles"),
    sidebar("?/X/,/q", "keys/plugins/options/quit"),
    in_pane("Ctrl+\\", "back to the sidebar"),
    in_pane("Shift+PgUp", "page back"),
    in_pane("Shift+PgDn", "page forward"),
    in_pane("other keys", "go to the program"),
    question("y", "yes; any other key, no"),
    question("Enter/Esc", "answer / cancel"),
    question("Ctrl+U", "clear the answer"),
    new_session("Tab ←/→", "next row, choose"),
    new_session("↑/↓", "earlier tasks"),
    new_session("Alt+Enter", "new line in the task"),
    new_session("Ctrl+E", "edit the command line"),
    mouse("click", "a session, pane, tab"),
    mouse("wheel", "move, or scroll a pane"),
    mouse("drag", "select and copy"),
];

const fn key(section: Section, label: &'static str, does: &'static str) -> Key {
    Key {
        label,
        does,
        section,
        plugin: None,
    }
}

const fn sidebar(label: &'static str, does: &'static str) -> Key {
    key(Section::Sidebar, label, does)
}

/// A sidebar key that's part of the plugin called `plugin`.
const fn of_plugin(plugin: &'static str, label: &'static str, does: &'static str) -> Key {
    Key {
        plugin: Some(plugin),
        ..sidebar(label, does)
    }
}

const fn in_pane(label: &'static str, does: &'static str) -> Key {
    key(Section::Pane, label, does)
}

const fn question(label: &'static str, does: &'static str) -> Key {
    key(Section::Question, label, does)
}

const fn new_session(label: &'static str, does: &'static str) -> Key {
    key(Section::NewSession, label, does)
}

const fn mouse(label: &'static str, does: &'static str) -> Key {
    key(Section::Mouse, label, does)
}

/// What the overlay lists beyond crystal's keys that are always there:
/// which of crystal's plugins are on, and the keys installed plugins'
/// actions took, each with what it does.
pub struct Shown<'a> {
    pub plugin_on: &'a dyn Fn(&str) -> bool,
    pub plugin_keys: &'a [(String, String)],
}

/// A row of the overlay: a key, or a few, and what it does.
struct Row {
    section: Section,
    label: String,
    does: String,
}

/// The overlay's left column: the sidebar's keys.
const LEFT: &[Section] = &[Section::Sidebar, Section::Plugins];

/// The overlay's right column: everything else.
const RIGHT: &[Section] = &[
    Section::Pane,
    Section::Question,
    Section::NewSession,
    Section::Mouse,
];

/// The sections left out, in this order, when there isn't room for them
/// all: what the new-session panel's own footer says, then how to answer
/// a question, which the question says.
const LEFT_OUT_FIRST: &[Section] = &[Section::NewSession, Section::Question];

/// Space between the two columns.
const GAP: u16 = 2;

/// Columns kept clear on each side of the overlay, inside its edge.
const SIDE: u16 = 2;

/// Draws the overlay over the middle of `area`: a panel of the theme's
/// own, or, where the theme paints nothing, a thin frame.
pub fn draw(frame: &mut Frame, theme: &Theme, area: Rect, shown: &Shown) {
    let rows = rows(shown);
    // The footer keeps the bottom row, and the overlay's title and how to
    // close it take one each.
    let room = usize::from(area.height.saturating_sub(3));
    let (left, right) = columns(&rows, room);
    let left = column(&left, &rows, theme);
    let right = column(&right, &rows, theme);
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

/// Which sections go in each column to fit `room` lines: the sidebar's
/// keys on the left, with the plugins' under them while they fit, and
/// everything else on the right. When the left is too long, the plugins'
/// keys start the right column instead; when the right is too long then,
/// the sections [`LEFT_OUT_FIRST`] names are left out until it fits.
fn columns(rows: &[Row], room: usize) -> (Vec<Section>, Vec<Section>) {
    let lines = |sections: &[Section]| column_lines(sections, rows);
    let mut left = LEFT.to_vec();
    let mut right = RIGHT.to_vec();
    if lines(&left) > room {
        left.retain(|section| *section != Section::Plugins);
        right.insert(0, Section::Plugins);
    }
    for section in LEFT_OUT_FIRST {
        if lines(&right) <= room {
            break;
        }
        right.retain(|kept| kept != section);
    }
    (left, right)
}

/// How many lines a column of `sections` takes: see [`column`].
fn column_lines(sections: &[Section], rows: &[Row]) -> usize {
    let shown: Vec<Section> = sections
        .iter()
        .copied()
        .filter(|section| rows.iter().any(|row| row.section == *section))
        .collect();
    let keys = rows
        .iter()
        .filter(|row| shown.contains(&row.section))
        .count();
    // Each section's heading, and a blank line between sections.
    keys + shown.len() + shown.len().saturating_sub(1)
}

/// Every row the overlay shows: crystal's keys, but those of plugins that
/// are off, then the keys installed plugins took.
fn rows(shown: &Shown) -> Vec<Row> {
    let own = KEYS
        .iter()
        .filter(|key| key.plugin.is_none_or(|plugin| (shown.plugin_on)(plugin)))
        .map(|key| Row {
            section: key.section,
            label: key.label.to_string(),
            does: key.does.to_string(),
        });
    let plugins = shown.plugin_keys.iter().map(|(label, does)| Row {
        section: Section::Plugins,
        label: label.clone(),
        does: does.clone(),
    });
    own.chain(plugins).collect()
}

/// The lines of one column: each section's heading, then its keys, with
/// the keys lined up and a blank line between sections. A section with no
/// keys isn't shown.
fn column(sections: &[Section], rows: &[Row], theme: &Theme) -> Vec<Line<'static>> {
    let rows: Vec<&Row> = rows
        .iter()
        .filter(|row| sections.contains(&row.section))
        .collect();
    let label_width = rows.iter().map(|row| width(&row.label)).max().unwrap_or(0);

    let mut lines = Vec::new();
    for &section in sections {
        if !rows.iter().any(|row| row.section == section) {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        let heading = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
        lines.push(Line::styled(section.heading(), heading));
        for row in rows.iter().filter(|row| row.section == section) {
            let padding = " ".repeat(label_width - width(&row.label) + 2);
            lines.push(Line::from(vec![
                Span::styled(row.label.clone(), Style::new().fg(theme.accent)),
                Span::raw(padding),
                Span::styled(row.does.clone(), Style::new().fg(theme.text)),
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

    /// The keys a label stands for: `j/k ↓/↑` is j, k, ↓ and ↑. A `/` on
    /// its own between spaces is that key: `u /` is u and /.
    fn keys_in(label: &str) -> Vec<String> {
        label
            .split(' ')
            .flat_map(|group| match group {
                "/" => vec![group],
                _ => group.split('/').collect(),
            })
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
        // The overlay puts keys that go together on one row to fit a small
        // terminal, and the README gives most a row each, so the keys are
        // compared, not the rows.
        let mut overlay: Vec<String> = KEYS
            .iter()
            .filter(|key| key.section == Section::Sidebar)
            .flat_map(|key| keys_in(key.label))
            .collect();
        let mut readme: Vec<String> = readme_sidebar_keys().into_iter().flatten().collect();
        overlay.sort();
        readme.sort();
        assert_eq!(overlay, readme);
    }

    /// Every row, with every plugin on and none installed.
    fn all_rows() -> Vec<Row> {
        rows(&Shown {
            plugin_on: &|_| true,
            plugin_keys: &[],
        })
    }

    #[test]
    fn the_keys_of_a_plugin_thats_off_arent_listed() {
        let on_but_memory = |plugin: &str| plugin != "memory";
        let keys = [("N".to_string(), "notes: add a note".to_string())];
        let rows = rows(&Shown {
            plugin_on: &on_but_memory,
            plugin_keys: &keys,
        });
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert!(!labels.contains(&"m"), "{labels:?}");
        assert!(labels.contains(&"b"), "{labels:?}");
        let text: Vec<String> = column(LEFT, &rows, &theme())
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert!(text.contains(&"Your plugins".to_string()), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.starts_with("N ") && line.ends_with("notes: add a note"))
        );
    }

    #[test]
    fn every_key_the_sidebar_takes_is_kept_from_plugins() {
        for key in KEYS.iter().filter(|key| key.section == Section::Sidebar) {
            let letters = keys_in(key.label).into_iter();
            for part in letters.filter(|part| part.len() == 1) {
                assert!(
                    crate::plugins::RESERVED_KEYS.contains(part.as_str()),
                    "{part} isn't kept from plugins"
                );
            }
        }
    }

    #[test]
    fn the_overlay_fits_an_80_by_24_terminal() {
        let rows = all_rows();
        let (width, height) = size(
            &column(LEFT, &rows, &theme()),
            &column(RIGHT, &rows, &theme()),
        );
        assert!(width <= 80, "the overlay is {width} columns wide");
        // The footer keeps the bottom row.
        assert!(height <= 23, "the overlay is {height} rows high");
    }

    #[test]
    fn a_column_is_as_long_as_it_says() {
        let rows = all_rows();
        for sections in [LEFT, RIGHT] {
            let lines = column(sections, &rows, &theme()).len();
            assert_eq!(column_lines(sections, &rows), lines);
        }
    }

    /// Every row, with every plugin on and `installed` keys taken by
    /// installed plugins' actions.
    fn rows_with_installed(installed: usize) -> Vec<Row> {
        let keys: Vec<(String, String)> = (0..installed)
            .map(|n| (format!("{n}"), format!("plugin action {n}")))
            .collect();
        rows(&Shown {
            plugin_on: &|_| true,
            plugin_keys: &keys,
        })
    }

    #[test]
    fn with_room_the_plugins_keys_go_under_the_sidebars_and_nothing_is_left_out() {
        let rows = rows_with_installed(2);
        let (left, right) = columns(&rows, 40);
        assert_eq!(left, LEFT);
        assert_eq!(right, RIGHT);
    }

    #[test]
    fn at_80_by_24_installed_plugins_keys_are_still_shown() {
        // 24 rows leave 21 for the columns.
        for installed in 1..=4 {
            let rows = rows_with_installed(installed);
            let (left, right) = columns(&rows, 21);
            assert!(right.contains(&Section::Plugins), "{installed}: {right:?}");
            assert!(column_lines(&left, &rows) <= 21);
            assert!(column_lines(&right, &rows) <= 21, "{installed}: {right:?}");
            assert!(right.contains(&Section::Pane) && right.contains(&Section::Mouse));
            assert!(!right.contains(&Section::NewSession));
        }
        // With more, how to answer a question goes too.
        let rows = rows_with_installed(8);
        let (_, right) = columns(&rows, 21);
        assert_eq!(right, [Section::Plugins, Section::Pane, Section::Mouse]);
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
        let lines = column(LEFT, &all_rows(), &theme());
        // Heading first, then `j/k ↓/↑`, padded out to `Tab/Shift+Tab`.
        let first: String = lines[1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let padded = format!("{:<15}select a session", "j/k ↓/↑");
        assert_eq!(first, padded);
    }
}
