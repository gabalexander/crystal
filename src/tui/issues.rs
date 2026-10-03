//! The issues view, `i` in the sidebar: the open issues of the selected
//! session's project on GitHub, the latest to change first, filtered as you
//! type, with the highlighted issue's text under the list. Enter on one
//! goes on to start a session for it.
//!
//! The state here is plain data: what gh answered arrives through
//! [`IssuesView::set_issues`] and [`IssuesView::set_body`], and the event
//! loop asks [`IssuesView::body_to_fetch`] what to fetch next.

use super::search::letters_in;
use super::sidebar::{ago, fit};
use super::text_input::TextInput;
use super::theme::Theme;
use crate::github::Issue;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// The most of the view's height the list takes, as a share: the rest is
/// the highlighted issue's text.
const LIST_SHARE: u16 = 2;

pub struct IssuesView {
    /// The project's main worktree, where gh is asked.
    pub project: PathBuf,
    pub project_name: String,
    /// What gh answered: `None` while it's still being asked.
    issues: Option<Result<Vec<Issue>, String>>,
    pub filter: TextInput,
    /// The issue the bar is on, by number. It stays on it while the filter
    /// changes, as long as the issue is still shown.
    highlighted: Option<u64>,
    /// Issues' text, by number, as gh gave it.
    bodies: HashMap<u64, Result<String, String>>,
    /// The issues whose text has been asked for, answered or not.
    asked: HashSet<u64>,
}

impl IssuesView {
    /// The view for the project at `project`, waiting for its issues.
    pub fn new(project: PathBuf, project_name: String) -> IssuesView {
        IssuesView {
            project,
            project_name,
            issues: None,
            filter: TextInput::default(),
            highlighted: None,
            bodies: HashMap::new(),
            asked: HashSet::new(),
        }
    }

    pub fn set_issues(&mut self, found: Result<Vec<Issue>, String>) {
        self.issues = Some(found);
        self.keep_highlight_shown();
    }

    pub fn set_body(&mut self, number: u64, body: Result<String, String>) {
        self.bodies.insert(number, body);
    }

    /// The issues that match the filter, in the order they're listed.
    pub fn shown(&self) -> Vec<&Issue> {
        let Some(Ok(issues)) = &self.issues else {
            return Vec::new();
        };
        let query = self.filter.text();
        issues
            .iter()
            .filter(|issue| matches(query, issue))
            .collect()
    }

    pub fn highlighted(&self) -> Option<&Issue> {
        let number = self.highlighted?;
        self.shown()
            .into_iter()
            .find(|issue| issue.number == number)
    }

    /// The highlighted issue's number, when its text hasn't been asked for
    /// yet; it counts as asked from then on.
    pub fn body_to_fetch(&mut self) -> Option<u64> {
        let number = self.highlighted()?.number;
        if self.asked.insert(number) {
            Some(number)
        } else {
            None
        }
    }

    /// Keys while the view is open, but for Enter and Esc, which the app
    /// handles: ↑ and ↓ (or Ctrl+P and Ctrl+N) move the bar, and every
    /// other key edits the filter.
    pub fn on_key(&mut self, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            _ => {
                self.filter.on_key(key);
                self.keep_highlight_shown();
            }
        }
    }

    /// Pasted text goes into the filter, as if typed.
    pub fn on_paste(&mut self, text: &str) {
        self.filter.insert_str(text);
        self.keep_highlight_shown();
    }

    fn move_by(&mut self, by: isize) {
        let shown = self.shown();
        let Some(at) = shown
            .iter()
            .position(|issue| Some(issue.number) == self.highlighted)
        else {
            return;
        };
        let to = at.saturating_add_signed(by).min(shown.len() - 1);
        self.highlighted = Some(shown[to].number);
    }

    /// Puts the bar on the first issue shown when the one it was on isn't
    /// shown any more.
    fn keep_highlight_shown(&mut self) {
        let shown = self.shown();
        let still_shown = shown
            .iter()
            .any(|issue| Some(issue.number) == self.highlighted);
        if !still_shown {
            self.highlighted = shown.first().map(|issue| issue.number);
        }
    }
}

/// Whether every word of `query` turns up in the issue: its number, title,
/// labels or author.
fn matches(query: &str, issue: &Issue) -> bool {
    let labels: Vec<&str> = issue
        .labels
        .iter()
        .map(|label| label.name.as_str())
        .collect();
    let text = format!(
        "#{} {} {} {}",
        issue.number,
        issue.title,
        labels.join(" "),
        issue.author.login
    );
    query
        .split_whitespace()
        .all(|word| letters_in(word, &text).is_some())
}

