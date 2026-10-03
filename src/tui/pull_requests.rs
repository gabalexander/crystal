//! The pull requests view, `O` in the sidebar: the pull requests open on
//! the selected session's project, merge requests on GitLab, filtered as
//! you type, each with whether it's a draft and how its checks and review
//! stand, and the highlighted one read under the list: its description, its
//! checks one by one, and its conversation. Enter goes on to start a session
//! in its worktree; Ctrl+D shows its diff, Ctrl+C comments on it, and
//! Ctrl+O opens it in the browser.
//!
//! The state here is plain data, kept apart from I/O: the list, the bar and
//! what's been read are a [`Listing`], and what it asks of the forge goes
//! out as a [`Step`] for the app to carry out.

use super::compose::{self, CommentBox, Typed};
use super::issues::{COMMENT_HINTS, ago_from, comment_lines};
use super::listing::{self, Item, Listing};
use super::sidebar::fit;
use super::theme::Theme;
use crate::forge::{CheckState, Checks, Forge, PullRequest, PullRequestDetail, Review};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::PathBuf;

impl Item for PullRequest {
    fn number(&self) -> u64 {
        self.number
    }

    /// Its number, title, author and branch.
    fn searched(&self) -> String {
        format!(
            "{} {} {} {}",
            self.label(),
            self.title,
            self.author,
            self.local_branch
        )
    }
}

/// What a key in the view asks for beyond it.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Start a session in this pull request's worktree.
    Start(PullRequest),
    /// Show what pull request `number` changes.
    Diff(u64),
    /// Open pull request `number` in the browser.
    Open(u64),
    Comment {
        number: u64,
        text: String,
    },
}

pub struct PullRequestsView {
    /// The project's main worktree, where the forge is asked.
    pub project: PathBuf,
    pub project_name: String,
    /// The forge, which names what the view lists: GitHub until it's known.
    pub forge: Forge,
    pub list: Listing<PullRequest, PullRequestDetail>,
    /// The comment being written on the highlighted pull request, while it
    /// is.
    pub comment: Option<CommentBox>,
}

impl PullRequestsView {
    /// The view for the project at `project`, with `known`, the pull
    /// requests listed last, until the forge lists them again.
    pub fn new(
        project: PathBuf,
        project_name: String,
        forge: Forge,
        known: Option<Vec<PullRequest>>,
    ) -> PullRequestsView {
        PullRequestsView {
            project,
            project_name,
            forge,
            list: Listing::new(known),
            comment: None,
        }
    }

    pub fn set_pull_requests(&mut self, found: Result<(Forge, Vec<PullRequest>), String>) {
        let found = found.map(|(forge, pull_requests)| {
            self.forge = forge;
            pull_requests
        });
        self.list.set_items(found);
    }

    pub fn highlighted(&self) -> Option<&PullRequest> {
        self.list.highlighted()
    }

    /// Keys while the view is open: the comment box's while it's open; else
    /// Esc closes the view, Enter starts a session on the pull request the
    /// bar is on, Ctrl+D, Ctrl+C and Ctrl+O show its diff, comment on it
    /// and open it in the browser, and the list takes the rest.
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let highlighted = self.list.highlighted().cloned();
        match (key.code, highlighted) {
            (KeyCode::Esc, _) => Step::Close,
            (KeyCode::Enter, Some(pull_request)) => Step::Start(pull_request),
            (KeyCode::Char('d'), Some(pull_request)) if ctrl => Step::Diff(pull_request.number),
            (KeyCode::Char('o'), Some(pull_request)) if ctrl => Step::Open(pull_request.number),
            (KeyCode::Char('c'), Some(pull_request)) if ctrl => {
                self.comment = Some(CommentBox::new(pull_request.number));
                Step::Stay
            }
            _ => {
                self.list.on_key(key);
                Step::Stay
            }
        }
    }

    /// Pasted text goes where typing would.
    pub fn on_paste(&mut self, text: &str) {
        match &mut self.comment {
            Some(comment) => comment.on_paste(text),
            None => self.list.on_paste(text),
        }
    }

    /// The comment on pull request `number` was posted, or why it wasn't:
    /// posted, the box goes and the pull request is read again, with it.
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
}

