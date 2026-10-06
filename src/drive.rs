//! `crystal send`, `wait`, `read`, `clear`, `result`, `answer` and
//! `interrupt`: how one agent drives another, or a script drives an agent.
//! They work from inside a session as well, since every session knows its
//! daemon's socket.
//!
//! A wait listens to the daemon's events about its session, or its task,
//! rather than asking again and again: each one is a reason to look again.
//! A wait that gives up is a [`TimedOut`], which `crystal` exits 2 for, so a
//! script can tell "not yet" from anything else going wrong; a prompt `send
//! --wait` never sees its agent start on is a [`Stalled`], which it exits 3
//! for.

use crate::client::{self, Subscription};
use crate::env;
use crate::events::{Event, Filter, Kind};
use crate::output::{out, outln};
use crate::printable;
use crate::protocol::{
    Activity, Answer, Front, Request, Response, SessionInfo, State, TaskOutcome, TaskState,
    TaskView,
};
use crate::shell;
use crate::tasks;
use anyhow::{Context, Result, bail, ensure};
use std::fmt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// How long `send --wait` gives an agent that wasn't working to be seen
/// starting on what it was sent, or to change its screen: past it, the
/// prompt has stalled. For keys, and a new task's run, which may be over
/// before it's seen starting, the wait goes on to the end of the turn
/// either way.
const START_GRACE: Duration = Duration::from_secs(5);

/// How long the screen is given to show what was just sent: it's looked at
/// until it holds still, this long at most. A change from how it shows
/// then is the agent's doing.
const LANDING: Duration = Duration::from_secs(1);

/// How often a screen is looked at while it's all there is to go by.
const LOOK_EVERY: Duration = Duration::from_millis(200);

/// How long the screen of an agent that has taken what it was sent, but
/// wasn't seen starting on it, must then hold still for its turn to be
/// over: one too short to fall between two looks at its screen, or one
/// getting under way on a loaded machine, whose work shows first.
const STILL_FOR: Duration = Duration::from_secs(2);

/// How long a wait for a task to close gives it once its session has ended
/// with it open: the daemon fails it as it sees the session end, a moment
/// later.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

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

/// What `send --wait` sent an agent that was never seen starting on it in
/// [`START_GRACE`]: `crystal` exits 3 for it. The agent may have taken it
/// all the same, say in a turn too short to see, so it's no reason to send
/// it again unread.
#[derive(Debug)]
pub struct Stalled(pub String);

