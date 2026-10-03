//! Tasks: Claude Code run without a terminal (`claude -p`), in the session
//! list like any other session.
//!
//! In a task, Claude has no terminal to draw on. It writes what it does as
//! JSON events instead, which crystal reads and draws on the session's
//! screen itself (see [`transcript`]), so the TUI, `attach`, `read` and the
//! history show a task the way they show any session.
//!
//! One `claude -p` takes the task's prompt, then each follow-up, a turn
//! each, over its standard input (see [`claude_stream`]). A permission it
//! asks for waits for the user's answer, and a turn can be stopped halfway.
//! A `claude` left idle for [`IDLE_KEEP`] is let go; the next follow-up
//! starts another, which carries the conversation on with `--resume`, as
//! one does after a restart. A turn that fails ends the task.

use crate::claude_stream::{self, Decision, Line, PermissionRequest, Rule};
use crate::config::Config;
use crate::distill::Record;
use crate::protocol::{AgentEvent, Answer, Asking, State, TaskResult, TaskSpec};
use crate::session::{STOP_GRACE, Term, signal_group};
use crate::spending::Spending;
use crate::transcript::{self, Event, Outcome};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How many of the last lines Claude wrote to its standard error a run that
/// crashed shows, to say why.
const ERROR_LINES: usize = 5;

/// How long a `claude` with no turn to take is kept for a follow-up before
/// it's let go: each one holds on to a fair amount of memory.
const IDLE_KEEP: Duration = Duration::from_secs(5 * 60);

pub struct Task {
    spec: TaskSpec,
    /// What each `claude` is given: the spec's own arguments, with what
    /// crystal adds to them.
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    /// The session's screen, which each turn is drawn on.
    term: Arc<Term>,
    /// The session's state: a turn that fails ends the task.
    state: Arc<Mutex<State>>,
    /// What every background task has spent today, which each turn adds
    /// to and a daily budget is held against.
    spending: Arc<Spending>,
    runs: Arc<Mutex<Runs>>,
    /// How the runs stood when the session last looked.
    seen: Seen,
}

/// What a task's runs have come to: written by the thread that reads its
/// `claude`, and read as the daemon keeps up with its sessions.
#[derive(Debug, Default)]
struct Runs {
    /// Turns are counted, not only marked as going on, so that a turn that
    /// starts and ends between two looks still counts.
    started: u32,
    ended: u32,
    /// The `claude` taking the task's turns, while there is one.
    claude: Option<Claude>,
    /// How many `claude`s the task has started. Each one's number tells the
    /// thread reading it whether it's still the task's.
    spawned: u64,
    /// When the last turn ended, to let an idle `claude` go.
    idle_since: Option<Instant>,
    /// The permissions Claude waits on the user for, the oldest first.
    asking: VecDeque<PermissionRequest>,
    /// The user has stopped the turn going on now.
    interrupting: bool,
    /// The last turn to end had been stopped by the user.
    interrupted: bool,
    conversation: Option<String>,
    /// What the last turn came to: Claude's answer, or what went wrong.
    result: Option<String>,
    failed: bool,
    /// What the task has cost so far, every `claude` of it together.
    cost_usd: f64,
    /// What its runs did, the end of it, for the distiller to read once
    /// the task has closed.
    record: Record,
}

impl Runs {
    fn working(&self) -> bool {
        self.started > self.ended
    }

    /// Lets the `claude` go once it has sat idle for [`IDLE_KEEP`]. Its
    /// input closes, so it ends of itself.
    fn let_idle_claude_go(&mut self) {
        if !self.working() && idle_for_long(self.idle_since) {
            self.claude = None;
        }
    }
}

/// Whether a `claude` idle since `since` has been for [`IDLE_KEEP`].
fn idle_for_long(since: Option<Instant>) -> bool {
    since.is_some_and(|since| since.elapsed() >= IDLE_KEEP)
}

