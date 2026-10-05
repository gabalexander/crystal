//! The backlog view, `b` in the sidebar: the selected session's project's
//! backlog, what's still to do first and what's done after it, and under
//! it the item the bar is on: its tags, its body and the tasks started for
//! it. Letters are the view's commands, so `/` starts a filter and `t`
//! keeps to a tag; `a` adds an item and `e` changes its line, Space ticks
//! one off or opens it again, `x` removes one once `y` says so, and Enter
//! goes on to start a task for it.
//!
//! The state here is plain data: the daemon's answer arrives through
//! [`BacklogView::set_backlog`], and what a key asks for comes back as a
//! [`Step`] for the app to carry out.

use super::search::letters_in;
use super::sidebar::{ago, fit};
use super::text_input::TextInput;
use super::theme::Theme;
use crate::protocol::{Backlog, BacklogItem, TaskView, task_label};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::path::PathBuf;

/// A change to the backlog, for the daemon to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BacklogChange {
    Add(String),
    /// A new line for an item.
    Edit {
        number: u64,
        text: String,
    },
    Mark {
        number: u64,
        done: bool,
    },
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

/// A line being typed in the footer: a new item's, or an item's new one.
pub struct Writing {
    /// The item whose line it is, or `None` for a new item.
    pub number: Option<u64>,
    pub input: TextInput,
}

impl Writing {
    /// What the footer says in front of it.
    pub fn label(&self) -> String {
        match self.number {
            Some(number) => format!(" #{number}'s line: "),
            None => " add to the backlog: ".to_string(),
        }
    }
}

