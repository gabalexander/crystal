//! Tasks: Claude Code run without a terminal (`claude -p`), one prompt at a
//! time, in the session list like any other session.
//!
//! In a task, Claude has no terminal to draw on. It writes what it does as
//! JSON events instead, which crystal reads and draws on the session's
//! screen itself (see [`transcript`]), so the TUI, `attach`, `read` and the
//! history show a task the way they show any session.
//!
//! Each `claude -p` is one run: the task's prompt to start with, then each
//! follow-up, which carries the conversation on with `--resume`. A run that
//! fails ends the task.

use crate::protocol::{AgentEvent, State, TaskResult, TaskSpec};
use crate::session::{STOP_GRACE, Term, signal_group};
use crate::transcript::{self, Event};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// How many of the last lines Claude wrote to its standard error a run that
/// crashed shows, to say why.
const ERROR_LINES: usize = 5;

pub struct Task {
    spec: TaskSpec,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    /// The session's screen, which each run is drawn on.
    term: Arc<Term>,
    /// The session's state: a run that fails ends the task.
    state: Arc<Mutex<State>>,
    runs: Arc<Mutex<Runs>>,
    /// How many runs had started and ended when the session last looked.
    seen: (u32, u32),
}

/// What a task's runs have come to: written by the thread that reads each
/// run, and read as the daemon keeps up with its sessions.
#[derive(Debug, Default)]
struct Runs {
    /// Runs are counted, not only marked as going on, so that a run that
    /// starts and ends between two looks still counts.
    started: u32,
    ended: u32,
    /// The process of the run going on now.
    current: Option<u32>,
    conversation: Option<String>,
    /// What the last run came to: Claude's answer, or what went wrong.
    result: Option<String>,
    failed: bool,
    cost_usd: f64,
}

impl Task {
    /// A task at rest, before its first run. Given a `conversation`, its
    /// runs carry that conversation on.
    pub fn new(
        spec: TaskSpec,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        term: Arc<Term>,
        state: Arc<Mutex<State>>,
        conversation: Option<String>,
    ) -> Task {
        let runs = Runs {
            conversation,
            ..Runs::default()
        };
        Task {
            spec,
            cwd,
            env,
            term,
            state,
            runs: Arc::new(Mutex::new(runs)),
            seen: (0, 0),
        }
    }

    pub fn spec(&self) -> &TaskSpec {
        &self.spec
    }

    pub fn conversation(&self) -> Option<String> {
        self.runs.lock().unwrap().conversation.clone()
    }

    /// The process of the run going on now, if there is one.
    pub fn pid(&self) -> Option<u32> {
        self.runs.lock().unwrap().current
    }

    /// Runs `prompt`: the task's own to start with, then each follow-up,
    /// which carries the conversation on. One run at a time.
    pub fn run(&self, prompt: &str) -> Result<()> {
        let mut runs = self.runs.lock().unwrap();
        ensure!(
            runs.current.is_none(),
            "it's still working on its last prompt: `crystal wait` for it first"
        );
        let child = self
            .command(prompt, runs.conversation.as_deref())
            .spawn()
            .context("couldn't start claude")?;
        runs.started += 1;
        runs.current = Some(child.id());
        drop(runs);

        self.term.show(transcript::prompt_lines(prompt).as_bytes());
        let reading = Reading {
            term: self.term.clone(),
            runs: self.runs.clone(),
            state: self.state.clone(),
        };
        thread::spawn(move || reading.follow(child));
        Ok(())
    }