impl fmt::Display for Stalled {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Stalled {}

/// What `crystal send` reads its text from when it's given as `-`.
pub const FROM_STDIN: &str = "-";

/// How `crystal send` sends its text.
#[derive(Debug, Default)]
pub struct Sending {
    /// Press Enter after it.
    pub enter: bool,
    /// Type it even while the agent is asking the user something.
    pub force: bool,
    /// Stop the run a background task is in the middle of first, and carry
    /// on from there.
    pub interrupt: bool,
    /// Then wait for the turn it starts to end, and print how it ended.
    pub wait: bool,
    /// Give up waiting after this long.
    pub timeout: Option<Duration>,
}

/// What the daemon's refusal starts with when an agent at its prompt never
/// took what it was typed: `crystal` exits 3 for it, as for [`Stalled`].
const PROMPT_STALLED: &str = "agent_prompt_stalled:";

/// Types `text` into the session called `name`, as `sending` says. Run in
/// a session, the message says it comes from that session. An agent at its
/// prompt is watched by the daemon until it takes it: one that never does
/// has [`Stalled`].
pub fn send(socket: &Path, name: &str, text: &str, sending: Sending) -> Result<()> {
    if sending.interrupt {
        stop_run(socket, name)?;
    }
    let deadline = deadline(sending.timeout.filter(|_| sending.wait));
    // Listening from before it goes, so a turn however short is heard, and
    // nothing said about the turn before is taken for it.
    let turn = match sending.wait {
        true => Some(Turn::expect(socket, name)?),
        false => None,
    };
    let request = Request::Send {
        name: name.to_string(),
        text: text.to_string(),
        enter: sending.enter,
        from: env::own_session_id(socket),
        force: sending.force,
    };
    let sent = match deadline {
        Some(deadline) => ask_before(socket, request, deadline),
        None => Some(ask(socket, &request)),
    };
    match sent {
        Some(Ok(_)) => {}
        Some(Err(err)) => {
            return match err.to_string() {
                said if said.starts_with(PROMPT_STALLED) => Err(Stalled(said).into()),
                _ => Err(err),
            };
        }
        None => {
            let seconds = sending.timeout.unwrap_or_default().as_secs_f64();
            let said = format!("{name} hadn't taken what it was sent after {seconds}s");
            return Err(TimedOut(said).into());
        }
    }
    match turn {
        // The daemon watched an agent at its prompt take it.
        Some(turn) => {
            let watched = sending.enter && turn.at_prompt();
            turn.prompted(sending.timeout, deadline, !watched)
        }
        None => Ok(()),
    }
}

/// Asks the daemon at `socket` `request`, giving up on its answer at
/// `deadline` with `None`: what it was asked goes on all the same.
fn ask_before(socket: &Path, request: Request, deadline: Instant) -> Option<Result<Response>> {
    let (answered, answer) = std::sync::mpsc::channel();
    let socket = socket.to_path_buf();
    thread::spawn(move || {
        // Nobody may be listening any more.
        let _ = answered.send(ask(&socket, &request));
    });
    let left = deadline.saturating_duration_since(Instant::now());
    answer.recv_timeout(left).ok()
}

/// Stops the run the background task `name` is in the middle of, if it's
/// in one, and waits for it to end: a task takes a follow-up only between
/// runs. A terminal's agent is stopped by its own key, which crystal doesn't
/// press for it.
fn stop_run(socket: &Path, name: &str) -> Result<()> {
    let mut watch = Watch::session(socket, name)?;
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
/// agent's question takes, since agents ignore a pasted answer. With
/// `wait`, then waits for the turn they start or carry on to end, and
/// prints how it ended, giving up after `timeout`.
pub fn send_keys(
    socket: &Path,
    name: &str,
    keys: Vec<String>,
    wait: bool,
    timeout: Option<Duration>,
) -> Result<()> {
    let turn = match wait {
        true => Some(Turn::expect(socket, name)?),
        false => None,
    };
    let request = Request::SendKeys {
        name: name.to_string(),
        keys,
    };
    ask(socket, &request)?;
    match turn {
        Some(turn) => turn.pressed(timeout),
        None => Ok(()),
    }
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

/// Clears the screen and history of the session called `name`, or the one
/// this runs in, but for the line its cursor is on.
pub fn clear(socket: &Path, name: Option<String>) -> Result<()> {
    let id = crate::work::own_session(socket, &name, "which session")?;
    ask(socket, &Request::Clear { id, name })?;
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

/// What `wait --until` waits for: what a session's agent comes to do, its
/// program's end, or its task closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Until {
    Working,
    Waiting,
    Done,
    Idle,
    #[value(alias = "exited")]
    Ended,
    /// Its task has closed, done, failed or cancelled.
    Closed,
}

impl Until {
    fn word(self) -> &'static str {
        match self {
            Until::Working => "working",
            Until::Waiting => "waiting",
            Until::Done => "done",
            Until::Idle => "idle",
            Until::Ended => "ended",
            Until::Closed => "closed",
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

/// Waits until the session called `name`'s agent isn't working, or its
/// program has ended, and prints which, unless `quiet`. Given a task's
/// number instead, like `t12`, waits until the task closes, and prints how
/// it went. Gives up after `timeout`, if there is one.
pub fn wait(socket: &Path, name: &str, timeout: Option<Duration>, quiet: bool) -> Result<()> {
    let mut watch = Watch::start(socket, name)?;
    if let About::Task(_) = watch.about {
        return wait_on(watch, &[Until::Closed], timeout, quiet);
    }
    match watch.settle(deadline(timeout))? {
        Some(settled) => say(&settled, quiet),
        None => timed_out(name, timeout),
    }
}

/// Waits until the session's agent comes to do one of `until`, with
/// `Ended` among them its program ends, or with `Closed` its task closes,
/// and prints which, unless `quiet`: a task's how it went. Its program
/// ending first is an error. `name` is a session's, or a task's number,
/// for the task and the session working on it. Gives up after `timeout`,
/// if there is one.
pub fn wait_until(
    socket: &Path,
    name: &str,
    until: &[Until],
    timeout: Option<Duration>,
    quiet: bool,
) -> Result<()> {
    let watch = Watch::start(socket, name)?;
    wait_on(watch, until, timeout, quiet)
}

fn wait_on(
    mut watch: Watch,
    until: &[Until],
    timeout: Option<Duration>,
    quiet: bool,
) -> Result<()> {
    match watch.reach(until, deadline(timeout))? {
        Some(status) => say(&status, quiet),
        None => {
            let seconds = timeout.unwrap_or_default().as_secs_f64();
            let words = words(until);
            let name = watch.name;
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

/// What a turn is seen starting by: its agent working, or asking
/// something at once, or its program ending.
const STARTED: [Until; 3] = [Until::Working, Until::Waiting, Until::Ended];

/// A turn about to start with what's sent to a session: its events heard
/// from before it's sent, and what its agent was doing then.
struct Turn<'a> {
    watch: Watch<'a>,
    before: Option<Activity>,
}

impl<'a> Turn<'a> {
    /// Listens to the session called `name`, before anything is sent to it.
    fn expect(socket: &'a Path, name: &'a str) -> Result<Turn<'a>> {
        let watch = Watch::session(socket, name)?;
        let before = watch.now()?.activity;
        Ok(Turn { watch, before })
    }

    /// Whether its agent sat at its prompt before anything was sent.
    fn at_prompt(&self) -> bool {
        matches!(self.before, Some(Activity::Idle | Activity::Done))
    }

    /// Waits for the turn a prompt just sent starts to end, and prints how
    /// it ended. Whatever the agent said about the turn before ends nothing:
    /// one that wasn't working has [`START_GRACE`] to be seen starting on
    /// the prompt, or to change its screen, or with `stalls` it has
    /// [`Stalled`]; without, the daemon has seen it take the prompt. One
    /// that was working takes the prompt once its turn is over, and that
    /// turn's end may be the wait's. It gives up at `deadline`, `timeout`
    /// after the send began.
    fn prompted(
        self,
        timeout: Option<Duration>,
        deadline: Option<Instant>,
        stalls: bool,
    ) -> Result<()> {
        let name = self.watch.name;
        match self.settled(deadline, stalls)? {
            Some(settled) => say(&settled, false),
            None => timed_out(name, timeout),
        }
    }

    /// Waits for the turn keys just pressed start or carry on to end, and
    /// prints how it ended, as [`Turn::prompted`] does; but keys can carry
    /// on a turn its agent doesn't say it's working on, like an answer to a
    /// permission, so keys that change nothing on its screen in
    /// [`START_GRACE`] end the wait on how it stands then, never a stall.
    fn pressed(self, timeout: Option<Duration>) -> Result<()> {
        let name = self.watch.name;
        match self.settled(deadline(timeout), false)? {
            Some(settled) => say(&settled, false),
            None => timed_out(name, timeout),
        }
    }

    /// What the session settles on once the turn is over, or `None` after
    /// `deadline`. The turn has started once it's seen to, or once the
    /// screen has changed from how it showed what was sent; then, past
    /// [`START_GRACE`], held still for [`STILL_FOR`], it's over. With
    /// `stalls`, an agent that says what it's doing, seen to do nothing with
    /// it, its screen unchanged, has [`Stalled`].
    fn settled(mut self, deadline: Option<Instant>, stalls: bool) -> Result<Option<String>> {
        if self.before == Some(Activity::Working) {
            return self.watch.settle(deadline);
        }
        let grace_over = Instant::now() + START_GRACE;
        let landed = self.watch.landed(deadline)?;
        match self.watch.taken(landed, grace_over, deadline)? {
            None => return Ok(None),
            Some(Taken::Seen | Taken::Unseen) => {}
            // A program that doesn't say what it's doing, or an agent that
            // hasn't yet: busy until it's seen starting, or until it ends.
            Some(Taken::Unmoved) if self.before.is_none() => {
                if self.watch.hear(&STARTED, deadline)?.is_none() {
                    return Ok(None);
                }
            }
            Some(Taken::Unmoved) if stalls => {
                let name = self.watch.name;
                let status = self.watch.now()?.status();
                let seconds = START_GRACE.as_secs();
                return Err(Stalled(format!(
                    "agent_prompt_stalled: {name} didn't start on what it was sent: {seconds}s \
                     on, it's {status}, its screen as it was once the text went in. It may have \
                     taken it all the same: `crystal read {name}` before sending it again"
                ))
                .into());
            }
            Some(Taken::Unmoved) => {}
        }
        self.watch.settle(deadline)
    }
}

/// Whether an agent has taken what it was just sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Taken {
    /// It was seen starting on it: working, asking something, or ending.
    Seen,
    /// It wasn't, but its screen changed, then held still: its turn went
    /// by between two looks, or showed only on its screen.
    Unseen,
    /// Neither, past [`START_GRACE`].
    Unmoved,
}

/// What a session's screen has done since what was sent to it showed,
/// while that's all there is to go by.
#[derive(Debug)]
struct Moves {
    screen: Vec<String>,
    moved: bool,
    /// When it was last seen changing, or first looked at.
    still_since: Instant,
}

impl Moves {
    fn new(landed: Vec<String>, now: Instant) -> Moves {
        Moves {
            screen: landed,
            moved: false,
            still_since: now,
        }
    }

    /// Takes in the screen as it is at `now`, and says what comes of it,
    /// if it's time to: nothing before `grace_over`.
    fn look(&mut self, screen: Vec<String>, now: Instant, grace_over: Instant) -> Option<Taken> {
        if screen != self.screen {
            self.screen = screen;
            self.moved = true;
            self.still_since = now;
        }
        if now < grace_over {
            return None;
        }
        if !self.moved {
            return Some(Taken::Unmoved);
        }
        let still = now.saturating_duration_since(self.still_since) >= STILL_FOR;
        still.then_some(Taken::Unseen)
    }
}

/// Waits for the run a background task has just started, and prints how it
/// ended: `done` or `failed`, as Claude's answer says, or else what the
/// task came to first, like `waiting` for a permission, or how its `claude`
/// ended.
pub fn wait_for_run(socket: &Path, name: &str, timeout: Option<Duration>) -> Result<()> {
    let Some(settled) = run_settled(socket, name, timeout)? else {
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

/// Waits for the run a new task has just started to end, and gives back
/// what the session settled on, or `None` after `timeout`. Its session is
/// there to listen to only once the task is, so the run may be under way,
/// or over, by then.
fn run_settled(socket: &Path, name: &str, timeout: Option<Duration>) -> Result<Option<String>> {
    let deadline = deadline(timeout);
    let grace_over = Instant::now() + START_GRACE;
    let start_deadline = deadline.map_or(grace_over, |deadline| deadline.min(grace_over));
    let mut watch = Watch::session(socket, name)?;
    // Seen starting or not, what's left is to wait for the run to end.
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

/// A session, or a task, a wait keeps an eye on: the daemon's events about
/// it, each a reason to look again.
struct Watch<'a> {
    socket: &'a Path,
    /// What the command line calls it.
    name: &'a str,
    about: About,
    events: Subscription,
}

/// What a wait is about.
#[derive(Debug, Clone, PartialEq, Eq)]
enum About {
    /// A session, by its id, which it keeps when it's renamed.
    Session(String),
    /// A task, by its number, and whichever session works on it: none while
    /// it waits to start, nor once that session has gone.
    Task(u64),
}

impl<'a> Watch<'a> {
    /// Watches the session called `name`.
    fn session(socket: &'a Path, name: &'a str) -> Result<Watch<'a>> {
        let id = session(socket, |session| session.name == name, name)?.id;
        Watch::listen(socket, name, About::Session(id))
    }

    /// Watches the session called `name`, or else the task whose number it
    /// is, like `t12`.
    fn start(socket: &'a Path, name: &'a str) -> Result<Watch<'a>> {
        let named = sessions(socket)?
            .into_iter()
            .find(|session| session.name == name);
        let about = match (named, tasks::parse_id(name)) {
            (Some(session), _) => About::Session(session.id),
            (None, Some(number)) => About::Task(number),
            (None, None) => bail!("no session named {name}"),
        };
        Watch::listen(socket, name, about)
    }

    fn listen(socket: &'a Path, name: &'a str, about: About) -> Result<Watch<'a>> {
        let mut filter = Filter {
            kinds: vec!["session.*".to_string(), "task.*".to_string()],
            ..Filter::default()
        };
        match &about {
            About::Session(id) => filter.session = Some(id.clone()),
            About::Task(number) => filter.task = Some(*number),
        }
        // Listening before looking: whatever happens after the look is
        // heard.
        let events = client::subscribe(socket, filter, None)?;
        Ok(Watch {
            socket,
            name,
            about,
            events,
        })
    }

    /// The session, as it is now: the one working on the task, for a task.
    fn now(&self) -> Result<SessionInfo> {
        match &self.about {
            About::Session(id) => session(self.socket, |session| session.id == *id, self.name),
            About::Task(number) => {
                let working = |session: &SessionInfo| task_number(session) == Some(*number);
                let found = sessions(self.socket)?.into_iter().find(working);
                found.with_context(|| format!("no session works on {}", self.name))
            }
        }
    }

    /// How what the wait is about stands now.
    fn look(&self) -> Result<Standing> {
        if let About::Task(number) = self.about {
            let working = |session: &&SessionInfo| task_number(session) == Some(number);
            let sessions = sessions(self.socket)?;
            if let Some(session) = sessions.iter().find(working) {
                return Ok(Standing::of_session(session));
            }
            // Waiting to start, or closed and its session gone.
            let request = Request::ShowTask {
                task: format!("t{number}"),
            };
            let Response::Task(task) = ask(self.socket, &request)? else {
                bail!("the daemon didn't send the task");
            };
            return Ok(Standing::of_task(&task));
        }
        Ok(Standing::of_session(&self.now()?))
    }

    /// What the session's screen shows now, a row each.
    fn screen(&self) -> Result<Vec<String>> {
        // By its name now: Claude Code can rename it as it takes a prompt.
        let request = Request::Read {
            name: self.now()?.name,
            history: false,
            unwrap: false,
            ansi: false,
            since_ms: None,
        };
        let Response::Screen { rows } = ask(self.socket, &request)? else {
            bail!("the daemon didn't send the screen");
        };
        Ok(rows)
    }

    /// The session's screen once what was just sent to it has had a moment
    /// to show: looked at until it holds still, for [`LANDING`] at most, or
    /// until `deadline`.
    fn landed(&self, deadline: Option<Instant>) -> Result<Vec<String>> {
        let landing = Instant::now() + LANDING;
        let until = deadline.map_or(landing, |deadline| deadline.min(landing));
        let mut screen = self.screen()?;
        while Instant::now() < until {
            thread::sleep(LOOK_EVERY);
            let again = self.screen()?;
            if again == screen {
                break;
            }
            screen = again;
        }
        Ok(screen)
    }

    /// Waits until the session's agent has taken what was just sent to it,
    /// its screen showing it as `landed`, as [`Moves`] tells, looking at
    /// the screen between events; `None` once `deadline` has passed.
    fn taken(
        &mut self,
        landed: Vec<String>,
        grace_over: Instant,
        deadline: Option<Instant>,
    ) -> Result<Option<Taken>> {
        let mut moves = Moves::new(landed, Instant::now());
        loop {
            let look = Instant::now() + LOOK_EVERY;
            let by = deadline.map_or(look, |deadline| deadline.min(look));
            if self.hear(&STARTED, Some(by))?.is_some() {
                return Ok(Some(Taken::Seen));
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(None);
            }
            if let Some(taken) = moves.look(self.screen()?, Instant::now(), grace_over) {
                return Ok(Some(taken));
            }
        }
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

    /// Waits until one of `wanted` holds, as it is now or as an event says,
    /// and says how it stands then: the agent may only be there a moment,
    /// which the event catches. `None` once `deadline` has passed. Its
    /// program ending, when that isn't wanted, is an error, and so is
    /// waiting only for a task to close where there's none.
    fn reach(&mut self, wanted: &[Until], deadline: Option<Instant>) -> Result<Option<String>> {
        let standing = self.look()?;
        if !standing.task && wanted.iter().all(|until| *until == Until::Closed) {
            bail!(
                "{} has no task to close: it wasn't started with something to do",
                self.name
            );
        }
        self.follow(standing, wanted, deadline)
    }

    /// As [`Watch::reach`] does, but by what happens from now on alone.
    fn hear(&mut self, wanted: &[Until], deadline: Option<Instant>) -> Result<Option<String>> {
        self.follow(Standing::default(), wanted, deadline)
    }

    /// Follows the events from `standing` until one of `wanted` holds.
    fn follow(
        &mut self,
        mut standing: Standing,
        wanted: &[Until],
        deadline: Option<Instant>,
    ) -> Result<Option<String>> {
        let mut ended = None;
        loop {
            if let Some(said) = standing.says(wanted) {
                return Ok(Some(said));
            }
            let mut until = deadline;
            if let Some((Until::Ended, status)) = &standing.session {
                let closing = standing.closing() && wanted.contains(&Until::Closed);
                let ended = *ended.get_or_insert_with(Instant::now);
                if !closing || ended.elapsed() >= CLOSE_GRACE {
                    bail!(
                        "{} ended ({status}) without being {}",
                        self.name,
                        words(wanted)
                    );
                }
                let closed_by = ended + CLOSE_GRACE;
                until = Some(deadline.map_or(closed_by, |deadline| deadline.min(closed_by)));
            }
            let Some(event) = self.events.next_before(until)? else {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Ok(None);
                }
                continue;
            };
            if event.kind == Kind::SessionRemoved {
                bail!("{} was killed", self.name);
            }
            if event.kind == Kind::SessionArchived {
                bail!("{} was archived", self.name);
            }
            standing.hear(&event);
        }
    }
}

/// How what a wait is about stands: where its session has got to, and
/// whether its task has closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Standing {
    /// Where its session has got to, with its status then: `None` with no
    /// session, or one that's nowhere a wait can be for.
    session: Option<(Until, String)>,
    /// Whether there's a task, open or not.
    task: bool,
    /// How its task went, once it has closed.
    closed: Option<TaskState>,
}

impl Standing {
    fn of_session(session: &SessionInfo) -> Standing {
        let outcome = session.task.as_ref().and_then(|task| task.outcome.as_ref());
        Standing {
            session: Until::of_session(session).map(|until| (until, session.status())),
            task: session.task.is_some(),
            closed: outcome.map(TaskOutcome::state),
        }
    }

    /// A task no session works on.
    fn of_task(task: &TaskView) -> Standing {
        Standing {
            session: None,
            task: true,
            closed: task.record.outcome.as_ref().map(TaskOutcome::state),
        }
    }

    /// Takes in what `event` says has changed.
    fn hear(&mut self, event: &Event) {
        if let Some(until) = Until::of_event(event.kind) {
            let status = event.session.as_ref().map(|session| session.status.clone());
            self.session = Some((until, status.unwrap_or_default()));
        }
        match event.kind {
            Kind::TaskClosed => {
                let outcome = event.task.as_ref().and_then(|task| task.outcome.as_ref());
                self.task = true;
                self.closed = outcome.map(TaskOutcome::state);
            }
            Kind::TaskOpened | Kind::TaskStarted => {
                self.task = true;
                self.closed = None;
            }
            _ => {}
        }
    }

    /// Whether its task is still to close, though its session has ended:
    /// the daemon fails it as it sees that.
    fn closing(&self) -> bool {
        self.task && self.closed.is_none()
    }

    /// The first of `wanted` that holds, as a wait prints it: a closed
    /// task's how it went, `done`, `failed` or `cancelled`, and anything
    /// else the session's status.
    fn says(&self, wanted: &[Until]) -> Option<String> {
        wanted.iter().find_map(|want| match want {
            Until::Closed => self.closed.map(|state| state.word().to_string()),
            want => match &self.session {
                Some((until, status)) if until == want => Some(status.clone()),
                _ => None,
            },
        })
    }
}

/// The number of the task `session` was given, if it has one.
fn task_number(session: &SessionInfo) -> Option<u64> {
    session.task.as_ref()?.id
}

/// Every session, as the daemon lists them.
fn sessions(socket: &Path) -> Result<Vec<SessionInfo>> {
    let Response::Sessions { sessions } = ask(socket, &Request::List)? else {
        bail!("the daemon didn't send the sessions");
    };
    Ok(sessions)
}

/// The session `which` finds, called `name`.
fn session(socket: &Path, which: impl Fn(&SessionInfo) -> bool, name: &str) -> Result<SessionInfo> {
    sessions(socket)?
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
            row: Default::default(),
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
    fn a_screen_that_changes_after_a_send_took_it_once_it_holds_still() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let grace_over = at(5000);
        let landed = rows(&["> fix it"]);

        // Nothing comes of a look before the grace is over; past it, a
        // screen just as it was once the text went in took nothing.
        let mut moves = Moves::new(landed.clone(), start);
        assert_eq!(moves.look(landed.clone(), at(1000), grace_over), None);
        let unmoved = moves.look(landed.clone(), at(5000), grace_over);
        assert_eq!(unmoved, Some(Taken::Unmoved));

        // A turn too short to see, over well before the grace is.
        let mut moves = Moves::new(landed.clone(), start);
        let answered = rows(&["> fix it", "fixed"]);
        assert_eq!(moves.look(answered.clone(), at(1000), grace_over), None);
        let unseen = moves.look(answered, at(5000), grace_over);
        assert_eq!(unseen, Some(Taken::Unseen));

        // One still drawing past the grace is waited on until it holds
        // still, even back to how it was.
        let mut moves = Moves::new(landed.clone(), start);
        assert_eq!(moves.look(rows(&["⠋"]), at(4900), grace_over), None);
        assert_eq!(moves.look(landed.clone(), at(5200), grace_over), None);
        assert_eq!(moves.look(landed.clone(), at(7000), grace_over), None);
        let still = moves.look(landed, at(7200), grace_over);
        assert_eq!(still, Some(Taken::Unseen));
    }

    /// A task, numbered 12, as `outcome` says it went: open with `None`.
    fn task(outcome: Option<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "id": 12,
            "goal": "fix it",
            "session": "agent",
            "project": "app",
            "outcome": outcome,
        })
    }

