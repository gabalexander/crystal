//! The sidebar: every session under its project and its worktree, each
//! with a mark for what it's doing and how long ago that last changed.

use super::app::{App, Hit};
use super::groups::Row;
use super::status::Status;
use super::theme::Theme;
use super::ui::Look;
use crate::github::{PullRequest, PullRequestState};
use crate::protocol::{Front, SessionInfo};
use crate::shell;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

/// The room left at the sidebar's left edge, in line with the top bar and
/// the footer.
const MARGIN: &str = " ";

/// How far a worktree's line is indented: under its project's heading.
const WORKTREE_INDENT: &str = "   ";

/// How far a session row is indented: under its worktree's line.
const SESSION_INDENT: &str = "     ";

pub fn draw(frame: &mut Frame, app: &App, look: &Look, area: Rect) {
    let rows = app.rows();
    let first = offset(app, area.height);
    let selected = selected_row(app);
    let shown = rows.iter().enumerate().skip(first).take(area.height.into());
    for (index, row) in shown {
        let y = area.y + (index - first) as u16;
        let line_area = Rect::new(area.x, y, area.width, 1);
        // A session's task line goes with it, selected or not.
        let is_selected = match row {
            Row::Task(_) => index > 0 && selected == Some(index - 1),
            _ => selected == Some(index),
        };
        if is_selected {
            frame
                .buffer_mut()
                .set_style(line_area, look.theme.selection);
        }
        let line = row_line(app, row, look, area.width, is_selected);
        frame.render_widget(line, line_area);
    }
}

/// Which sidebar row is on screen `row`, in a sidebar drawn in `area`.
pub fn hit(area: Rect, app: &App, row: u16) -> Hit {
    let index = offset(app, area.height) + usize::from(row - area.y);
    if index < app.rows().len() {
        Hit::SidebarRow(index)
    } else {
        Hit::Sidebar
    }
}

/// The first row on screen, when the rows don't all fit in `height`: the
/// list scrolls just far enough to keep the selection in sight. Drawing and
/// clicking both go by this, so a click lands on the row drawn there.
fn offset(app: &App, height: u16) -> usize {
    let height = usize::from(height.max(1));
    // The selected session's task line is kept in sight with it.
    let last = selected_row(app).map(|row| match app.rows().get(row + 1) {
        Some(Row::Task(_)) => row + 1,
        _ => row,
    });
    match last {
        Some(last) if last >= height => last + 1 - height,
        _ => 0,
    }
}

/// The row the bar is on: the selected session's, or, while `/`'s filter
/// is open, the one its bar is on.
fn selected_row(app: &App) -> Option<usize> {
    let index = app.sidebar_cursor()?;
    app.rows()
        .iter()
        .position(|row| *row == Row::Session(index))
}

/// One row, fitted to `width` columns.
fn row_line<'a>(app: &'a App, row: &Row, look: &Look, width: u16, selected: bool) -> Line<'a> {
    let theme = look.theme;
    match row {
        Row::Project { name, path } => {
            let mut line = heading(name, Style::new().fg(theme.text), look, width);
            if let Some(open) = app.backlog_open(path) {
                to_do_on_heading(&mut line, open, theme);
            }
            line
        }
        Row::OutsideGit => heading("outside git", Style::new().fg(theme.muted), look, width),
        Row::Worktree {
            project,
            branch,
            main,
        } => worktree_line(app, project, branch.as_deref(), *main, theme, width),
        Row::Directory(dir) => {
            let dir = shell::home_relative(dir);
            let room = usize::from(width).saturating_sub(WORKTREE_INDENT.len());
            Line::from(vec![
                Span::raw(WORKTREE_INDENT),
                Span::styled(fit(&dir, room), Style::new().fg(theme.muted)),
            ])
        }
        Row::Session(index) => {
            let marked = app.marked_letters(*index);
            session_line(&app.sessions()[*index], &marked, look, width, selected)
        }
        Row::Task(index) => task_line(&app.sessions()[*index], theme, width),
    }
}