/// The keys the footer shows while the view is open.
pub fn hints(view: &PullRequestsView) -> &'static [(&'static str, &'static str)] {
    if view.comment.is_some() {
        return COMMENT_HINTS;
    }
    &[
        ("↑/↓", "select"),
        ("enter", "start a session on it"),
        ("ctrl+d", "diff"),
        ("ctrl+c", "comment"),
        ("ctrl+o", "open"),
        ("esc", "close"),
    ]
}

/// Draws the view in `area`: a heading, the filter, the list, and the
/// highlighted pull request read, or the comment box in its place. `now`
/// is seconds since the Unix epoch.
pub fn draw(frame: &mut Frame, view: &PullRequestsView, theme: &Theme, now: u64, area: Rect) {
    let [heading, filter, rest] = listing::frame_areas(frame, theme, area);
    let forge = view.forge;
    let open = match view.list.items() {
        Some(Ok(_)) => Some(view.list.shown().len()),
        _ => None,
    };
    let title = forge.pull_requests();
    listing::draw_heading(frame, theme, title, &view.project_name, open, heading);
    let writing = view.comment.is_some();
    listing::draw_filter(frame, theme, &view.list.filter, !writing, filter);

    let pull_requests = match view.list.items() {
        None => {
            let note = format!("asking {}…", forge.name());
            return listing::draw_note(frame, theme, &note, rest);
        }
        Some(Err(reason)) => return listing::draw_note(frame, theme, reason, rest),
        Some(Ok(_)) => view.list.shown(),
    };
    if pull_requests.is_empty() {
        let note = format!("no open {} match", forge.pull_requests());
        return listing::draw_note(frame, theme, &note, rest);
    }
    let [list, rule, reading] = listing::list_areas(rest, pull_requests.len());
    draw_list(frame, view, &pull_requests, theme, list);
    listing::draw_rule(frame, theme, rule);
    if let Some(comment) = &view.comment {
        let heading = format!("comment on {}", forge.label(comment.number));
        compose::draw_comment_box(frame, theme, comment, &heading, reading);
    } else if let Some(pull_request) = view.highlighted() {
        let lines = reading_lines(view, pull_request, theme, now);
        listing::draw_reading(frame, lines, view.list.scroll, reading);
    }
}

/// One row a pull request: its number, its title and how it stands, and
/// on the right who opened it and its branch.
fn draw_list(
    frame: &mut Frame,
    view: &PullRequestsView,
    pull_requests: &[&PullRequest],
    theme: &Theme,
    area: Rect,
) {
    let at = view.list.highlighted_at().unwrap_or(0);
    let first = listing::first_drawn(at, area.height);
    let shown = pull_requests.iter().enumerate().skip(first);
    for (index, pull_request) in shown.take(usize::from(area.height)) {
        let highlighted = index == at;
        let row = listing::row_area(frame, theme, area, index - first, highlighted);
        let right = format!("{} · {} ", pull_request.author, pull_request.local_branch);
        let right = fit(&right, usize::from(area.width / 3));
        let room = usize::from(area.width).saturating_sub(right.chars().count() + 1);
        frame.render_widget(
            pull_request_line(pull_request, theme, highlighted, room),
            row,
        );
        let right = Line::styled(right, Style::new().fg(theme.muted));
        frame.render_widget(right.right_aligned(), row);
    }
}

/// A pull request's number, title, and then what's worth knowing about
/// how it stands, in `width` columns: the title is cut for them.
fn pull_request_line<'a>(
    pull_request: &PullRequest,
    theme: &Theme,
    highlighted: bool,
    width: usize,
) -> Line<'a> {
    let mut title = Style::new().fg(theme.text);
    if highlighted {
        title = title.add_modifier(Modifier::BOLD);
    }
    let number = format!(" {:<6}", pull_request.label());
    let marks = marks(pull_request, theme);
    let marks_width: usize = marks.iter().map(Span::width).sum();
    let room = width.saturating_sub(number.chars().count() + marks_width);
    let mut spans = vec![
        Span::styled(number, Style::new().fg(theme.muted)),
        Span::styled(fit(&pull_request.title, room), title),
    ];
    spans.extend(marks);
    Line::from(spans)
}

