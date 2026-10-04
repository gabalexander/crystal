//! The overlay `?` opens: every key the TUI takes, grouped by where it
//! works. The sidebar's keys come from the keymap's table,
//! [`keymap::HELP`], written as the user's `[keys]` has them; the others
//! from [`KEYS`]. The README's table of sidebar keys is checked against
//! the defaults. A key that belongs to one of crystal's plugins is only
//! listed while that plugin is on, and the keys installed plugins' actions
//! took are listed after the sidebar's own.

use super::keymap::{self, Chord, Keymap, ModeKey};
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
    /// The user's own, from `[[keys.command]]`.
    Commands,
    Pane,
    Resize,
    View,
    Question,
    NewSession,
    TextBox,
    Mouse,
}

impl Section {
    fn heading(self) -> &'static str {
        match self {
            Section::Sidebar => "In the sidebar",
            Section::Plugins => "Your plugins",
            Section::Commands => "Your commands",
            Section::Pane => "In a pane",
            Section::Resize => "In resize mode",
            Section::View => "In a view",
            Section::Question => "Answering a question",
            Section::NewSession => "Starting a session",
            Section::TextBox => "In a text box",
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

/// Every key the TUI takes but the sidebar's commands, the prefix and the
/// key that hands the keyboard back, which the keymap has, in the order
/// the overlay lists them.
pub const KEYS: &[Key] = &[
    in_pane("Shift+PgUp", "page back"),
    in_pane("Shift+PgDn", "page forward"),
    in_pane("other keys", "go to the program"),
    in_pane("Ctrl+C", "stop a task's run"),
    in_pane("Space", "a task's follow-up"),
    question("y", "yes; any other key, no"),
    question("Enter/Esc", "answer / cancel"),
    new_session("Tab ←/→", "next row, choose"),
    new_session("↑/↓", "earlier tasks"),
    new_session("Alt+Enter", "new line in the task"),
    new_session("Ctrl+E", "edit the command line"),
    new_session("Esc", "close, keeping a draft"),
    text_box("Alt+B/F", "a word back / on"),
    text_box("Ctrl+A/E", "start / end of line"),
    text_box("Ctrl+W", "delete a word back"),
    text_box("Alt+D", "delete a word on"),
    text_box("Ctrl+U/K", "delete to start / end"),
    mouse("click", "a session, pane, tab"),
    mouse("right click", "a menu of what it does"),
    mouse("wheel/drag", "scroll / select, copy"),
    mouse("2×/3× click", "select a word / a line"),
    mouse("scrollbar", "drag it to scroll"),
    mouse("Ctrl+click", "open a link"),
];

const fn key(section: Section, label: &'static str, does: &'static str) -> Key {
    Key {
        label,
        does,
        section,
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

const fn text_box(label: &'static str, does: &'static str) -> Key {
    key(Section::TextBox, label, does)
}

const fn mouse(label: &'static str, does: &'static str) -> Key {
    key(Section::Mouse, label, does)
}

/// What the overlay lists beyond crystal's keys that are always there:
/// which of crystal's plugins are on, the keys installed plugins' actions
/// took, each with what it does, and the keys as the user's `[keys]` has
/// them.
pub struct Shown<'a> {
    pub plugin_on: &'a dyn Fn(&str) -> bool,
    pub plugin_keys: &'a [(String, String)],
    pub keymap: &'a Keymap,
}

/// A row of the overlay: a key, or a few, and what it does.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    section: Section,
    label: String,
    does: String,
}

/// The order the sections are listed in, page after page.
const ORDER: &[Section] = &[
    Section::Sidebar,
    Section::Plugins,
    Section::Commands,
    Section::Pane,
    Section::Resize,
    Section::View,
    Section::Question,
    Section::NewSession,
    Section::TextBox,
    Section::Mouse,
];

/// Space between the two columns.
const GAP: u16 = 2;

/// Columns kept clear on each side of the overlay, inside its edge.
const SIDE: u16 = 2;

/// A line of a column: a section's heading, the blank line before a
/// heading that isn't the first, or a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry<'a> {
    Heading(Section),
    Blank,
    Key(&'a Row),
}

/// What one page of the overlay shows: a column, and another beside it
/// when both fit.
struct Page<'a> {
    left: Vec<Entry<'a>>,
    right: Vec<Entry<'a>>,
}

/// How many pages the overlay takes to list every key in `area`.
pub fn page_count(shown: &Shown, area: Rect) -> usize {
    paged(&rows(shown), area).len()
}

/// Draws page `page` of the overlay over the middle of `area`: a panel of
/// the theme's own, or, where the theme paints nothing, a thin frame.
pub fn draw(frame: &mut Frame, theme: &Theme, area: Rect, shown: &Shown, page: usize) {
    let rows = rows(shown);
    let pages = paged(&rows, area);
    let page = page.min(pages.len() - 1);
    let left = lines(&pages[page].left, theme);
    let right = lines(&pages[page].right, theme);
    let (width, height) = size(&pages[page]);
    let overlay = centered(area, width, height);

    frame.render_widget(Clear, overlay);
    let framed = theme.panel == Color::Reset;
    let block = if framed {
        Block::bordered().border_style(Style::new().fg(theme.rule))
    } else {
        Block::new()
    };
    let title = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let closing = match pages.len() {
        1 => " any key closes this ".to_string(),
        count => format!(" {}/{count} · ← → turn · any other key closes ", page + 1),
    };
    let block = block
        .style(Style::new().bg(theme.panel).fg(theme.text))
        .title(Line::styled(" keys ", title))
        .title_bottom(Line::styled(closing, Style::new().fg(theme.muted)));
    // The title rows are kept either way; a frame takes a column a side of
    // the room around the columns.
    let margin = if framed { SIDE - 1 } else { SIDE };
    let inside = block.inner(overlay).inner(Margin::new(margin, 0));
    frame.render_widget(block, overlay);

    // A page of one column has no gap to keep beside it.
    let gap = if right.is_empty() { 0 } else { GAP };
    let [left_area, _, right_area] = Layout::horizontal([
        Constraint::Length(widest(&left)),
        Constraint::Length(gap),
        Constraint::Fill(1),
    ])
    .areas(inside);
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
}

/// The overlay's pages in `area`: the keys flowed into columns as tall as
/// it leaves room for, two to a page where both fit its width.
fn paged(rows: &[Row], area: Rect) -> Vec<Page<'_>> {
    // The footer keeps the bottom row, and the overlay's title and how to
    // close it take one each.
    let room = usize::from(area.height.saturating_sub(3));
    let mut columns = flow(rows, room).into_iter().peekable();
    let mut pages = Vec::new();
    while let Some(left) = columns.next() {
        let fits = |right: &Vec<Entry>| {
            let both = Page {
                left: left.clone(),
                right: right.clone(),
            };
            size(&both).0 <= area.width
        };
        let right = columns.next_if(fits).unwrap_or_default();
        pages.push(Page { left, right });
    }
    pages
}