/// How many backlog items are still to do, at the end of a project's
/// heading, in place of the end of its rule: `payments ──── 3 to do`. When
/// the rule is too short for it, it isn't shown.
fn to_do_on_heading(line: &mut Line, open: usize, theme: &Theme) {
    let to_do = format!(" {open} to do");
    let Some(rule) = line.spans.last_mut() else {
        return;
    };
    let rule_width = rule.content.chars().count();
    let to_do_width = to_do.chars().count();
    // Leave at least three columns of rule before it.
    if rule_width < to_do_width + 3 {
        return;
    }
    rule.content = "─".repeat(rule_width - to_do_width).into();
    line.spans
        .push(Span::styled(to_do, Style::new().fg(theme.muted)));
}

/// The line under a session with a task: what it was asked to do, while
/// it's open, or how it went, marked done or failed, once it's closed.
fn task_line<'a>(session: &SessionInfo, theme: &Theme, width: u16) -> Line<'a> {
    let Some(task) = &session.task else {
        return Line::default();
    };
    // Under the session's name: its indent and mark.
    let indent = format!("{SESSION_INDENT}  ");
    let room = usize::from(width).saturating_sub(indent.len() + 1);
    let goal = task.goal.lines().next().unwrap_or("");
    let (mark, color, said) = match &task.outcome {
        None => ("", theme.muted, goal),
        Some(outcome) => {
            let said = if outcome.summary.is_empty() {
                goal
            } else {
                outcome.summary.as_str()
            };
            if outcome.failed {
                ("✗ ", theme.failed, said)
            } else {
                ("✓ ", theme.done, said)
            }
        }
    };
    let room = room.saturating_sub(mark.chars().count());
    Line::from(vec![
        Span::raw(indent),
        Span::styled(mark, Style::new().fg(color)),
        Span::styled(fit(said, room), Style::new().fg(theme.muted)),
    ])
}

/// A worktree's line: its mark and branch, and on the right its pull
/// request when GitHub knows of one, `#57` and a mark for what matters most
/// about it. Short of room, the mark goes first, then the number, before
/// the branch is cut.
fn worktree_line<'a>(
    app: &App,
    project: &Path,
    branch: Option<&str>,
    main: bool,
    theme: &Theme,
    width: u16,
) -> Line<'a> {
    let mark = if main { "⌂ " } else { "⎇ " };
    let name = branch.unwrap_or("(detached)");
    // The indent and the mark before the branch; a space at the end.
    let room = usize::from(width).saturating_sub(WORKTREE_INDENT.len() + 2 + 1);
    let pull_request = branch.and_then(|branch| app.pull_request(project, branch));
    let right = pull_request
        .map(|pull_request| pull_request_spans(pull_request, theme))
        .unwrap_or_default()
        .into_iter()
        .find(|spans| name.chars().count() + 1 + width_of(spans) <= room)
        .unwrap_or_default();

    let mut line = vec![
        Span::raw(WORKTREE_INDENT),
        Span::styled(mark, Style::new().fg(theme.muted)),
    ];
    if right.is_empty() {
        line.push(Span::styled(fit(name, room), Style::new().fg(theme.branch)));
    } else {
        let gap = room - name.chars().count() - width_of(&right);
        line.push(Span::styled(
            name.to_string(),
            Style::new().fg(theme.branch),
        ));
        line.push(Span::raw(" ".repeat(gap)));
        line.extend(right);
    }
    Line::from(line)
}

/// What a worktree line can say on the right about its pull request, the
/// most first: its number and a mark, then its number alone.
fn pull_request_spans<'a>(pull_request: &PullRequest, theme: &Theme) -> Vec<Vec<Span<'a>>> {
    let number = Span::styled(
        format!("#{}", pull_request.number),
        Style::new().fg(theme.muted),
    );
    let mut forms = Vec::new();
    if let Some((mark, color)) = pull_request_mark(pull_request.state(), theme) {
        forms.push(vec![
            number.clone(),
            Span::raw(" "),
            Span::styled(mark, Style::new().fg(color)),
        ]);
    }
    forms.push(vec![number]);
    forms
}