/// Whether it's a draft, how its checks stand, and what its reviewers
/// decided, each in its own color, the way the worktree lines mark them.
fn marks<'a>(pull_request: &PullRequest, theme: &Theme) -> Vec<Span<'a>> {
    let mut marks = Vec::new();
    let mut mark = |text: &str, color: Color| {
        marks.push(Span::styled(format!("  {text}"), Style::new().fg(color)));
    };
    if pull_request.draft {
        mark("draft", theme.muted);
    }
    match pull_request.checks {
        Checks::Failed => mark("✗ checks", theme.failed),
        Checks::Running => mark("◌ checks", theme.working),
        Checks::Passed => mark("✓ checks", theme.done),
        Checks::None => {}
    }
    match pull_request.review {
        Review::Approved => mark("approved", theme.done),
        Review::ChangesRequested => mark("changes asked", theme.waiting),
        Review::Required => mark("needs review", theme.muted),
        Review::None => {}
    }
    marks
}

/// The highlighted pull request read: where it would merge, its checks
/// one by one, its description, then its conversation.
fn reading_lines<'a>(
    view: &PullRequestsView,
    pull_request: &PullRequest,
    theme: &Theme,
    now: u64,
) -> Vec<Line<'a>> {
    let text = Style::new().fg(theme.text);
    let muted = Style::new().fg(theme.muted);
    let detail = match view.list.detail(pull_request.number) {
        None => return vec![Line::styled(" …", text)],
        Some(Err(reason)) => return vec![Line::styled(format!(" {reason}"), text)],
        Some(Ok(detail)) => detail,
    };
    let into = if detail.base.is_empty() {
        String::new()
    } else {
        format!(" into {}", detail.base)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {}", pull_request.author), text),
        Span::styled(" wants to merge ", muted),
        Span::styled(
            pull_request.local_branch.clone(),
            Style::new().fg(theme.branch),
        ),
        Span::styled(into, muted),
        Span::styled(
            format!(" · {}", ago_from(&pull_request.updated_at, now)),
            muted,
        ),
    ])];
    if !detail.checks.is_empty() {
        let mut checks = vec![Span::styled(" checks", muted)];
        for check in &detail.checks {
            let (mark, color) = match check.state {
                CheckState::Passed => ("✓", theme.done),
                CheckState::Failed => ("✗", theme.failed),
                CheckState::Running => ("◌", theme.working),
                CheckState::Skipped => ("-", theme.muted),
            };
            checks.push(Span::styled(format!("  {mark} "), Style::new().fg(color)));
            checks.push(Span::styled(check.name.clone(), text));
        }
        lines.push(Line::from(checks));
    }
    lines.push(Line::default());
    if detail.body.trim().is_empty() {
        lines.push(Line::styled(" (no description)", muted));
    } else {
        lines.extend(listing::text_lines(&detail.body, text));
    }
    for comment in &detail.conversation {
        lines.push(Line::default());
        lines.extend(comment_lines(comment, theme, now));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeName;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn pull_request(number: u64, title: &str, branch: &str) -> PullRequest {
        PullRequest {
            forge: Forge::GitHub,
            number,
            title: title.into(),
            author: "ana".into(),
            branch: branch.into(),
            from_fork: false,
            local_branch: branch.into(),
            draft: false,
            checks: Checks::None,
            review: Review::None,
            updated_at: "2026-10-02T09:30:00Z".into(),
            url: format!("https://github.com/acme/app/pull/{number}"),
        }
    }

    fn view_of(pull_requests: Vec<PullRequest>) -> PullRequestsView {
        let mut view = PullRequestsView::new(
            PathBuf::from("/code/app"),
            "app".into(),
            Forge::GitHub,
            None,
        );
        view.set_pull_requests(Ok((Forge::GitHub, pull_requests)));
        view
    }

    fn press(view: &mut PullRequestsView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(view: &mut PullRequestsView, c: char) -> Step {
        view.on_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn drawn(view: &PullRequestsView) -> String {
        let theme = Theme::new(ThemeName::Dark, false);
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        terminal
            .draw(|frame| draw(frame, view, &theme, 1_790_940_000, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let row: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                row.trim_end().to_string() + "\n"
            })
            .collect()
    }

    #[test]
    fn the_keys_ask_for_the_highlighted_pull_request() {
        let mut view = view_of(vec![
            pull_request(57, "Fix the login redirect", "fix-login"),
            pull_request(58, "Dark mode", "dark"),
        ]);
        press(&mut view, KeyCode::Down);
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Step::Start(pull_request(58, "Dark mode", "dark"))
        );
        assert_eq!(ctrl(&mut view, 'd'), Step::Diff(58));
        assert_eq!(ctrl(&mut view, 'o'), Step::Open(58));
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Close);
    }

    #[test]
    fn typing_filters_by_title_author_or_branch() {
        let mut view = view_of(vec![
            pull_request(57, "Fix the login redirect", "fix-login"),
            pull_request(58, "Dark mode", "dark"),
        ]);
        for c in "dark".chars() {
            press(&mut view, KeyCode::Char(c));
        }
        assert_eq!(view.highlighted().map(|pr| pr.number), Some(58));
        assert_eq!(view.list.shown().len(), 1);
    }

    #[test]
    fn a_comment_stays_on_its_pull_request_until_the_forge_takes_it() {
        let mut view = view_of(vec![pull_request(57, "Fix it", "fix-login")]);
        ctrl(&mut view, 'c');
        for c in "LGTM".chars() {
            press(&mut view, KeyCode::Char(c));
        }
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Step::Comment {
                number: 57,
                text: "LGTM".into()
            }
        );
        // The list changing under it doesn't move it.
        view.set_pull_requests(Ok((Forge::GitHub, vec![pull_request(9, "Other", "o")])));
        view.commented(57, Err("gh: not logged in".into()));
        assert_eq!(view.comment.as_ref().map(|c| c.number), Some(57));
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Stay);
        assert!(view.comment.is_none());
    }

    #[test]
    fn a_row_says_how_the_pull_request_stands_and_the_pane_reads_it() {
        let failing = PullRequest {
            draft: true,
            checks: Checks::Failed,
            review: Review::ChangesRequested,
            ..pull_request(57, "Fix the login redirect", "fix-login")
        };
        let mut view = view_of(vec![failing]);
        let detail = PullRequestDetail {
            base: "main".into(),
            body: "Sends you home after login.".into(),
            checks: vec![crate::forge::Check {
                name: "build".into(),
                state: CheckState::Failed,
            }],
            conversation: vec![crate::forge::Comment {
                author: "bo".into(),
                at: "2026-10-02T10:00:00Z".into(),
                body: "Not this way.".into(),
                verdict: Some(Review::ChangesRequested),
            }],
        };
        view.list.set_detail(57, Ok(detail));
        let screen = drawn(&view);
        assert!(screen.contains("pull requests · app"), "{screen}");
        assert!(
            screen.contains("#57   Fix the login redirect  draft  ✗ checks  changes asked"),
            "{screen}"
        );
        assert!(
            screen.contains("ana wants to merge fix-login into main"),
            "{screen}"
        );
        assert!(screen.contains("checks  ✗ build"), "{screen}");
        assert!(screen.contains("Sends you home after login."), "{screen}");
        assert!(screen.contains("bo asked for changes"), "{screen}");
        assert!(screen.contains("Not this way."), "{screen}");
    }

    #[test]
    fn on_gitlab_it_lists_merge_requests() {
        let mut view = PullRequestsView::new(
            PathBuf::from("/code/app"),
            "app".into(),
            Forge::GitHub,
            None,
        );
        let merge_request = PullRequest {
            forge: Forge::GitLab,
            ..pull_request(57, "Fix it", "fix-login")
        };
        view.set_pull_requests(Ok((Forge::GitLab, vec![merge_request])));
        let screen = drawn(&view);
        assert!(screen.contains("merge requests · app"), "{screen}");
        assert!(screen.contains("!57"), "{screen}");
    }
}
