//! The issues view, `i` in the sidebar: the open issues of the selected
//! session's project on its forge, the latest to change first, filtered as
//! you type, with the highlighted issue read under the list: its text, then
//! what's been said on it. Enter goes on to start a session for it; Ctrl+C
//! comments on it, Ctrl+E changes its title and text, Ctrl+O opens it in
//! the browser, and Ctrl+R asks the forge again.
//!
//! The state here is plain data, kept apart from I/O: the list, the bar and
//! what's been read are a [`Listing`], and what's being written goes to the
//! forge as a [`Step`] for the app to carry out.

use super::compose::{self, CommentBox, IssueForm, Typed};
use super::listing::{self, Item, Listing};
use super::sidebar::{ago, fit};
use super::theme::Theme;
use crate::forge::{Comment, Forge, Issue, IssueDetail, Review};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::PathBuf;

impl Item for Issue {
    fn number(&self) -> u64 {
        self.number
    }

    /// Its number, title, labels and author.
    fn searched(&self) -> String {
        format!(
            "#{} {} {} {}",
            self.number,
            self.title,
            self.labels.join(" "),
            self.author
        )
    }
}

/// What a key in the view asks for beyond it.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Start a session for this issue.
    Start(Issue),
    /// Open issue `number` in the browser.
    Open(u64),
    Comment {
        number: u64,
        text: String,
    },
    Edit {
        number: u64,
        title: String,
        body: String,
    },
    /// Ask the forge for the list again, and the highlighted one with it.
    Refresh,
    /// Tell the user this, at the bottom.
    Say(String),
}

pub struct IssuesView {
    /// The project's main worktree, where the forge is asked.
    pub project: PathBuf,
    pub project_name: String,
    /// The forge, once it's known: until then it's GitHub, which only
    /// changes what the view is waiting on.
    pub forge: Forge,
    pub list: Listing<Issue, IssueDetail>,
    /// The comment being written on the highlighted issue, while it is.
    pub comment: Option<CommentBox>,
    /// The highlighted issue's title and text being changed, while they
    /// are.
    pub form: Option<IssueForm>,
}

impl IssuesView {
    /// The view for the project at `project`, with `known`, the issues
    /// listed last, until the forge lists them again.
    pub fn new(
        project: PathBuf,
        project_name: String,
        forge: Forge,
        known: Option<Vec<Issue>>,
    ) -> IssuesView {
        IssuesView {
            project,
            project_name,
            forge,
            list: Listing::new(known),
            comment: None,
            form: None,
        }
    }

    pub fn set_issues(&mut self, found: Result<(Forge, Vec<Issue>), String>) {
        let found = found.map(|(forge, issues)| {
            self.forge = forge;
            issues
        });
        self.list.set_items(found);
    }

    pub fn highlighted(&self) -> Option<&Issue> {
        self.list.highlighted()
    }

    /// Keys while the view is open: the comment box's or the form's while
    /// one is open; else Esc closes it, Enter starts a session for the
    /// issue the bar is on, Ctrl+C, Ctrl+E and Ctrl+O comment on it, change
    /// it and open it in the browser, Ctrl+R asks for the list again, and
    /// the list takes the rest.
    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        if let Some(comment) = &mut self.comment {
            return match comment.on_key(key) {
                Typed::Stay => Step::Stay,
                Typed::Cancel => {
                    self.comment = None;
                    Step::Stay
                }
                Typed::Send(text) => Step::Comment {
                    number: comment.number,
                    text,
                },
            };
        }
        if let Some(form) = &mut self.form {
            return match form.on_key(key) {
                Typed::Stay => Step::Stay,
                Typed::Cancel => {
                    self.form = None;
                    Step::Stay
                }
                Typed::Send((title, body)) => Step::Edit {
                    number: form.number,
                    title,
                    body,
                },
            };
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let highlighted = self.list.highlighted().cloned();
        match (key.code, highlighted) {
            (KeyCode::Esc, _) => Step::Close,
            (KeyCode::Char('r'), _) if ctrl => {
                self.list.ask_again();
                Step::Refresh
            }
            (KeyCode::Enter, Some(issue)) => Step::Start(issue),
            (KeyCode::Char('o'), Some(issue)) if ctrl => Step::Open(issue.number),
            (KeyCode::Char('c'), Some(issue)) if ctrl => {
                self.comment = Some(CommentBox::new(issue.number));
                Step::Stay
            }
            (KeyCode::Char('e'), Some(issue)) if ctrl => self.edit(&issue),
            _ => {
                self.list.on_key(key);
                Step::Stay
            }
        }
    }

