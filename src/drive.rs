//! `crystal send`, `wait`, `read`, `result`, `answer` and `interrupt`: how
//! one agent drives another, or a script drives an agent. They work from
//! inside a session as well, since every session knows its daemon's socket.
//!
//! A wait listens to the daemon's events about its session rather than
//! asking again and again: each one is a reason to look again. A wait that
//! gives up is a [`TimedOut`], which `crystal` exits 2 for, so a script can
//! tell "not yet" from anything else going wrong.

use crate::client::{self, Subscription};
use crate::env;
use crate::events::{Filter, Kind};
use crate::output::{out, outln};
use crate::printable;
use crate::protocol::{Activity, Answer, Front, Request, Response, SessionInfo, State};
use crate::shell;
use anyhow::{Context, Result, bail, ensure};
use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long `send --wait` gives an agent to start on what it was sent. A
/// turn can also be over before it's seen starting, so after this the wait
/// goes on to the end of the turn either way.
const START_GRACE: Duration = Duration::from_secs(5);

/// How long `send --interrupt` waits for the run it stopped to end.
const STOP_GRACE: Duration = Duration::from_secs(30);

/// A wait that gave up before what it waited for came: `crystal` exits 2
/// for it, and 1 for anything else, a mistyped flag included, so 2 always
/// means "not yet".
#[derive(Debug)]
pub struct TimedOut(pub String);

impl fmt::Display for TimedOut {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TimedOut {}

/// What `crystal send` reads its text from when it's given as `-`.
pub const FROM_STDIN: &str = "-";

/// Types `text` into the session called `name`, and presses Enter after it
/// when `enter` is set; with `force`, even while its agent is asking the
/// user something. Run in a session, the message says it comes from that
/// session. With `interrupt`, a background task's run is stopped first,
/// and the text carries on from there.
pub fn send(
    socket: &Path,
    name: &str,
    text: &str,
    enter: bool,
    force: bool,
    interrupt: bool,
) -> Result<()> {
    if interrupt {
        stop_run(socket, name)?;
    }
    let request = Request::Send {
        name: name.to_string(),
        text: text.to_string(),
        enter,
        from: env::own_session_id(socket),
        force,
    };
    ask(socket, &request)?;
    Ok(())
}

/// Stops the run the background task `name` is in the middle of, if it's
/// in one, and waits for it to end: a task takes a follow-up only between
/// runs. A terminal's agent is stopped by its own key, which crystal doesn't
/// press for it.
fn stop_run(socket: &Path, name: &str) -> Result<()> {
    let mut watch = Watch::start(socket, name)?;
    let now = watch.now()?;
    ensure!(
        now.front == Some(Front::Task),
        "--interrupt stops a background task's run, and {name} isn't a task: an agent in a \
         terminal is stopped by its own key, like `crystal send-keys {name} Escape`"
    );
    if now.activity != Some(Activity::Working) {
        return Ok(());
    }
    interrupt(socket, name)?;
    match watch.settle(deadline(Some(STOP_GRACE)))? {
        Some(_) => Ok(()),
        None => bail!(
            "{name} was still working {}s after it was interrupted",
            STOP_GRACE.as_secs()
        ),
    }
}

/// Reads what `crystal send -` sends from standard input, to its end.
pub fn read_stdin() -> Result<String> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
        .context("couldn't read the text from standard input")?;
    Ok(text)
}

/// Presses `keys` in the session called `name`: key names like `Enter`,
/// or text, typed as keys rather than pasted. That's what answering an
/// agent's question takes, since agents ignore a pasted answer.
pub fn send_keys(socket: &Path, name: &str, keys: Vec<String>) -> Result<()> {
    let request = Request::SendKeys {
        name: name.to_string(),
        keys,
    };
    ask(socket, &request)?;
    Ok(())
}

