//! The backlog view, `b` in the sidebar: the selected session's project's
//! backlog, what's still to do first and what's done after it. Letters are
//! the view's commands, so `/` starts a filter; `a` adds an item, Space
//! ticks one off or opens it again, `x` removes one once `y` says so, and
//! Enter goes on to start a task for it.
//!
//! The state here is plain data: the daemon's answer arrives through
//! [`BacklogView::set_backlog`], and what a key asks for comes back as a
//! [`Step`] for the app to carry out.

use super::search::letters_in;
use super::sidebar::fit;
use super::text_input::TextInput;
use super::theme::Theme;
use crate::protocol::{Backlog, BacklogItem};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};
use std::path::PathBuf;

/// A change to the backlog, for the daemon to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BacklogChange {
    Add(String),
    Mark { number: u64, done: bool },
    Remove(u64),
}

/// What a key in the view asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    Change(BacklogChange),
    /// Start a task for this item.
    Start(BacklogItem),
}

pub struct BacklogView {
    /// A directory in the project, which the daemon finds the project from.
    pub dir: PathBuf,
    pub project_name: String,
    /// What the daemon answered: `None` until it has.
    items: Option<Result<Vec<BacklogItem>, String>>,
    pub filter: TextInput,
    /// Whether keys go to the filter, after `/`.
    filtering: bool,
    /// The item the bar is on, by number. It stays on it while the list
    /// changes, as long as the item is still shown.
    highlighted: Option<u64>,
    /// While `a` is adding an item: what's been typed.
    pub adding: Option<TextInput>,
    /// The item `x` asks about removing, until the next key answers.
    removing: Option<u64>,
}

impl BacklogView {
    /// The view for the project `dir` is in, called `project_name`, waiting
    /// for its backlog.
    pub fn new(dir: PathBuf, project_name: String) -> BacklogView {
        BacklogView {
            dir,
            project_name,
            items: None,
            filter: TextInput::default(),
            filtering: false,
            highlighted: None,
            adding: None,
            removing: None,
        }
    }

    pub fn set_backlog(&mut self, found: Result<Backlog, String>) {
        self.items = Some(found.map(|backlog| backlog.items));
        self.keep_highlight_shown();
    }

    /// The items that match the filter, open ones first, as the daemon
    /// ordered them.
    pub fn shown(&self) -> Vec<&BacklogItem> {
        let Some(Ok(items)) = &self.items else {
            return Vec::new();
        };
        let query = self.filter.text();
        items.iter().filter(|item| matches(query, item)).collect()
    }

    pub fn highlighted(&self) -> Option<&BacklogItem> {
        let number = self.highlighted?;
        self.shown().into_iter().find(|item| item.number == number)
    }

    pub fn open_count(&self) -> usize {
        match &self.items {
            Some(Ok(items)) => items.iter().filter(|item| !item.done).count(),
            _ => 0,
        }
    }

    /// The question `x` asks, while it waits for its answer.
    pub fn removing(&self) -> Option<String> {
        self.removing.map(|number| format!("remove #{number}? y/n"))
    }

    /// Whether a key typed is a character, not a move: while an item or
    /// the filter is typed, or `x` asks.
    pub fn typing(&self) -> bool {
        self.adding.is_some() || self.filtering || self.removing.is_some()
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        if let Some(number) = self.removing.take() {
            return match key.code {
                KeyCode::Char('y') => Step::Change(BacklogChange::Remove(number)),
                _ => Step::Stay,
            };
        }
        if self.adding.is_some() {
            return self.on_adding_key(key);
        }
        if self.filtering {
            self.on_filter_key(key);
            return Step::Stay;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Step::Close,
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('a') => self.adding = Some(TextInput::default()),
            KeyCode::Char(' ') => {
                if let Some(item) = self.highlighted() {
                    let (number, done) = (item.number, !item.done);
                    return Step::Change(BacklogChange::Mark { number, done });
                }
            }
            KeyCode::Char('x') => self.removing = self.highlighted().map(|item| item.number),
            // A done item has nothing left to start.
            KeyCode::Enter => match self.highlighted() {
                Some(item) if !item.done => return Step::Start(item.clone()),
                _ => {}
            },
            _ => {}
        }
        Step::Stay
    }