pub struct BacklogView {
    /// A directory in the project, which the daemon finds the project from.
    pub dir: PathBuf,
    pub project_name: String,
    /// What the daemon answered: `None` until it has.
    items: Option<Result<Vec<BacklogItem>, String>>,
    /// The tasks started for an item, which the item the bar is on lists.
    tasks: Vec<TaskView>,
    pub filter: TextInput,
    /// Whether keys go to the filter, after `/`.
    filtering: bool,
    /// The tag `t` keeps the list to, if any.
    tag: Option<String>,
    /// The item the bar is on, by number. It stays on it while the list
    /// changes, as long as the item is still shown.
    highlighted: Option<u64>,
    /// While `a` adds an item, or `e` changes one's line: what's been typed.
    pub writing: Option<Writing>,
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
            tasks: Vec::new(),
            filter: TextInput::default(),
            filtering: false,
            tag: None,
            highlighted: None,
            writing: None,
            removing: None,
        }
    }

    pub fn set_backlog(&mut self, found: Result<Backlog, String>) {
        match found {
            Ok(backlog) => {
                self.items = Some(Ok(backlog.items));
                self.tasks = backlog.tasks;
            }
            Err(why) => self.items = Some(Err(why)),
        }
        // A tag no item carries any more keeps nothing.
        if self
            .tag
            .as_ref()
            .is_some_and(|tag| !self.tags().contains(tag))
        {
            self.tag = None;
        }
        self.keep_highlight_shown();
    }

    /// The items that match the filter and carry the tag `t` keeps to,
    /// open ones first, as the daemon ordered them.
    pub fn shown(&self) -> Vec<&BacklogItem> {
        let Some(Ok(items)) = &self.items else {
            return Vec::new();
        };
        let query = self.filter.text();
        items
            .iter()
            .filter(|item| matches(query, item))
            .filter(|item| self.tag.as_ref().is_none_or(|tag| item.tags.contains(tag)))
            .collect()
    }

    /// Puts the bar on item `number`, once it's listed.
    pub fn highlight(&mut self, number: u64) {
        self.highlighted = Some(number);
    }

    pub fn highlighted(&self) -> Option<&BacklogItem> {
        let number = self.highlighted?;
        self.shown().into_iter().find(|item| item.number == number)
    }

    /// The tasks started for item `number`, the oldest first.
    pub fn tasks_for(&self, number: u64) -> Vec<&TaskView> {
        let mut tasks: Vec<&TaskView> = self
            .tasks
            .iter()
            .filter(|task| task.record.backlog == Some(number))
            .collect();
        tasks.sort_by_key(|task| (task.record.created, task.record.id));
        tasks
    }

    /// Every tag the project's items carry, each once, in order.
    fn tags(&self) -> Vec<String> {
        let Some(Ok(items)) = &self.items else {
            return Vec::new();
        };
        let mut tags: Vec<String> = items.iter().flat_map(|item| item.tags.clone()).collect();
        tags.sort();
        tags.dedup();
        tags
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
        self.writing.is_some() || self.filtering || self.removing.is_some()
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        if let Some(number) = self.removing.take() {
            return match key.code {
                KeyCode::Char('y') => Step::Change(BacklogChange::Remove(number)),
                _ => Step::Stay,
            };
        }
        if self.writing.is_some() {
            return self.on_writing_key(key);
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
            KeyCode::Char('t') => self.next_tag(),
            KeyCode::Char('a') => {
                self.writing = Some(Writing {
                    number: None,
                    input: TextInput::default(),
                })
            }
            KeyCode::Char('e') => {
                if let Some(item) = self.highlighted() {
                    let mut input = TextInput::default();
                    input.insert_str(&item.text);
                    self.writing = Some(Writing {
                        number: Some(item.number),
                        input,
                    });
                }
            }
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

    /// Pasted text goes where typing would: into the line being written,
    /// or into the filter.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(writing) = &mut self.writing {
            writing.input.insert_str(&text.replace(['\r', '\n'], " "));
        } else {
            self.filtering = true;
            self.filter.insert_str(text);
            self.keep_highlight_shown();
        }
    }

    /// Enter puts what's typed on the backlog, or makes it the item's new
    /// line; Esc gives up.
    fn on_writing_key(&mut self, key: &KeyEvent) -> Step {
        match key.code {
            KeyCode::Esc => self.writing = None,
            KeyCode::Enter => {
                let Some(writing) = self.writing.take() else {
                    return Step::Stay;
                };
                let text = writing.input.text().trim().to_string();
                if text.is_empty() {
                    return Step::Stay;
                }
                return Step::Change(match writing.number {
                    Some(number) => BacklogChange::Edit { number, text },
                    None => BacklogChange::Add(text),
                });
            }
            _ => {
                if let Some(writing) = &mut self.writing {
                    writing.input.on_key(key);
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

    /// Keeps the list to the next tag the items carry, after the last none.
    fn next_tag(&mut self) {
        let tags = self.tags();
        self.tag = match &self.tag {
            None => tags.first().cloned(),
            Some(tag) => {
                let at = tags.iter().position(|each| each == tag);
                at.and_then(|at| tags.get(at + 1)).cloned()
            }
        };
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

/// Whether every word of `query` turns up in the item: its number, line,
/// body or tags.
fn matches(query: &str, item: &BacklogItem) -> bool {
    let text = format!(
        "#{} {} {} {}",
        item.number,
        item.text,
        item.body,
        item.tags.join(" ")
    );
    query
        .split_whitespace()
        .all(|word| letters_in(word, &text).is_some())
}

/// Draws the view in `area`: a heading, the filter, the list, and the item
/// the bar is on under it.
pub fn draw(frame: &mut Frame, view: &BacklogView, theme: &Theme, now: u64, area: Rect) {
    // A style alone would leave the characters drawn there before, the
    // sidebar and the panes, showing through.
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.base()), area);
    let [heading, filter, rest] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    draw_heading(frame, view, theme, heading);
    draw_filter(frame, view, theme, filter);

    let items = match &view.items {
        None => return draw_note(frame, theme, "reading the backlog…", rest),
        Some(Err(reason)) => return draw_note(frame, theme, reason, rest),
        Some(Ok(_)) => view.shown(),
    };
    if items.is_empty() {
        let note = if view.filter.text().is_empty() && view.tag.is_none() {
            "nothing on the backlog yet: a adds something"
        } else {
            "nothing on the backlog matches"
        };
        return draw_note(frame, theme, note, rest);
    }
    let about = view
        .highlighted()
        .map(|item| about_lines(item, &view.tasks_for(item.number), theme, now))
        .unwrap_or_default();
    // The item takes at most half the room, the list the rest.
    let wanted = if about.is_empty() { 0 } else { about.len() + 1 };
    let below = (wanted as u16).min(rest.height / 2);
    let [list, rule, below_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(below > 0)),
        Constraint::Length(below.saturating_sub(1)),
    ])
    .areas(rest);
    draw_list(frame, view, &items, theme, list);
    if below > 0 {
        let line = Line::styled(
            "─".repeat(usize::from(rule.width)),
            Style::new().fg(theme.rule),
        );
        frame.render_widget(line, rule);
        let about = Paragraph::new(about).wrap(Wrap { trim: false });
        frame.render_widget(about, below_area);
    }
}

/// What's shown of the item the bar is on under the list: when it was
/// added and done and its tags, the tasks started for it, then its body.
/// Nothing for an item that's only its line.
fn about_lines<'a>(
    item: &BacklogItem,
    tasks: &[&TaskView],
    theme: &Theme,
    now: u64,
) -> Vec<Line<'a>> {
    if item.body.is_empty() && tasks.is_empty() {
        return Vec::new();
    }
    let muted = Style::new().fg(theme.muted);
    let when = |then| match ago(then, now).as_str() {
        "now" => "just now".to_string(),
        ago => format!("{ago} ago"),
    };
    let mut said = vec![Span::styled(format!(" #{}", item.number), muted)];
    said.push(Span::styled(
        format!(" · added {}", when(item.created)),
        muted,
    ));
    if let Some(closed) = item.closed {
        said.push(Span::styled(format!(" · done {}", when(closed)), muted));
    }
    for tag in &item.tags {
        said.push(Span::styled(
            format!("  #{tag}"),
            Style::new().fg(theme.branch),
        ));
    }
    let mut lines = vec![Line::from(said)];
    for task in tasks {
        let record = &task.record;
        let how = match &record.outcome {
            Some(outcome) if !outcome.summary.is_empty() => format!(": {}", outcome.summary),
            Some(_) => String::new(),
            None if record.session.is_empty() => String::new(),
            None => format!(", in {}", record.session),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", task_label(record.id)), muted),
            Span::styled(task.state.word().to_string(), Style::new().fg(theme.text)),
            Span::styled(how, muted),
        ]));
    }
    if !item.body.is_empty() {
        lines.push(Line::raw(""));
        for line in item.body.lines() {
            lines.push(Line::styled(
                format!(" {line}"),
                Style::new().fg(theme.text),
            ));
        }
    }
    lines
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

