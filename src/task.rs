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
//! one does after a restart. So is one whose turn failed, by Claude's own
//! answer: the task's session stays, for a follow-up. A `claude` that dies
//! in the middle of a turn, saying nothing of how it ended, ends it.
//!
//! A handover (see [`crate::handover`]) hands the `claude` over too, its
//! pipes and all, so a turn halfway through carries on in the next daemon:
//! a `claude` is a child like a terminal's program, and its pipes are
//! descriptors like a terminal.

use crate::agents;
use crate::claude_stream::{self, Decision, Line, PermissionRequest, Rule};
use crate::config::{Config, TaskSettings};
use crate::distill::Record;
use crate::events::ToolUse;
use crate::handover::{self, Got};
use crate::protocol::{AgentEvent, Answer, Asking, ContextUse, State, TaskResult, TaskSpec};
use crate::session::{STOP_GRACE, Term, signal_group};
use crate::spending::Spending;
use crate::transcript::{self, Event, Outcome};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How many of the last lines Claude wrote to its standard error a run that
/// crashed shows, to say why.
const ERROR_LINES: usize = 5;

/// How long a `claude` with no turn to take is kept for a follow-up before
/// it's let go: each one holds on to a fair amount of memory.
const IDLE_KEEP: Duration = Duration::from_secs(5 * 60);

/// How much of the end of the transcript Claude Code keeps of a task's
/// conversation is drawn again after a restart.
const KEPT_BYTES: u64 = 512 * 1024;

/// How many tokens a model takes, until a run's result has said: Claude's
/// models take this many, unless they're run with a million, which their
/// names say.
const CONTEXT_WINDOW: u64 = 200_000;
const MILLION_WINDOW: u64 = 1_000_000;

pub struct Task {
    spec: TaskSpec,
    /// What each `claude` is given: the spec's own arguments, with what
    /// crystal adds to them.
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    /// The session's screen, which each turn is drawn on.
    term: Arc<Term>,
    /// The session's state: a `claude` that dies in the middle of a turn
    /// ends the task's session.
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
#[derive(Debug, Default, Serialize, Deserialize)]
struct Runs {
    /// Turns are counted, not only marked as going on, so that a turn that
    /// starts and ends between two looks still counts.
    started: u32,
    ended: u32,
    /// The `claude` taking the task's turns, while there is one.
    #[serde(skip)]
    claude: Option<Claude>,
    /// The threads reading each `claude` that hasn't been read to its end,
    /// for a handover to wait for once it has stopped them.
    #[serde(skip)]
    readers: Vec<JoinHandle<Option<Stopped>>>,
    /// How many `claude`s the task has started. Each one's number tells the
    /// thread reading it whether it's still the task's.
    spawned: u64,
    /// When the last turn ended, to let an idle `claude` go.
    #[serde(skip)]
    idle_since: Option<Instant>,
    /// The permissions Claude waits on the user for, the oldest first.
    asking: VecDeque<PermissionRequest>,
    /// The tools Claude has used since the session last asked, in order:
    /// each one's name and gist.
    #[serde(skip)]
    tools_used: Vec<ToolUse>,
    /// The user has stopped the turn going on now.
    interrupting: bool,
    /// The last turn to end had been stopped by the user.
    interrupted: bool,
    conversation: Option<String>,
    /// The model Claude said it runs on, as its last run started.
    #[serde(default)]
    model: Option<String>,
    /// How many tokens the model was given for the conversation's last
    /// message, and which model that was, as the message names it.
    #[serde(default)]
    context: Option<(u64, Option<String>)>,
    /// How many tokens each model the task's runs used takes, as their
    /// results said.
    #[serde(default)]
    windows: BTreeMap<String, u64>,
    /// What the last turn was asked.
    prompt: String,
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