/// The keys in columns of no more than `room` lines, in [`ORDER`]. A
/// section that doesn't fit what's left of a column starts the next one,
/// and only one too long for a whole column is split, going on in the next
/// under its heading again.
fn flow(rows: &[Row], room: usize) -> Vec<Vec<Entry<'_>>> {
    // A heading and a key, however small the terminal.
    let room = room.max(2);
    let mut columns: Vec<Vec<Entry>> = vec![Vec::new()];
    for &section in ORDER {
        let keys: Vec<&Row> = rows.iter().filter(|row| row.section == section).collect();
        let mut keys = keys.as_slice();
        while let Some(column) = columns.last_mut()
            && !keys.is_empty()
        {
            let blank = usize::from(!column.is_empty());
            // How many keys fit under the heading, here.
            let free = room.saturating_sub(column.len() + blank + 1);
            let fits_a_column = keys.len() < room;
            if !column.is_empty() && (free == 0 || free < keys.len() && fits_a_column) {
                columns.push(Vec::new());
                continue;
            }
            if blank == 1 {
                column.push(Entry::Blank);
            }
            column.push(Entry::Heading(section));
            let fits = (room - column.len()).min(keys.len());
            column.extend(keys[..fits].iter().map(|row| Entry::Key(row)));
            keys = &keys[fits..];
        }
    }
    columns.retain(|column| !column.is_empty());
    columns
}

/// How long what a key of the user's own does may be in the overlay, the
/// rest cut, so that it fits a column.
const COMMAND_WIDTH: usize = 28;

