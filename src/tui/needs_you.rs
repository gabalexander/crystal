//! The needs-you view, `U` in the sidebar: everything waiting on the user
//! right now, in every tab, the most urgent first, so that a question is
//! answered without first finding the session it's in. A background task
//! asking for a permission is answered where it stands with `y`, `n` or
//! `Y`, a flow run at its gate with `g` (go on) or `f` (send it back), the
//! same keys as on the session's own row; Enter goes to the session.
//!
//! The rows come from what the TUI already has, the sessions and the flow
//! runs, so the view never waits: the app reads them again with each fresh
//! list, and the bar stays on the thing it was on, by what it's about, as
//! answered rows go and new ones come.

use super::listing;
use super::sidebar::{ago, fit};
use super::status::Status;
use super::theme::Theme;
use crate::flow_run::{FlowRun, RunState, StepState};
use crate::protocol::{Activity, Answer, SessionInfo, State};
use crate::shell;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};
use std::collections::HashSet;

/// How urgent a row is, the most urgent first. All but the last stop a
/// session or a run until the user answers; a finished turn only waits to
/// be seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// A background task's Claude asks for a permission, mid-turn.
    Permission,
    /// A flow run stopped at the gate after a step.
    Gate,
    /// A task's agent ended its turn with the task still open: it's asking
    /// something.
    Task,
    /// An agent asks the user something, like whether to run a command.
    Question,
    /// An agent finished a turn nobody has looked at.
    Done,
}

impl Tier {
    /// Whether a session or a run waits on the answer.
    pub fn blocking(self) -> bool {
        self != Tier::Done
    }
}

/// What a row is about: one row a thing, so the thing holds the bar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum About {
    /// The session with this id.
    Session(String),
    /// The flow run with this name.
    Run(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub tier: Tier,
    pub about: About,
    /// The session Enter goes to, by name: a gate's is its step's, when it
    /// has one.
    pub session: Option<String>,
    /// The session's name, or the run's and its step's: `ship-1 · review`.
    pub name: String,
    /// Where it runs: `app ⎇ fix/login`, or its directory.
    pub place: String,
    /// What it waits for, in a few words.
    pub what: String,
    /// All of it, for the reading pane: the command asked about, the step's
    /// answer, the task.
    pub detail: String,
    /// When it started waiting, in seconds since the Unix epoch, 0 when
    /// that isn't known.
    pub since: u64,
}