/// The filter: with the cursor in it after `/`, or a hint before; and the
/// tag `t` keeps to.
fn draw_filter(frame: &mut Frame, view: &BacklogView, theme: &Theme, area: Rect) {
    let tag = view
        .tag
        .as_ref()
        .map(|tag| Span::styled(format!(" tag: #{tag} "), Style::new().fg(theme.branch)));
    if !view.filtering && view.filter.text().is_empty() {
        let mut spans = Vec::from_iter(tag);
        spans.push(Span::styled(" / filters", Style::new().fg(theme.muted)));
        frame.render_widget(Line::from(spans), area);
        return;
    }
    let label = " filter: ";
    let mut spans = vec![
        Span::styled(
            label,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(view.filter.text().to_string(), Style::new().fg(theme.text)),
    ];
    spans.extend(tag.map(|tag| Span::styled(format!("  {}", tag.content), tag.style)));
    frame.render_widget(Line::from(spans), area);
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

/// An item's row: its number, its tick, its line, a `+` when it has a
/// body, and its tags.
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
    let more = if item.body.is_empty() { "" } else { " +" };
    let tags: String = item.tags.iter().map(|tag| format!("  #{tag}")).collect();
    let room =
        usize::from(width).saturating_sub(number.len() + 2 + more.len() + tags.chars().count());
    let text = item.text.lines().next().unwrap_or("");
    Line::from(vec![
        Span::styled(number, Style::new().fg(theme.muted)),
        Span::styled(tick, Style::new().fg(tick_color)),
        Span::styled(fit(text, room), text_style),
        Span::styled(more, Style::new().fg(theme.muted)),
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
            body: String::new(),
            tags: vec!["docs".into()],
            done,
            created: 0,
            closed: None,
        }
    }

    fn backlog(items: Vec<BacklogItem>) -> Backlog {
        Backlog {
            project: "shop".into(),
            path: PathBuf::from("/code/shop"),
            items,
            tasks: Vec::new(),
        }
    }

    fn view_of(items: Vec<BacklogItem>) -> BacklogView {
        let mut view = BacklogView::new(PathBuf::from("/code/shop"), "shop".into());
        view.set_backlog(Ok(backlog(items)));
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
    /// sidebar and the panes, and returns what's on the screen, a row a
    /// line.
    fn drawn_over_the_screen(view: &BacklogView) -> String {
        let theme = Theme::new(ThemeName::DARK, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let behind = "¤".repeat(usize::from(area.width * area.height));
                frame.render_widget(Paragraph::new(behind).wrap(Wrap { trim: false }), area);
                draw(frame, view, &theme, 3600, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let cells: Vec<&str> = buffer.content().iter().map(|cell| cell.symbol()).collect();
        cells
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.concat().trim_end().to_string() + "\n")
            .collect()
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
        assert!(view.writing.is_none());

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
        view.set_backlog(Ok(backlog(vec![item(3, "c", false), item(2, "b", false)])));
        assert_eq!(view.highlighted().map(|item| item.number), Some(2));
        assert_eq!(view.open_count(), 2);
    }

    #[test]
    fn e_changes_the_line_of_the_item_the_bar_is_on() {
        let mut view = view_of(vec![item(1, "write the docs", false)]);
        press(&mut view, KeyCode::Char('e'));
        let writing = view.writing.as_ref().unwrap();
        assert_eq!(writing.label(), " #1's line: ");
        assert_eq!(writing.input.text(), "write the docs");
        type_text(&mut view, " too");
        let edited = BacklogChange::Edit {
            number: 1,
            text: "write the docs too".into(),
        };
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Change(edited));
        assert!(view.writing.is_none());

        // Esc gives up, and an empty line changes nothing.
        press(&mut view, KeyCode::Char('e'));
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Stay);
        assert!(view.writing.is_none());
    }

    #[test]
    fn t_keeps_to_each_tag_in_turn_then_to_none() {
        let tagged = |number, tags: &[&str]| BacklogItem {
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..item(number, "x", false)
        };
        let mut view = view_of(vec![
            tagged(1, &["ui"]),
            tagged(2, &["ci", "ui"]),
            tagged(3, &[]),
        ]);
        press(&mut view, KeyCode::Char('t'));
        assert_eq!(view.tag.as_deref(), Some("ci"));
        assert_eq!(numbers(&view), [2]);
        press(&mut view, KeyCode::Char('t'));
        assert_eq!(numbers(&view), [1, 2]);
        press(&mut view, KeyCode::Char('t'));
        assert_eq!(view.tag, None);
        assert_eq!(numbers(&view), [1, 2, 3]);

        // A tag no item carries any more keeps to none.
        press(&mut view, KeyCode::Char('t'));
        view.set_backlog(Ok(backlog(vec![tagged(1, &["ui"])])));
        assert_eq!(view.tag, None);
    }

    #[test]
    fn the_item_the_bar_is_on_shows_its_tasks_and_body_under_the_list() {
        use crate::protocol::{TaskOutcome, TaskRecord, TaskState};
        let mut first = item(1, "write the docs", false);
        first.body = "The guide, then the reference.".into();
        let mut started = TaskView::of_record(TaskRecord {
            id: Some(7),
            goal: "write the docs".into(),
            session: "docs".into(),
            project: "shop".into(),
            branch: None,
            background: false,
            backlog: Some(1),
            pending: false,
            waiting: false,
            created: 100,
            outcome: Some(TaskOutcome::new(TaskState::Failed, "no network", 200)),
            artifacts: Vec::new(),
            brief: Default::default(),
        });
        started.state = TaskState::Failed;
        let mut view = BacklogView::new(PathBuf::from("/code/shop"), "shop".into());
        view.set_backlog(Ok(Backlog {
            tasks: vec![started],
            ..backlog(vec![first, item(2, "fix the cart", false)])
        }));
        let screen = drawn_over_the_screen(&view);
        assert!(
            screen.contains(" #1    · write the docs +  #docs\n"),
            "{screen}"
        );
        assert!(screen.contains(" #1 · added 1h ago  #docs\n"), "{screen}");
        assert!(screen.contains(" t7 failed: no network\n"), "{screen}");
        assert!(
            screen.contains(" The guide, then the reference.\n"),
            "{screen}"
        );

        // An item that's only its line shows nothing more.
        press(&mut view, KeyCode::Down);
        assert!(!drawn_over_the_screen(&view).contains("added"));
        // The filter finds an item by its body.
        type_text(&mut view, "/reference");
        assert_eq!(numbers(&view), [1]);
    }
}