/// A `claude` taking a task's turns.
#[derive(Debug)]
struct Claude {
    number: u64,
    pid: u32,
    /// Where its prompts and answers go. Dropping it closes it, which is
    /// how `claude` learns there's nothing more to do.
    stdin: ChildStdin,
    /// What it has cost so far, as it counts: from its start.
    cost: f64,
}

impl Claude {
    fn send(&mut self, message: &Value) -> io::Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()
    }
}

/// How a task's runs stood when the session last looked.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Seen {
    started: u32,
    ended: u32,
    asking: bool,
}

impl Task {
    /// A task at rest, before its first run. Given a `conversation`, its
    /// runs carry that conversation on.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        spec: TaskSpec,
        args: Vec<String>,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        term: Arc<Term>,
        state: Arc<Mutex<State>>,
        spending: Arc<Spending>,
        conversation: Option<String>,
    ) -> Task {
        let runs = Runs {
            conversation,
            ..Runs::default()
        };
        Task {
            spec,
            args,
            cwd,
            env,
            term,
            state,
            spending,
            runs: Arc::new(Mutex::new(runs)),
            seen: Seen::default(),
        }
    }

    pub fn spec(&self) -> &TaskSpec {
        &self.spec
    }

    pub fn conversation(&self) -> Option<String> {
        self.runs.lock().unwrap().conversation.clone()
    }

    /// The process of the `claude` taking the task's turns, while there is
    /// one.
    pub fn pid(&self) -> Option<u32> {
        Some(self.runs.lock().unwrap().claude.as_ref()?.pid)
    }

    /// Whether a turn is going on.
    pub fn is_working(&self) -> bool {
        self.runs.lock().unwrap().working()
    }

    /// Runs `prompt`, the task's own to start with, then each follow-up,
    /// which carries the conversation on: on the `claude` there is, or a
    /// new one. One turn at a time, and none once today's spending is past
    /// the daily budget.
    pub fn run(&self, prompt: &str) -> Result<()> {
        let config = Config::load().unwrap_or_default();
        let mut runs = self.runs.lock().unwrap();
        ensure!(
            !runs.working(),
            "it's still working on its last prompt: `crystal wait` for it first"
        );
        self.spending.check(config.tasks.daily_budget_usd)?;
        let message = claude_stream::user_message(prompt);
        // A `claude` that has gone since its last turn, without that being
        // read yet, can't take it: another does.
        let taken = runs
            .claude
            .as_mut()
            .is_some_and(|claude| claude.send(&message).is_ok());
        if !taken {
            self.start_claude(&mut runs, config.tasks.max_budget_usd)?;
            let claude = runs.claude.as_mut().expect("just started");
            claude
                .send(&message)
                .context("couldn't give claude the prompt")?;
        }
        runs.started += 1;
        runs.idle_since = None;
        runs.record.push(format!("USER: {}", prompt.trim()));
        // Drawn while the runs are held, so that nothing Claude says in
        // answer can come before it.
        self.term.show(transcript::prompt_lines(prompt).as_bytes());
        Ok(())
    }

    /// Starts a `claude` for the task's turns, in its conversation if it
    /// has one, and a thread that reads what it says.
    fn start_claude(&self, runs: &mut Runs, budget: f64) -> Result<()> {
        let mut child = self
            .command(runs.conversation.as_deref(), budget)
            .spawn()
            .context("couldn't start claude")?;
        let stdin = child.stdin.take().context("claude has no input")?;
        runs.spawned += 1;
        runs.claude = Some(Claude {
            number: runs.spawned,
            pid: child.id(),
            stdin,
            cost: 0.0,
        });
        let reading = Reading {
            number: runs.spawned,
            term: self.term.clone(),
            runs: self.runs.clone(),
            state: self.state.clone(),
            spending: self.spending.clone(),
        };
        thread::spawn(move || reading.follow(child));
        Ok(())
    }

    /// What its runs mean for the session since it last looked: a turn
    /// that started or ended, a permission it's asking for, or one answered
    /// so it works on. And an idle `claude` is let go.
    pub fn events(&mut self) -> Vec<AgentEvent> {
        let mut runs = self.runs.lock().unwrap();
        runs.let_idle_claude_go();
        let now = Seen {
            started: runs.started,
            ended: runs.ended,
            asking: !runs.asking.is_empty(),
        };
        let events = turn_events(self.seen, now);
        self.seen = now;
        events
    }

    /// The last run's answer, and what the task has come to: `None` until a
    /// run has ended.
    pub fn result(&self) -> Option<TaskResult> {
        let runs = self.runs.lock().unwrap();
        Some(TaskResult {
            text: runs.result.clone()?,
            failed: runs.failed,
            conversation: runs.conversation.clone(),
            cost_usd: runs.cost_usd,
            runs: runs.started,
        })
    }

    /// Whether the user stopped the last turn to end, which then says
    /// nothing about how the task went.
    pub fn was_interrupted(&self) -> bool {
        self.runs.lock().unwrap().interrupted
    }

    /// What the task has cost so far.
    pub fn cost_usd(&self) -> f64 {
        self.runs.lock().unwrap().cost_usd
    }

    /// The permission Claude is waiting on the user for, the oldest when
    /// there are several.
    pub fn asking(&self) -> Option<Asking> {
        let runs = self.runs.lock().unwrap();
        let request = runs.asking.front()?;
        Some(Asking {
            tool: request.tool_name.clone(),
            gist: transcript::tool_gist(&request.tool_name, &request.input),
        })
    }

    /// Answers the permission Claude has waited on longest. Always allows
    /// it and keeps a rule for calls like it, which Claude adds to the
    /// checkout's local settings. A denial tells Claude `message`, or that
    /// the user said no.
    pub fn answer(&self, answer: Answer, message: Option<&str>) -> Result<()> {
        let mut runs = self.runs.lock().unwrap();
        let request = runs
            .asking
            .front()
            .context("it isn't asking for anything")?
            .clone();
        let decision = match answer {
            Answer::Allow => Decision::Allow { rule: None },
            Answer::Always => {
                let rule = Rule::for_call(&request.tool_name, &request.input).with_context(|| {
                    format!(
                        "there's no rule to keep for a {} call without a command: allow it once instead",
                        request.tool_name
                    )
                })?;
                Decision::Allow { rule: Some(rule) }
            }
            Answer::Deny => Decision::Deny {
                message: message
                    .map(str::trim)
                    .filter(|message| !message.is_empty())
                    .unwrap_or(claude_stream::DENIED)
                    .to_string(),
            },
        };
        let claude = runs.claude.as_mut().context("its claude has gone")?;
        claude
            .send(&claude_stream::answer(&request, &decision))
            .context("its claude has gone")?;
        runs.asking.pop_front();
        let said = match &decision {
            Decision::Allow { rule: None } => "allowed".to_string(),
            Decision::Allow { rule: Some(rule) } => format!("allowed always · {rule}"),
            Decision::Deny { .. } => "denied".to_string(),
        };
        self.term.show(transcript::answered_lines(&said).as_bytes());
        Ok(())
    }

    /// Stops the turn going on. Claude ends it at once, and its end leaves
    /// the task open, waiting on the user.
    pub fn interrupt(&self) -> Result<()> {
        let mut runs = self.runs.lock().unwrap();
        ensure!(runs.working(), "it isn't working on anything");
        if runs.interrupting {
            return Ok(());
        }
        let request = claude_stream::interrupt(&format!("interrupt-{}", runs.started));
        let claude = runs.claude.as_mut().context("its claude has gone")?;
        claude.send(&request).context("its claude has gone")?;
        runs.interrupting = true;
        self.term
            .show(transcript::note_lines("interrupted").as_bytes());
        Ok(())
    }

    /// The end of what its runs have done.
    pub fn record(&self) -> Record {
        self.runs.lock().unwrap().record.clone()
    }

    /// Draws a note on the screen, between runs.
    pub fn note(&self, note: &str) {
        self.term.show(transcript::note_lines(note).as_bytes());
    }

    /// Stops the task. Its `claude` is asked to stop, the way process
    /// supervisors ask, then killed if it hasn't after [`STOP_GRACE`]. A
    /// turn cut short ends the task with how the process ended; a task at
    /// rest just ends.
    pub fn stop(&self) {
        let runs = self.runs.lock().unwrap();
        if let Some(claude) = &runs.claude {
            let (pid, number) = (claude.pid, claude.number);
            signal_group(pid, libc::SIGTERM);
            let runs = self.runs.clone();
            thread::spawn(move || {
                thread::sleep(STOP_GRACE);
                let runs = runs.lock().unwrap();
                if runs.claude.as_ref().is_some_and(|c| c.number == number) {
                    signal_group(pid, libc::SIGKILL);
                }
            });
        }
        if !runs.working() {
            *self.state.lock().unwrap() = State::Exited { code: 0 };
            self.term.close();
        }
    }

    /// `claude -p`, taking prompts on its input and writing its events as
    /// JSON, in the task's directory and environment.
    fn command(&self, conversation: Option<&str>, budget: f64) -> Command {
        let mut command = Command::new("claude");
        command.args(claude_stream::ARGS);
        // A budget given with the task's own arguments wins.
        if budget > 0.0
            && !self
                .args
                .iter()
                .any(|arg| arg.starts_with("--max-budget-usd"))
        {
            command.args(["--max-budget-usd", &budget.to_string()]);
        }
        if let Some(id) = conversation {
            command.args(["--resume", id]);
        }
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A process group of its own, so that stopping the task stops
            // whatever Claude started too.
            .process_group(0);
        command
    }
}

