//! The sidebar: every session under its project and its worktree, each
//! with a mark for what it's doing and how long ago that last changed, and
//! each flow run under its project, a row for each of its steps. A
//! worktree's agents come first; its terminals, the shells and other
//! programs, come after a line of their own and are drawn quieter, with a
//! prompt's chevron for a mark, so the two never look alike.

use super::app::{App, Hit};
use super::groups::{self, Row};
use super::status::Status;
use super::theme::Theme;
use super::ui::Look;
use crate::flow_run::{FlowRun, RunState, StepState};
use crate::forge::{PullRequest, PullRequestState};
use crate::protocol::{Front, SessionInfo, TaskState};
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

/// A terminal's mark while it runs: a prompt's chevron. No agent's status
/// uses it, so a terminal never passes for an agent at its prompt, even
/// without color.
const TERMINAL_MARK: &str = "❯";

pub fn draw(frame: &mut Frame, app: &App, look: &Look, area: Rect) {
    let rows = app.rows();
    if rows.is_empty() && app.filter().is_none() && !app.sessions().is_empty() {
        draw_empty_tab(frame, look.theme, area);
        return;
    }
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

/// The sidebar of a tab with no sessions in it, while other tabs have
/// some: how to start something there, or close it.
fn draw_empty_tab(frame: &mut Frame, theme: &Theme, area: Rect) {
    let muted = Style::new().fg(theme.muted);
    let key = Style::new().fg(theme.text);
    let lines = vec![
        Line::styled(" nothing in this tab yet", muted),
        Line::from(vec![
            Span::styled(" n", key),
            Span::styled(" starts a session here", muted),
        ]),
        Line::from(vec![
            Span::styled(" &", key),
            Span::styled(" closes the tab", muted),
        ]),
    ];
    let shown = lines.into_iter().take(area.height.into());
    for (offset, line) in shown.enumerate() {
        let line_area = Rect::new(area.x, area.y + offset as u16, area.width, 1);
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

/// The row the bar is on: the selected session's, or the worktree's with
/// no sessions the selection is on, or, while `/`'s filter is open, the
/// one its bar is on.
fn selected_row(app: &App) -> Option<usize> {
    let rows = app.rows();
    if app.filter().is_none()
        && let Some(worktree) = app.selected_empty_worktree()
    {
        let row = Row::NoSessions(worktree.path.clone());
        return rows.iter().position(|shown| *shown == row);
    }
    let index = app.sidebar_cursor()?;
    rows.iter().position(|row| *row == Row::Session(index))
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
            path,
            branch,
            main,
        } => worktree_line(app, project, path, branch.as_deref(), *main, theme, width),
        Row::Directory(dir) => {
            let dir = shell::home_relative(dir);
            let room = usize::from(width).saturating_sub(WORKTREE_INDENT.len());
            Line::from(vec![
                Span::raw(WORKTREE_INDENT),
                Span::styled(fit(&dir, room), Style::new().fg(theme.muted)),
            ])
        }
        Row::Session(index) => {
            let session = &app.sessions()[*index];
            if let Some((run, step)) = app.flow_step_of(*index) {
                return step_line(run, step, Some(session), look, width, selected);
            }
            let marked = app.marked_letters(*index);
            session_line(session, &marked, look, width, selected)
        }
        Row::Terminals => terminals_line(theme, width),
        Row::NoSessions(_) => no_sessions_line(theme, width, selected),
        Row::Task(index) => task_line(&app.sessions()[*index], theme, width),
        Row::Flow(run) => flow_heading(&app.flows()[*run], look, width),
        Row::Step { run, step } => step_line(&app.flows()[*run], *step, None, look, width, false),
    }
}

/// A flow run's heading, in line with the worktrees: a mark in the color
/// of how the run stands, the flow's name and the goal, and on the right
/// the round once it has been sent back. Short of room, the round goes
/// first, before the goal is cut.
fn flow_heading<'a>(run: &FlowRun, look: &Look, width: u16) -> Line<'a> {
    let theme = look.theme;
    let color = match run.state() {
        RunState::Running => theme.working,
        RunState::AtGate => theme.waiting,
        RunState::Done => theme.done,
        RunState::Failed | RunState::Interrupted => theme.failed,
    };
    // The indent and the mark before the name; a space at the end.
    let room = usize::from(width).saturating_sub(WORKTREE_INDENT.len() + 2 + 1);
    let name = fit(&run.flow.name, room);
    let goal_room = room.saturating_sub(name.chars().count() + 1);
    let round = if run.round > 1 {
        format!("round {}", run.round)
    } else {
        String::new()
    };
    let goal = run.goal.lines().next().unwrap_or("");
    let round_fits = !round.is_empty() && goal_room >= round.chars().count() + 6;
    let goal_room = if round_fits {
        goal_room - round.chars().count() - 1
    } else {
        goal_room
    };
    let goal = fit(goal, goal_room);
    let mut spans = vec![
        Span::raw(WORKTREE_INDENT),
        Span::styled("◇ ", Style::new().fg(color)),
        Span::styled(name, Style::new().fg(theme.branch)),
        Span::raw(" "),
        Span::styled(goal.clone(), Style::new().fg(theme.muted)),
    ];
    if round_fits {
        let gap = goal_room.saturating_sub(goal.chars().count()) + 1;
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(round, Style::new().fg(theme.muted)));
    }
    Line::from(spans)
}