/// Everything waiting on the user, the most urgent first, and in a tier,
/// whatever has waited longest. A thing is one row, in its most urgent
/// tier: a task asking for a permission isn't listed again as waiting, and
/// a gate's step isn't listed again as done. Only running sessions count.
pub fn rows(sessions: &[SessionInfo], runs: &[FlowRun]) -> Vec<Row> {
    let running: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|session| session.state == State::Running)
        .collect();
    let mut rows = Vec::new();
    let mut covered: HashSet<&str> = HashSet::new();
    for session in &running {
        if let Some(asking) = &session.asking {
            covered.insert(&session.name);
            let what = format!("{} {}", asking.tool, asking.gist);
            rows.push(session_row(session, Tier::Permission, what.clone(), what));
        }
    }
    for run in runs.iter().filter(|run| run.state() == RunState::AtGate) {
        let Some(step) = run.steps.iter().position(|s| s.state == StepState::AtGate) else {
            continue;
        };
        let session = run.steps[step]
            .session
            .as_deref()
            .and_then(|name| running.iter().find(|session| session.name == name));
        if let Some(session) = session {
            covered.insert(&session.name);
        }
        let name = run.step_name(step);
        rows.push(Row {
            tier: Tier::Gate,
            about: About::Run(run.name.clone()),
            session: session.map(|session| session.name.clone()),
            name: format!("{} · {name}", run.name),
            place: session.map_or_else(|| shell::home_relative(&run.cwd), |s| place(s)),
            what: format!("gate after {name}"),
            detail: run.steps[step].answer.clone().unwrap_or_default(),
            since: session.map_or(run.started, |session| session.changed),
        });
    }
    for session in running {
        if covered.contains(session.name.as_str()) {
            continue;
        }
        let task = session.task.as_ref().filter(|task| task.is_open());
        // An agent that reports its own state may say what it waits for.
        let said = session.reporter.as_ref().and_then(|r| r.message.as_deref());
        let (tier, what) = match session.activity {
            _ if task.is_some_and(|task| task.waiting) => (Tier::Task, "its task waits on you"),
            Some(Activity::Waiting) => (Tier::Question, said.unwrap_or("asking you something")),
            Some(Activity::Done) => (Tier::Done, "finished its turn"),
            _ => continue,
        };
        let command = || {
            let words: Vec<String> = session.command.iter().map(|a| shell::quote(a)).collect();
            words.join(" ")
        };
        let goal = task.map(|task| task.goal.clone());
        let detail = goal.or(said.map(String::from)).unwrap_or_else(command);
        rows.push(session_row(session, tier, what.to_string(), detail));
    }
    rows.sort_by(|a, b| {
        (a.tier, a.since)
            .cmp(&(b.tier, b.since))
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

fn session_row(session: &SessionInfo, tier: Tier, what: String, detail: String) -> Row {
    Row {
        tier,
        about: About::Session(session.id.clone()),
        session: Some(session.name.clone()),
        name: session.name.clone(),
        place: place(session),
        what,
        detail,
        since: session.changed,
    }
}

/// Where a session runs: its project and branch, or its directory.
fn place(session: &SessionInfo) -> String {
    match &session.worktree {
        Some(worktree) => {
            let mark = if worktree.main { "⌂" } else { "⎇" };
            let branch = worktree.branch.as_deref().unwrap_or("(detached)");
            format!("{} {mark} {branch}", worktree.project)
        }
        None => shell::home_relative(&session.cwd),
    }
}

/// What a key in the view asks for beyond it.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Close,
    /// Go to the session with this name.
    Go(String),
    /// Answer the permission the session with this name asks for.
    Answer {
        name: String,
        answer: Answer,
    },
    /// Go on past the gate of the flow run with this name.
    GoOn(String),
    /// Send the flow run with this name back from its gate, with notes.
    SendBack(String),
    /// Say why the key did nothing.
    Say(String),
}

pub struct NeedsYouView {
    rows: Vec<Row>,
    /// Where the bar is among the rows.
    at: usize,
}

impl NeedsYouView {
    pub fn new(rows: Vec<Row>) -> NeedsYouView {
        NeedsYouView { rows, at: 0 }
    }

    pub fn highlighted(&self) -> Option<&Row> {
        self.rows.get(self.at)
    }

    /// Takes the rows as they are now. The bar stays on the thing it was
    /// on; when that has gone, answered say, it stays where it was, on
    /// whatever took its place.
    pub fn refresh(&mut self, rows: Vec<Row>) {
        let on = self.highlighted().map(|row| row.about.clone());
        self.rows = rows;
        let found = on.and_then(|on| self.rows.iter().position(|row| row.about == on));
        self.at = found
            .unwrap_or(self.at)
            .min(self.rows.len().saturating_sub(1));
    }

    /// `j`/`k` and the arrows move; Enter goes to the row's session; `y`,
    /// `n` and `Y` answer a permission, `g` and `f` a gate; Esc or `q`
    /// closes.
    pub fn on_key(&mut self, key: &KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Step::Close,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            _ => {}
        }
        let Some(row) = self.highlighted() else {
            return Step::Stay;
        };
        match (key.code, &row.about) {
            (KeyCode::Enter, _) => match &row.session {
                Some(name) => Step::Go(name.clone()),
                None => Step::Say(format!("{} has no session to go to yet", row.name)),
            },
            (KeyCode::Char(letter @ ('y' | 'n' | 'Y')), _) if !ctrl => {
                match (row.tier, &row.session) {
                    (Tier::Permission, Some(name)) => Step::Answer {
                        name: name.clone(),
                        answer: answer_of(letter),
                    },
                    _ => Step::Say(format!("{} isn't asking for a permission", row.name)),
                }
            }
            (KeyCode::Char('g'), About::Run(run)) => Step::GoOn(run.clone()),
            (KeyCode::Char('f'), About::Run(run)) => Step::SendBack(run.clone()),
            (KeyCode::Char('g' | 'f'), _) => {
                Step::Say(format!("{} isn't waiting at a gate", row.name))
            }
            _ => Step::Stay,
        }
    }

    fn move_by(&mut self, by: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.at = self.at.saturating_add_signed(by).min(last);
    }
}