/// Answers the permission the background task `task` names, by its number
/// or its session's name, is waiting on: with a denial, Claude is told
/// `message`.
pub fn answer(socket: &Path, task: &str, answer: Answer, message: Option<String>) -> Result<()> {
    let request = Request::Answer {
        task: task.to_string(),
        answer,
        message,
    };
    ask(socket, &request)?;
    Ok(())
}

/// Stops the run the background task `task` names is in the middle of.
pub fn interrupt(socket: &Path, task: &str) -> Result<()> {
    let request = Request::Interrupt {
        task: task.to_string(),
    };
    ask(socket, &request)?;
    Ok(())
}

/// What `wait --until` waits for: what a session's agent comes to do, or
/// its program's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Until {
    Working,
    Waiting,
    Done,
    Idle,
    #[value(alias = "exited")]
    Ended,
}

impl Until {
    fn word(self) -> &'static str {
        match self {
            Until::Working => "working",
            Until::Waiting => "waiting",
            Until::Done => "done",
            Until::Idle => "idle",
            Until::Ended => "ended",
        }
    }

    /// Where `session` has got to now, if it's anywhere a wait can be for.
    fn of_session(session: &SessionInfo) -> Option<Until> {
        // One waiting its turn to start again after a restart is on its
        // way, under the same id.
        if session.state == State::Starting {
            return None;
        }
        if session.state != State::Running {
            return Some(Until::Ended);
        }
        Some(match session.activity? {
            Activity::Working => Until::Working,
            Activity::Waiting => Until::Waiting,
            Activity::Done => Until::Done,
            Activity::Idle => Until::Idle,
        })
    }

    /// Where an event of `kind` says its session has got to.
    fn of_event(kind: Kind) -> Option<Until> {
        match kind {
            Kind::SessionWorking => Some(Until::Working),
            Kind::SessionWaiting => Some(Until::Waiting),
            Kind::SessionDone => Some(Until::Done),
            Kind::SessionIdle => Some(Until::Idle),
            Kind::SessionEnded | Kind::SessionStartFailed => Some(Until::Ended),
            _ => None,
        }
    }
}

/// Waits until the session's agent isn't working, or its program has
/// ended, and prints which, unless `quiet`. Gives up after `timeout`, if
/// there is one.
pub fn wait(socket: &Path, name: &str, timeout: Option<Duration>, quiet: bool) -> Result<()> {
    let mut watch = Watch::start(socket, name)?;
    match watch.settle(deadline(timeout))? {
        Some(settled) => say(&settled, quiet),
        None => timed_out(name, timeout),
    }
}

/// Waits until the session's agent comes to do one of `until`, or, with
/// `Ended` among them, its program ends, and prints which, unless `quiet`.
/// Its program ending first is an error. Gives up after `timeout`, if
/// there is one.
pub fn wait_until(
    socket: &Path,
    name: &str,
    until: &[Until],
    timeout: Option<Duration>,
    quiet: bool,
) -> Result<()> {
    let mut watch = Watch::start(socket, name)?;
    match watch.reach(until, deadline(timeout))? {
        Some(status) => say(&status, quiet),
        None => {
            let seconds = timeout.unwrap_or_default().as_secs_f64();
            let words = words(until);
            Err(TimedOut(format!("{name} wasn't {words} after {seconds}s")).into())
        }
    }
}

/// How many times a wait for output asks again, for what's left of its
/// time, when the daemon hangs up on it: a daemon handed over to a new
/// crystal does, once.
const ASK_AGAIN: usize = 3;