/// The mark for what matters most about a pull request, in the color that
/// says how it stands. One that's simply ready has none.
fn pull_request_mark(state: PullRequestState, theme: &Theme) -> Option<(&'static str, Color)> {
    let mark = match state {
        PullRequestState::ChecksFailing => ("✗", theme.failed),
        PullRequestState::ChangesRequested => ("±", theme.waiting),
        PullRequestState::Draft => ("draft", theme.muted),
        PullRequestState::ChecksRunning => ("◌", theme.working),
        PullRequestState::Approved => ("✓", theme.done),
        PullRequestState::Ready => return None,
    };
    Some(mark)
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// A heading: the name in bold, then a thin rule nearly to the edge.
fn heading<'a>(name: &str, style: Style, look: &Look, width: u16) -> Line<'a> {
    let width = usize::from(width).saturating_sub(MARGIN.len());
    let name = fit(name, width);
    // A space before the rule, and one kept clear after it.
    let rule = width.saturating_sub(name.chars().count() + 2);
    Line::from(vec![
        Span::raw(MARGIN),
        Span::styled(name, style.add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::new().fg(look.theme.rule)),
    ])
}

/// A session's row: its mark, its name, what's in front in it when the
/// name doesn't say, and on the right how long ago it changed. Short of
/// room, what's in front goes first, then the time, before the name is
/// cut. The letters at `marked` in the name are those `/`'s filter matched.
fn session_line<'a>(
    session: &SessionInfo,
    marked: &[usize],
    look: &Look,
    width: u16,
    selected: bool,
) -> Line<'a> {
    let theme = look.theme;
    let status = Status::of(session);
    let mut name_style = Style::new().fg(theme.text);
    if selected {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    // A shell at its prompt is the quiet kind of running: its mark fades,
    // so the agents stand out.
    let at_a_shell = matches!(session.front, Some(Front::Shell { .. }));
    let mark_color = if at_a_shell && status == Status::Running {
        theme.muted
    } else {
        theme.status(status)
    };
    // The indent, the mark and a space before the name; a space at the end.
    let room = usize::from(width).saturating_sub(SESSION_INDENT.len() + 2 + 1);
    let when = changed_ago(session, look.now);
    let label = front_label(session).unwrap_or_default();
    let (label, when) = fitting_extras(session.name.chars().count(), label, &when, room);

    let mut spans = vec![
        Span::raw(SESSION_INDENT),
        Span::styled(status.mark(look.spin), Style::new().fg(mark_color)),
        Span::raw(" "),
    ];
    let marked_style = name_style
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let name = fit(&session.name, room);
    spans.extend(marked_spans(&name, marked, name_style, marked_style));
    if !label.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            label.to_string(),
            Style::new().fg(theme.muted),
        ));
    }
    if !when.is_empty() {
        let used: usize = spans.iter().skip(3).map(Span::width).sum();
        let gap = room.saturating_sub(used + when.chars().count());
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(when.to_string(), Style::new().fg(theme.muted)));
    }
    Line::from(spans)
}

/// What's in front in the session, in a word, when its name doesn't say
/// already: a session called `refund-fix` with Claude Code in front shows
/// `claude`, and one called `claude-2` shows nothing more.
fn front_label(session: &SessionInfo) -> Option<&str> {
    let word = session.front.as_ref()?.word();
    let named = session.name.to_lowercase().contains(&word.to_lowercase());
    if named { None } else { Some(word) }
}

/// Which of a row's extras fit beside a name `name_width` wide in `room`
/// columns, a space before each: what's in front and the time, the time
/// alone, or neither.
fn fitting_extras<'b>(
    name_width: usize,
    label: &'b str,
    when: &'b str,
    room: usize,
) -> (&'b str, &'b str) {
    let width = |text: &str| {
        if text.is_empty() {
            0
        } else {
            1 + text.chars().count()
        }
    };
    if name_width + width(label) + width(when) <= room {
        (label, when)
    } else if name_width + width(when) <= room {
        ("", when)
    } else {
        ("", "")
    }
}