/// The events for what changed between two looks at a task's runs.
fn turn_events(seen: Seen, now: Seen) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    if now.ended > seen.ended {
        events.push(AgentEvent::TurnEnded);
    }
    if now.started <= now.ended {
        return events;
    }
    // A turn is going on now; it may be one the session hasn't seen yet.
    let new_turn = now.started > seen.started;
    if new_turn {
        events.push(AgentEvent::TurnStarted);
    }
    if now.asking && (new_turn || !seen.asking) {
        events.push(AgentEvent::Asking);
    } else if !now.asking && seen.asking && !new_turn {
        // Answered, so Claude works on.
        events.push(AgentEvent::ToolFinished);
    }
    events
}

/// What the thread reading a task's `claude` needs: which `claude` it is,
/// the screen to draw on, and where to note how its turns go.
struct Reading {
    number: u64,
    term: Arc<Term>,
    runs: Arc<Mutex<Runs>>,
    state: Arc<Mutex<State>>,
    spending: Arc<Spending>,
}

impl Reading {
    /// Reads what `claude` says until it ends, then says how it ended.
    fn follow(self, mut child: Child) {
        // What Claude writes to its standard error is read on a thread of
        // its own, so that neither pipe can fill up and stall it.
        let errors = child
            .stderr
            .take()
            .map(|stderr| thread::spawn(move || last_lines(stderr, ERROR_LINES)));
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                self.take(&line);
            }
        }
        let status = child.wait();
        let errors = errors
            .and_then(|reading| reading.join().ok())
            .unwrap_or_default();
        self.end(status, &errors);
    }

    /// Whether the `claude` read is still the task's: one it has let go of
    /// is read to its end, but has no say any more.
    fn is_current(&self, runs: &Runs) -> bool {
        runs.claude
            .as_ref()
            .is_some_and(|c| c.number == self.number)
    }

    /// Acts on one line of what Claude says.
    fn take(&self, line: &str) {
        let mut runs = self.runs.lock().unwrap();
        if !self.is_current(&runs) {
            return;
        }
        match claude_stream::read(line) {
            Line::Conversation => self.converse(&mut runs, line),
            Line::Permission(request) => {
                let gist = transcript::tool_gist(&request.tool_name, &request.input);
                let asked = transcript::asking_lines(&request.tool_name, &gist);
                self.term.show(asked.as_bytes());
                runs.asking.push_back(request);
            }
            Line::Withdrawn { request_id } => {
                let before = runs.asking.len();
                runs.asking.retain(|asked| asked.request_id != request_id);
                if runs.asking.len() < before {
                    let withdrawn = transcript::answered_lines("taken back by claude");
                    self.term.show(withdrawn.as_bytes());
                }
            }
            Line::Unsupported {
                request_id,
                subtype,
            } => {
                if let Some(claude) = runs.claude.as_mut() {
                    let _ = claude.send(&claude_stream::unsupported(&request_id, &subtype));
                }
            }
            Line::Reply {
                error: Some(error), ..
            } => {
                let refused = format!("claude refused: {error}");
                self.term.show(transcript::note_lines(&refused).as_bytes());
            }
            Line::Reply { error: None, .. } | Line::Nothing => {}
        }
    }

    /// Draws a line of the conversation, and keeps what the session needs
    /// from it: the conversation it's in, and how each turn ends.
    fn converse(&self, runs: &mut Runs, line: &str) {
        runs.record.push_claude(line);
        for event in transcript::events(line) {
            match event {
                Event::Started { conversation } => runs.conversation = Some(conversation),
                Event::Finished(outcome) => self.finish(runs, outcome),
                event => self.term.show(transcript::lines(&event).as_bytes()),
            }
        }
    }

    /// Notes how a turn ended, and what it cost. One that failed ends the
    /// task, unless the user stopped it.
    fn finish(&self, runs: &mut Runs, outcome: Outcome) {
        let Some(claude) = runs.claude.as_mut() else {
            return;
        };
        let cost = (outcome.cost_usd - claude.cost).max(0.0);
        claude.cost = claude.cost.max(outcome.cost_usd);
        self.spending.add(cost);
        runs.cost_usd += cost;
        if !outcome.conversation.is_empty() {
            runs.conversation = Some(outcome.conversation.clone());
        }
        runs.result = Some(outcome.result.clone());
        runs.failed = outcome.failed;
        runs.interrupted = std::mem::take(&mut runs.interrupting);
        runs.asking.clear();
        runs.ended = (runs.ended + 1).min(runs.started);
        runs.idle_since = Some(Instant::now());
        let shown = Outcome {
            cost_usd: cost,
            ..outcome
        };
        self.term
            .show(transcript::lines(&Event::Finished(shown)).as_bytes());
        if runs.failed && !runs.interrupted {
            // Let go: its input closes, and it ends.
            runs.claude = None;
            *self.state.lock().unwrap() = State::Exited { code: 1 };
            self.term.close();
        }
    }

    /// Notes that the `claude` has ended. Between turns, the next turn
    /// starts another. In the middle of one, it crashed or was stopped,
    /// which ends the task.
    fn end(&self, status: io::Result<ExitStatus>, errors: &[String]) {
        let mut runs = self.runs.lock().unwrap();
        if !self.is_current(&runs) {
            return;
        }
        runs.claude = None;
        runs.asking.clear();
        if !runs.working() {
            return;
        }
        let ended = ended(status);
        self.term
            .show(transcript::cut_short_lines(&ended.to_string(), errors).as_bytes());
        runs.result = Some(match errors.last() {
            Some(error) => error.clone(),
            None => ended.to_string(),
        });
        runs.failed = true;
        runs.interrupting = false;
        runs.ended = runs.started;
        *self.state.lock().unwrap() = ended;
        self.term.close();
    }
}