/// Waits until a row on the session's screen, or just scrolled off it,
/// matches the regular expression `pattern`, and prints the row, unless
/// `quiet`. The daemon looks each time the program writes something.
pub fn wait_for_output(
    socket: &Path,
    name: &str,
    pattern: &str,
    timeout: Option<Duration>,
    quiet: bool,
) -> Result<()> {
    let deadline = deadline(timeout);
    let mut left = timeout;
    let mut asked = 0;
    let response = loop {
        let request = Request::WaitOutput {
            name: name.to_string(),
            pattern: pattern.to_string(),
            timeout_ms: left.map(|left| left.as_millis() as u64),
        };
        asked += 1;
        match ask(socket, &request) {
            Err(err) if err.is::<client::HungUp>() && asked <= ASK_AGAIN => {
                left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            }
            response => break response?,
        }
    };
    let line = match response {
        Response::Matched { line } => line,
        Response::TimedOut { message } => return Err(TimedOut(message).into()),
        _ => bail!("the daemon didn't say what matched"),
    };
    say(&line, quiet)
}

/// Waits for the turn that a `send` has just started: first for the agent
/// to start working, then as [`wait`] does. Right after the send the agent
/// may not have started yet, and whatever it said about the turn before
/// would end the wait at once.
pub fn wait_for_turn(
    socket: &Path,
    name: &str,
    timeout: Option<Duration>,
    quiet: bool,
) -> Result<()> {
    match turn_settled(socket, name, timeout)? {
        Some(settled) => say(&settled, quiet),
        None => timed_out(name, timeout),
    }
}

/// Waits for the run a background task has just started, as
/// [`wait_for_turn`] does, and prints how it ended: `done` or `failed`, as
/// Claude's answer says, or else what the task came to first, like
/// `waiting` for a permission, or how its `claude` ended.
pub fn wait_for_run(socket: &Path, name: &str, timeout: Option<Duration>) -> Result<()> {
    let Some(settled) = turn_settled(socket, name, timeout)? else {
        return timed_out(name, timeout);
    };
    let answered = matches!(settled.as_str(), "done" | "idle");
    let result = Request::Result {
        name: name.to_string(),
    };
    let said = match ask(socket, &result) {
        Ok(Response::Result(result)) if answered && result.failed => "failed".to_string(),
        Ok(Response::Result(_)) if answered => "done".to_string(),
        _ => settled,
    };
    outln!("{said}")?;
    Ok(())
}

/// Waits for the turn that a `send` has just started to end, and gives
/// back what the session settled on, or `None` after `timeout`.
fn turn_settled(socket: &Path, name: &str, timeout: Option<Duration>) -> Result<Option<String>> {
    let deadline = deadline(timeout);
    let grace_over = Instant::now() + START_GRACE;
    let start_deadline = deadline.map_or(grace_over, |deadline| deadline.min(grace_over));
    let mut watch = Watch::start(socket, name)?;
    // Seen starting or not, what's left is to wait for the turn to end.
    let _seen_starting = watch.reach(&[Until::Working, Until::Ended], Some(start_deadline))?;
    watch.settle(deadline)
}

/// Prints a task's answer: what Claude said at the end of its last run. With
/// `json`, everything the task has come to, for scripts: whether the run
/// failed, the conversation's id, the cost so far and how many runs it's
/// had.
pub fn result(socket: &Path, name: &str, json: bool) -> Result<()> {
    let request = Request::Result {
        name: name.to_string(),
    };
    let Response::Result(result) = ask(socket, &request)? else {
        bail!("the daemon didn't send the result");
    };
    if json {
        outln!("{}", serde_json::to_string_pretty(&result)?)?;
    } else {
        // What Claude said: text to read, never orders for the terminal.
        outln!("{}", printable::text(result.text.trim_end()))?;
    }
    Ok(())
}

/// What `crystal read` was asked for.
#[derive(Debug, Default)]
pub struct Reading {
    /// Only that many of the last rows that aren't blank.
    pub lines: Option<usize>,
    /// The history ahead of the screen.
    pub history: bool,
    /// The rows a long line wrapped onto joined again.
    pub unwrap: bool,
    /// With its colors.
    pub ansi: bool,
    /// Only what came since then: a while back, like `10m`, or a time, as
    /// `crystal events --since` takes it.
    pub since: Option<String>,
}