/// `text` as spans: the characters at `marked` in `marked_style`, the rest
/// in `style`.
fn marked_spans<'a>(
    text: &str,
    marked: &[usize],
    style: Style,
    marked_style: Style,
) -> Vec<Span<'a>> {
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut run_marked = false;
    for (place, c) in text.chars().enumerate() {
        let is_marked = marked.contains(&place);
        if is_marked != run_marked && !run.is_empty() {
            let style = if run_marked { marked_style } else { style };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_marked = is_marked;
        run.push(c);
    }
    if !run.is_empty() {
        let style = if run_marked { marked_style } else { style };
        spans.push(Span::styled(run, style));
    }
    spans
}

/// How long ago the session changed, or nothing from a daemon that
/// doesn't say.
fn changed_ago(session: &SessionInfo, now: u64) -> String {
    if session.changed == 0 {
        return String::new();
    }
    ago(session.changed, now)
}

/// How long ago `then` was, at `now`, both in seconds since the Unix
/// epoch, in as few characters as it takes: "now", "45s", "12m", "3h",
/// "2d".
pub fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    let minute = 60;
    let hour = 60 * minute;
    let day = 24 * hour;
    if seconds < 10 {
        "now".to_string()
    } else if seconds < minute {
        format!("{seconds}s")
    } else if seconds < hour {
        format!("{}m", seconds / minute)
    } else if seconds < day {
        format!("{}h", seconds / hour)
    } else {
        format!("{}d", seconds / day)
    }
}

/// `text` cut down to `width` characters, ending in `…` when it's cut.
pub fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_in_the_last_ten_seconds_is_now() {
        assert_eq!(ago(1000, 1000), "now");
        assert_eq!(ago(1000, 1009), "now");
    }

    #[test]
    fn older_changes_are_counted_in_the_largest_whole_unit() {
        assert_eq!(ago(0, 45), "45s");
        assert_eq!(ago(0, 12 * 60 + 30), "12m");
        assert_eq!(ago(0, 3 * 3600 + 59 * 60), "3h");
        assert_eq!(ago(0, 2 * 86400 + 5), "2d");
    }

    #[test]
    fn a_clock_that_went_back_says_now() {
        assert_eq!(ago(2000, 1000), "now");
    }

    #[test]
    fn marked_letters_get_a_span_of_their_own() {
        let plain = Style::new();
        let marked = Style::new().add_modifier(Modifier::BOLD);
        let spans = marked_spans("refund-fix", &[0, 7, 8, 9], plain, marked);
        let texts: Vec<&str> = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(texts, ["r", "efund-", "fix"]);
        assert_eq!(spans[0].style, marked);
        assert_eq!(spans[1].style, plain);
    }

    #[test]
    fn what_s_in_front_shows_when_the_name_doesn_t_say_it() {
        let mut session = crate::protocol::SessionInfo {
            front: Some(Front::Agent {
                program: "claude".into(),
                name: "Claude Code".into(),
            }),
            name: "refund-fix".into(),
            id: "1".into(),
            command: vec!["claude".into()],
            cwd: "/".into(),
            pid: None,
            state: crate::protocol::State::Running,
            activity: None,
            worktree: None,
            changed: 0,
            task: None,
        };
        assert_eq!(front_label(&session), Some("claude"));
        session.name = "Claude-2".into();
        assert_eq!(front_label(&session), None);
        session.front = None;
        assert_eq!(front_label(&session), None);
    }

    #[test]
    fn short_of_room_what_s_in_front_goes_before_the_time() {
        // "refund-fix" is 10 wide; " claude" 7 and " 12m" 4 more.
        assert_eq!(fitting_extras(10, "claude", "12m", 21), ("claude", "12m"));
        assert_eq!(fitting_extras(10, "claude", "12m", 20), ("", "12m"));
        assert_eq!(fitting_extras(10, "claude", "12m", 13), ("", ""));
    }

    #[test]
    fn text_too_long_is_cut_with_an_ellipsis() {
        assert_eq!(fit("planner", 10), "planner");
        assert_eq!(fit("a-long-session-name", 8), "a-long-…");
        assert_eq!(fit("abc", 0), "");
    }
}