/// Every row the overlay shows: the sidebar's commands, but those of
/// plugins that are off and those the user left with no key, then the keys
/// installed plugins took, then the user's own, then the prefix, the
/// hand-back key and the keys the user gave to work in a pane, then resize
/// mode's and the views', then the rest.
fn rows(shown: &Shown) -> Vec<Row> {
    let keymap = shown.keymap;
    let sidebar = keymap::HELP
        .iter()
        .filter(|row| row.plugin.is_none_or(|plugin| (shown.plugin_on)(plugin)))
        .filter_map(|row| {
            Some(Row {
                section: Section::Sidebar,
                label: keymap.row_label(row)?,
                does: row.does.to_string(),
            })
        });
    let plugins = shown.plugin_keys.iter().map(|(label, does)| Row {
        section: Section::Plugins,
        label: label.clone(),
        does: does.clone(),
    });
    let hand_back = Row {
        section: Section::Pane,
        label: keymap.hand_back().label(),
        does: "back to the sidebar".to_string(),
    };
    let commands = keymap.custom().iter().filter_map(|(command, chords)| {
        let mut does: String = command.label().chars().take(COMMAND_WIDTH).collect();
        if command.label().chars().count() > COMMAND_WIDTH {
            does.push('…');
        }
        Some(Row {
            section: Section::Commands,
            label: chords.first()?.label(),
            does,
        })
    });
    let prefixes: Vec<String> = keymap.prefixes().iter().map(|key| key.label()).collect();
    let prefix = (!prefixes.is_empty()).then(|| Row {
        section: Section::Pane,
        label: format!("{}, a key", prefixes.join("/")),
        does: "that key's command".to_string(),
    });
    // The keys the user gave to work in a pane without the prefix.
    let direct = keymap::COMMANDS.iter().flat_map(|spec| {
        let keys = keymap.keys(spec.command).iter();
        keys.filter(|key| keymap.is_direct(**key)).map(|key| Row {
            section: Section::Pane,
            label: key.label(),
            does: spec.does.to_string(),
        })
    });
    let resize = keymap::RESIZE_HELP.iter().filter_map(|row| {
        Some(Row {
            section: Section::Resize,
            label: keymap.row_label(row)?,
            does: row.does.to_string(),
        })
    });
    let views = VIEW_ROWS.iter().map(|&(one, other, does)| Row {
        section: Section::View,
        label: view_label(keymap, one, other),
        does: does.to_string(),
    });
    let own = KEYS.iter().map(|key| Row {
        section: key.section,
        label: key.label.to_string(),
        does: key.does.to_string(),
    });
    sidebar
        .chain(plugins)
        .chain(commands)
        .chain([hand_back])
        .chain(prefix)
        .chain(direct)
        .chain(resize)
        .chain(views)
        .chain(own)
        .collect()
}

/// The views' keys in the overlay, two to a row.
const VIEW_ROWS: &[(ModeKey, ModeKey, &str)] = &[
    (ModeKey::ViewDown, ModeKey::ViewUp, "the row below / above"),
    (
        ModeKey::ViewPageDown,
        ModeKey::ViewPageUp,
        "a page on / back",
    ),
    (
        ModeKey::ViewOpen,
        ModeKey::ViewClose,
        "open / close, or back",
    ),
];

/// How the overlay writes two of the views' keys: their own keys, which
/// always work, then the first of the others each has, as the user's
/// `[keys]` or the defaults give them: `↓/↑ j/k`, `Enter/Esc q`.
fn view_label(keymap: &Keymap, one: ModeKey, other: ModeKey) -> String {
    let own = |key: ModeKey| {
        let own = keymap::mode_spec_of(key).own;
        own.and_then(|own| Chord::parse(own).ok())
            .map_or(String::new(), |own| own.label())
    };
    let first = |key: ModeKey| keymap.mode_keys(key).first().map(Chord::label);
    let mut label = format!("{}/{}", own(one), own(other));
    match (first(one), first(other)) {
        (None, None) => {}
        (Some(one), None) => label += &format!(" {one}/-"),
        (None, Some(other)) => label += &format!(" {other}"),
        (Some(one), Some(other)) => label += &format!(" {one}/{other}"),
    }
    label
}