/// A flow step's row: a mark for how it stands, its name, and, when it has
/// a session, how long ago that changed. A step with no session is muted.
fn step_line<'a>(
    run: &FlowRun,
    step: usize,
    session: Option<&SessionInfo>,
    look: &Look,
    width: u16,
    selected: bool,
) -> Line<'a> {
    let theme = look.theme;
    let (mark, color) = match run.steps[step].state {
        StepState::Pending => ("·", theme.muted),
        StepState::Running => (Status::Working.mark(look.spin), theme.working),
        StepState::AtGate => (Status::Waiting.mark(look.spin), theme.waiting),
        StepState::Done => ("✓", theme.done),
        StepState::Failed => ("✗", theme.failed),
        StepState::Interrupted => ("■", theme.failed),
    };
    let mut name_style = Style::new().fg(if session.is_some() {
        theme.text
    } else {
        theme.muted
    });
    if selected {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    // The indent, the mark and a space before the name; a space at the end.
    let room = usize::from(width).saturating_sub(SESSION_INDENT.len() + 2 + 1);
    let name = run.step_name(step);
    let when = session.map_or(String::new(), |session| changed_ago(session, look.now));
    let (_, when) = fitting_extras(name.chars().count(), "", &when, room);
    let name = fit(name, room);
    let gap = room.saturating_sub(name.chars().count() + when.chars().count());
    Line::from(vec![
        Span::raw(SESSION_INDENT),
        Span::styled(mark, Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(when.to_string(), Style::new().fg(theme.muted)),
    ])
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

/// The line between a worktree's agents and its terminals: `terminals`,
/// then a dotted rule to the edge, fainter than a heading's.
fn terminals_line<'a>(theme: &Theme, width: u16) -> Line<'a> {
    // A space kept clear at the end, as on a heading.
    let room = usize::from(width).saturating_sub(SESSION_INDENT.len() + 1);
    let label = fit("terminals", room);
    let rule = room.saturating_sub(label.chars().count() + 1);
    Line::from(vec![
        Span::raw(SESSION_INDENT),
        Span::styled(label, Style::new().fg(theme.muted)),
        Span::raw(" "),
        Span::styled("┄".repeat(rule), Style::new().fg(theme.rule)),
    ])
}

/// The row under a linked worktree with no sessions, where a session's
/// would be: quiet, since nothing runs there, but the selection can be on
/// it.
fn no_sessions_line<'a>(theme: &Theme, width: u16, selected: bool) -> Line<'a> {
    let mut style = Style::new().fg(theme.muted);
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    // The indent, the mark and a space before the words; a space at the end.
    let room = usize::from(width).saturating_sub(SESSION_INDENT.len() + 2 + 1);
    Line::from(vec![
        Span::raw(SESSION_INDENT),
        Span::styled("· ", Style::new().fg(theme.muted)),
        Span::styled(fit("no sessions", room), style),
    ])
}

/// The line under a session with a task: what it was asked to do while
/// it's open, `▲` when it waits on the user, or `⚠` and the permission a
/// background task asks for; once it's closed, how it went, marked done,
/// failed or cancelled.
fn task_line<'a>(session: &SessionInfo, theme: &Theme, width: u16) -> Line<'a> {
    let Some(task) = &session.task else {
        return Line::default();
    };
    // Under the session's name: its indent and mark.
    let indent = format!("{SESSION_INDENT}  ");
    let room = usize::from(width).saturating_sub(indent.len() + 1);
    let goal = task.goal.lines().next().unwrap_or("");
    let summary = task
        .outcome
        .as_ref()
        .map(|outcome| outcome.summary.as_str())
        .filter(|summary| !summary.is_empty())
        .unwrap_or(goal);
    let asked;
    let (mark, color, said) = match (&session.asking, task.state()) {
        (Some(asking), _) => {
            asked = format!("{} {}", asking.tool, asking.gist);
            ("⚠ ", theme.waiting, asked.as_str())
        }
        (None, TaskState::Waiting) => ("▲ ", theme.waiting, goal),
        (None, TaskState::Done) => ("✓ ", theme.done, summary),
        (None, TaskState::Failed) => ("✗ ", theme.failed, summary),
        (None, TaskState::Cancelled) => ("– ", theme.muted, summary),
        (None, TaskState::Running | TaskState::Pending) => ("", theme.muted, goal),
    };
    let room = room.saturating_sub(mark.chars().count());
    Line::from(vec![
        Span::raw(indent),
        Span::styled(mark, Style::new().fg(color)),
        Span::styled(fit(said, room), Style::new().fg(theme.muted)),
    ])
}

/// A worktree's line: its mark and branch, and on the right its pull
/// request when its forge knows of one, `#57` (`!57` on GitLab) and a mark
/// for what matters most about it, or `removing…` while git removes it. Short of room, the mark
/// goes first, then the number, before the branch is cut.
fn worktree_line<'a>(
    app: &App,
    project: &Path,
    path: &Path,
    branch: Option<&str>,
    main: bool,
    theme: &Theme,
    width: u16,
) -> Line<'a> {
    let mark = if main { "⌂ " } else { "⎇ " };
    let name = branch.unwrap_or("(detached)");
    // The indent and the mark before the branch; a space at the end.
    let room = usize::from(width).saturating_sub(WORKTREE_INDENT.len() + 2 + 1);
    let forms = if app.removing(path) {
        vec![vec![Span::styled(
            "removing…",
            Style::new().fg(theme.muted),
        )]]
    } else {
        let pull_request = branch.and_then(|branch| app.pull_request(project, branch));
        pull_request
            .map(|pull_request| pull_request_spans(pull_request, theme))
            .unwrap_or_default()
    };
    let right = forms
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
    let number = Span::styled(pull_request.label(), Style::new().fg(theme.muted));
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
    let (mark, mark_color) = session_mark(session, look);
    // A terminal's name is muted, so the agents stand out.
    let name_color = if groups::is_terminal(session) {
        theme.muted
    } else {
        theme.text
    };
    let mut name_style = Style::new().fg(name_color);
    if selected {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }
    // The indent, the mark and a space before the name; a space at the end.
    let room = usize::from(width).saturating_sub(SESSION_INDENT.len() + 2 + 1);
    let when = changed_ago(session, look.now);
    let label = front_label(session).unwrap_or_default();
    let (label, when) = fitting_extras(session.name.chars().count(), label, &when, room);

    let mut spans = vec![
        Span::raw(SESSION_INDENT),
        Span::styled(mark, Style::new().fg(mark_color)),
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

/// A session's mark, in its color, the same in its row and in its pane's
/// header. A running terminal has the chevron: muted at a shell's prompt,
/// in running's color with a program in front. An agent, or a terminal
/// that has ended, has its status's mark.
pub fn session_mark(session: &SessionInfo, look: &Look) -> (&'static str, Color) {
    let theme = look.theme;
    let status = Status::of(session);
    if status == Status::Running && groups::is_terminal(session) {
        let at_a_shell = matches!(session.front, Some(Front::Shell { .. }));
        let color = if at_a_shell {
            theme.muted
        } else {
            theme.running
        };
        return (TERMINAL_MARK, color);
    }
    (status.mark(look.spin), theme.status(status))
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

    /// A running session called `name`, with `front` in front.
    fn session(name: &str, front: Front) -> SessionInfo {
        SessionInfo {
            front: Some(front),
            name: name.into(),
            id: "1".into(),
            command: vec!["sh".into()],
            cwd: "/".into(),
            pid: None,
            state: crate::protocol::State::Running,
            activity: None,
            worktree: None,
            changed: 0,
            task: None,
            asking: None,
        }
    }

    /// What a session's task line says, after its indent.
    fn task_words(session: &SessionInfo) -> String {
        let theme = Theme::new(crate::config::ThemeName::Dark, false);
        let line = task_line(session, &theme, 60);
        let words: String = line.spans[1..].iter().map(|s| s.content.as_ref()).collect();
        words.trim_end().to_string()
    }

    #[test]
    fn a_task_line_says_how_the_task_stands() {
        use crate::protocol::{Asking, TaskInfo, TaskOutcome};
        let mut fixer = session("fixer", Front::Task);
        fixer.task = Some(TaskInfo {
            id: Some(1),
            goal: "fix the tests".into(),
            background: true,
            backlog: None,
            waiting: false,
            created: 0,
            outcome: None,
        });
        assert_eq!(task_words(&fixer), "fix the tests");
        fixer.task.as_mut().unwrap().waiting = true;
        assert_eq!(task_words(&fixer), "▲ fix the tests");
        fixer.asking = Some(Asking {
            tool: "Bash".into(),
            gist: "cargo test".into(),
        });
        assert_eq!(task_words(&fixer), "⚠ Bash cargo test");
        fixer.asking = None;
        let cancelled = TaskOutcome::new(TaskState::Cancelled, "", 1);
        fixer.task.as_mut().unwrap().outcome = Some(cancelled);
        assert_eq!(task_words(&fixer), "– fix the tests");
    }

    fn claude() -> Front {
        Front::Agent {
            program: "claude".into(),
            name: "Claude Code".into(),
        }
    }

    #[test]
    fn a_running_terminal_has_the_chevron_and_an_agent_its_status() {
        let theme = Theme::new(crate::config::ThemeName::Dark, false);
        let look = Look {
            theme: &theme,
            now: 0,
            spin: 0,
        };
        let zsh = session("zsh-2", Front::Shell { name: "zsh".into() });
        let vite = session(
            "server",
            Front::Program {
                name: "vite".into(),
            },
        );
        let agent = session("claude", claude());
        assert_eq!(session_mark(&zsh, &look), ("❯", theme.muted));
        assert_eq!(session_mark(&vite, &look), ("❯", theme.running));
        assert_eq!(session_mark(&agent, &look), ("▸", theme.running));
        // Once it has ended, a terminal says how, the way an agent does.
        let ended = SessionInfo {
            state: crate::protocol::State::Exited { code: 1 },
            ..zsh
        };
        assert_eq!(session_mark(&ended, &look), ("■", theme.failed));
    }

    #[test]
    fn a_terminal_s_name_is_muted_and_an_agent_s_is_not() {
        let theme = Theme::new(crate::config::ThemeName::Dark, false);
        let look = Look {
            theme: &theme,
            now: 0,
            spin: 0,
        };
        let name_color = |session: &SessionInfo| {
            let line = session_line(session, &[], &look, 28, false);
            // The indent, the mark and a space, then the name.
            line.spans[3].style.fg
        };
        let zsh = session("zsh-2", Front::Shell { name: "zsh".into() });
        assert_eq!(name_color(&zsh), Some(theme.muted));
        assert_eq!(name_color(&session("claude", claude())), Some(theme.text));
    }

    #[test]
    fn a_worktree_git_is_removing_says_so_on_its_line() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let theme = Theme::new(crate::config::ThemeName::Dark, false);
        let project = Path::new("/code/app");
        let path = Path::new("/code/app.worktrees/old");
        // An agent that has ended in the worktree, which `W` removes.
        let ended = SessionInfo {
            state: crate::protocol::State::Exited { code: 0 },
            worktree: Some(crate::protocol::Worktree {
                project: "app".into(),
                project_path: project.into(),
                path: path.into(),
                main: false,
                branch: Some("old".into()),
            }),
            ..session("fixer", claude())
        };
        let mut app = App::new(None);
        app.set_sessions(vec![ended]);
        let line = |app: &App| worktree_line(app, project, path, Some("old"), false, &theme, 28);
        assert_eq!(line(&app).to_string().trim_end(), "   ⎇ old");

        for key in ['W', 'y'] {
            app.on_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
        }
        assert!(app.removing(path));
        let removing = line(&app);
        assert_eq!(removing.width(), 27);
        assert!(removing.to_string().ends_with("old          removing…"));
    }

    #[test]
    fn the_line_before_the_terminals_fills_the_row() {
        let theme = Theme::new(crate::config::ThemeName::Dark, false);
        let line = terminals_line(&theme, 28);
        assert_eq!(line.width(), 27);
        assert!(line.to_string().starts_with("     terminals ┄┄"));
    }

    #[test]
    fn what_s_in_front_shows_when_the_name_doesn_t_say_it() {
        let mut session = session("refund-fix", claude());
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