/// Seconds since the Unix epoch for a time GitHub writes like
/// `2026-10-02T09:30:00Z`, or `None` for anything else.
pub fn unix_seconds(time: &str) -> Option<u64> {
    let (date, clock) = time.trim_end_matches('Z').split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut clock = clock
        .split(':')
        .map(|part| part.split('.').next().unwrap_or(part).parse::<i64>());
    let (hour, minute, second) = (
        clock.next()?.ok()?,
        clock.next()?.ok()?,
        clock.next()?.ok()?,
    );
    let seconds = days_since_epoch(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    u64::try_from(seconds).ok()
}

/// Days from 1970-01-01 to the given day of the Gregorian calendar, by
/// Howard Hinnant's well-known reckoning: count from a year that starts in
/// March, so that February's leap day falls at the end of it.
fn days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Draws the view in `area`: a heading, the filter, the list, and the
/// highlighted issue's text. `now` is seconds since the Unix epoch.
pub fn draw(frame: &mut Frame, view: &IssuesView, theme: &Theme, now: u64, area: Rect) {
    frame.render_widget(Block::new().style(theme.base()), area);
    let [heading, filter, rest] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    draw_heading(frame, view, theme, heading);
    draw_filter(frame, view, theme, filter);

    let issues = match &view.issues {
        None => return draw_note(frame, theme, "asking GitHub…", rest),
        Some(Err(reason)) => return draw_note(frame, theme, reason, rest),
        Some(Ok(_)) => view.shown(),
    };
    if issues.is_empty() {
        return draw_note(frame, theme, "no open issues match", rest);
    }
    let list_height = (issues.len() as u16)
        .min(rest.height * LIST_SHARE / 3)
        .max(1);
    let [list, rule, body] = Layout::vertical([
        Constraint::Length(list_height),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(rest);
    draw_list(frame, view, &issues, theme, now, list);
    frame.render_widget(
        Line::styled(
            "─".repeat(usize::from(rule.width)),
            Style::new().fg(theme.rule),
        ),
        rule,
    );
    draw_body(frame, view, theme, body);
}

fn draw_heading(frame: &mut Frame, view: &IssuesView, theme: &Theme, area: Rect) {
    let count = view.shown().len();
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "issues",
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {}", view.project_name),
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(line, area);
    if matches!(view.issues, Some(Ok(_))) {
        let open = Line::styled(format!("{count} open "), Style::new().fg(theme.muted));
        frame.render_widget(open.right_aligned(), area);
    }
}

/// The filter, with the cursor in it: it takes the keys while the view is
/// open.
fn draw_filter(frame: &mut Frame, view: &IssuesView, theme: &Theme, area: Rect) {
    let label = " filter: ";
    let line = Line::from(vec![
        Span::styled(
            label,
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(view.filter.text().to_string(), Style::new().fg(theme.text)),
    ]);
    frame.render_widget(line, area);
    let column = area.x + (label.len() + view.filter.cursor()) as u16;
    frame.set_cursor_position((column.min(area.right().saturating_sub(1)), area.y));
}

/// One row an issue: its number, its title and labels, and on the right who
/// opened it and how long ago it changed. The list scrolls to keep the bar
/// in sight.
fn draw_list(
    frame: &mut Frame,
    view: &IssuesView,
    issues: &[&Issue],
    theme: &Theme,
    now: u64,
    area: Rect,
) {
    let height = usize::from(area.height.max(1));
    let at = issues
        .iter()
        .position(|issue| Some(issue.number) == view.highlighted)
        .unwrap_or(0);
    let first = (at + 1).saturating_sub(height);
    for (row, issue) in issues.iter().enumerate().skip(first).take(height) {
        let line_area = Rect::new(area.x, area.y + (row - first) as u16, area.width, 1);
        let highlighted = Some(issue.number) == view.highlighted;
        if highlighted {
            frame.buffer_mut().set_style(line_area, theme.selection);
        }
        frame.render_widget(issue_line(issue, theme, highlighted, area.width), line_area);
        let when = unix_seconds(&issue.updated_at).map_or_else(String::new, |then| ago(then, now));
        let right = Line::styled(
            format!("{} · {when} ", issue.author.login),
            Style::new().fg(theme.muted),
        );
        if usize::from(area.width) > 60 {
            frame.render_widget(right.right_aligned(), line_area);
        }
    }
}

fn issue_line<'a>(issue: &Issue, theme: &Theme, highlighted: bool, width: u16) -> Line<'a> {
    let mut title = Style::new().fg(theme.text);
    if highlighted {
        title = title.add_modifier(Modifier::BOLD);
    }
    let number = format!(" #{:<5}", issue.number);
    // Room for the author and age on the right, when it's wide enough to
    // show them.
    let right = if width > 60 { 24 } else { 0 };
    let room = usize::from(width).saturating_sub(number.len() + right);
    let mut spans = vec![
        Span::styled(number, Style::new().fg(theme.muted)),
        Span::styled(fit(&issue.title, room), title),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    for label in &issue.labels {
        let label = format!("  {}", label.name);
        if used + label.chars().count() > usize::from(width).saturating_sub(right) {
            break;
        }
        spans.push(Span::styled(label, Style::new().fg(theme.branch)));
    }
    Line::from(spans)
}

/// The highlighted issue's text, as gh gave it.
fn draw_body(frame: &mut Frame, view: &IssuesView, theme: &Theme, area: Rect) {
    let Some(issue) = view.highlighted() else {
        return;
    };
    let text = match view.bodies.get(&issue.number) {
        None => "…".to_string(),
        Some(Err(reason)) => reason.clone(),
        Some(Ok(body)) if body.trim().is_empty() => "(no description)".to_string(),
        Some(Ok(body)) => body.clone(),
    };
    let lines: Vec<Line> = text
        .lines()
        .map(|line| Line::styled(format!(" {line}"), Style::new().fg(theme.text)))
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_note(frame: &mut Frame, theme: &Theme, note: &str, area: Rect) {
    let line = Line::styled(format!(" {note}"), Style::new().fg(theme.muted));
    frame.render_widget(line, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{Author, Label};

    fn issue(number: u64, title: &str, label: &str) -> Issue {
        Issue {
            number,
            title: title.into(),
            labels: vec![Label { name: label.into() }],
            updated_at: "2026-10-02T09:30:00Z".into(),
            author: Author {
                login: "ana".into(),
            },
            url: format!("https://github.com/acme/app/issues/{number}"),
        }
    }

    fn view_of(issues: Vec<Issue>) -> IssuesView {
        let mut view = IssuesView::new(PathBuf::from("/code/app"), "app".into());
        view.set_issues(Ok(issues));
        view
    }

    fn type_text(view: &mut IssuesView, text: &str) {
        for c in text.chars() {
            view.on_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn numbers(view: &IssuesView) -> Vec<u64> {
        view.shown().iter().map(|issue| issue.number).collect()
    }

    #[test]
    fn the_bar_starts_on_the_first_issue() {
        let view = view_of(vec![
            issue(42, "Login loops", "bug"),
            issue(7, "Dark mode", "idea"),
        ]);
        assert_eq!(view.highlighted().map(|i| i.number), Some(42));
    }

    #[test]
    fn typing_filters_by_title_label_or_number() {
        let mut view = view_of(vec![
            issue(42, "Login loops", "bug"),
            issue(7, "Dark mode", "idea"),
        ]);
        type_text(&mut view, "dark");
        assert_eq!(numbers(&view), [7]);
        assert_eq!(view.highlighted().map(|i| i.number), Some(7));

        let mut view = view_of(vec![
            issue(42, "Login loops", "bug"),
            issue(7, "Dark mode", "idea"),
        ]);
        type_text(&mut view, "bug");
        assert_eq!(numbers(&view), [42]);
        let mut view = view_of(vec![
            issue(42, "Login loops", "bug"),
            issue(7, "Dark mode", "idea"),
        ]);
        type_text(&mut view, "#7");
        assert_eq!(numbers(&view), [7]);
    }

    #[test]
    fn the_arrows_move_the_bar_and_stop_at_the_ends() {
        let mut view = view_of(vec![issue(1, "a", "x"), issue(2, "b", "x")]);
        view.on_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        view.on_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(view.highlighted().map(|i| i.number), Some(2));
        view.on_key(&KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(view.highlighted().map(|i| i.number), Some(1));
    }

    #[test]
    fn each_issues_text_is_asked_for_once() {
        let mut view = view_of(vec![issue(1, "a", "x"), issue(2, "b", "x")]);
        assert_eq!(view.body_to_fetch(), Some(1));
        assert_eq!(view.body_to_fetch(), None);
        view.on_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(view.body_to_fetch(), Some(2));
    }

    #[test]
    fn github_times_turn_into_seconds_since_the_epoch() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(unix_seconds("2026-10-02T09:30:15Z"), Some(1_790_933_415));
        assert_eq!(unix_seconds("yesterday"), None);
    }
}