    fn with_task(mut session: SessionInfo, outcome: Option<serde_json::Value>) -> SessionInfo {
        session.task = Some(serde_json::from_value(task(outcome)).unwrap());
        session
    }

    fn cancelled() -> Option<serde_json::Value> {
        let outcome = r#"{"failed": false, "cancelled": true, "summary": "", "closed": 1}"#;
        Some(serde_json::from_str(outcome).unwrap())
    }

    #[test]
    fn a_wait_says_the_first_it_waits_for_that_holds() {
        let open = with_task(session(State::Running, Some(Activity::Waiting)), None);
        let standing = Standing::of_session(&open);
        assert!(standing.task && standing.closing());
        assert_eq!(standing.says(&[Until::Closed]), None);
        let either = [Until::Closed, Until::Waiting];
        assert_eq!(standing.says(&either), Some("waiting".into()));

        let closed = with_task(session(State::Running, Some(Activity::Idle)), cancelled());
        let standing = Standing::of_session(&closed);
        assert!(!standing.closing());
        assert_eq!(standing.says(&[Until::Closed]), Some("cancelled".into()));
        assert_eq!(
            standing.says(&[Until::Idle, Until::Closed]),
            Some("idle".into())
        );
        assert_eq!(standing.says(&[Until::Working]), None);

        let plain = Standing::of_session(&session(State::Running, Some(Activity::Done)));
        assert!(!plain.task && !plain.closing());
        assert_eq!(plain.says(&[Until::Closed]), None);
    }