    /// Pasted text goes where typing would: into the item being added, or
    /// into the filter.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(adding) = &mut self.adding {
            adding.insert_str(&text.replace(['\r', '\n'], " "));
        } else {
            self.filtering = true;
            self.filter.insert_str(text);
            self.keep_highlight_shown();
        }
    }

    /// Enter puts what's typed on the backlog; Esc gives up.
    fn on_adding_key(&mut self, key: &KeyEvent) -> Step {
        match key.code {
            KeyCode::Esc => self.adding = None,
            KeyCode::Enter => {
                let text = self
                    .adding
                    .take()
                    .map(|input| input.text().trim().to_string());
                if let Some(text) = text.filter(|text| !text.is_empty()) {
                    return Step::Change(BacklogChange::Add(text));
                }
            }
            _ => {
                if let Some(adding) = &mut self.adding {
                    adding.on_key(key);
                }
            }
        }
        Step::Stay
    }

    /// Enter keeps the filter and gives the keys back to the list; Esc
    /// clears it; ↑ and ↓ still move the bar.
    fn on_filter_key(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Enter => self.filtering = false,
            KeyCode::Esc => {
                self.filtering = false;
                self.filter = TextInput::default();
            }
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            _ => self.filter.on_key(key),
        }
        self.keep_highlight_shown();
    }

    fn move_by(&mut self, by: isize) {
        let shown = self.shown();
        let Some(at) = shown
            .iter()
            .position(|item| Some(item.number) == self.highlighted)
        else {
            return;
        };
        let to = at.saturating_add_signed(by).min(shown.len() - 1);
        self.highlighted = Some(shown[to].number);
    }

    /// Puts the bar on the first item shown when the one it was on isn't
    /// shown any more.
    fn keep_highlight_shown(&mut self) {
        let shown = self.shown();
        let still_shown = shown
            .iter()
            .any(|item| Some(item.number) == self.highlighted);
        if !still_shown {
            self.highlighted = shown.first().map(|item| item.number);
        }
    }
}

/// Whether every word of `query` turns up in the item: its number, text or
/// tags.
fn matches(query: &str, item: &BacklogItem) -> bool {
    let text = format!("#{} {} {}", item.number, item.text, item.tags.join(" "));
    query
        .split_whitespace()
        .all(|word| letters_in(word, &text).is_some())
}

/// Draws the view in `area`: a heading, the filter, and the list.
pub fn draw(frame: &mut Frame, view: &BacklogView, theme: &Theme, area: Rect) {
    // A style alone would leave the characters drawn there before, the
    // sidebar and the panes, showing through.
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.base()), area);
    let [heading, filter, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    draw_heading(frame, view, theme, heading);
    draw_filter(frame, view, theme, filter);

    let items = match &view.items {
        None => return draw_note(frame, theme, "reading the backlog…", list),
        Some(Err(reason)) => return draw_note(frame, theme, reason, list),
        Some(Ok(_)) => view.shown(),
    };
    if items.is_empty() {
        let note = if view.filter.text().is_empty() {
            "nothing on the backlog yet: a adds something"
        } else {
            "nothing on the backlog matches"
        };
        return draw_note(frame, theme, note, list);
    }
    draw_list(frame, view, &items, theme, list);
}

fn draw_heading(frame: &mut Frame, view: &BacklogView, theme: &Theme, area: Rect) {
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "backlog",
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {}", view.project_name),
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(line, area);
    if matches!(view.items, Some(Ok(_))) {
        let count = format!("{} to do ", view.open_count());
        let open = Line::styled(count, Style::new().fg(theme.muted));
        frame.render_widget(open.right_aligned(), area);
    }
}