/// Prints what's on the session's screen, as `reading` asks.
pub fn read(socket: &Path, name: &str, reading: Reading) -> Result<()> {
    let since_ms = reading
        .since
        .as_deref()
        .map(|when| crate::events_cli::parse_since(when, crate::events::now_ms()))
        .transpose()?;
    let request = Request::Read {
        name: name.to_string(),
        history: reading.history,
        unwrap: reading.unwrap,
        ansi: reading.ansi,
        since_ms,
    };
    let Response::Screen { rows } = ask(socket, &request)? else {
        bail!("the daemon didn't send the screen");
    };
    out!("{}", screen_text(&rows, reading.lines))?;
    Ok(())
}

/// Prints what runs in the session's terminal: the processes in front, a
/// line each, or with `json`, all the daemon said.
pub fn process_info(socket: &Path, name: &str, json: bool) -> Result<()> {
    let request = Request::ProcessInfo {
        name: name.to_string(),
    };
    let Response::Processes(processes) = ask(socket, &request)? else {
        bail!("the daemon didn't say what runs there");
    };
    if json {
        outln!("{}", serde_json::to_string_pretty(&processes)?)?;
        return Ok(());
    }
    if processes.foreground.is_empty() {
        bail!("{name}'s terminal doesn't say what's in front");
    }
    out!("{}", process_lines(&processes.foreground))?;
    Ok(())
}

/// The processes as `process-info` prints them: a header, then a line each,
/// in columns, with nothing a terminal would take as an order.
fn process_lines(processes: &[crate::protocol::ProcessInfo]) -> String {
    let rows: Vec<[String; 4]> = processes
        .iter()
        .map(|process| {
            let command: Vec<String> = process.argv.iter().map(|arg| shell::quote(arg)).collect();
            [
                process.pid.to_string(),
                process.name.clone(),
                process
                    .cwd
                    .as_deref()
                    .map_or("-".to_string(), shell::home_relative),
                command.join(" "),
            ]
            .map(|cell| printable::line(&cell).into_owned())
        })
        .collect();
    let header = ["PID", "NAME", "DIRECTORY", "COMMAND"].map(String::from);
    let mut widths = [0; 4];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in std::iter::once(&header).chain(&rows) {
        let cells: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// What a session has settled on, once it isn't busy: how its program
/// ended, or what its agent is doing when that isn't working. A program
/// that doesn't say what it's doing is busy until it ends.
fn settled(session: &SessionInfo) -> Option<String> {
    if session.state == State::Starting {
        return None;
    }
    if session.state != State::Running {
        return Some(session.state.to_string());
    }
    match session.activity {
        Some(Activity::Working) | None => None,
        Some(activity) => Some(activity.to_string()),
    }
}

/// The screen as text: each row without its trailing spaces, and no blank
/// rows after the last one with something on it. With `lines`, only that
/// many of the last rows that aren't blank.
fn screen_text(rows: &[String], lines: Option<usize>) -> String {
    let rows: Vec<&str> = rows.iter().map(|row| row.trim_end()).collect();
    let kept: Vec<&str> = match lines {
        Some(lines) => {
            let filled: Vec<&str> = rows.into_iter().filter(|row| !row.is_empty()).collect();
            let first = filled.len().saturating_sub(lines);
            filled[first..].to_vec()
        }
        None => {
            let end = rows
                .iter()
                .rposition(|row| !row.is_empty())
                .map_or(0, |last| last + 1);
            rows[..end].to_vec()
        }
    };
    kept.iter().map(|row| format!("{row}\n")).collect()
}

/// A session a wait keeps an eye on: the daemon's events about it, each
/// a reason to look again.
struct Watch<'a> {
    socket: &'a Path,
    name: &'a str,
    /// It keeps its id when it's renamed.
    id: String,
    events: Subscription,
}

impl<'a> Watch<'a> {
    fn start(socket: &'a Path, name: &'a str) -> Result<Watch<'a>> {
        let id = session(socket, |session| session.name == name, name)?.id;
        let filter = Filter {
            kinds: vec!["session.*".to_string()],
            session: Some(id.clone()),
            ..Filter::default()
        };
        // Listening before looking: whatever happens after the look is
        // heard.
        let events = client::subscribe(socket, filter, None)?;
        Ok(Watch {
            socket,
            name,
            id,
            events,
        })
    }

    fn now(&self) -> Result<SessionInfo> {
        session(self.socket, |session| session.id == self.id, self.name)
    }

    /// Waits until the session settles, as [`settled`] says, looking again
    /// after each event about it; `None` once `deadline` has passed.
    fn settle(&mut self, deadline: Option<Instant>) -> Result<Option<String>> {
        loop {
            if let Some(settled) = settled(&self.now()?) {
                return Ok(Some(settled));
            }
            if self.events.next_before(deadline)?.is_none() {
                return Ok(None);
            }
        }
    }

    /// Waits until the session gets to one of `wanted`, as it is now or as
    /// an event says, and says how it stands then: the agent may only be
    /// there a moment, which the event catches. `None` once `deadline` has
    /// passed. Its program ending, when that isn't wanted, is an error.
    fn reach(&mut self, wanted: &[Until], deadline: Option<Instant>) -> Result<Option<String>> {
        let now = self.now()?;
        let mut reached = Until::of_session(&now).map(|until| (until, now.status()));
        loop {
            if let Some((until, status)) = reached {
                if wanted.contains(&until) {
                    return Ok(Some(status));
                }
                if until == Until::Ended {
                    bail!(
                        "{} ended ({status}) without being {}",
                        self.name,
                        words(wanted)
                    );
                }
            }
            let Some(event) = self.events.next_before(deadline)? else {
                return Ok(None);
            };
            if event.kind == Kind::SessionRemoved {
                bail!("{} was killed", self.name);
            }
            if event.kind == Kind::SessionArchived {
                bail!("{} was archived", self.name);
            }
            let status = event
                .session
                .map(|session| session.status)
                .unwrap_or_default();
            reached = Until::of_event(event.kind).map(|until| (until, status));
        }
    }
}