    #[test]
    fn a_task_no_session_works_on_stands_as_its_record_says() {
        let pending: TaskView = serde_json::from_value(serde_json::json!({
            "state": "pending",
            "pending": true,
            "id": 12,
            "goal": "fix it",
            "session": "",
            "project": "app",
        }))
        .unwrap();
        let standing = Standing::of_task(&pending);
        assert_eq!(standing.session, None);
        assert!(standing.closing());
        let mut gone = task(cancelled());
        gone["state"] = "cancelled".into();
        let gone: TaskView = serde_json::from_value(gone).unwrap();
        let standing = Standing::of_task(&gone);
        assert_eq!(standing.says(&[Until::Closed]), Some("cancelled".into()));
    }

    #[test]
    fn a_wait_hears_its_session_move_on_and_its_task_close_and_open_again() {
        let info = with_task(session(State::Running, Some(Activity::Idle)), None);
        let mut standing = Standing::default();
        standing.hear(&Event::activity(&info, None, Activity::Working));
        assert_eq!(standing.session, Some((Until::Working, "working".into())));
        // What says nothing of where it has got to leaves it there.
        standing.hear(&Event::about_session(Kind::SessionRenamed, &info));
        assert_eq!(standing.session, Some((Until::Working, "working".into())));

        let record = |outcome| serde_json::from_value(task(outcome)).unwrap();
        standing.hear(&Event::task(Kind::TaskClosed, &info, record(cancelled())));
        assert!(standing.task);
        assert_eq!(standing.says(&[Until::Closed]), Some("cancelled".into()));
        standing.hear(&Event::task(Kind::TaskOpened, &info, record(None)));
        assert_eq!(standing.says(&[Until::Closed]), None);
        assert!(standing.closing());

        let ended = session(State::Exited { code: 3 }, None);
        standing.hear(&Event::ended(&ended, "exited 3".into()));
        assert_eq!(standing.says(&STARTED), Some("exited 3".into()));
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