    /// How full the conversation is, once a message has said: of what its
    /// model takes as a result said, or else of what its name says.
    fn context(&self) -> Option<ContextUse> {
        let (tokens, model) = self.context.as_ref()?;
        let said = model.as_ref().and_then(|model| self.windows.get(model));
        let window = match said {
            Some(&window) => window,
            None if self
                .model
                .as_ref()
                .is_some_and(|model| model.ends_with("[1m]")) =>
            {
                MILLION_WINDOW
            }
            None => CONTEXT_WINDOW,
        };
        Some(ContextUse {
            tokens: *tokens,
            window,
        })
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
    stdin: File,
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

    /// The model Claude runs the task on, once a run has said.
    pub fn model(&self) -> Option<String> {
        self.runs.lock().unwrap().model.clone()
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

    /// Whether the session has taken how the runs stand now: no run has
    /// started or ended since it last looked ([`Task::events`]).
    pub fn caught_up(&self) -> bool {
        let runs = self.runs.lock().unwrap();
        (self.seen.started, self.seen.ended) == (runs.started, runs.ended)
    }

    /// Runs `prompt`, the task's own to start with, then each follow-up,
    /// which carries the conversation on: on the `claude` there is, or a
    /// new one. One turn at a time, and none once today's spending is past
    /// the daily budget.
    pub fn run(&self, prompt: &str) -> Result<()> {
        let config = Config::load().unwrap_or_default();
        // What Claude draws in its answers follows the settings at once.
        crate::mermaid::set_ascii(config.mermaid_ascii);
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
            self.start_claude(&mut runs, &config.tasks)?;
            let claude = runs.claude.as_mut().expect("just started");
            claude
                .send(&message)
                .context("couldn't give claude the prompt")?;
        }
        runs.started += 1;
        runs.idle_since = None;
        runs.prompt = prompt.to_string();
        runs.record.push(format!("USER: {}", prompt.trim()));
        // Drawn while the runs are held, so that nothing Claude says in
        // answer can come before it.
        self.term.show(transcript::prompt_lines(prompt).as_bytes());
        Ok(())
    }

    /// Starts a `claude` for the task's turns, in its conversation if it
    /// has one, and a thread that reads what it says.
    fn start_claude(&self, runs: &mut Runs, settings: &TaskSettings) -> Result<()> {
        let mut child = self
            .command(runs.conversation.as_deref(), settings)
            .spawn()
            .context("couldn't start claude")?;
        let pipe = |pipe: Option<OwnedFd>| pipe.map(File::from).context("claude has no pipes");
        let stdin = pipe(child.stdin.take().map(OwnedFd::from))?;
        let pipes = Pipes {
            pid: child.id(),
            stdout: pipe(child.stdout.take().map(OwnedFd::from))?,
            stderr: pipe(child.stderr.take().map(OwnedFd::from))?,
        };
        // Waited for by its pid, the same way a daemon it's handed over to
        // waits for it.
        drop(child);
        runs.spawned += 1;
        runs.claude = Some(Claude {
            number: runs.spawned,
            pid: pipes.pid,
            stdin,
            cost: 0.0,
        });
        self.follow(runs, runs.spawned, pipes, Vec::new(), Errors::default());
        Ok(())
    }

    /// Reads `claude` number `number`, on a thread of its own, from where
    /// the last reader left it: `unread`, what it wrote after its last
    /// whole line, and `errors`.
    fn follow(&self, runs: &mut Runs, number: u64, pipes: Pipes, unread: Vec<u8>, errors: Errors) {
        let reading = Reading {
            number,
            term: self.term.clone(),
            runs: self.runs.clone(),
            state: self.state.clone(),
            spending: self.spending.clone(),
        };
        runs.readers.retain(|reader| !reader.is_finished());
        runs.readers
            .push(thread::spawn(move || reading.follow(pipes, unread, errors)));
    }

    /// Hands the task over to the next daemon, once a handover has stopped
    /// its readers: how its runs stand, and each `claude` not read to its
    /// end, with its pipes kept open across the exec. Leaves the task
    /// empty: only the exec comes after.
    pub fn hand_over(&self) -> io::Result<Handed> {
        let readers = std::mem::take(&mut self.runs.lock().unwrap().readers);
        let stopped: Vec<Stopped> = readers
            .into_iter()
            .filter_map(|reader| reader.join().ok().flatten())
            .collect();
        let mut runs = std::mem::take(&mut *self.runs.lock().unwrap());
        let mut current = runs.claude.take();
        let mut claudes = Vec::new();
        for stopped in stopped {
            let claude = current.take_if(|claude| claude.number == stopped.number);
            claudes.push(HandedClaude {
                number: stopped.number,
                pid: stopped.pid,
                cost: claude.as_ref().map_or(0.0, |claude| claude.cost),
                stdin: claude
                    .map(|claude| keep_across_exec(claude.stdin))
                    .transpose()?,
                stdout: keep_across_exec(stopped.stdout)?,
                stderr: keep_across_exec(stopped.stderr)?,
                unread: stopped.unread,
                errors: stopped.errors,
            });
        }
        Ok(Handed {
            spec: self.spec.clone(),
            args: self.args.clone(),
            runs,
            seen: self.seen,
            claudes,
        })
    }

    /// Carries on a task the last daemon handed over, in `cwd` with `env`:
    /// its runs as they stood, and each of its `claude`s read again from
    /// where the last daemon left off. Fails when what it was handed isn't
    /// open.
    pub fn adopt(
        handed: Handed,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        term: Arc<Term>,
        state: Arc<Mutex<State>>,
        spending: Arc<Spending>,
    ) -> Result<Task> {
        let Handed {
            spec,
            args,
            mut runs,
            seen,
            claudes,
        } = handed;
        let mut reading = Vec::new();
        for claude in claudes {
            let pipe = |fd| handover::inherit(fd).map(File::from);
            let pipes = Pipes {
                pid: claude.pid,
                stdout: pipe(claude.stdout)?,
                stderr: pipe(claude.stderr)?,
            };
            if let Some(stdin) = claude.stdin {
                runs.claude = Some(Claude {
                    number: claude.number,
                    pid: claude.pid,
                    stdin: pipe(stdin)?,
                    cost: claude.cost,
                });
            }
            reading.push((claude.number, pipes, claude.unread, claude.errors));
        }
        // An idle `claude` is let go a while from now.
        if runs.claude.is_some() && !runs.working() {
            runs.idle_since = Some(Instant::now());
        }
        let task = Task {
            spec,
            args,
            cwd,
            env,
            term,
            state,
            spending,
            runs: Arc::new(Mutex::new(runs)),
            seen,
        };
        {
            let mut runs = task.runs.lock().unwrap();
            for (number, pipes, unread, errors) in reading {
                task.follow(&mut runs, number, pipes, unread, errors);
            }
        }
        Ok(task)
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

    /// The tools Claude has used since this was last asked, in order.
    pub fn tools_used(&mut self) -> Vec<ToolUse> {
        std::mem::take(&mut self.runs.lock().unwrap().tools_used)
    }

    /// What the last run to start was asked.
    pub fn last_prompt(&self) -> String {
        self.runs.lock().unwrap().prompt.clone()
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

    /// How full its conversation is, once Claude has said.
    pub fn context(&self) -> Option<ContextUse> {
        self.runs.lock().unwrap().context()
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

    /// Draws what the task did before a cold restart, which took what its
    /// screen showed, again: from the transcript Claude Code keeps of its
    /// conversation, its last [`KEPT_BYTES`]. What was asked, what Claude
    /// said and the tools it used come back, and how full the conversation
    /// is; what only crystal drew, the permissions asked for and how each
    /// run ended, doesn't. With no transcript to read, it says what's gone.
    pub fn draw_kept(&self) {
        let conversation = self.conversation();
        let kept = conversation
            .as_deref()
            .and_then(|conversation| crate::distill::transcript_of(conversation, &self.env))
            .and_then(|path| kept_lines(&path).ok())
            .filter(|lines| !lines.is_empty());
        let Some(lines) = kept else {
            self.note(
                "crystal restarted, and what this task showed before is gone. \
                 `crystal send` carries its conversation on.",
            );
            return;
        };
        self.note("What this task did before, from Claude Code's transcript of its conversation:");
        {
            let mut runs = self.runs.lock().unwrap();
            for line in lines {
                for event in transcript::kept_events(&line) {
                    match event {
                        Event::Context { tokens, model } => runs.context = Some((tokens, model)),
                        Event::Started { .. } | Event::Finished(_) => {}
                        event => {
                            let lines = transcript::lines(&event, self.term.columns());
                            self.term.show(lines.as_bytes());
                        }
                    }
                }
            }
        }
        self.term.show(b"\r\n");
        self.note("`crystal send` carries its conversation on.");
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
    /// JSON, in the task's directory and environment: with the budget, the
    /// permission mode and the rules the settings give every task, as they
    /// are now.
    fn command(&self, conversation: Option<&str>, settings: &TaskSettings) -> Command {
        let mut command = Command::new("claude");
        command.args(claude_stream::ARGS);
        // A budget given with the task's own arguments wins, and so does a
        // permission mode.
        let budget = settings.max_budget_usd;
        if budget > 0.0 && !given(&self.args, "--max-budget-usd") {
            command.args(["--max-budget-usd", &budget.to_string()]);
        }
        if let Some(mode) = settings.permission_mode()
            && !given(&self.args, "--permission-mode")
        {
            command.args(["--permission-mode", mode]);
        }
        if let Some(id) = conversation {
            command.args(["--resume", id]);
        }
        let args = match settings.allowed_tools.as_slice() {
            [] => self.args.clone(),
            rules => agents::with_value(
                &self.args,
                &["--allowedTools", "--allowed-tools"],
                &rules.join(","),
            ),
        };
        command
            .args(&args)
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

/// The options of a task's own arguments that only `claude -p` takes, each
/// with whether it takes a value: a task opened in a terminal leaves them
/// out, and crystal chooses the conversation itself.
const PRINT_ONLY: &[(&str, bool)] = &[
    ("-p", false),
    ("--print", false),
    ("--max-budget-usd", true),
    ("--max-turns", true),
    ("--output-format", true),
    ("--input-format", true),
    ("--include-partial-messages", false),
    ("--replay-user-messages", false),
    ("--permission-prompt-tool", true),
    ("--fallback-model", true),
    ("--resume", true),
    ("-r", true),
    ("--continue", false),
    ("-c", false),
];

/// A task's own arguments, `args`, as Claude Code in a terminal takes them:
/// without those only `claude -p` takes.
pub fn terminal_args(args: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let option = arg
            .split_once('=')
            .map_or(arg.as_str(), |(option, _)| option);
        match PRINT_ONLY.iter().find(|(name, _)| *name == option) {
            Some((_, true)) if !arg.contains('=') => {
                args.next();
            }
            Some(_) => {}
            None => kept.push(arg.clone()),
        }
    }
    kept
}

/// Whether `args` give the option `option`, on its own or as
/// `option=value`.
fn given(args: &[String], option: &str) -> bool {
    args.iter().any(|arg| {
        arg.strip_prefix(option)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
    })
}

/// The events for what changed between two looks at a task's runs. A run
/// at a time: a turn ends before the next starts.
fn turn_events(seen: Seen, now: Seen) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    let was_working = seen.started > seen.ended;
    let new_turn = now.started > seen.started;
    let working = now.started > now.ended;
    if new_turn {
        if was_working && now.ended > seen.ended {
            events.push(AgentEvent::TurnEnded);
        }
        events.push(AgentEvent::TurnStarted);
        // One that started and ended between two looks still started, so
        // the session is seen working before it's done.
        if !working {
            events.push(AgentEvent::TurnEnded);
        }
    } else if now.ended > seen.ended {
        events.push(AgentEvent::TurnEnded);
    }
    if !working {
        return events;
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
    /// Reads what `claude` says until it ends, then says how it ended; or,
    /// stopped by a handover, gives back how far it got, for the next
    /// daemon to carry on from.
    fn follow(self, claude: Pipes, mut unread: Vec<u8>, errors: Errors) -> Option<Stopped> {
        let Pipes {
            pid,
            stdout,
            stderr,
        } = claude;
        // What Claude writes to its standard error is read on a thread of
        // its own, so that neither pipe can fill up and stall it.
        let reading_errors = thread::spawn(move || {
            let mut errors = errors;
            errors.read(&stderr);
            (stderr, errors)
        });
        let stopped = self.read_lines(&stdout, &mut unread);
        let (stderr, errors) = reading_errors.join().ok()?;
        if stopped {
            return Some(Stopped {
                number: self.number,
                pid,
                stdout,
                stderr,
                unread,
                errors,
            });
        }
        let status = handover::reap(pid);
        self.end(status, &errors.lines());
        None
    }

    /// Takes what `claude` says a line at a time, until it ends (`false`),
    /// or a handover stops the reading (`true`). What comes after the last
    /// whole line waits in `unread`.
    fn read_lines(&self, stdout: &File, unread: &mut Vec<u8>) -> bool {
        let mut buf = [0; 16 * 1024];
        loop {
            match handover::readers().read(stdout, &mut buf) {
                Ok(Got::Bytes(n)) => {
                    unread.extend_from_slice(&buf[..n]);
                    while let Some(end) = unread.iter().position(|&byte| byte == b'\n') {
                        let line: Vec<u8> = unread.drain(..=end).collect();
                        self.take(&text_of(&line));
                    }
                }
                Ok(Got::Stopped) => return true,
                Ok(Got::End) | Err(_) => {
                    if !unread.is_empty() {
                        self.take(&text_of(&std::mem::take(unread)));
                    }
                    return false;
                }
            }
        }
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
                Event::Started {
                    conversation,
                    model,
                } => {
                    runs.conversation = Some(conversation);
                    if model.is_some() {
                        runs.model = model;
                    }
                }
                Event::Context { tokens, model } => runs.context = Some((tokens, model)),
                Event::Finished(outcome) => self.finish(runs, outcome),
                event => {
                    if let Event::UsedTool { name, gist } = &event {
                        let used = ToolUse {
                            name: name.clone(),
                            gist: gist.clone(),
                        };
                        runs.tools_used.push(used);
                    }
                    let lines = transcript::lines(&event, self.term.columns());
                    self.term.show(lines.as_bytes());
                }
            }
        }
    }

    /// Notes how a turn ended, and what it cost. One that failed lets its
    /// `claude` go, unless the user stopped it: a follow-up starts another,
    /// with a budget of its own, in the same conversation.
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
        runs.windows.extend(outcome.windows.iter().cloned());
        runs.interrupted = std::mem::take(&mut runs.interrupting);
        runs.asking.clear();
        runs.ended = (runs.ended + 1).min(runs.started);
        runs.idle_since = Some(Instant::now());
        let shown = Outcome {
            cost_usd: cost,
            ..outcome
        };
        let lines = transcript::lines(&Event::Finished(shown), self.term.columns());
        self.term.show(lines.as_bytes());
        if runs.failed && !runs.interrupted {
            // Let go: its input closes, and it ends.
            runs.claude = None;
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

/// The lines at the end of the transcript at `path`, its last
/// [`KEPT_BYTES`], from the first whole line in them.
fn kept_lines(path: &std::path::Path) -> io::Result<Vec<String>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(KEPT_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    // Begun partway through a line, the end of it isn't one.
    if start > 0 {
        let first = bytes
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(bytes.len(), |at| at + 1);
        bytes.drain(..first);
    }
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(String::from)
        .collect())
}

/// A line read off a pipe, as text, without its line ending.
fn text_of(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

/// A `claude`'s pipes, as the thread reading it has them.
struct Pipes {
    pid: u32,
    stdout: File,
    stderr: File,
}

/// How far a thread reading a `claude` had got when a handover stopped
/// it.
#[derive(Debug)]
struct Stopped {
    number: u64,
    pid: u32,
    stdout: File,
    stderr: File,
    unread: Vec<u8>,
    errors: Errors,
}

/// The last [`ERROR_LINES`] lines a `claude` wrote to its standard error
/// that aren't blank, to say why a run that crashed did.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Errors {
    last: VecDeque<String>,
    /// What came after the last whole line.
    unread: Vec<u8>,
}

impl Errors {
    /// Reads `stderr` to its end, or until a handover stops it.
    fn read(&mut self, stderr: &File) {
        let mut buf = [0; 4096];
        while let Ok(Got::Bytes(n)) = handover::readers().read(stderr, &mut buf) {
            self.take(&buf[..n]);
        }
    }

    fn take(&mut self, bytes: &[u8]) {
        self.unread.extend_from_slice(bytes);
        while let Some(end) = self.unread.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.unread.drain(..=end).collect();
            self.keep(text_of(&line));
        }
    }

    fn keep(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        self.last.push_back(line);
        if self.last.len() > ERROR_LINES {
            self.last.pop_front();
        }
    }

    /// The lines, the last one too if it had no line ending.
    fn lines(mut self) -> Vec<String> {
        let rest = std::mem::take(&mut self.unread);
        self.keep(text_of(&rest));
        self.last.into()
    }
}

/// A task as one daemon hands it to the next: how its runs stand, and its
/// `claude`s.
#[derive(Serialize, Deserialize)]
pub struct Handed {
    spec: TaskSpec,
    args: Vec<String>,
    runs: Runs,
    seen: Seen,
    claudes: Vec<HandedClaude>,
}

impl Handed {
    pub fn spec(&self) -> &TaskSpec {
        &self.spec
    }

    pub fn conversation(&self) -> Option<String> {
        self.runs.conversation.clone()
    }

    /// Its `claude`s, which the next daemon has to reap.
    pub fn processes(&self) -> Vec<u32> {
        self.claudes.iter().map(|claude| claude.pid).collect()
    }
}

/// A `claude` a task hands over: which it is, and its pipes by the numbers
/// of the descriptors the next daemon inherits, the way the last reader
/// left them.
#[derive(Serialize, Deserialize)]
struct HandedClaude {
    number: u64,
    pid: u32,
    /// What it has cost so far, as it counts.
    cost: f64,
    /// Its input, while it's the task's `claude`: one let go has none.
    stdin: Option<RawFd>,
    stdout: RawFd,
    stderr: RawFd,
    unread: Vec<u8>,
    errors: Errors,
}

/// Keeps `pipe` open across the exec, for the next daemon, which owns it
/// from then on: it's never closed here.
fn keep_across_exec(pipe: File) -> io::Result<RawFd> {
    handover::keep_across_exec(pipe.as_fd())?;
    Ok(pipe.into_raw_fd())
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
    fn a_turn_that_ended_is_a_turn_ended() {
        assert_eq!(
            turn_events(seen(1, 0, false), seen(1, 1, false)),
            [TurnEnded]
        );
    }

    #[test]
    fn a_turn_that_started_and_ended_between_two_looks_is_both() {
        assert_eq!(
            turn_events(seen(0, 0, false), seen(1, 1, false)),
            [TurnStarted, TurnEnded]
        );
        // And after one that was going on, that one's end first.
        assert_eq!(
            turn_events(seen(1, 0, false), seen(2, 2, false)),
            [TurnEnded, TurnStarted, TurnEnded]
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
    fn a_task_handed_over_comes_back_with_its_runs_as_they_stood() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open(&dir.path().join("crystal.sock")).unwrap();
        let asking = PermissionRequest {
            request_id: "perm-2".into(),
            tool_name: "Bash".into(),
            input: serde_json::json!({"command": "cargo test"}),
        };
        let runs = Runs {
            started: 2,
            ended: 1,
            spawned: 2,
            asking: [asking].into(),
            conversation: Some("conv-1".into()),
            prompt: "now the docs".into(),
            result: Some("All green".into()),
            cost_usd: 0.5,
            ..Runs::default()
        };
        let handed = Handed {
            spec: TaskSpec {
                prompt: "fix the tests".into(),
                args: Vec::new(),
            },
            args: Vec::new(),
            runs,
            seen: seen(2, 1, true),
            claudes: Vec::new(),
        };
        // Through the file it's handed over in.
        let handed: Handed =
            serde_json::from_str(&serde_json::to_string(&handed).unwrap()).unwrap();
        assert_eq!(handed.conversation().as_deref(), Some("conv-1"));

        let mut task = Task::adopt(
            handed,
            dir.path().to_path_buf(),
            BTreeMap::new(),
            Arc::new(Term::without_terminal()),
            Arc::new(Mutex::new(State::Running)),
            Arc::new(Spending::new(db)),
        )
        .unwrap();
        assert!(task.is_working());
        assert_eq!(task.last_prompt(), "now the docs");
        assert_eq!(task.asking().unwrap().gist, "cargo test");
        assert_eq!(task.cost_usd(), 0.5);
        assert_eq!(task.spec().prompt, "fix the tests");
        // What the session had seen of its runs isn't news again.
        assert!(task.events().is_empty());
    }

    #[test]
    fn the_tools_claude_uses_wait_for_the_session_to_take_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut task = task_in(dir.path(), BTreeMap::new(), &[], None);
        let reading = Reading {
            number: 0,
            term: task.term.clone(),
            runs: task.runs.clone(),
            state: task.state.clone(),
            spending: task.spending.clone(),
        };
        let line = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Testing."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        reading.converse(&mut task.runs.lock().unwrap(), line);
        let used = ToolUse {
            name: "Bash".into(),
            gist: "cargo test".into(),
        };
        assert_eq!(task.tools_used(), [used]);
        assert!(task.tools_used().is_empty());
    }

    /// A task at rest in `dir`, in conversation `conversation`, with
    /// `env` and its own arguments `args`.
    fn task_in(
        dir: &std::path::Path,
        env: BTreeMap<String, String>,
        args: &[&str],
        conversation: Option<&str>,
    ) -> Task {
        let db = crate::db::Db::open(&dir.join("crystal.sock")).unwrap();
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        Task::new(
            TaskSpec {
                prompt: "fix the tests".into(),
                args: args.clone(),
            },
            args,
            dir.to_path_buf(),
            env,
            Arc::new(Term::without_terminal()),
            Arc::new(Mutex::new(State::Running)),
            Arc::new(Spending::new(db)),
            conversation.map(String::from),
        )
    }

    fn args_of(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn each_claude_starts_as_the_settings_say_unless_the_task_says_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let settings = TaskSettings {
            permission_mode: "acceptEdits".into(),
            allowed_tools: vec!["Bash(make:*)".into(), "Edit".into()],
            ..TaskSettings::default()
        };
        let task = task_in(
            dir.path(),
            BTreeMap::new(),
            &["--allowedTools", "Bash(crystal ls:*)"],
            None,
        );
        let args = args_of(&task.command(Some("conv-1"), &settings));
        let after_print = &args[claude_stream::ARGS.len()..];
        assert_eq!(
            after_print,
            [
                "--max-budget-usd",
                "5",
                "--permission-mode",
                "acceptEdits",
                "--resume",
                "conv-1",
                "--allowedTools",
                "Bash(make:*),Edit",
                "Bash(crystal ls:*)",
            ]
        );
        // What the task's own arguments say wins.
        let own = task_in(
            dir.path(),
            BTreeMap::new(),
            &["--permission-mode=plan", "--max-budget-usd", "1"],
            None,
        );
        let args = args_of(&own.command(None, &TaskSettings::default()));
        assert!(!args[..claude_stream::ARGS.len() + 2].contains(&"--permission-mode".into()));
        assert_eq!(
            &args[claude_stream::ARGS.len()..],
            ["--permission-mode=plan", "--max-budget-usd", "1"]
        );
        assert!(given(&["--max-turns=3".into()], "--max-turns"));
        assert!(!given(&["--max-turnsx".into()], "--max-turns"));
    }

    #[test]
    fn a_task_in_a_terminal_leaves_out_what_only_claude_p_takes() {
        let args: Vec<String> = [
            "--model",
            "opus",
            "--max-budget-usd",
            "2",
            "--max-turns=9",
            "-p",
            "--resume",
            "old",
            "--allowedTools",
            "Edit",
            "--verbose",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        assert_eq!(
            terminal_args(&args),
            ["--model", "opus", "--allowedTools", "Edit", "--verbose"]
        );
    }

    #[test]
    fn the_context_is_of_what_the_model_takes_as_the_result_said() {
        let mut runs = Runs::default();
        assert_eq!(runs.context(), None);
        runs.context = Some((50_000, Some("claude-sonnet-5-5".into())));
        // Before a result says, Claude's models take 200k.
        assert_eq!(runs.context().unwrap().window, CONTEXT_WINDOW);
        assert_eq!(runs.context().unwrap().percent(), 25);
        runs.model = Some("claude-sonnet-5-5[1m]".into());
        assert_eq!(runs.context().unwrap().window, MILLION_WINDOW);
        runs.windows.insert("claude-sonnet-5-5".into(), 400_000);
        assert_eq!(
            runs.context(),
            Some(ContextUse {
                tokens: 50_000,
                window: 400_000
            })
        );
    }

    #[test]
    fn after_a_restart_a_task_is_drawn_again_from_claude_s_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("claude/projects/-work-app");
        std::fs::create_dir_all(&kept).unwrap();
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"fix the tests"},"isSidechain":false}"#,
            r#"{"type":"assistant","message":{"model":"m","content":[{"type":"text","text":"Running them."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}],"usage":{"input_tokens":10,"cache_read_input_tokens":20000,"output_tokens":30}},"isSidechain":false}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"test result: ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"A subagent's."}]},"isSidechain":true}"#,
        ];
        std::fs::write(kept.join("conv-1.jsonl"), lines.join("\n")).unwrap();
        let env = BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.path().join("claude").display().to_string(),
        )]);
        let task = task_in(dir.path(), env.clone(), &[], Some("conv-1"));
        task.draw_kept();
        let screen = task.term.rows(true).join("\n");
        assert!(screen.contains("Claude Code's transcript"), "{screen}");
        assert!(screen.contains("> fix the tests"), "{screen}");
        assert!(screen.contains("Running them."), "{screen}");
        assert!(screen.contains("▸ Bash cargo test"), "{screen}");
        assert!(screen.contains("└ test result: ok"), "{screen}");
        assert!(!screen.contains("A subagent's."), "{screen}");
        assert!(screen.contains("`crystal send` carries"), "{screen}");
        assert_eq!(task.context().unwrap().tokens, 20_040);