    /// Pasted text goes where typing would.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(comment) = &mut self.comment {
            comment.on_paste(text);
        } else if let Some(form) = &mut self.form {
            form.on_paste(text);
        } else {
            self.list.on_paste(text);
        }
    }

    /// The comment on issue `number` was posted, or why it wasn't: posted,
    /// the box goes and the issue is read again, with it.
    pub fn commented(&mut self, number: u64, posted: Result<(), String>) {
        match posted {
            Ok(()) => {
                self.comment = None;
                self.list.read_again(number);
            }
            Err(reason) => {
                if let Some(comment) = &mut self.comment {
                    comment.refused(reason);
                }
            }
        }
    }

    /// Issue `number` has this title and text now, or why it doesn't:
    /// saved, the form goes and the list and the reading pane say them.
    pub fn edited(&mut self, number: u64, title: String, body: String, saved: Result<(), String>) {
        if let Err(reason) = saved {
            if let Some(form) = &mut self.form {
                form.refused(reason);
            }
            return;
        }
        self.form = None;
        if let Some(issue) = self.list.item_mut(number) {
            issue.title = title;
        }
        if let Some(detail) = self.list.detail_mut(number) {
            detail.body = body;
        }
    }

    /// Opens the form on `issue`, once its text has been read.
    fn edit(&mut self, issue: &Issue) -> Step {
        match self.list.detail(issue.number) {
            Some(Ok(detail)) => {
                self.form = Some(IssueForm::new(issue.number, &issue.title, &detail.body));
                Step::Stay
            }
            Some(Err(reason)) => Step::Say(reason.clone()),
            None => Step::Say(format!("still reading #{}", issue.number)),
        }
    }
}

/// The keys the footer shows while the view is open.
pub fn hints(view: &IssuesView) -> &'static [(&'static str, &'static str)] {
    if view.comment.is_some() {
        COMMENT_HINTS
    } else if view.form.is_some() {
        FORM_HINTS
    } else {
        &[
            ("↑/↓", "select"),
            ("enter", "start a session on it"),
            ("ctrl+c", "comment"),
            ("ctrl+e", "edit"),
            ("ctrl+o", "open"),
            ("ctrl+r", "refresh"),
            ("esc", "close"),
        ]
    }
}

/// The keys while a comment is being written, here or on a pull request.
pub const COMMENT_HINTS: &[(&str, &str)] = &[
    ("enter", "post"),
    ("alt+enter", "new line"),
    ("esc", "cancel"),
];

const FORM_HINTS: &[(&str, &str)] = &[
    ("enter", "save"),
    ("tab", "title/text"),
    ("alt+enter", "new line"),
    ("esc", "cancel"),
];

/// Seconds since the Unix epoch for a time a forge writes like
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

/// How long ago `time`, as a forge writes it, was, `now` being seconds
/// since the Unix epoch: `2h`, or nothing for a time it can't read.
pub fn ago_from(time: &str, now: u64) -> String {
    unix_seconds(time).map_or_else(String::new, |then| ago(then, now))
}

/// Draws the view in `area`: a heading, the filter, the list, and the
/// highlighted issue read, or the comment box or the form in its place.
/// `now` is seconds since the Unix epoch.
pub fn draw(frame: &mut Frame, view: &IssuesView, theme: &Theme, now: u64, area: Rect) {
    let [heading, filter, rest] = listing::frame_areas(frame, theme, area);
    let mut said = Vec::new();
    if let Some(Ok(_)) = view.list.items() {
        said.push(format!("{} open", view.list.shown().len()));
    }
    said.extend(view.list.asking_note(view.forge.name()));
    listing::draw_heading(frame, theme, "issues", &view.project_name, &said, heading);
    let writing = view.comment.is_some() || view.form.is_some();
    listing::draw_filter(frame, theme, &view.list.filter, !writing, filter);

    let issues = match view.list.items() {
        None => {
            let note = format!("asking {}…", view.forge.name());
            return listing::draw_note(frame, theme, &note, rest);
        }
        Some(Err(reason)) => return listing::draw_note(frame, theme, reason, rest),
        Some(Ok(_)) => view.list.shown(),
    };
    if issues.is_empty() {
        return listing::draw_note(frame, theme, "no open issues match", rest);
    }
    let [list, rule, reading] = listing::list_areas(rest, issues.len());
    draw_list(frame, view, &issues, theme, now, list);
    listing::draw_rule(frame, theme, rule);
    if let Some(comment) = &view.comment {
        let heading = format!("comment on #{}", comment.number);
        compose::draw_comment_box(frame, theme, comment, &heading, reading);
    } else if let Some(form) = &view.form {
        compose::draw_issue_form(frame, theme, form, reading);
    } else if let Some(issue) = view.highlighted() {
        let lines = reading_lines(view, issue, theme, now);
        listing::draw_reading(frame, lines, view.list.scroll, reading);
    }
}