/// The session `which` finds, called `name`.
fn session(socket: &Path, which: impl Fn(&SessionInfo) -> bool, name: &str) -> Result<SessionInfo> {
    let Response::Sessions { sessions } = ask(socket, &Request::List)? else {
        bail!("the daemon didn't send the sessions");
    };
    sessions
        .into_iter()
        .find(which)
        .with_context(|| format!("no session named {name}"))
}

fn deadline(timeout: Option<Duration>) -> Option<Instant> {
    timeout.map(|timeout| Instant::now() + timeout)
}

/// What a wait waits for, in words: `waiting or done`.
fn words(until: &[Until]) -> String {
    let words: Vec<&str> = until.iter().map(|until| until.word()).collect();
    words.join(" or ")
}

/// Asks the daemon, which must be running already: these commands only
/// make sense for a session that exists.
fn ask(socket: &Path, request: &Request) -> Result<Response> {
    match client::ask(socket, request, false)? {
        Some(response) => Ok(response),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

fn timed_out(name: &str, timeout: Option<Duration>) -> Result<()> {
    let seconds = timeout.unwrap_or_default().as_secs_f64();
    Err(TimedOut(format!("{name} was still busy after {seconds}s")).into())
}

/// Prints what a wait came to, unless it's `quiet`.
fn say(what: &str, quiet: bool) -> Result<()> {
    if !quiet {
        outln!("{what}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(state: State, activity: Option<Activity>) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            name: "agent".into(),
            id: "1".into(),
            command: vec!["claude".into()],
            cwd: PathBuf::from("/"),
            pid: Some(1),
            state,
            activity,
            worktree: None,
            changed: 0,
            task: None,
            asking: None,
            reporter: None,
            subagents: 0,
            model: None,
            line: None,
            bell: false,
            unseen_copies: 0,
            context: None,
            output_waits: 0,
        }
    }

    fn rows(text: &[&str]) -> Vec<String> {
        text.iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn an_agent_settles_on_anything_but_working() {
        let running = |activity| settled(&session(State::Running, activity));
        assert_eq!(running(Some(Activity::Working)), None);
        assert_eq!(running(Some(Activity::Done)), Some("done".into()));
        assert_eq!(running(Some(Activity::Waiting)), Some("waiting".into()));
        assert_eq!(running(Some(Activity::Idle)), Some("idle".into()));
    }

    #[test]
    fn a_program_that_says_nothing_settles_when_it_ends() {
        assert_eq!(settled(&session(State::Running, None)), None);
        let ended = session(State::Exited { code: 4 }, Some(Activity::Working));
        assert_eq!(settled(&ended), Some("exited 4".into()));
    }

    #[test]
    fn a_session_waiting_to_start_again_is_waited_for_until_it_has() {
        let starting = session(State::Starting, None);
        assert_eq!(settled(&starting), None);
        assert_eq!(Until::of_session(&starting), None);
        let why = "command not found: claude".to_string();
        let failed = session(State::Failed { why }, None);
        assert_eq!(settled(&failed), Some("couldn't start".into()));
        assert_eq!(Until::of_session(&failed), Some(Until::Ended));
        let failing = Kind::SessionStartFailed;
        assert_eq!(Until::of_event(failing), Some(Until::Ended));
    }

    #[test]
    fn a_wait_until_knows_where_a_session_has_got_to() {
        let running = |activity| Until::of_session(&session(State::Running, activity));
        assert_eq!(running(Some(Activity::Working)), Some(Until::Working));
        assert_eq!(running(Some(Activity::Idle)), Some(Until::Idle));
        assert_eq!(running(None), None);
        let ended = session(State::Exited { code: 0 }, Some(Activity::Done));
        assert_eq!(Until::of_session(&ended), Some(Until::Ended));
        assert_eq!(Until::of_event(Kind::SessionWaiting), Some(Until::Waiting));
        assert_eq!(Until::of_event(Kind::SessionRenamed), None);
        assert_eq!(words(&[Until::Waiting, Until::Done]), "waiting or done");
    }

    #[test]
    fn processes_line_up_in_columns() {
        use crate::protocol::ProcessInfo;
        let processes = [
            ProcessInfo {
                pid: 41388,
                name: "claude".into(),
                argv: vec!["claude".into(), "--resume".into(), "a b".into()],
                cwd: Some(PathBuf::from("/code/app")),
            },
            ProcessInfo {
                pid: 7,
                name: "evil\x1b]0;x\x07".into(),
                argv: Vec::new(),
                cwd: None,
            },
        ];
        let lines = process_lines(&processes);
        let lines: Vec<&str> = lines.lines().collect();
        assert_eq!(lines[0], "PID    NAME      DIRECTORY  COMMAND");
        assert_eq!(
            lines[1],
            "41388  claude    /code/app  claude --resume 'a b'"
        );
        assert_eq!(lines[2], "7      evil]0;x  -");
    }

    #[test]
    fn the_screen_loses_its_trailing_blanks() {
        let screen = rows(&["one   ", "", "two", "", ""]);
        assert_eq!(screen_text(&screen, None), "one\n\ntwo\n");
        assert_eq!(screen_text(&rows(&["", ""]), None), "");
    }

    #[test]
    fn lines_keeps_the_last_rows_that_say_something() {
        let screen = rows(&["one", "", "two", "three", "", ""]);
        assert_eq!(screen_text(&screen, Some(2)), "two\nthree\n");
        assert_eq!(screen_text(&screen, Some(10)), "one\ntwo\nthree\n");
    }
}