        // With no transcript, it says what's gone.
        let gone = task_in(dir.path(), env, &[], Some("conv-2"));
        gone.draw_kept();
        let screen = gone.term.rows(true).join("\n");
        assert!(
            screen.contains("what this task showed before is gone"),
            "{screen}"
        );
    }

    #[test]
    fn only_the_end_of_a_long_transcript_is_read_from_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.jsonl");
        let line = "x".repeat(1000);
        let text: String = (0..KEPT_BYTES / 1000 + 10)
            .map(|n| format!("{n} {line}\n"))
            .collect();
        std::fs::write(&path, &text).unwrap();
        let lines = kept_lines(&path).unwrap();
        assert!(lines.iter().all(|kept| kept.ends_with(&line)));
        assert!(
            lines
                .last()
                .unwrap()
                .starts_with(&format!("{} ", KEPT_BYTES / 1000 + 9))
        );
        let bytes: usize = lines.iter().map(|kept| kept.len() + 1).sum();
        assert!(bytes as u64 <= KEPT_BYTES);
    }

    #[test]
    fn the_last_lines_of_errors_skip_the_blank_ones() {
        let mut errors = Errors::default();
        errors.take(b"one\n\ntwo\r\nthr");
        errors.take(b"ee\n\nfour");
        assert_eq!(errors.lines(), ["one", "two", "three", "four"]);

        let mut errors = Errors::default();
        for line in 0..9 {
            errors.take(format!("line {line}\n").as_bytes());
        }
        assert_eq!(errors.lines().len(), ERROR_LINES);
    }
}