/// One row an issue: its number, its title and labels, and on the right who
/// opened it and how long ago it changed.
fn draw_list(
    frame: &mut Frame,
    view: &IssuesView,
    issues: &[&Issue],
    theme: &Theme,
    now: u64,
    area: Rect,
) {
    let at = view.list.highlighted_at().unwrap_or(0);
    let first = listing::first_drawn(at, area.height);
    let shown = issues.iter().enumerate().skip(first);
    for (index, issue) in shown.take(usize::from(area.height)) {
        let highlighted = index == at;
        let row = listing::row_area(frame, theme, area, index - first, highlighted);
        frame.render_widget(issue_line(issue, theme, highlighted, area.width), row);
        let right = Line::styled(
            format!("{} · {} ", issue.author, ago_from(&issue.updated_at, now)),
            Style::new().fg(theme.muted),
        );
        if usize::from(area.width) > 60 {
            frame.render_widget(right.right_aligned(), row);
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
        let label = format!("  {label}");
        if used + label.chars().count() > usize::from(width).saturating_sub(right) {
            break;
        }
        spans.push(Span::styled(label, Style::new().fg(theme.branch)));
    }
    Line::from(spans)
}

/// The highlighted issue read: its text, then each comment under who
/// wrote it and when.
fn reading_lines<'a>(view: &IssuesView, issue: &Issue, theme: &Theme, now: u64) -> Vec<Line<'a>> {
    let text = Style::new().fg(theme.text);
    let detail = match view.list.detail(issue.number) {
        None => return vec![Line::styled(" …", text)],
        Some(Err(reason)) => return vec![Line::styled(format!(" {reason}"), text)],
        Some(Ok(detail)) => detail,
    };
    let mut lines = if detail.body.trim().is_empty() {
        vec![Line::styled(
            " (no description)",
            Style::new().fg(theme.muted),
        )]
    } else {
        listing::text_lines(&detail.body, text)
    };
    for comment in &detail.comments {
        lines.push(Line::default());
        lines.extend(comment_lines(comment, theme, now));
    }
    lines
}

/// A comment in a reading pane: who wrote it, how long ago and what it
/// decided, if it's a review that did, then what it says.
pub fn comment_lines<'a>(comment: &Comment, theme: &Theme, now: u64) -> Vec<Line<'a>> {
    let verdict = match comment.verdict {
        Some(Review::Approved) => Some(("approved", theme.done)),
        Some(Review::ChangesRequested) => Some(("asked for changes", theme.waiting)),
        _ => None,
    };
    let mut heading = vec![Span::styled(
        format!(" {}", comment.author),
        Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
    )];
    if let Some((verdict, color)) = verdict {
        heading.push(Span::styled(format!(" {verdict}"), Style::new().fg(color)));
    }
    heading.push(Span::styled(
        format!(" · {}", ago_from(&comment.at, now)),
        Style::new().fg(theme.muted),
    ));
    let mut lines = vec![Line::from(heading)];
    lines.extend(listing::text_lines(
        &comment.body,
        Style::new().fg(theme.text),
    ));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Paragraph, Wrap};
    use std::time::Instant;

    fn issue(number: u64, title: &str, label: &str) -> Issue {
        Issue {
            number,
            title: title.into(),
            labels: vec![label.into()],
            updated_at: "2026-10-02T09:30:00Z".into(),
            author: "ana".into(),
            url: format!("https://github.com/acme/app/issues/{number}"),
        }
    }

    fn view_of(issues: Vec<Issue>) -> IssuesView {
        let mut view = IssuesView::new(
            PathBuf::from("/code/app"),
            "app".into(),
            Forge::GitHub,
            None,
        );
        view.set_issues(Ok((Forge::GitHub, issues)));
        view
    }

    fn press(view: &mut IssuesView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(view: &mut IssuesView, c: char) -> Step {
        view.on_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn type_text(view: &mut IssuesView, text: &str) {
        for c in text.chars() {
            press(view, KeyCode::Char(c));
        }
    }

    /// Draws `view` over a screen full of `¤`, the way it opens over the
    /// sidebar and the panes, and returns what's on the screen.
    fn drawn_over_the_screen(view: &IssuesView) -> String {
        let theme = Theme::new(ThemeName::DARK, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let behind = "¤".repeat(usize::from(area.width * area.height));
                frame.render_widget(Paragraph::new(behind).wrap(Wrap { trim: false }), area);
                draw(frame, view, &theme, 1_000, area);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn nothing_behind_the_issues_shows_through_them() {
        let mut view = view_of(vec![issue(42, "Login loops", "bug")]);
        assert!(!drawn_over_the_screen(&view).contains('¤'));
        type_text(&mut view, "nothing like it");
        assert!(!drawn_over_the_screen(&view).contains('¤'));
    }

    #[test]
    fn typing_filters_by_title_label_or_author() {
        let mut view = view_of(vec![
            issue(42, "Login loops", "bug"),
            issue(7, "Dark mode", "idea"),
        ]);
        type_text(&mut view, "bug");
        assert_eq!(view.highlighted().map(|i| i.number), Some(42));
        let mut view = view_of(vec![issue(42, "Login loops", "bug")]);
        type_text(&mut view, "ana");
        assert_eq!(view.list.shown().len(), 1);
    }

    #[test]
    fn ctrl_r_asks_again_for_the_issues_and_reads_the_highlighted_one_again() {
        let mut view = view_of(vec![issue(42, "Login loops", "bug")]);
        assert_eq!(view.list.detail_to_fetch(), Some(42));
        assert_eq!(ctrl(&mut view, 'r'), Step::Refresh);
        assert_eq!(
            view.list.asking_note("GitHub").as_deref(),
            Some("asking GitHub…")
        );
        assert_eq!(view.list.detail_to_fetch(), Some(42));
    }

    #[test]
    fn enter_starts_on_the_issue_and_esc_closes() {
        let mut view = view_of(vec![issue(42, "Login loops", "bug")]);
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Step::Start(issue(42, "Login loops", "bug"))
        );
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Close);
        assert_eq!(ctrl(&mut view, 'o'), Step::Open(42));
    }

    #[test]
    fn a_comment_goes_on_the_highlighted_issue_and_it_is_read_again() {
        let mut view = view_of(vec![issue(42, "Login loops", "bug")]);
        assert_eq!(view.list.detail_to_fetch(), Some(42));
        assert_eq!(ctrl(&mut view, 'c'), Step::Stay);
        // Letters go into the comment now, not the filter.
        type_text(&mut view, "Same here");
        assert_eq!(view.list.filter.text(), "");
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Step::Comment {
                number: 42,
                text: "Same here".into()
            }
        );
        view.commented(42, Err("gh: not logged in".into()));
        let comment = view.comment.as_ref().unwrap();
        assert_eq!(comment.text.text(), "Same here");
        assert_eq!(comment.problem.as_deref(), Some("gh: not logged in"));
        press(&mut view, KeyCode::Enter);
        view.commented(42, Ok(()));
        assert!(view.comment.is_none());
        assert_eq!(view.list.detail_to_fetch(), Some(42));
    }

    #[test]
    fn an_issue_is_edited_once_its_text_is_read() {
        let mut view = view_of(vec![issue(42, "Login loops", "bug")]);
        assert_eq!(ctrl(&mut view, 'e'), Step::Say("still reading #42".into()));
        let detail = IssueDetail {
            body: "It loops.".into(),
            comments: Vec::new(),
        };
        view.list.set_detail(42, Ok(detail), Instant::now());
        ctrl(&mut view, 'e');
        type_text(&mut view, " forever");
        let step = press(&mut view, KeyCode::Enter);
        assert_eq!(
            step,
            Step::Edit {
                number: 42,
                title: "Login loops forever".into(),
                body: "It loops.".into()
            }
        );
        view.edited(42, "Login loops forever".into(), "It loops.".into(), Ok(()));
        assert!(view.form.is_none());
        assert_eq!(view.highlighted().unwrap().title, "Login loops forever");
    }

    #[test]
    fn forge_times_turn_into_seconds_since_the_epoch() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(unix_seconds("2026-10-02T09:30:15Z"), Some(1_790_933_415));
        assert_eq!(
            unix_seconds("2026-10-02T09:30:15.123Z"),
            Some(1_790_933_415)
        );
        assert_eq!(unix_seconds("yesterday"), None);
    }
}
