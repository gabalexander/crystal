//! The command list, `:` in the sidebar: every command by its name, the
//! installed plugins' actions with them, filtered as you type, Enter
//! running the one the bar is on. It's how to reach a command that has no
//! key, or whose key the user doesn't remember; each row says its key, so
//! next time it needn't be looked up.
//!
//! Before anything is typed, the commands run from it most lately come
//! first. The state is kept apart from I/O: the app runs what it picks.

use super::keymap::{COMMANDS, Command, Keymap};
use super::search::letters_in;
use super::text_input::TextInput;
use super::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};

/// How many commands lately run lead the list before anything's typed.
pub const RECENTS: usize = 5;

/// What a row runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Command(Command),
    /// The action called `action` of the installed plugin `plugin`.
    Plugin {
        plugin: String,
        action: String,
    },
}

/// One row: what it runs, its name, what it does and its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub pick: Pick,
    pub name: String,
    pub does: String,
    /// Its keys, as the overlay writes them; empty when it has none.
    pub keys: String,
}

/// A plugin action the list offers: the plugin, the action's id and title,
/// and the key it took, if it took one.
pub struct PluginAction<'a> {
    pub plugin: &'a str,
    pub action: &'a str,
    pub title: &'a str,
    pub key: Option<char>,
}

/// The rows the list offers: crystal's commands, but those of its plugins
/// that are off and the list itself, then the plugins' actions.
pub fn rows(
    keymap: &Keymap,
    plugin_on: &dyn Fn(&str) -> bool,
    actions: &[PluginAction],
) -> Vec<Row> {
    let commands = COMMANDS
        .iter()
        .filter(|spec| spec.command != Command::Commands)
        .filter(|spec| spec.plugin.is_none_or(plugin_on))
        .map(|spec| Row {
            pick: Pick::Command(spec.command),
            name: spec.id.to_string(),
            does: spec.does.to_string(),
            keys: keymap.label(spec.command),
        });
    let plugins = actions.iter().map(|action| Row {
        pick: Pick::Plugin {
            plugin: action.plugin.to_string(),
            action: action.action.to_string(),
        },
        name: format!("{}:{}", action.plugin, action.action),
        does: action.title.to_string(),
        keys: action.key.map(String::from).unwrap_or_default(),
    });
    commands.chain(plugins).collect()
}

/// What a key did to the list.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Still open.
    Stay,
    Close,
    /// Close it, and run this.
    Run(Pick),
}

/// The list while it's open.
pub struct CommandList {
    rows: Vec<Row>,
    pub input: TextInput,
    /// The rows shown, by index into `rows`, best first, with the letters
    /// of each one's name the query marked.
    shown: Vec<(usize, Vec<usize>)>,
    /// Where the bar is in `shown`.
    pub at: usize,
    /// What was run from it lately, the latest first.
    recents: Vec<Pick>,
}

impl CommandList {
    pub fn new(rows: Vec<Row>, recents: &[Pick]) -> CommandList {
        let mut list = CommandList {
            rows,
            input: TextInput::default(),
            shown: Vec::new(),
            at: 0,
            recents: recents.to_vec(),
        };
        list.filter();
        list
    }

    /// The rows shown, best first, each with its name's marked letters.
    pub fn shown(&self) -> impl Iterator<Item = (&Row, &[usize])> {
        self.shown
            .iter()
            .map(|(index, marked)| (&self.rows[*index], marked.as_slice()))
    }

    /// Whether the row at `index` in [`CommandList::shown`] is one lately
    /// run, leading the list before anything's typed.
    pub fn is_recent(&self, index: usize) -> bool {
        self.input.text().is_empty()
            && self
                .shown
                .get(index)
                .is_some_and(|(row, _)| self.recents.contains(&self.rows[*row].pick))
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => {
                return match self.shown.get(self.at) {
                    Some((row, _)) => Outcome::Run(self.rows[*row].pick.clone()),
                    None => Outcome::Stay,
                };
            }
            KeyCode::Up => self.move_bar(-1),
            KeyCode::Down => self.move_bar(1),
            KeyCode::Char('p') if ctrl => self.move_bar(-1),
            KeyCode::Char('n') if ctrl => self.move_bar(1),
            KeyCode::PageUp => self.move_bar(-10),
            KeyCode::PageDown => self.move_bar(10),
            _ => {
                self.input.on_key(key);
                self.filter();
            }
        }
        Outcome::Stay
    }

    pub fn on_paste(&mut self, text: &str) {
        self.input.insert_str(text);
        self.filter();
    }

    fn move_bar(&mut self, by: isize) {
        let last = self.shown.len().saturating_sub(1);
        self.at = self.at.saturating_add_signed(by).min(last);
    }

    /// Shows the rows that match what's typed, the bar on the first: those
    /// whose name has the query's letters side by side first, then in
    /// order, then those that match only by what they do or their key.
    /// Before anything's typed, every row, the lately run first.
    fn filter(&mut self) {
        let query = self.input.text().trim().to_string();
        self.at = 0;
        if query.is_empty() {
            let recent = |pick: &Pick| self.recents.iter().position(|recent| recent == pick);
            let mut order: Vec<usize> = (0..self.rows.len()).collect();
            order.sort_by_key(|&index| recent(&self.rows[index].pick).unwrap_or(usize::MAX));
            self.shown = order.into_iter().map(|index| (index, Vec::new())).collect();
            return;
        }
        let mut scored: Vec<(u8, usize, Vec<usize>)> = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            if let Some((score, marked)) = score(&query, row) {
                scored.push((score, index, marked));
            }
        }
        scored.sort_by_key(|(score, index, _)| (*score, *index));
        self.shown = scored
            .into_iter()
            .map(|(_, index, marked)| (index, marked))
            .collect();
    }
}