/// The widest key label in `column`, which the others are padded out to.
fn label_width(column: &[Entry]) -> usize {
    column
        .iter()
        .filter_map(|entry| match entry {
            Entry::Key(row) => Some(width(&row.label)),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// How many columns `column` takes on screen, its keys lined up.
fn column_width(column: &[Entry]) -> u16 {
    let labels = label_width(column);
    let widest = column
        .iter()
        .map(|entry| match entry {
            Entry::Heading(section) => width(section.heading()),
            Entry::Blank => 0,
            Entry::Key(row) => labels + 2 + width(&row.does),
        })
        .max()
        .unwrap_or(0);
    widest as u16
}

/// The lines of one column: the headings in bold, and the keys lined up
/// after them.
fn lines(column: &[Entry], theme: &Theme) -> Vec<Line<'static>> {
    let labels = label_width(column);
    let heading = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
    column
        .iter()
        .map(|entry| match entry {
            Entry::Heading(section) => Line::styled(section.heading(), heading),
            Entry::Blank => Line::from(""),
            Entry::Key(row) => {
                let padding = " ".repeat(labels - width(&row.label) + 2);
                Line::from(vec![
                    Span::styled(row.label.clone(), Style::new().fg(theme.accent)),
                    Span::raw(padding),
                    Span::styled(row.does.clone(), Style::new().fg(theme.text)),
                ])
            }
        })
        .collect()
}

/// A page's size: its columns side by side, the room on each side, and a
/// row above and below for its title and how to close it.
fn size(page: &Page) -> (u16, u16) {
    let right = if page.right.is_empty() {
        0
    } else {
        GAP + column_width(&page.right)
    };
    let width = SIDE * 2 + column_width(&page.left) + right;
    let height = 2 + page.left.len().max(page.right.len()) as u16;
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
        Theme::new(ThemeName::DARK, false)
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
    /// between backticks in its first column, with a `|` written `\|` so
    /// it doesn't end the cell.
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
                    .map(|key| key.replace("\\|", "|"))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_overlay_and_the_readme_list_the_same_sidebar_keys() {
        // The overlay puts keys that go together on one row to fit a small
        // terminal, and the README gives most a row each, so the keys are
        // compared, not the rows.
        let mut overlay: Vec<String> = keymap::HELP
            .iter()
            .flat_map(|row| keys_in(row.label))
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
            keymap: &Keymap::default(),
        })
    }

    /// An 80 by 24 terminal.
    const SMALL: Rect = Rect::new(0, 0, 80, 24);

    /// Every key on every page, in order.
    fn keys_on(pages: &[Page]) -> Vec<String> {
        pages
            .iter()
            .flat_map(|page| page.left.iter().chain(&page.right))
            .filter_map(|entry| match entry {
                Entry::Key(row) => Some(format!("{} {}", row.label, row.does)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_keys_of_a_plugin_thats_off_arent_listed() {
        let on_but_memory = |plugin: &str| plugin != "memory";
        let keys = [("N".to_string(), "notes: add a note".to_string())];
        let rows = rows(&Shown {
            plugin_on: &on_but_memory,
            plugin_keys: &keys,
            keymap: &Keymap::default(),
        });
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert!(!labels.contains(&"m"), "{labels:?}");
        assert!(labels.contains(&"b"), "{labels:?}");
        let pages = paged(&rows, SMALL);
        let headings: Vec<Section> = pages
            .iter()
            .flat_map(|page| page.left.iter().chain(&page.right))
            .filter_map(|entry| match entry {
                Entry::Heading(section) => Some(*section),
                _ => None,
            })
            .collect();
        assert!(headings.contains(&Section::Plugins), "{headings:?}");
        assert!(keys_on(&pages).contains(&"N notes: add a note".to_string()));
    }

    #[test]
    fn every_key_the_sidebar_takes_is_kept_from_plugins() {
        for row in keymap::HELP {
            let letters = keys_in(row.label).into_iter();
            for part in letters.filter(|part| part.len() == 1) {
                assert!(
                    crate::plugins::RESERVED_KEYS.contains(part.as_str()),
                    "{part} isn't kept from plugins"
                );
            }
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
            keymap: &Keymap::default(),
        })
    }

    #[test]
    fn every_page_fits_an_80_by_24_terminal() {
        for installed in 0..=8 {
            for page in paged(&rows_with_installed(installed), SMALL) {
                let (width, height) = size(&page);
                assert!(width <= 80, "a page is {width} columns wide");
                // The footer keeps the bottom row.
                assert!(height <= 23, "a page is {height} rows high");
            }
        }
    }

    #[test]
    fn every_key_is_on_a_page_once() {
        for installed in [0, 3, 8] {
            let rows = rows_with_installed(installed);
            let listed: Vec<String> = ORDER
                .iter()
                .flat_map(|section| rows.iter().filter(|row| row.section == *section))
                .map(|row| format!("{} {}", row.label, row.does))
                .collect();
            assert_eq!(keys_on(&paged(&rows, SMALL)), listed);
        }
    }

    #[test]
    fn with_room_every_key_is_on_one_page() {
        let rows = all_rows();
        let pages = paged(&rows, Rect::new(0, 0, 200, 80));
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn a_narrow_terminal_shows_a_column_a_page() {
        let rows = all_rows();
        let pages = paged(&rows, Rect::new(0, 0, 45, 24));
        assert!(pages.len() > 2);
        assert!(pages.iter().all(|page| page.right.is_empty()));
    }

    #[test]
    fn a_section_too_long_for_a_column_goes_on_under_its_heading() {
        let rows = all_rows();
        let columns = flow(&rows, 21);
        assert_eq!(columns[0][0], Entry::Heading(Section::Sidebar));
        assert_eq!(columns[1][0], Entry::Heading(Section::Sidebar));
    }

    #[test]
    fn a_short_section_isnt_split_and_no_heading_ends_a_column() {
        let rows = all_rows();
        for room in 3..40 {
            for column in flow(&rows, room) {
                assert!(column.len() <= room, "{room}: {column:?}");
                let last = column.last();
                assert!(matches!(last, Some(Entry::Key(_))), "{room}: {column:?}");
            }
        }
        // At 80 by 24 the mouse's keys go together.
        let columns = flow(&rows, 21);
        let mouse_headings = columns
            .iter()
            .flatten()
            .filter(|entry| **entry == Entry::Heading(Section::Mouse))
            .count();
        assert_eq!(mouse_headings, 1);
    }

    #[test]
    fn every_section_is_listed() {
        for key in KEYS {
            assert!(ORDER.contains(&key.section), "{} isn't shown", key.label);
        }
    }

    #[test]
    fn the_overlay_writes_the_keys_the_user_gave() {
        let settings = [
            ("split-right".to_string(), keymap::Binding::One("V".into())),
            ("prefix".to_string(), keymap::Binding::One("ctrl+a".into())),
            (
                "hand-back".to_string(),
                keymap::Binding::One("ctrl+g".into()),
            ),
        ];
        let keymap = Keymap::new(&settings.into_iter().collect()).unwrap();
        let rows = rows(&Shown {
            plugin_on: &|_| true,
            plugin_keys: &[],
            keymap: &keymap,
        });
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert!(labels.contains(&"V/-"), "{labels:?}");
        assert!(labels.contains(&"Ctrl+G"), "{labels:?}");
        assert!(labels.contains(&"Ctrl+A, a key"), "{labels:?}");
        assert!(!labels.contains(&"|/-"), "{labels:?}");
    }

    #[test]
    fn the_overlay_lists_the_users_own_keys_where_they_work() {
        let toml = r#"
prefix = ["ctrl+b", "ctrl+a"]
pane-left = ["shift+left", "direct+ctrl+alt+h"]
view-down = "ctrl+j"
resize-even = "e"

[[command]]
key = "direct+ctrl+alt+g"
type = "popup"
command = "lazygit"
description = "git in a popup, over everything, until it ends"
"#;
        let settings: keymap::KeySettings = toml::from_str(toml).unwrap();
        let keymap = Keymap::new(&settings).unwrap();
        let rows = rows(&Shown {
            plugin_on: &|_| true,
            plugin_keys: &[],
            keymap: &keymap,
        });
        let row = |section: Section, label: &str| {
            rows.iter()
                .find(|row| row.section == section && row.label == label)
                .map(|row| row.does.as_str())
        };
        assert_eq!(
            row(Section::Commands, "Ctrl+Alt+G"),
            Some("git in a popup, over everyth…")
        );
        assert_eq!(
            row(Section::Pane, "Ctrl+Alt+H"),
            Some("the pane to the left")
        );
        assert!(row(Section::Pane, "Ctrl+B/Ctrl+A, a key").is_some());
        assert!(row(Section::View, "↓/↑ Ctrl+J/k").is_some(), "{rows:?}");
        assert!(row(Section::Resize, "e").is_some(), "{rows:?}");
        let defaults = all_rows();
        assert!(defaults.iter().all(|row| row.section != Section::Commands));
        assert!(defaults.iter().any(|row| row.label == "↓/↑ j/k"));
        assert!(defaults.iter().any(|row| row.label == "Enter/Esc q"));
    }

    #[test]
    fn a_key_label_lines_up_with_the_others() {
        let rows = all_rows();
        let columns = flow(&rows, 21);
        let lines = lines(&columns[0], &theme());
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