fn answer_of(letter: char) -> Answer {
    match letter {
        'y' => Answer::Allow,
        'Y' => Answer::Always,
        _ => Answer::Deny,
    }
}

/// How many rows hold up a session or a run until the user answers.
pub fn blocking(rows: &[Row]) -> usize {
    rows.iter().filter(|row| row.tier.blocking()).count()
}

/// The keys the footer offers, for the row the bar is on.
pub fn hints(view: &NeedsYouView) -> &'static [(&'static str, &'static str)] {
    match view.highlighted().map(|row| row.tier) {
        Some(Tier::Permission) => &[
            ("y", "allow"),
            ("n", "deny"),
            ("Y", "always"),
            ("enter", "go to it"),
            ("esc", "close"),
        ],
        Some(Tier::Gate) => &[
            ("g", "go on"),
            ("f", "send back"),
            ("enter", "go to it"),
            ("esc", "close"),
        ],
        _ => &[("enter", "go to it"), ("j/k", "move"), ("esc", "close")],
    }
}

/// Draws the view in `area`: a heading, a row each thing, and what the bar
/// is on, said whole. `now` is seconds since the Unix epoch.
pub fn draw(frame: &mut Frame, view: &NeedsYouView, theme: &Theme, now: u64, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(theme.base()), area);
    let [heading, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let count = blocking(&view.rows);
    let heading_line = Line::from(vec![
        Span::styled(
            " needs you",
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {count} waiting on you"),
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(heading_line, heading);
    if view.rows.is_empty() {
        return listing::draw_note(frame, theme, "nothing needs you", rest);
    }
    let [list, rule, reading] = listing::list_areas(rest, view.rows.len());
    let first = listing::first_drawn(view.at, list.height);
    let shown = view.rows.iter().enumerate().skip(first);
    for (index, row) in shown.take(usize::from(list.height)) {
        let highlighted = index == view.at;
        let area = listing::row_area(frame, theme, list, index - first, highlighted);
        frame.render_widget(row_line(row, theme, highlighted, area.width), area);
        let since = (row.since > 0).then(|| ago(row.since, now));
        if let Some(since) = since {
            let since = Line::styled(format!("{since} "), Style::new().fg(theme.muted));
            frame.render_widget(since.right_aligned(), area);
        }
    }
    listing::draw_rule(frame, theme, rule);
    if let Some(row) = view.highlighted() {
        listing::draw_reading(frame, reading_lines(row, theme), 0, reading);
    }
}

/// The mark for a tier, in its color.
fn mark(tier: Tier, theme: &Theme) -> (&'static str, Color) {
    match tier {
        Tier::Permission => ("⚠", theme.waiting),
        Tier::Gate | Tier::Task | Tier::Question => (Status::Waiting.mark(0), theme.waiting),
        Tier::Done => (Status::Done.mark(0), theme.done),
    }
}

/// A row: its mark, its name, where it runs and what it waits for; how
/// long it has goes on the right.
fn row_line<'a>(row: &Row, theme: &Theme, highlighted: bool, width: u16) -> Line<'a> {
    const NAME: usize = 20;
    const PLACE: usize = 24;
    let (mark, color) = mark(row.tier, theme);
    let mut name = Style::new().fg(theme.text);
    if highlighted {
        name = name.add_modifier(Modifier::BOLD);
    }
    // The mark and its spaces before, the time after.
    let room = usize::from(width).saturating_sub(4 + 5);
    let place_room = PLACE.min(room.saturating_sub(NAME + 1));
    let what_room = room.saturating_sub(NAME + 1 + place_room + 1);
    Line::from(vec![
        Span::styled(format!(" {mark} "), Style::new().fg(color)),
        Span::styled(format!("{:<NAME$} ", fit(&row.name, NAME)), name),
        Span::styled(
            format!("{:<place_room$} ", fit(&row.place, place_room)),
            Style::new().fg(theme.branch),
        ),
        Span::styled(fit(&row.what, what_room), Style::new().fg(theme.text)),
    ])
}

/// What the bar is on, said whole, and how to answer it.
fn reading_lines<'a>(row: &Row, theme: &Theme) -> Vec<Line<'a>> {
    let how = match row.tier {
        Tier::Permission => format!("{} asks to use {}", row.name, row.what),
        Tier::Gate => format!("{} waits at its gate", row.name),
        Tier::Task => format!("{}'s task waits on you", row.name),
        Tier::Question => format!("{} is asking you something", row.name),
        Tier::Done => format!("{} finished its turn", row.name),
    };
    let mut lines = vec![Line::styled(
        format!(" {how}"),
        Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
    )];
    if row.tier != Tier::Permission && !row.detail.trim().is_empty() {
        lines.extend(listing::text_lines(
            &row.detail,
            Style::new().fg(theme.text),
        ));
    }
    let answer = match row.tier {
        Tier::Permission => "y allows it, n denies it, Y allows calls like it from now on",
        Tier::Gate => "g goes on to the next step; f sends the run back, with notes",
        _ => "enter goes to it",
    };
    lines.push(Line::default());
    lines.push(Line::styled(
        format!(" {answer}"),
        Style::new().fg(theme.muted),
    ));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::{Flow, Step as FlowStep};
    use crate::protocol::{Asking, TaskInfo, Worktree};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn session(name: &str, activity: Option<Activity>, changed: u64) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            id: format!("id-{name}"),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/code/app"),
            pid: Some(1),
            state: State::Running,
            activity,
            worktree: Some(Worktree {
                project: "app".into(),
                project_path: "/code/app".into(),
                path: "/code/app".into(),
                main: true,
                branch: Some("main".into()),
            }),
            changed,
            front: None,
            task: None,
            asking: None,
            reporter: None,
        }
    }

    fn asking(name: &str, changed: u64) -> SessionInfo {
        SessionInfo {
            asking: Some(Asking {
                tool: "Bash".into(),
                gist: "cargo test".into(),
            }),
            ..session(name, Some(Activity::Waiting), changed)
        }
    }

    fn task_waiting(name: &str, changed: u64) -> SessionInfo {
        SessionInfo {
            task: Some(TaskInfo {
                id: Some(1),
                goal: "Fix the login".into(),
                background: false,
                backlog: None,
                waiting: true,
                created: 1,
                outcome: None,
            }),
            ..session(name, Some(Activity::Waiting), changed)
        }
    }

    /// A run of a two-step flow stopped at the gate after its first step,
    /// which ran in the session called `step_session`.
    fn at_gate(step_session: &str) -> FlowRun {
        let step = |name: &str| FlowStep {
            name: name.into(),
            profile: None,
            prompt: "{goal}".into(),
            placement: None,
            worktree: false,
            gate: true,
            back_to: None,
            max_rounds: None,
        };
        let flow = Flow {
            name: "ship".into(),
            description: None,
            steps: vec![step("plan"), step("build")],
        };
        let mut run = FlowRun::new(
            "ship-1".into(),
            flow,
            &[],
            "add retries".into(),
            PathBuf::from("/code/app"),
            BTreeMap::new(),
            5,
        );
        run.steps[0].state = StepState::AtGate;
        run.steps[0].session = Some(step_session.into());
        run.steps[0].answer = Some("The plan: three steps".into());
        run
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    fn press(view: &mut NeedsYouView, code: KeyCode) -> Step {
        view.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn the_most_urgent_come_first_and_the_longest_waiting_first_among_them() {
        let sessions = vec![
            session("finished", Some(Activity::Done), 10),
            session("asks", Some(Activity::Waiting), 20),
            task_waiting("tasked", 30),
            asking("late", 50),
            asking("early", 40),
            session("busy", Some(Activity::Working), 1),
            session("plan-step", Some(Activity::Done), 60),
        ];
        let rows = rows(&sessions, &[at_gate("plan-step")]);
        assert_eq!(
            names(&rows),
            [
                "early",
                "late",
                "ship-1 · plan",
                "tasked",
                "asks",
                "finished"
            ]
        );
        assert_eq!(rows[0].what, "Bash cargo test");
        assert_eq!(rows[2].session.as_deref(), Some("plan-step"));
        assert_eq!(rows[2].detail, "The plan: three steps");
        assert_eq!(rows[3].detail, "Fix the login");
        assert_eq!(blocking(&rows), 5);
    }

    #[test]
    fn an_agent_reporting_its_own_state_says_what_it_waits_for() {
        let reported = SessionInfo {
            reporter: Some(crate::protocol::Reporter {
                agent: "pi".into(),
                message: Some("Which database, staging or prod?".into()),
                resume: None,
            }),
            ..session("pi", Some(Activity::Waiting), 1)
        };
        let rows = rows(
            &[reported, session("plain", Some(Activity::Waiting), 2)],
            &[],
        );
        assert_eq!(rows[0].what, "Which database, staging or prod?");
        assert_eq!(rows[0].detail, "Which database, staging or prod?");
        assert_eq!(rows[1].what, "asking you something");
    }

    #[test]
    fn a_session_that_has_ended_needs_nothing() {
        let ended = SessionInfo {
            state: State::Exited { code: 0 },
            ..asking("gone", 1)
        };
        assert!(rows(&[ended], &[]).is_empty());
    }

    #[test]
    fn a_permission_is_answered_where_it_stands_and_a_gate_with_its_own_keys() {
        let sessions = vec![asking("fixer", 1), session("plan-step", None, 2)];
        let mut view = NeedsYouView::new(rows(&sessions, &[at_gate("plan-step")]));
        let answered = |answer| Step::Answer {
            name: "fixer".into(),
            answer,
        };
        assert_eq!(
            press(&mut view, KeyCode::Char('y')),
            answered(Answer::Allow)
        );
        assert_eq!(press(&mut view, KeyCode::Char('n')), answered(Answer::Deny));
        assert_eq!(
            press(&mut view, KeyCode::Char('Y')),
            answered(Answer::Always)
        );
        let not_a_gate = press(&mut view, KeyCode::Char('g'));
        assert_eq!(
            not_a_gate,
            Step::Say("fixer isn't waiting at a gate".into())
        );

        assert_eq!(press(&mut view, KeyCode::Char('j')), Step::Stay);
        assert_eq!(
            press(&mut view, KeyCode::Char('g')),
            Step::GoOn("ship-1".into())
        );
        assert_eq!(
            press(&mut view, KeyCode::Char('f')),
            Step::SendBack("ship-1".into())
        );
        let not_asking = press(&mut view, KeyCode::Char('y'));
        assert_eq!(
            not_asking,
            Step::Say("ship-1 · plan isn't asking for a permission".into())
        );
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Step::Go("plan-step".into())
        );
        assert_eq!(press(&mut view, KeyCode::Esc), Step::Close);
    }

    #[test]
    fn the_bar_stays_on_its_row_as_others_come_and_go() {
        let first = vec![asking("a", 1), session("b", Some(Activity::Waiting), 2)];
        let mut view = NeedsYouView::new(rows(&first, &[]));
        press(&mut view, KeyCode::Down);
        assert_eq!(view.highlighted().unwrap().name, "b");

        // One more comes ahead of it: the bar stays on b.
        let more = vec![asking("z", 0), asking("a", 1), first[1].clone()];
        view.refresh(rows(&more, &[]));
        assert_eq!(view.highlighted().unwrap().name, "b");

        // b is answered and goes: the bar stays where it was, on what's left.
        view.refresh(rows(&more[..2], &[]));
        assert_eq!(view.highlighted().unwrap().name, "a");
        view.refresh(Vec::new());
        assert!(view.highlighted().is_none());
        assert_eq!(press(&mut view, KeyCode::Enter), Step::Stay);
    }
}