/// How well `row` matches `query`, lower first, and the letters of its
/// name to mark; `None` when some word of the query is nowhere in it.
fn score(query: &str, row: &Row) -> Option<(u8, Vec<usize>)> {
    let mut worst = 0;
    let mut marked = Vec::new();
    for word in query.split_whitespace() {
        let (rank, found) = match letters_in(word, &row.name) {
            Some(found) => {
                let side_by_side = found.windows(2).all(|pair| pair[1] == pair[0] + 1);
                (u8::from(!side_by_side), found)
            }
            None => {
                let elsewhere = row.does.to_lowercase().contains(&word.to_lowercase())
                    || row.keys.split(' ').any(|key| key == word);
                if !elsewhere {
                    return None;
                }
                (2, Vec::new())
            }
        };
        worst = worst.max(rank);
        marked.extend(found);
    }
    marked.sort_unstable();
    marked.dedup();
    Some((worst, marked))
}

/// Puts `pick` at the front of `recents`, keeping [`RECENTS`] of them.
pub fn remember(recents: &mut Vec<Pick>, pick: Pick) {
    recents.retain(|recent| *recent != pick);
    recents.insert(0, pick);
    recents.truncate(RECENTS);
}

/// The widest the list gets, and how much of the screen's height it takes
/// at most.
const WIDTH: u16 = 76;