/// The filter: with the cursor in it after `/`, or a hint before.
fn draw_filter(frame: &mut Frame, view: &BacklogView, theme: &Theme, area: Rect) {
    if !view.filtering && view.filter.text().is_empty() {
        let hint = Line::styled(" / filters", Style::new().fg(theme.muted));
        frame.render_widget(hint, area);
        return;
    }
    let label = " filter: ";
    let line = Line::from(vec![
        Span::styled(
            label,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(view.filter.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, area);
    if view.filtering {
        let column = area.x + (label.len() + view.filter.cursor()) as u16;
        frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
    }
}

/// One row an item: its number, a box ticked when it's done, its text and
/// its tags. The list scrolls to keep the bar in sight.
fn draw_list(
    frame: &mut Frame,
    view: &BacklogView,
    items: &[&BacklogItem],
    theme: &Theme,
    area: Rect,
) {
    let height = usize::from(area.height.max(1));
    let at = items
        .iter()
        .position(|item| Some(item.number) == view.highlighted)
        .unwrap_or(0);
    let first = (at + 1).saturating_sub(height);
    for (row, item) in items.iter().enumerate().skip(first).take(height) {
        let line_area = Rect::new(area.x, area.y + (row - first) as u16, area.width, 1);
        let highlighted = Some(item.number) == view.highlighted;
        if highlighted {
            frame.buffer_mut().set_style(line_area, theme.selection);
        }
        frame.render_widget(item_line(item, theme, highlighted, area.width), line_area);
    }
}

fn item_line<'a>(item: &BacklogItem, theme: &Theme, highlighted: bool, width: u16) -> Line<'a> {
    let (tick, tick_color, text_color) = if item.done {
        ("✓ ", theme.done, theme.muted)
    } else {
        ("· ", theme.muted, theme.text)
    };
    let mut text_style = Style::new().fg(text_color);
    if highlighted {
        text_style = text_style.add_modifier(Modifier::BOLD);
    }
    let number = format!(" #{:<4} ", item.number);
    let tags: String = item.tags.iter().map(|tag| format!("  #{tag}")).collect();
    let room = usize::from(width).saturating_sub(number.len() + 2 + tags.chars().count());
    let text = item.text.lines().next().unwrap_or("");
    Line::from(vec![
        Span::styled(number, Style::new().fg(theme.muted)),
        Span::styled(tick, Style::new().fg(tick_color)),
        Span::styled(fit(text, room), text_style),
        Span::styled(tags, Style::new().fg(theme.branch)),
    ])
}

fn draw_note(frame: &mut Frame, theme: &Theme, note: &str, area: Rect) {
    let line = Line::styled(format!(" {note}"), Style::new().fg(theme.muted));
    frame.render_widget(line, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Paragraph, Wrap};

    fn item(number: u64, text: &str, done: bool) -> BacklogItem {
        BacklogItem {
            number,
            text: text.into(),
            tags: vec!["docs".into()],
            done,
            created: 0,
            closed: None,
        }
    }

    fn view_of(items: Vec<BacklogItem>) -> BacklogView {
        let mut view = BacklogView::new(PathBuf::from("/code/shop"), "shop".into());
        view.set_backlog(Ok(Backlog {
            project: "shop".into(),
            path: PathBuf::from("/code/shop"),
            items,
        }));
        view
    }

    fn press(view: &mut BacklogView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(view: &mut BacklogView, text: &str) {
        for c in text.chars() {
            press(view, KeyCode::Char(c));
        }
    }

    fn numbers(view: &BacklogView) -> Vec<u64> {
        view.shown().iter().map(|item| item.number).collect()
    }

    /// Draws `view` over a screen full of `¤`, the way it opens over the
    /// sidebar and the panes, and returns what's on the screen.
    fn drawn_over_the_screen(view: &BacklogView) -> String {
        let theme = Theme::new(ThemeName::DARK, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let behind = "¤".repeat(usize::from(area.width * area.height));
                frame.render_widget(Paragraph::new(behind).wrap(Wrap { trim: false }), area);
                draw(frame, view, &theme, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn nothing_behind_the_backlog_shows_through_it() {
        let mut view = view_of(vec![item(1, "Write the guide", false)]);
        assert!(!drawn_over_the_screen(&view).contains('¤'));
        press(&mut view, KeyCode::Char('/'));
        type_text(&mut view, "nothing like it");
        assert!(!drawn_over_the_screen(&view).contains('¤'));
    }

    #[test]
    fn letters_are_commands_until_slash_starts_the_filter() {
        let mut view = view_of(vec![
            item(1, "write the docs", false),
            item(2, "fix the cart", false),
        ]);
        type_text(&mut view, "/cart");
        assert_eq!(numbers(&view), [2]);
        press(&mut view, KeyCode::Enter);
        assert!(!view.filtering, "Enter keeps the filter and leaves it");
        assert_eq!(numbers(&view), [2]);
        assert_eq!(press(&mut view, KeyCode::Char('q')), Step::Close);

        let mut view = view_of(vec![item(1, "a", false), item(2, "b", false)]);
        type_text(&mut view, "/b");
        press(&mut view, KeyCode::Esc);
        assert_eq!(numbers(&view), [1, 2], "Esc clears the filter");
    }

    #[test]
    fn space_ticks_an_item_off_or_opens_it_again() {
        let mut view = view_of(vec![item(1, "a", false), item(2, "b", true)]);
        let ticked = BacklogChange::Mark {
            number: 1,
            done: true,
        };
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Step::Change(ticked));
        press(&mut view, KeyCode::Down);
        let reopened = BacklogChange::Mark {
            number: 2,
            done: false,
        };
        assert_eq!(press(&mut view, KeyCode::Char(' ')), Step::Change(reopened));
    }

    #[test]
    fn a_adds_what_is_typed_and_x_removes_only_after_a_yes() {
        let mut view = view_of(vec![item(1, "a", false)]);
        type_text(&mut view, "aship it");
        let added = BacklogChange::Add("ship it".into());
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Change(added));
        assert!(view.adding.is_none());

        press(&mut view, KeyCode::Char('x'));
        assert_eq!(view.removing().as_deref(), Some("remove #1? y/n"));
        assert_eq!(press(&mut view, KeyCode::Char('n')), Step::Stay);
        assert_eq!(view.removing(), None);
        press(&mut view, KeyCode::Char('x'));
        let removed = BacklogChange::Remove(1);
        assert_eq!(press(&mut view, KeyCode::Char('y')), Step::Change(removed));
    }

    #[test]
    fn enter_starts_a_task_for_an_open_item_only() {
        let mut view = view_of(vec![
            item(1, "write the docs", false),
            item(2, "done one", true),
        ]);
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Step::Start(item(1, "write the docs", false))
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Stay);
    }

    #[test]
    fn the_bar_stays_on_its_item_when_the_list_comes_back() {
        let mut view = view_of(vec![item(1, "a", false), item(2, "b", false)]);
        press(&mut view, KeyCode::Down);
        view.set_backlog(Ok(Backlog {
            project: "shop".into(),
            path: PathBuf::from("/code/shop"),
            items: vec![item(3, "c", false), item(2, "b", false)],
        }));
        assert_eq!(view.highlighted().map(|item| item.number), Some(2));
        assert_eq!(view.open_count(), 2);
    }
}