    /// What its runs mean for the session since it last looked: a turn
    /// that started, one that ended.
    pub fn events(&mut self) -> Vec<AgentEvent> {
        let runs = self.runs.lock().unwrap();
        let now = (runs.started, runs.ended);
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

    /// Draws a note on the screen, between runs.
    pub fn note(&self, note: &str) {
        self.term.show(transcript::note_lines(note).as_bytes());
    }

    /// Stops the task. A run going on is asked to stop, the way process
    /// supervisors ask, then killed if it hasn't after [`STOP_GRACE`]; how
    /// it ended then ends the task. A task at rest just ends.
    pub fn stop(&self) {
        let Some(pid) = self.pid() else {
            *self.state.lock().unwrap() = State::Exited { code: 0 };
            self.term.close();
            return;
        };
        signal_group(pid, libc::SIGTERM);
        let runs = self.runs.clone();
        thread::spawn(move || {
            thread::sleep(STOP_GRACE);
            if runs.lock().unwrap().current == Some(pid) {
                signal_group(pid, libc::SIGKILL);
            }
        });
    }

    /// `claude -p` for `prompt`, its events streamed as JSON, in the
    /// task's directory and environment.
    fn command(&self, prompt: &str, conversation: Option<&str>) -> Command {
        let mut command = Command::new("claude");
        command.args(["-p", "--output-format", "stream-json", "--verbose"]);
        if let Some(id) = conversation {
            command.args(["--resume", id]);
        }
        // The prompt goes after `--`, so that an argument that takes
        // several values, like `--allowedTools Read Grep`, can't take it
        // too, and a prompt starting with `-` isn't taken for one.
        command
            .args(&self.spec.args)
            .arg("--")
            .arg(prompt)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A process group of its own, so that stopping the task stops
            // whatever Claude started too.
            .process_group(0);
        command
    }
}

/// The events for runs that started or ended between two looks, given as
/// (runs started, runs ended) then and now.
fn turn_events(seen: (u32, u32), now: (u32, u32)) -> Vec<AgentEvent> {
    let (seen_started, seen_ended) = seen;
    let (started, ended) = now;
    let mut events = Vec::new();
    if ended > seen_ended {
        events.push(AgentEvent::TurnEnded);
    }
    // A run is going on now, and it's one the session hasn't seen yet.
    if started > ended && started > seen_started {
        events.push(AgentEvent::TurnStarted);
    }
    events
}

/// What the thread reading a run needs: the screen to draw it on, and where
/// to note how it went.
struct Reading {
    term: Arc<Term>,
    runs: Arc<Mutex<Runs>>,
    state: Arc<Mutex<State>>,
}

impl Reading {
    /// Reads a run's events until it ends: draws each one, notes the
    /// conversation and the outcome, then says how the run ended.
    fn follow(self, mut child: Child) {
        // What Claude writes to its standard error is read on a thread of
        // its own, so that neither pipe can fill up and stall it.
        let errors = child
            .stderr
            .take()
            .map(|stderr| thread::spawn(move || last_lines(stderr, ERROR_LINES)));
        let mut answered = false;
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                for event in transcript::events(&line) {
                    self.term.show(transcript::lines(&event).as_bytes());
                    answered |= matches!(event, Event::Finished(_));
                    self.remember(event);
                }
            }
        }
        let status = child.wait();
        let errors = errors
            .and_then(|reading| reading.join().ok())
            .unwrap_or_default();
        self.end(status, answered, &errors);
    }

    /// Keeps what the session needs from `event`: the conversation it's in,
    /// and how the run ended.
    fn remember(&self, event: Event) {
        let mut runs = self.runs.lock().unwrap();
        match event {
            Event::Started { conversation } => runs.conversation = Some(conversation),
            Event::Finished(outcome) => {
                if !outcome.conversation.is_empty() {
                    runs.conversation = Some(outcome.conversation);
                }
                runs.result = Some(outcome.result);
                runs.failed = outcome.failed;
                runs.cost_usd = outcome.cost_usd;
            }
            _ => {}
        }
    }

    /// Notes that the run is over. One that failed, or that ended without
    /// saying how (it crashed, or was stopped), ends the task too.
    fn end(&self, status: io::Result<ExitStatus>, answered: bool, errors: &[String]) {
        let mut runs = self.runs.lock().unwrap();
        let failed = !answered || runs.failed;
        if failed {
            let ended = ended(status);
            if !answered {
                self.term
                    .show(transcript::cut_short_lines(&ended.to_string(), errors).as_bytes());
                runs.result = Some(match errors.last() {
                    Some(error) => error.clone(),
                    None => ended.to_string(),
                });
                runs.failed = true;
            }
            *self.state.lock().unwrap() = ended;
            self.term.close();
        }
        runs.current = None;
        runs.ended += 1;
    }
}

/// How a failed run's process ended, as the session's state. A run can say
/// it failed and still exit 0, which counts as 1 here.
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

    #[test]
    fn a_run_going_on_is_a_turn_started() {
        assert_eq!(turn_events((0, 0), (1, 0)), [TurnStarted]);
        assert_eq!(turn_events((1, 0), (1, 0)), []);
    }

    #[test]
    fn a_run_that_ended_is_a_turn_ended_even_if_it_was_never_seen_going() {
        assert_eq!(turn_events((1, 0), (1, 1)), [TurnEnded]);
        assert_eq!(turn_events((0, 0), (1, 1)), [TurnEnded]);
    }

    #[test]
    fn a_follow_up_going_on_after_a_run_that_ended_is_a_turn_started_again() {
        assert_eq!(turn_events((1, 0), (2, 1)), [TurnEnded, TurnStarted]);
    }

    #[test]
    fn a_failed_run_ends_the_task_with_how_its_process_ended() {
        assert_eq!(
            ended(Ok(ExitStatus::from_raw(3 << 8))),
            State::Exited { code: 3 }
        );
        // Claude said it failed, but its process exited 0.
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