/// How a `claude` that ended in the middle of a turn ended, as the
/// session's state. One that exited 0 even so counts as 1 here.
fn ended(status: io::Result<ExitStatus>) -> State {
    let Ok(status) = status else {
        return State::Exited { code: 1 };
    };
    if let Some(signal) = status.signal() {
        return State::Signaled {
            signal: signal_name(signal),
        };
    }
    match status.code() {
        Some(code) if code != 0 => State::Exited { code: code as u32 },
        _ => State::Exited { code: 1 },
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "Hangup".to_string(),
        libc::SIGINT => "Interrupt".to_string(),
        libc::SIGKILL => "Killed".to_string(),
        libc::SIGTERM => "Terminated".to_string(),
        other => format!("signal {other}"),
    }
}

/// The last `count` lines of `input` that aren't blank.
fn last_lines(input: impl Read, count: usize) -> Vec<String> {
    let lines: Vec<String> = BufReader::new(input)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let first = lines.len().saturating_sub(count);
    lines[first..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentEvent::*;

    fn seen(started: u32, ended: u32, asking: bool) -> Seen {
        Seen {
            started,
            ended,
            asking,
        }
    }

    #[test]
    fn a_turn_going_on_is_a_turn_started() {
        assert_eq!(
            turn_events(seen(0, 0, false), seen(1, 0, false)),
            [TurnStarted]
        );
        assert_eq!(turn_events(seen(1, 0, false), seen(1, 0, false)), []);
    }

    #[test]
    fn a_turn_that_ended_is_a_turn_ended_even_if_it_was_never_seen_going() {
        assert_eq!(
            turn_events(seen(1, 0, false), seen(1, 1, false)),
            [TurnEnded]
        );
        assert_eq!(
            turn_events(seen(0, 0, false), seen(1, 1, false)),
            [TurnEnded]
        );
    }

    #[test]
    fn a_follow_up_going_on_after_a_turn_that_ended_is_a_turn_started_again() {
        assert_eq!(
            turn_events(seen(1, 0, false), seen(2, 1, false)),
            [TurnEnded, TurnStarted]
        );
    }

    #[test]
    fn a_permission_waits_on_the_user_until_it_is_answered() {
        assert_eq!(turn_events(seen(1, 0, false), seen(1, 0, true)), [Asking]);
        assert_eq!(turn_events(seen(1, 0, true), seen(1, 0, true)), []);
        assert_eq!(
            turn_events(seen(1, 0, true), seen(1, 0, false)),
            [ToolFinished]
        );
        // Asked for at once, in a turn not seen yet.
        assert_eq!(
            turn_events(seen(0, 0, false), seen(1, 0, true)),
            [TurnStarted, Asking]
        );
        // A turn that ends answers nothing: it's over.
        assert_eq!(
            turn_events(seen(1, 0, true), seen(1, 1, false)),
            [TurnEnded]
        );
    }

    #[test]
    fn a_claude_is_let_go_once_it_has_been_idle_a_while() {
        assert!(!idle_for_long(None), "never idle: a turn is going on");
        assert!(!idle_for_long(Some(Instant::now())));
        let long_ago = Instant::now().checked_sub(IDLE_KEEP);
        assert!(long_ago.is_none_or(|since| idle_for_long(Some(since))));
    }

    #[test]
    fn a_crashed_run_ends_the_task_with_how_its_process_ended() {
        assert_eq!(
            ended(Ok(ExitStatus::from_raw(3 << 8))),
            State::Exited { code: 3 }
        );
        // It crashed, but its process exited 0.
        assert_eq!(
            ended(Ok(ExitStatus::from_raw(0))),
            State::Exited { code: 1 }
        );
        assert_eq!(
            ended(Ok(ExitStatus::from_raw(libc::SIGTERM))),
            State::Signaled {
                signal: "Terminated".into()
            }
        );
    }

    #[test]
    fn the_last_lines_skip_the_blank_ones() {
        let text = "one\n\ntwo\nthree\n\n";
        assert_eq!(last_lines(text.as_bytes(), 2), ["two", "three"]);
        assert_eq!(last_lines(text.as_bytes(), 9), ["one", "two", "three"]);
    }
}