/// Draws the list over the middle of `area`: the query on top, then a row
/// a command, its name, its key and what it does.
pub fn draw(frame: &mut Frame, list: &CommandList, theme: &Theme, area: Rect) {
    let width = WIDTH.min(area.width);
    let height = (area.height * 2 / 3)
        .max(area.height.min(8))
        .min(area.height);
    let panel = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 4,
        width,
        height,
    );
    frame.render_widget(Clear, panel);
    let block = Block::bordered()
        .border_style(Style::new().fg(theme.accent))
        .style(Style::new().bg(theme.panel).fg(theme.text))
        .title(Line::styled(
            " commands ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(
            " enter runs · ↑/↓ choose · esc closes ",
            Style::new().fg(theme.muted),
        ));
    let inside = block.inner(panel);
    frame.render_widget(block, panel);
    if inside.height == 0 {
        return;
    }
    let prompt = Line::from(vec![
        Span::styled(
            " : ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(list.input.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(prompt, Rect::new(inside.x, inside.y, inside.width, 1));
    let column = inside.x + 3 + list.input.cursor() as u16;
    frame.set_cursor_position((column.min(inside.right().saturating_sub(1)), inside.y));

    let rows_area = Rect::new(
        inside.x,
        inside.y + 1,
        inside.width,
        inside.height.saturating_sub(1),
    );
    let height = usize::from(rows_area.height);
    if height == 0 {
        return;
    }
    if list.shown.is_empty() {
        let none = Line::styled(" no command by that name", Style::new().fg(theme.muted));
        frame.render_widget(
            none,
            Rect::new(rows_area.x, rows_area.y, rows_area.width, 1),
        );
        return;
    }
    let first = (list.at + 1).saturating_sub(height);
    let name_width = list
        .shown()
        .map(|(row, _)| row.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(28);
    let key_width = list
        .shown()
        .map(|(row, _)| row.keys.chars().count())
        .max()
        .unwrap_or(0)
        .min(12);
    for (offset, (row, marked)) in list.shown().enumerate().skip(first).take(height) {
        let y = rows_area.y + (offset - first) as u16;
        let line_area = Rect::new(rows_area.x, y, rows_area.width, 1);
        let highlighted = offset == list.at;
        if highlighted {
            frame.buffer_mut().set_style(line_area, theme.selection);
        }
        let line = row_line(
            row,
            marked,
            list.is_recent(offset),
            (name_width, key_width),
            highlighted,
            theme,
        );
        frame.render_widget(line, line_area);
    }
}

/// A row: a mark for one lately run, its name with the query's letters
/// marked, its key, and what it does.
fn row_line<'a>(
    row: &Row,
    marked: &[usize],
    recent: bool,
    (name_width, key_width): (usize, usize),
    highlighted: bool,
    theme: &Theme,
) -> Line<'a> {
    let mut name_style = Style::new().fg(theme.text);
    if highlighted {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    let marked_style = name_style
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut spans = vec![Span::styled(
        if recent { " ↺ " } else { "   " },
        Style::new().fg(theme.muted),
    )];
    let name = super::sidebar::fit(&row.name, name_width);
    for (at, c) in name.chars().enumerate() {
        let style = if marked.contains(&at) {
            marked_style
        } else {
            name_style
        };
        spans.push(Span::styled(c.to_string(), style));
    }
    let pad = name_width.saturating_sub(name.chars().count()) + 2;
    spans.push(Span::raw(" ".repeat(pad)));
    let keys = super::sidebar::fit(&row.keys, key_width);
    let key_pad = key_width.saturating_sub(keys.chars().count()) + 2;
    spans.push(Span::styled(keys, Style::new().fg(theme.accent)));
    spans.push(Span::raw(" ".repeat(key_pad)));
    spans.push(Span::styled(row.does.clone(), Style::new().fg(theme.muted)));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(recents: &[Pick]) -> CommandList {
        let actions = [PluginAction {
            plugin: "notes",
            action: "add",
            title: "add a note",
            key: Some('N'),
        }];
        let rows = rows(&Keymap::default(), &|_| true, &actions);
        CommandList::new(rows, recents)
    }

    fn type_text(list: &mut CommandList, text: &str) {
        for c in text.chars() {
            list.on_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn names(list: &CommandList) -> Vec<&str> {
        list.shown().map(|(row, _)| row.name.as_str()).collect()
    }

    fn enter(list: &mut CommandList) -> Outcome {
        list.on_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    }

    #[test]
    fn every_command_is_listed_but_the_list_itself() {
        let list = list(&[]);
        let names = names(&list);
        assert!(names.contains(&"new-session"));
        assert!(names.contains(&"notes:add"));
        assert!(!names.contains(&"commands"));
    }

    #[test]
    fn typing_narrows_it_names_first_and_enter_runs_the_first() {
        let mut list = list(&[]);
        type_text(&mut list, "zoom");
        assert_eq!(names(&list)[0], "zoom");
        assert_eq!(enter(&mut list), Outcome::Run(Pick::Command(Command::Zoom)));
    }

    #[test]
    fn what_a_command_does_and_its_key_find_it_too() {
        let mut list = list(&[]);
        type_text(&mut list, "editor");
        assert_eq!(names(&list), ["edit-history"]);
        let mut by_key = self::list(&[]);
        type_text(&mut by_key, "N");
        assert!(names(&by_key).contains(&"notes:add"));
    }

    #[test]
    fn the_lately_run_lead_until_something_is_typed() {
        let recents = [Pick::Command(Command::Diff), Pick::Command(Command::Quit)];
        let mut list = list(&recents);
        assert_eq!(&names(&list)[..2], ["diff", "quit"]);
        assert!(list.is_recent(0) && !list.is_recent(2));
        type_text(&mut list, "q");
        assert!(!list.is_recent(0));
    }

    #[test]
    fn the_bar_moves_within_the_rows_and_esc_closes() {
        let mut list = list(&[]);
        list.on_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(list.at, 0);
        list.on_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(list.at, 1);
        let esc = list.on_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(esc, Outcome::Close);
        type_text(&mut list, "nothing-is-called-this");
        assert_eq!(enter(&mut list), Outcome::Stay);
    }

    #[test]
    fn remembering_keeps_the_latest_first_and_a_few() {
        let mut recents = Vec::new();
        for command in [Command::Diff, Command::Zoom, Command::Diff] {
            remember(&mut recents, Pick::Command(command));
        }
        assert_eq!(
            recents,
            [Pick::Command(Command::Diff), Pick::Command(Command::Zoom)]
        );
        for tab in 1..=9 {
            remember(&mut recents, Pick::Command(Command::Tab(tab)));
        }
        assert_eq!(recents.len(), RECENTS);
    }
}
